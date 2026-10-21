/* A stdin-controlled, non-GUI memory workload owns only its mappings and never queries other processes. */
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

/* One local owner holds the mapping; successful munmap removes every accessible pointer from this record. */
struct allocation { void *address; size_t bytes; };

/* Normal and failed exits share one bounded release path, without accumulating mappings from earlier stages. */
static bool release_allocation(struct allocation *allocation) {
  if (!allocation->address) {
    /* FREE is idempotent when empty, without issuing munmap again for an already released mapping. */
    return true;
  }
  if (munmap(allocation->address, allocation->bytes)) {
    /* Preserve the owner on release failure so the remaining mapping is not silently lost. */
    return false;
  }
  allocation->address = NULL;
  allocation->bytes = 0;
  return true;
}

/* Touch every page of a bounded mapping, creating real footprint instead of reserving only virtual addresses. */
static bool allocate_and_touch(struct allocation *allocation, size_t bytes) {
  if (!release_allocation(allocation)) return false; /* Refuse replacement while the old mapping is still owned. */
  long raw_page_size = sysconf(_SC_PAGESIZE);
  if (raw_page_size <= 0 || (uint64_t)raw_page_size > bytes) {
    /* An unknown page size cannot silently become a guessed stride. */
    return false;
  }
  void *address = mmap(NULL, bytes, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANON, -1, 0);
  if (address == MAP_FAILED) return false; /* Allocation failure must not enter the page-touch loop. */
  allocation->address = address;
  allocation->bytes = bytes;
  volatile unsigned char *payload = address;
  for (size_t offset = 0; offset < bytes; offset += (size_t)raw_page_size) {
    /* Volatile writes prevent optimization; these fixed nonzero bytes contain no user content. */
    payload[offset] = 0xa5;
  }
  payload[bytes - 1] = 0x5a;
  return true;
}

/* A complete acknowledgement line synchronizes stages, without guessed sleeps for allocation completion. */
static bool acknowledge(const char *stage, const struct allocation *allocation) {
  return printf("{\"stage\":\"%s\",\"mapped_bytes\":\"%zu\"}\n", stage, allocation->bytes) > 0 &&
         fflush(stdout) == 0;
}

/* Bounded command reads recheck the continuous-clock deadline while waiting and also stop on stdin closure. */
static int read_command(char *line, size_t capacity, uint64_t origin,
                         const mach_timebase_info_data_t *base) {
  size_t used = 0;
  for (;;) {
    uint64_t now = mach_continuous_time();
    if (now < origin || (__uint128_t)(now - origin) * base->numer / base->denom >= LIFETIME_NS) {
      /* The independent lifetime limit ends ownership rather than retaining mappings while waiting for the parent. */
      return -1;
    }
    struct pollfd input = {.fd = STDIN_FILENO, .events = POLLIN, .revents = 0};
    int ready = poll(&input, 1, 250);
    if (ready < 0) {
      /* A signal interrupts only this wait; other errors end the control session. */
      if (errno == EINTR) continue;
      return -1;
    }
    if (!ready) continue; /* Recheck the deadline within 250 ms, without background workload. */
    if (input.revents & (POLLERR | POLLNVAL)) return -1; /* A broken input source cannot accept commands. */
    unsigned char byte = 0;
    ssize_t count = read(STDIN_FILENO, &byte, 1);
    if (count < 0 && errno == EINTR) continue; /* Keep the bounded line prefix across interrupted reads. */
    if (count != 1) return -1; /* EOF and other read errors both trigger release. */
    if (byte == '\n') {
      /* Only a complete line permits the caller to execute a known stage. */
      line[used] = 0;
      return 0;
    }
    if (byte == 0 || used + 1 >= capacity) return -1; /* Never execute a truncated or embedded-NUL command. */
    line[used++] = (char)byte;
  }
}

/* No PID argument, GUI, output file, or setting is exposed; the parent's stdin pipe is the sole control channel. */
int main(void) {
  mach_timebase_info_data_t base = {0};
  struct allocation allocation = {0};
  if (mach_timebase_info(&base) != KERN_SUCCESS || !base.numer || !base.denom) return 65;
  uint64_t origin = mach_continuous_time();
  int status = 65;
  if (!acknowledge("ready", &allocation)) return 74; /* No mapping exists if the parent cannot receive readiness. */
  for (unsigned index = 0; index < MAX_COMMANDS; ++index) {
    /* Both command count and an independent deadline bound this control process's lifetime. */
    char command[16];
    if (read_command(command, sizeof(command), origin, &base)) break;
    if (!strcmp(command, "ALLOC32")) {
      /* This touches 32 MiB; the OS measures actual footprint growth, which need not equal mapped bytes. */
      if (!allocate_and_touch(&allocation, 32 * MIB) || !acknowledge("allocated32", &allocation)) break;
    } else if (!strcmp(command, "ALLOC192")) {
      /* Optional 192 MiB is 201326592 bytes, crossing 200 MB; release must not erase the observed lifetime peak. */
      if (!allocate_and_touch(&allocation, 192 * MIB) || !acknowledge("allocated192", &allocation)) break;
    } else if (!strcmp(command, "FREE")) {
      /* Acknowledge only after munmap succeeds; release does not reset the lifetime peak. */
      if (!release_allocation(&allocation) || !acknowledge("freed", &allocation)) break;
    } else if (!strcmp(command, "EXIT")) {
      /* Normal exit releases before acknowledgement; the parent must still wait for the actual exit code. */
      status = release_allocation(&allocation) && acknowledge("exiting", &allocation) ? 0 : 74;
      break;
    } else {
      /* Unknown commands cannot expand into arbitrary allocations or shell operations. */
      break;
    }
  }
  if (!release_allocation(&allocation)) status = 74; /* Early failure must also preserve a release error. */
  return status;
}
