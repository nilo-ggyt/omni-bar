/*
 * 对一个明确选择的同用户 PID 进行有界 physical-footprint 采样。
 * 本工具只报告单进程事实；进程集合归属、完整工作负载和持续增长仍未证明。
 * libproc.h 将其接口标为 private/subject to change；只使用当前 SDK 已声明的 ABI。
 */
#include <errno.h>
#include <fcntl.h>
#include <inttypes.h>
#include <libproc.h>
#include <limits.h>
#include <mach/mach_time.h>
#include <stdbool.h>
#include <stdarg.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/resource.h>
#include <sys/stat.h>
#include <time.h>
#include <unistd.h>

#define MAX_RECORDS 65536u
#define MAX_OUTPUT_BYTES (64u * 1024u * 1024u)
#define MAX_LINE_BYTES 16384u
#define FOOTER_RESERVE 2048u
#define BUDGET_BYTES UINT64_C(100000000)
#define NS_PER_MS UINT64_C(1000000)
#define NS_PER_SECOND UINT64_C(1000000000)

enum { EXIT_BUDGET = 1, EXIT_USAGE = 64, EXIT_INVALID = 65, EXIT_IO = 74,
       EXIT_MEMBERSHIP_UNVERIFIED = 78 };

/* 出生坐标和路径共同固定选择，不以 PID、名称或 PPID 代替进程身份。 */
struct identity {
  pid_t pid;
  uid_t uid;
  uint64_t birth_sec;
  uint64_t birth_usec;
  uint64_t start_ticks;
  char executable[PROC_PIDPATHINFO_MAXSIZE];
};

/* 两侧时钟括号保留查询耗时和时钟读取不确定度，不伪装成原子快照。 */
struct point {
  struct identity identity;
  uint64_t current_bytes;
  uint64_t lifetime_peak_bytes;
  uint64_t before_ticks;
  uint64_t before_end_ticks;
  uint64_t awake_before_ticks;
  uint64_t after_begin_ticks;
  uint64_t after_ticks;
  uint64_t awake_after_ticks;
};

/* 参数和记录数在任何目标查询或输出文件创建前完成验证。 */
struct options {
  bool identify;
  struct identity expected;
  uint64_t duration_ns;
  uint64_t interval_ns;
  uint64_t planned_records;
  const char *output;
};

/* 累计状态只保留标量；不缓存整个样本序列。 */
struct summary {
  uint64_t records;
  uint64_t max_current;
  uint64_t max_lifetime;
  uint64_t max_gap_ns;
  uint64_t max_query_ns;
};

/* 每条记录在固定缓冲中完整编码后写出，预留错误 footer 的有限空间。 */
struct writer { FILE *file; uint64_t bytes; };

/* 只接受完整十进制无符号输入；拒绝符号、空白、NaN 和部分解析。 */
static bool parse_u64(const char *text, uint64_t min, uint64_t max, uint64_t *out) {
  if (!text || !*text) {
    /* 缺值不能隐式选中 PID 0 或零时长。 */
    return false;
  }
  uint64_t value = 0;
  for (const unsigned char *cursor = (const unsigned char *)text; *cursor; ++cursor) {
    /* 在乘加前验证每一位，避免溢出及 strtoull 的负数转换。 */
    if (*cursor < '0' || *cursor > '9' || value > (UINT64_MAX - (*cursor - '0')) / 10) {
      return false;
    }
    value = value * 10 + (*cursor - '0');
  }
  if (value < min || value > max) {
    /* 超出 ABI 或运行预算的数值不能到达采样循环。 */
    return false;
  }
  *out = value;
  return true;
}

/* 不启动任何工具、原生应用或权限查询；help 是纯参数说明。 */
static void usage(void) {
  puts("usage: memory-sampler identify --pid PID\n"
       "       memory-sampler sample --pid PID --birth-sec SEC --birth-usec USEC\n"
       "         --start-abstime TICKS --executable ABS_PATH --duration-ms MS\n"
       "         --interval-ms MS --output NEW_ABS_PATH\n"
       "limits: duration <= 3600000ms; interval 10..10000ms; <=65536 records; 64MiB output\n"
       "scope: single PID only; whole-product membership remains unverified (valid collection exits 78)");
}

