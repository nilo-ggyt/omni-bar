/* stdin 控制的非 GUI 内存 fixture；只创建本进程映射，不查询其他进程。 */
#include <errno.h>
#include <inttypes.h>
#include <mach/mach_time.h>
#include <poll.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/mman.h>
#include <unistd.h>

#define MIB ((size_t)1024 * 1024)
#define LIFETIME_NS UINT64_C(120000000000)
#define MAX_COMMANDS 16

/* 已映射区域由唯一局部 owner 持有；munmap 后不再保留可访问指针。 */
struct allocation { void *address; size_t bytes; };

/* 所有正常/失败退出都执行同一个有界释放路径，不累积历次映射。 */
static bool release_allocation(struct allocation *allocation) {
  if (!allocation->address) {
    /* 未分配时 FREE 幂等，不伪造已释放映射的第二次 munmap。 */
    return true;
  }
  if (munmap(allocation->address, allocation->bytes)) {
    /* 释放失败必须报告失败；不能丢失仍需清理的 owner。 */
    return false;
  }
  allocation->address = NULL;
  allocation->bytes = 0;
  return true;
}

/* mmap 的有限页逐页写入，避免只预留虚拟地址而没有 physical-footprint 负载。 */
static bool allocate_and_touch(struct allocation *allocation, size_t bytes) {
  if (!release_allocation(allocation)) return false; /* 旧映射未释放就拒绝替换。 */
  long raw_page_size = sysconf(_SC_PAGESIZE);
  if (raw_page_size <= 0 || (uint64_t)raw_page_size > bytes) {
    /* 未知页大小不能被静默替换成猜测值。 */
    return false;
  }
  void *address = mmap(NULL, bytes, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANON, -1, 0);
  if (address == MAP_FAILED) return false; /* 系统分配失败不进入触页循环。 */
  allocation->address = address;
  allocation->bytes = bytes;
  volatile unsigned char *payload = address;
  for (size_t offset = 0; offset < bytes; offset += (size_t)raw_page_size) {
    /* volatile 写入阻止编译器消除；固定非零值不代表任何用户内容。 */
    payload[offset] = 0xa5;
  }
  payload[bytes - 1] = 0x5a;
  return true;
}

/* 完整行 ack 是阶段转换的同步点，不用猜测 sleep 等待分配完成。 */
static bool acknowledge(const char *stage, const struct allocation *allocation) {
  return printf("{\"stage\":\"%s\",\"mapped_bytes\":\"%zu\"}\n", stage, allocation->bytes) > 0 &&
         fflush(stdout) == 0;
}

/* 有界命令读取在等待输入时仍检查 continuous 截止时间，stdin 关闭也会退出。 */
static int read_command(char *line, size_t capacity, uint64_t origin,
                         const mach_timebase_info_data_t *base) {
  size_t used = 0;
  for (;;) {
    uint64_t now = mach_continuous_time();
    if (now < origin || (__uint128_t)(now - origin) * base->numer / base->denom >= LIFETIME_NS) {
      /* 到达独立 lifetime 上限时不能继续保留映射等待父进程。 */
      return -1;
    }
    struct pollfd input = {.fd = STDIN_FILENO, .events = POLLIN, .revents = 0};
    int ready = poll(&input, 1, 250);
    if (ready < 0) {
      /* 信号只中断本次等待；其他错误终止控制会话。 */
      if (errno == EINTR) continue;
      return -1;
    }
    if (!ready) continue; /* 最多 250ms 后重查 deadline，不进行后台业务工作。 */
    if (input.revents & (POLLERR | POLLNVAL)) return -1; /* 损坏 stdin 不再接收命令。 */
    unsigned char byte = 0;
    ssize_t count = read(STDIN_FILENO, &byte, 1);
    if (count < 0 && errno == EINTR) continue; /* 保留已读取的有限行。 */
    if (count != 1) return -1; /* EOF/其他错误均触发释放。 */
    if (byte == '\n') {
      /* 完成单行后才允许调用者执行一个已知阶段。 */
      line[used] = 0;
      return 0;
    }
    if (byte == 0 || used + 1 >= capacity) return -1; /* 超长/嵌入 NUL 不截断执行。 */
    line[used++] = (char)byte;
  }
}

/* 无 PID 参数、GUI、文件输出或系统设置路径；唯一控制权限来自父进程拥有的 stdin。 */
int main(void) {
  mach_timebase_info_data_t base = {0};
  struct allocation allocation = {0};
  if (mach_timebase_info(&base) != KERN_SUCCESS || !base.numer || !base.denom) return 65;
  uint64_t origin = mach_continuous_time();
  int status = 65;
  if (!acknowledge("ready", &allocation)) return 74; /* 父进程不可读时没有已分配资源。 */
  for (unsigned index = 0; index < MAX_COMMANDS; ++index) {
    /* 有限命令数和独立 deadline 同时限制控制程序生命周期。 */
    char command[16];
    if (read_command(command, sizeof(command), origin, &base)) break;
    if (!strcmp(command, "ALLOC32")) {
      /* 32MiB 是明确触页负载，实际 footprint 增量由 OS 测量，不硬断言相等。 */
      if (!allocate_and_touch(&allocation, 32 * MIB) || !acknowledge("allocated32", &allocation)) break;
    } else if (!strcmp(command, "ALLOC192")) {
      /* 可选192MiB阶段明确越过150MB新上限，释放后仍须保留实际历史峰值。 */
      if (!allocate_and_touch(&allocation, 192 * MIB) || !acknowledge("allocated192", &allocation)) break;
    } else if (!strcmp(command, "FREE")) {
      /* ack 只在 munmap 成功后发布；lifetime peak 不会因此被重置。 */
      if (!release_allocation(&allocation) || !acknowledge("freed", &allocation)) break;
    } else if (!strcmp(command, "EXIT")) {
      /* 正常退出先释放映射，再确认阶段；父进程还必须 wait 获得实际退出码。 */
      status = release_allocation(&allocation) && acknowledge("exiting", &allocation) ? 0 : 74;
      break;
    } else {
      /* 未知命令不扩展成任意大小分配或 shell 操作。 */
      break;
    }
  }
  if (!release_allocation(&allocation)) status = 74; /* 任何提前失败也保留释放错误。 */
  return status;
}