/* 严格参数集合防止重复字段覆盖选择，也不提供声称 whole-product 的开关。 */
static bool parse_options(int argc, char **argv, struct options *options) {
  memset(options, 0, sizeof(*options));
  if (argc < 2 || (strcmp(argv[1], "identify") && strcmp(argv[1], "sample"))) {
    /* 模式必须明确，不能因拼写错误落入实际查询。 */
    return false;
  }
  options->identify = !strcmp(argv[1], "identify");
  uint32_t seen = 0;
  for (int index = 2; index < argc; index += 2) {
    /* 所有字段都必须有值；未知和重复参数都失败，不执行恢复性查询。 */
    if (index + 1 >= argc) return false;
    const char *key = argv[index], *value = argv[index + 1];
    uint32_t bit = 0;
    uint64_t parsed = 0;
    if (!strcmp(key, "--pid")) {
      /* 只接受可由 pid_t 表示的正 PID，排除进程组和任意进程选择。 */
      bit = 1u;
      if (!parse_u64(value, 1, INT_MAX, &parsed)) return false;
      options->expected.pid = (pid_t)parsed;
    } else if (!strcmp(key, "--birth-sec")) {
      /* 出生时间不能为空；它来自操作者先前明确核对的 identify。 */
      bit = 2u;
      if (!parse_u64(value, 1, UINT64_MAX, &options->expected.birth_sec)) return false;
    } else if (!strcmp(key, "--birth-usec")) {
      /* BSD 微秒字段只接受一个秒内的精确范围。 */
      bit = 4u;
      if (!parse_u64(value, 0, 999999, &options->expected.birth_usec)) return false;
    } else if (!strcmp(key, "--start-abstime")) {
      /* 独立的 rusage 出生坐标不能用 PID 相等替代。 */
      bit = 8u;
      if (!parse_u64(value, 1, UINT64_MAX, &options->expected.start_ticks)) return false;
    } else if (!strcmp(key, "--executable")) {
      /* 固定长度保存绝对路径字节；超长路径不能截断后比较。 */
      bit = 16u;
      size_t length = strlen(value);
      if (value[0] != '/' || length >= sizeof(options->expected.executable)) return false;
      memcpy(options->expected.executable, value, length + 1);
    } else if (!strcmp(key, "--duration-ms")) {
      /* 整数毫秒和一小时上限在乘法前约束，不允许 NaN 或浮点转换。 */
      bit = 32u;
      if (!parse_u64(value, 10, 3600000, &parsed)) return false;
      options->duration_ns = parsed * NS_PER_MS;
    } else if (!strcmp(key, "--interval-ms")) {
      /* 10ms 下限约束频率，10s 上限避免将稀疏终点包装成瞬态观测。 */
      bit = 64u;
      if (!parse_u64(value, 10, 10000, &parsed)) return false;
      options->interval_ns = parsed * NS_PER_MS;
    } else if (!strcmp(key, "--output")) {
      /* 输出路径必须显式且绝对，不把当前工作目录当作隐含发布位置。 */
      bit = 128u;
      if (value[0] != '/' || strlen(value) >= PATH_MAX) return false;
      options->output = value;
    } else {
      /* 不允许别名、位置参数或未知字段扩大采样权限。 */
      return false;
    }
    /* 重复参数即使值相同也拒绝，避免覆盖前一选择的歧义。 */
    if (seen & bit) return false;
    seen |= bit;
  }
  if (options->identify) {
    /* 身份模式只接受 PID，不创建文件，也不承诺进程集合归属。 */
    return seen == 1u;
  }
  /* sample 必须携带全部身份与预算，且至少有一个完整间隔。 */
  if (seen != 255u || options->duration_ns < options->interval_ns) return false;
  options->planned_records = (options->duration_ns + options->interval_ns - 1) /
                             options->interval_ns + 1;
  return options->planned_records <= MAX_RECORDS;
}

/* 宽整数先乘后除，精确保留字节和时钟标量，不假设 Apple Silicon 的 timebase 为 1:1。 */
static bool ticks_to_ns(uint64_t ticks, const mach_timebase_info_data_t *base, uint64_t *ns) {
  /* 不支持的 timebase 不得退化为隐含 1:1。 */
  if (!base->numer || !base->denom) return false;
  __uint128_t value = (__uint128_t)ticks * base->numer / base->denom;
  /* 结果仍须适配输出协议的 u64，失败时不发布截断值。 */
  if (value > UINT64_MAX) return false;
  *ns = (uint64_t)value;
  return true;
}

/* 同一 Mach 域内只比较有序差值，原始 absolute 与 continuous 的 epoch 不直接相减。 */
static bool delta_ns(uint64_t before, uint64_t after,
                     const mach_timebase_info_data_t *base, uint64_t *ns) {
  /* 时钟倒退是无效观测，不能无符号回绕成巨大间隙。 */
  if (after < before) return false;
  return ticks_to_ns(after - before, base, ns);
}

/* 枚举字段完整且出生坐标合理时才承认一次 BSD 身份读取。 */
static bool read_bsd(pid_t pid, struct proc_bsdinfo *info) {
  /* SDK 大小和已初始化、自然对齐的栈存储构成该 C ABI 的缓冲区约束。 */
  memset(info, 0, sizeof(*info));
  return proc_pidinfo(pid, PROC_PIDTBSDINFO, 0, info, sizeof(*info)) == sizeof(*info) &&
         info->pbi_pid == (uint32_t)pid && info->pbi_start_tvsec != 0 &&
         info->pbi_start_tvusec < 1000000 && info->pbi_uid == getuid();
}

/* 目标路径由前后两次出生信息及两次 rusage start 身份包围，PID 复用不能继承授权。 */
static bool capture_point(pid_t pid, struct point *point) {
  memset(point, 0, sizeof(*point));
  point->before_ticks = mach_continuous_time();
  point->awake_before_ticks = mach_absolute_time();
  point->before_end_ticks = mach_continuous_time();
  struct proc_bsdinfo first, last;
  struct rusage_info_v4 first_usage = {0}, last_usage = {0};
  char last_path[PROC_PIDPATHINFO_MAXSIZE] = {0};
  /* 只传当前 SDK 声明的完整 v4 存储，两个路径缓冲均有确定上限。
   * 查询前后身份和路径必须相同；这不是对同用户恶意 exec/ABA 的原子授权证明。 */
  if (!read_bsd(pid, &first) ||
      proc_pid_rusage(pid, RUSAGE_INFO_V4, (rusage_info_t *)&first_usage) != 0 ||
      proc_pidpath(pid, point->identity.executable, sizeof(point->identity.executable)) <= 0 ||
      !read_bsd(pid, &last) ||
      proc_pid_rusage(pid, RUSAGE_INFO_V4, (rusage_info_t *)&last_usage) != 0 ||
      proc_pidpath(pid, last_path, sizeof(last_path)) <= 0) {
    /* 不可读、退出及 ABI 失败都是缺样，不以零内存替代。 */
    return false;
  }
  point->after_begin_ticks = mach_continuous_time();
  point->awake_after_ticks = mach_absolute_time();
  point->after_ticks = mach_continuous_time();
  if (first.pbi_start_tvsec != last.pbi_start_tvsec ||
      first.pbi_start_tvusec != last.pbi_start_tvusec || first.pbi_uid != last.pbi_uid ||
      !first_usage.ri_proc_start_abstime ||
      first_usage.ri_proc_start_abstime != last_usage.ri_proc_start_abstime ||
      first_usage.ri_proc_exit_abstime || last_usage.ri_proc_exit_abstime ||
      !memchr(point->identity.executable, 0, sizeof(point->identity.executable)) ||
      !memchr(last_path, 0, sizeof(last_path)) ||
      point->identity.executable[0] != '/' || strcmp(point->identity.executable, last_path)) {
    /* 已退出进程可由 rusage 返回，但不构成本工具要求的完整存活观测窗口。 */
    return false;
  }
  point->identity.pid = pid;
  point->identity.uid = last.pbi_uid;
  point->identity.birth_sec = last.pbi_start_tvsec;
  point->identity.birth_usec = last.pbi_start_tvusec;
  point->identity.start_ticks = last_usage.ri_proc_start_abstime;
  point->current_bytes = last_usage.ri_phys_footprint;
  point->lifetime_peak_bytes = last_usage.ri_lifetime_max_phys_footprint;
  return true;
}

/* 不经 UTF-8 猜测编码文件系统路径；十六进制仍是私有本地身份数据，不能自动发布。 */
static void path_hex(const char *path, char *encoded) {
  const char digits[] = "0123456789abcdef";
  size_t length = strlen(path);
  for (size_t index = 0; index < length; ++index) {
    unsigned char value = (unsigned char)path[index];
    encoded[index * 2] = digits[value >> 4];
    encoded[index * 2 + 1] = digits[value & 15];
  }
  encoded[length * 2] = 0;
}

/* 匹配操作者选择的全部身份坐标；UID 相同不代表属于同一个产品。 */
static bool same_identity(const struct identity *observed, const struct identity *expected) {
  return observed->pid == expected->pid && observed->uid == expected->uid &&
         observed->birth_sec == expected->birth_sec && observed->birth_usec == expected->birth_usec &&
         observed->start_ticks == expected->start_ticks &&
         !strcmp(observed->executable, expected->executable);
}

/* 以包含 awake 读取的 continuous 括号中点估计样本时间，保留完整括号用于误差说明。 */
static uint64_t midpoint(const struct point *point) {
  return point->before_ticks + (point->before_end_ticks - point->before_ticks) / 2;
}

/* 原始时钟次序必须先于 midpoint 的无符号减法验证。 */
static bool ordered_clock(const struct point *point) {
  return point->before_ticks <= point->before_end_ticks &&
         point->before_end_ticks <= point->after_begin_ticks &&
         point->after_begin_ticks <= point->after_ticks &&
         point->awake_before_ticks <= point->awake_after_ticks;
}

/* 从同域差值中扣除完整读取括号，只有确定超出误差的部分才报告休眠下界。 */
static bool suspended_ns(uint64_t continuous_delta, uint64_t awake_delta, uint64_t uncertainty,
                         const mach_timebase_info_data_t *base, uint64_t *ns) {
  uint64_t lower = 0;
  if (continuous_delta > awake_delta && continuous_delta - awake_delta > uncertainty) {
    /* 只保留超过全部读取误差的正差，避免正常读取顺序产生假休眠。 */
    lower = continuous_delta - awake_delta - uncertainty;
  }
  return ticks_to_ns(lower, base, ns);
}

/* 纯策略函数供合成 fixture 验证身份、时钟、间隙和 peak 不变量，无系统查询。 */
static const char *validate_point(const struct point *point, const struct point *previous,
                                  const struct identity *expected, uint64_t origin,
                                  uint64_t planned_ns, uint64_t interval_ns,
                                  const mach_timebase_info_data_t *base,
                                  uint64_t *elapsed, uint64_t *gap, uint64_t *span,
                                  uint64_t *suspension) {
  /* 路径、出生坐标及 UID 任一变化均终止原选择，不重新选中一个同名进程。 */
  if (!same_identity(&point->identity, expected)) return "identity_changed";
  /* 括号有序时，其两段宽度之和不超过整个 query，不会溢出。 */
  if (!ordered_clock(point) || (previous && !ordered_clock(previous))) return "clock_order";
  /* 所有输出时间须可精确表示为 u64 ns；不能用饱和值掩盖不支持。 */
  if (!delta_ns(origin, midpoint(point), base, elapsed) ||
      !delta_ns(point->before_ticks, point->after_ticks, base, span)) return "clock_conversion";
  /* 已声明的 peak 也要验证关系，避免不支持字段被解释为已知零峰值。 */
  if (point->lifetime_peak_bytes < point->current_bytes) return "unsupported_peak_counter";
  uint64_t query_uncertainty = (point->before_end_ticks - point->before_ticks) +
                               (point->after_ticks - point->after_begin_ticks);
  /* 检查 query 内休眠；样本间休眠随后使用同域增量独立复核。 */
  if (!suspended_ns(point->after_ticks - point->before_ticks,
                    point->awake_after_ticks - point->awake_before_ticks,
                    query_uncertainty, base, suspension)) return "clock_conversion";
  *gap = 0;
  if (previous) {
    /* 上一个有效样本提供同一 epoch 的间隙、时钟和 lifetime 单调性基线。 */
    if (midpoint(point) < midpoint(previous) ||
        point->awake_before_ticks < previous->awake_before_ticks) return "clock_order";
    /* 同一存活 epoch 的 lifetime peak 不应下降；下降是计数器语义失败。 */
    if (point->lifetime_peak_bytes < previous->lifetime_peak_bytes) return "peak_moved_backwards";
    uint64_t current_width = point->before_end_ticks - point->before_ticks;
    uint64_t previous_width = previous->before_end_ticks - previous->before_ticks;
    /* 不可信 fixture 的极大括号同样必须失败，不能用溢出低估读取误差。 */
    if (UINT64_MAX - current_width < previous_width) return "clock_conversion";
    uint64_t uncertainty = current_width + previous_width;
    uint64_t gap_suspension = 0;
    /* 转换失败与无法比较的跨样本时钟都不能留下有效 gap=0。 */
    if (!delta_ns(midpoint(previous), midpoint(point), base, gap) ||
        !suspended_ns(midpoint(point) - midpoint(previous),
                      point->awake_before_ticks - previous->awake_before_ticks,
                      uncertainty, base, &gap_suspension)) return "clock_conversion";
    /* 报告 query 内和两次查询间已证明的较大休眠下界。 */
    if (gap_suspension > *suspension) *suspension = gap_suspension;
  }
  /* 任意可证休眠污染整个窗口；零值仅代表没有超过读取不确定度的证据。 */
  if (*suspension) return "suspension_observed";
  /* 查询本身占满一个间隔时，声明频率无法成立。 */
  if (*span > interval_ns) return "query_exceeds_interval";
  /* 不插值缺失点；绝对计划迟到还会独立拒绝不足以构成两倍 gap 的失期。 */
  if (*gap > interval_ns * 2) return "sampling_gap";
  /* 每次采样只能在声明截止点之后开始，不能用提前读取伪造末端覆盖。 */
  if (*elapsed < planned_ns) return "sample_before_deadline";
  /* 迟到达到一个 interval 即缺少计划点，不能突发追赶后声称连续覆盖。 */
  if (*elapsed > planned_ns && *elapsed - planned_ns >= interval_ns) return "missed_deadline";
  return NULL;
}

/* 固定缓冲和总字节预算使格式化、文件增长及内存所有权都有上限。 */
static bool write_record(struct writer *writer, bool footer, const char *format, ...) {
  char line[MAX_LINE_BYTES];
  va_list arguments;
  va_start(arguments, format);
  int size = vsnprintf(line, sizeof(line), format, arguments);
  va_end(arguments);
  uint64_t limit = MAX_OUTPUT_BYTES - (footer ? 0 : FOOTER_RESERVE);
  /* 格式化截断或预算用尽时不写入半条新记录。 */
  if (size < 0 || (size_t)size >= sizeof(line) || writer->bytes + (uint64_t)size > limit) return false;
  /* 实际 I/O 可产生部分文件；任何短写/flush 失败都禁止发布完整观测。 */
  if (fwrite(line, 1, (size_t)size, writer->file) != (size_t)size || fflush(writer->file)) return false;
  writer->bytes += (uint64_t)size;
  return true;
}

/* header 明确区分数据有效性和整产品验收，调用者不能通过参数自封完整归属。 */
static bool write_header(struct writer *writer, const struct options *options,
                          const mach_timebase_info_data_t *base, uint64_t origin) {
  char encoded[PROC_PIDPATHINFO_MAXSIZE * 2 + 1];
  path_hex(options->expected.executable, encoded);
  return write_record(writer, false,
    "{\"type\":\"header\",\"schema\":\"omni-process-memory-v1\",\"scope\":\"single_pid\","
    "\"membership\":\"unverified\",\"whole_product_budget_pass\":false,\"pid\":%d,\"uid\":%u,"
    "\"birth_sec\":\"%" PRIu64 "\",\"birth_usec\":\"%" PRIu64 "\",\"start_abstime\":\"%" PRIu64 "\","
    "\"executable_hex\":\"%s\",\"origin_continuous_ticks\":\"%" PRIu64 "\","
    "\"duration_ns\":\"%" PRIu64 "\",\"interval_ns\":\"%" PRIu64 "\",\"planned_records\":%" PRIu64 ","
    "\"timebase_numer\":%u,\"timebase_denom\":%u,\"budget_bytes\":\"100000000\","
    "\"api\":\"SDK_declared_libproc_private_subject_to_change\",\"clock\":\"mach_continuous_time\"}\n",
    options->expected.pid, (unsigned)options->expected.uid, options->expected.birth_sec,
    options->expected.birth_usec, options->expected.start_ticks, encoded, origin,
    options->duration_ns, options->interval_ns, options->planned_records, base->numer, base->denom);
}

/* 所有内存值和 Mach ticks 使用十进制字符串，避免下游二进制浮点丢失整数精度。 */
static bool write_sample(struct writer *writer, const struct point *point, uint64_t index,
                          uint64_t planned, uint64_t elapsed, uint64_t gap, uint64_t span,
                          uint64_t suspension) {
  return write_record(writer, false,
    "{\"type\":\"sample\",\"index\":%" PRIu64 ",\"planned_ns\":\"%" PRIu64 "\","
    "\"elapsed_ns\":\"%" PRIu64 "\",\"gap_ns\":\"%" PRIu64 "\",\"query_span_ns\":\"%" PRIu64 "\","
    "\"suspension_lower_bound_ns\":\"%" PRIu64 "\",\"physical_footprint_bytes\":\"%" PRIu64 "\","
    "\"lifetime_peak_bytes\":\"%" PRIu64 "\",\"identity_matches_anchor\":true,\"pid\":%d,\"uid\":%u,\"birth_sec\":\"%" PRIu64 "\","
    "\"birth_usec\":\"%" PRIu64 "\",\"start_abstime\":\"%" PRIu64 "\","
    "\"continuous_before_ticks\":\"%" PRIu64 "\",\"continuous_before_end_ticks\":\"%" PRIu64 "\","
    "\"awake_before_ticks\":\"%" PRIu64 "\",\"continuous_after_begin_ticks\":\"%" PRIu64 "\","
    "\"continuous_after_ticks\":\"%" PRIu64 "\",\"awake_after_ticks\":\"%" PRIu64 "\"}\n",
    index, planned, elapsed, gap, span, suspension, point->current_bytes, point->lifetime_peak_bytes,
    point->identity.pid, (unsigned)point->identity.uid, point->identity.birth_sec, point->identity.birth_usec, point->identity.start_ticks,
    point->before_ticks, point->before_end_ticks, point->awake_before_ticks,
    point->after_begin_ticks, point->after_ticks, point->awake_after_ticks);
}

/* 失败的原始时钟与间隙仍写入独立记录；它不进入有效样本摘要，也不能授权通过。 */
static bool write_failure(struct writer *writer, const struct point *point, uint64_t index,
                           uint64_t planned, const char *failure, uint64_t elapsed,
                           uint64_t gap, uint64_t span, uint64_t suspension) {
  return write_record(writer, false,
    "{\"type\":\"failure\",\"failure\":\"%s\",\"index\":%" PRIu64 ",\"planned_ns\":\"%" PRIu64 "\","
    "\"elapsed_ns\":\"%" PRIu64 "\",\"gap_ns\":\"%" PRIu64 "\",\"query_span_ns\":\"%" PRIu64 "\","
    "\"suspension_lower_bound_ns\":\"%" PRIu64 "\",\"raw_observation_valid\":false,"
    "\"continuous_before_ticks\":\"%" PRIu64 "\",\"continuous_before_end_ticks\":\"%" PRIu64 "\","
    "\"awake_before_ticks\":\"%" PRIu64 "\",\"continuous_after_begin_ticks\":\"%" PRIu64 "\","
    "\"continuous_after_ticks\":\"%" PRIu64 "\",\"awake_after_ticks\":\"%" PRIu64 "\"}\n",
    failure, index, planned, elapsed, gap, span, suspension,
    point->before_ticks, point->before_end_ticks, point->awake_before_ticks,
    point->after_begin_ticks, point->after_ticks, point->awake_after_ticks);
}

/* 没有完整归属和工作负载证据时，即使样本低于上限也永远不发出整产品 PASS。 */
static bool write_footer(struct writer *writer, const struct summary *summary, bool complete,
                          const char *failure) {
  return write_record(writer, true,
    "{\"type\":\"complete\",\"collection_complete\":%s,\"failure\":\"%s\",\"records\":%" PRIu64 ","
    "\"max_current_bytes\":\"%" PRIu64 "\",\"max_lifetime_peak_bytes\":\"%" PRIu64 "\","
    "\"max_gap_ns\":\"%" PRIu64 "\",\"max_query_span_ns\":\"%" PRIu64 "\","
    "\"single_pid_current_within_budget\":%s,\"single_pid_lifetime_within_budget\":%s,"
    "\"membership\":\"unverified\",\"steady_growth_assessed\":false,\"whole_product_budget_pass\":false,"
    "\"durability_certified\":false}\n",
    complete ? "true" : "false", failure ? failure : "none", summary->records,
    summary->max_current, summary->max_lifetime, summary->max_gap_ns, summary->max_query_ns,
    complete && summary->max_current <= BUDGET_BYTES ? "true" : "false",
    complete && summary->max_lifetime <= BUDGET_BYTES ? "true" : "false");
}

/* 按绝对 continuous 截止时间等待；休眠/调度迟到仍由采样有效性检查揭示。 */
static bool wait_until(uint64_t origin, uint64_t target_ns, const mach_timebase_info_data_t *base) {
  for (;;) {
    uint64_t elapsed = 0;
    /* 等待期间出现倒退或不支持的换算时，不继续等待一个错误截止点。 */
    if (!delta_ns(origin, mach_continuous_time(), base, &elapsed)) return false;
    /* 已达到唯一绝对截止点；不因 EINTR 重置整个声明时长。 */
    if (elapsed >= target_ns) return true;
    uint64_t remaining = target_ns - elapsed;
    struct timespec request = { .tv_sec = (time_t)(remaining / NS_PER_SECOND),
                               .tv_nsec = (long)(remaining % NS_PER_SECOND) };
    /* 信号后重算剩余绝对时间；其他等待错误使观测无效。 */
    if (nanosleep(&request, NULL) && errno != EINTR) return false;
  }
}

/* 仅独占创建新 0600 普通文件；不覆盖、删除或跟随既存最终路径。 */
static FILE *create_output(const char *path) {
  int fd = open(path, O_WRONLY | O_CREAT | O_EXCL | O_NOFOLLOW | O_CLOEXEC, 0600);
  /* 创建失败时没有本工具拥有的新文件，绝不删除操作者的既存路径。 */
  if (fd < 0) return NULL;
  struct stat info;
  if (fstat(fd, &info) || !S_ISREG(info.st_mode) || info.st_nlink != 1) {
    /* 非独占普通文件不能写入；只关闭刚取得的描述符，不按名字清理。 */
    close(fd);
    return NULL;
  }
  FILE *output = fdopen(fd, "w");
  /* fdopen 尚未接管时由本层关闭；失败留下的新空文件是无效观测。 */
  if (!output) close(fd);
  return output;
}

/* 标量摘要只在一个身份验证且有效的样本写出后推进。 */
static void accumulate(struct summary *summary, const struct point *point, uint64_t gap, uint64_t span) {
  ++summary->records;
  /* 分别保留 current、lifetime 和有效采样质量的 MAX，不能把它们混成同一峰值。 */
  if (point->current_bytes > summary->max_current) summary->max_current = point->current_bytes;
  if (point->lifetime_peak_bytes > summary->max_lifetime) summary->max_lifetime = point->lifetime_peak_bytes;
  if (gap > summary->max_gap_ns) summary->max_gap_ns = gap;
  if (span > summary->max_query_ns) summary->max_query_ns = span;
}

/* 无权限提升、task port、coalition 猜测、GUI 或目标进程启动路径。 */
int main(int argc, char **argv) {
  if (argc == 2 && !strcmp(argv[1], "--help")) {
    /* 说明路径在身份、时钟和权限检查前结束，不查询目标进程。 */
    usage();
    return 0;
  }
  struct options options;
  if (!parse_options(argc, argv, &options)) {
    /* 参数拒绝发生于任何查询和文件创建之前。 */
    fputs("invalid memory sampler arguments; use --help\n", stderr);
    return EXIT_USAGE;
  }
  if (getuid() == 0 || geteuid() != getuid()) {
    /* 不把 root 或 setuid 环境当成普通用户观测的替代。 */
    fputs("memory sampler requires an ordinary non-elevated user\n", stderr);
    return EXIT_INVALID;
  }
  options.expected.uid = getuid();
  mach_timebase_info_data_t base;
  struct point first;
  if (mach_timebase_info(&base) != KERN_SUCCESS || !base.numer || !base.denom ||
      !capture_point(options.expected.pid, &first) || !ordered_clock(&first)) {
    /* 第一个身份/时钟读取失败时尚未拥有输出文件。 */
    fputs("target identity or rusage observation unavailable\n", stderr);
    return EXIT_INVALID;
  }
  if (options.identify) {
    /* 身份输出不是内存验收；本模式不创建任何输出文件。 */
    char encoded[PROC_PIDPATHINFO_MAXSIZE * 2 + 1];
    path_hex(first.identity.executable, encoded);
    printf("{\"scope\":\"single_pid_identity\",\"membership\":\"unverified\",\"pid\":%d,\"uid\":%u,"
           "\"birth_sec\":\"%" PRIu64 "\",\"birth_usec\":\"%" PRIu64 "\","
           "\"start_abstime\":\"%" PRIu64 "\",\"executable_hex\":\"%s\"}\n",
           first.identity.pid, (unsigned)first.identity.uid, first.identity.birth_sec,
           first.identity.birth_usec, first.identity.start_ticks, encoded);
    return ferror(stdout) ? EXIT_IO : 0;
  }
  if (!same_identity(&first.identity, &options.expected)) {
    /* identify 与 sample 之间 PID 复用、exec 或 UID 变化不能自动重新授权。 */
    fputs("selected target identity does not match\n", stderr);
    return EXIT_INVALID;
  }
  FILE *output = create_output(options.output);
  if (!output) {
    /* 输出不能独占创建就停止，不换一个静默路径或覆盖既存文件。 */
    fputs("cannot exclusively create private output\n", stderr);
    return EXIT_IO;
  }
  struct writer writer = { .file = output, .bytes = 0 };
  struct summary summary = {0};
  const char *failure = NULL;
  uint64_t origin = midpoint(&first);
  /* header 必须先完整写出；失败不再采样，但仍尝试有限错误 footer。 */
  if (!write_header(&writer, &options, &base, origin)) failure = "output_failure";
  struct point previous = {0};
  for (uint64_t index = 0; !failure && index < options.planned_records; ++index) {
    /* 最后一个截止点精确落在声明时长，而非向上延长成另一工作负载。 */
    uint64_t planned = index * options.interval_ns;
    if (planned > options.duration_ns) planned = options.duration_ns;
    struct point point = index ? (struct point){0} : first;
    uint64_t elapsed = 0, gap = 0, span = 0, suspension = 0;
    if (index && (!wait_until(origin, planned, &base) ||
                  !capture_point(options.expected.pid, &point))) {
      /* 缺样保留实际已取得的原始字段；零值表示相应阶段未完成，而不是零内存。 */
      failure = "sample_unavailable";
      (void)write_failure(&writer, &point, index, planned, failure, elapsed, gap, span, suspension);
      break;
    }
    failure = validate_point(&point, index ? &previous : NULL, &options.expected,
                             origin, planned, options.interval_ns, &base,
                             &elapsed, &gap, &span, &suspension);
    if (failure) {
      /* 无效样本不能更新 MAX 或记录数；footer 的 complete 保持 false。 */
      (void)write_failure(&writer, &point, index, planned, failure, elapsed, gap, span, suspension);
      break;
    }
    if (!write_sample(&writer, &point, index, planned, elapsed, gap, span, suspension)) {
      /* 未完整写出的点不进入摘要；文件可能有部分行，离线复核必然拒绝。 */
      failure = "output_failure";
      break;
    }
    accumulate(&summary, &point, gap, span);
    previous = point;
  }
  bool complete = !failure && summary.records == options.planned_records;
  /* footer 和 close 都完成后才能发布有限范围的退出状态；不宣称断电持久性。 */
  bool footer_written = write_footer(&writer, &summary, complete, failure);
  bool closed = fclose(output) == 0;
  /* I/O 失败优先于看似完整的内存数值，防止完整 footer 掩盖 fclose 错误。 */
  if (!footer_written || !closed) return EXIT_IO;
  /* 任何缺样或语义失败保留非通过状态，不将未见峰值视为低于预算。 */
  if (!complete) return EXIT_INVALID;
  /* 已知 lifetime 超限也保留失败，但不会把其发生时间标成本区间。 */
  if (summary.max_current > BUDGET_BYTES || summary.max_lifetime > BUDGET_BYTES) return EXIT_BUDGET;
  /* 单 PID 记录再完整也不能证明 whole-product membership。 */
  return EXIT_MEMBERSHIP_UNVERIFIED;
}
