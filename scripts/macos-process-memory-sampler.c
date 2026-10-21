/*
 * Sample a bounded physical-footprint window for one explicitly selected same-user PID.
 * Results establish single-process facts only, without proving product membership, workload completeness, or steady growth.
 * libproc.h marks its interfaces private/subject to change; calls use only the ABI declared by the current SDK.
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
/* The user's product budget is strictly below decimal 200 MB; a single-PID observation uses that same boundary. */
#define BUDGET_BYTES UINT64_C(200000000)
#define NS_PER_MS UINT64_C(1000000)
#define NS_PER_SECOND UINT64_C(1000000000)

enum { EXIT_BUDGET = 1, EXIT_USAGE = 64, EXIT_INVALID = 65, EXIT_IO = 74,
       EXIT_MEMBERSHIP_UNVERIFIED = 78 };

/* Birth coordinates and executable path anchor selection; PID, name, or PPID alone cannot identify a process epoch. */
struct identity {
  pid_t pid;
  uid_t uid;
  uint64_t birth_sec;
  uint64_t birth_usec;
  uint64_t start_ticks;
  char executable[PROC_PIDPATHINFO_MAXSIZE];
};

/* Clock brackets preserve query cost and read uncertainty rather than claiming an atomic snapshot. */
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

/* Validate arguments and the record budget before querying a target or creating output. */
struct options {
  bool identify;
  struct identity expected;
  uint64_t duration_ns;
  uint64_t interval_ns;
  uint64_t planned_records;
  const char *output;
};

/* Retain scalar aggregates only, never the full sample sequence. */
struct summary {
  uint64_t records;
  uint64_t max_current;
  uint64_t max_lifetime;
  uint64_t max_gap_ns;
  uint64_t max_query_ns;
};

/* Encode each complete record in a fixed buffer, reserving finite output capacity for a failure footer. */
struct writer { FILE *file; uint64_t bytes; };

/* Parse complete unsigned decimal input, rejecting signs, whitespace, NaN, and accepted prefixes of invalid text. */
static bool parse_u64(const char *text, uint64_t min, uint64_t max, uint64_t *out) {
  if (!text || !*text) {
    /* A missing value cannot implicitly select PID zero or a zero-length observation. */
    return false;
  }
  uint64_t value = 0;
  for (const unsigned char *cursor = (const unsigned char *)text; *cursor; ++cursor) {
    /* Check each digit before multiplication and addition, excluding overflow and strtoull's signed-input conversion. */
    if (*cursor < '0' || *cursor > '9' || value > (UINT64_MAX - (*cursor - '0')) / 10) {
      return false;
    }
    value = value * 10 + (*cursor - '0');
  }
  if (value < min || value > max) {
    /* Values outside the ABI or observation budget must not reach the sampling loop. */
    return false;
  }
  *out = value;
  return true;
}

/* Help describes arguments without launching tools, native applications, or permission queries. */
static void usage(void) {
  puts("usage: memory-sampler identify --pid PID\n"
       "       memory-sampler sample --pid PID --birth-sec SEC --birth-usec USEC\n"
       "         --start-abstime TICKS --executable ABS_PATH --duration-ms MS\n"
       "         --interval-ms MS --output NEW_ABS_PATH\n"
       "limits: duration <= 3600000ms; interval 10..10000ms; <=65536 records; 64MiB output\n"
       "scope: single PID only; whole-product membership remains unverified (valid collection exits 78)");
}

/* One exact argument set prevents duplicate selection fields; no switch can self-certify whole-product membership. */
static bool parse_options(int argc, char **argv, struct options *options) {
  memset(options, 0, sizeof(*options));
  if (argc < 2 || (strcmp(argv[1], "identify") && strcmp(argv[1], "sample"))) {
    /* A misspelled mode must not fall through to a live query. */
    return false;
  }
  options->identify = !strcmp(argv[1], "identify");
  uint32_t seen = 0;
  for (int index = 2; index < argc; index += 2) {
    /* Every field needs a value; unknown or duplicate arguments fail before any recovery query. */
    if (index + 1 >= argc) return false;
    const char *key = argv[index], *value = argv[index + 1];
    uint32_t bit = 0;
    uint64_t parsed = 0;
    if (!strcmp(key, "--pid")) {
      /* Accept only a positive representable PID, excluding process-group and wildcard selection. */
      bit = 1u;
      if (!parse_u64(value, 1, INT_MAX, &parsed)) return false;
      options->expected.pid = (pid_t)parsed;
    } else if (!strcmp(key, "--birth-sec")) {
      /* Require the nonzero birth time previously obtained through explicit identify. */
      bit = 2u;
      if (!parse_u64(value, 1, UINT64_MAX, &options->expected.birth_sec)) return false;
    } else if (!strcmp(key, "--birth-usec")) {
      /* The BSD microsecond coordinate must lie within one second. */
      bit = 4u;
      if (!parse_u64(value, 0, 999999, &options->expected.birth_usec)) return false;
    } else if (!strcmp(key, "--start-abstime")) {
      /* The independent rusage birth coordinate cannot be replaced with PID equality. */
      bit = 8u;
      if (!parse_u64(value, 1, UINT64_MAX, &options->expected.start_ticks)) return false;
    } else if (!strcmp(key, "--executable")) {
      /* Retain the full absolute path in fixed storage; truncating an oversized identity is not allowed. */
      bit = 16u;
      size_t length = strlen(value);
      if (value[0] != '/' || length >= sizeof(options->expected.executable)) return false;
      memcpy(options->expected.executable, value, length + 1);
    } else if (!strcmp(key, "--duration-ms")) {
      /* Bound integer milliseconds to one hour before multiplying; no floating-point conversion is involved. */
      bit = 32u;
      if (!parse_u64(value, 10, 3600000, &parsed)) return false;
      options->duration_ns = parsed * NS_PER_MS;
    } else if (!strcmp(key, "--interval-ms")) {
      /* The 10 ms floor bounds query frequency; the 10 s ceiling excludes arbitrarily sparse endpoint claims. */
      bit = 64u;
      if (!parse_u64(value, 10, 10000, &parsed)) return false;
      options->interval_ns = parsed * NS_PER_MS;
    } else if (!strcmp(key, "--output")) {
      /* Output must be explicit and absolute; cwd cannot become an implicit publication location. */
      bit = 128u;
      if (value[0] != '/' || strlen(value) >= PATH_MAX) return false;
      options->output = value;
    } else {
      /* Aliases, positional arguments, and unknown fields cannot broaden sampling authority. */
      return false;
    }
    /* Reject repeated arguments even when values match, avoiding ambiguous replacement of a prior selection. */
    if (seen & bit) return false;
    seen |= bit;
  }
  if (options->identify) {
    /* Identify accepts only a PID, creates no file, and makes no product-membership claim. */
    return seen == 1u;
  }
  /* Sampling requires the complete identity and budget, covering at least one full interval. */
  if (seen != 255u || options->duration_ns < options->interval_ns) return false;
  options->planned_records = (options->duration_ns + options->interval_ns - 1) /
                             options->interval_ns + 1;
  return options->planned_records <= MAX_RECORDS;
}

/* Multiply in a wide integer before division; Apple Silicon does not imply a 1:1 timebase. */
static bool ticks_to_ns(uint64_t ticks, const mach_timebase_info_data_t *base, uint64_t *ns) {
  /* An unsupported timebase cannot silently become 1:1. */
  if (!base->numer || !base->denom) return false;
  __uint128_t value = (__uint128_t)ticks * base->numer / base->denom;
  /* The result must still fit the output protocol's u64; never publish a truncated conversion. */
  if (value > UINT64_MAX) return false;
  *ns = (uint64_t)value;
  return true;
}

/* Subtract ordered timestamps only within one Mach clock domain, never unrelated absolute/continuous epochs. */
static bool delta_ns(uint64_t before, uint64_t after,
                     const mach_timebase_info_data_t *base, uint64_t *ns) {
  /* A backward clock is invalid evidence, not an unsigned wraparound gap. */
  if (after < before) return false;
  return ticks_to_ns(after - before, base, ns);
}

/* Accept a BSD identity only when the complete declared structure and plausible birth coordinates are available. */
static bool read_bsd(pid_t pid, struct proc_bsdinfo *info) {
  /* SDK-sized, initialized, naturally aligned stack storage satisfies this C ABI's buffer contract. */
  memset(info, 0, sizeof(*info));
  return proc_pidinfo(pid, PROC_PIDTBSDINFO, 0, info, sizeof(*info)) == sizeof(*info) &&
         info->pbi_pid == (uint32_t)pid && info->pbi_start_tvsec != 0 &&
         info->pbi_start_tvusec < 1000000 && info->pbi_uid == getuid();
}

/* Bracket the executable path with BSD and rusage birth identities so PID reuse cannot inherit selection. */
static bool capture_point(pid_t pid, struct point *point) {
  memset(point, 0, sizeof(*point));
  point->before_ticks = mach_continuous_time();
  point->awake_before_ticks = mach_absolute_time();
  point->before_end_ticks = mach_continuous_time();
  struct proc_bsdinfo first, last;
  struct rusage_info_v4 first_usage = {0}, last_usage = {0};
  char last_path[PROC_PIDPATHINFO_MAXSIZE] = {0};
  /* Supply complete SDK-declared v4 storage and bounded path buffers.
   * Identity and paths must agree across reads; this is not an atomic authorization proof against same-user exec/ABA. */
  if (!read_bsd(pid, &first) ||
      proc_pid_rusage(pid, RUSAGE_INFO_V4, (rusage_info_t *)&first_usage) != 0 ||
      proc_pidpath(pid, point->identity.executable, sizeof(point->identity.executable)) <= 0 ||
      !read_bsd(pid, &last) ||
      proc_pid_rusage(pid, RUSAGE_INFO_V4, (rusage_info_t *)&last_usage) != 0 ||
      proc_pidpath(pid, last_path, sizeof(last_path)) <= 0) {
    /* Unreadable, exited, or ABI-failed observations are missing samples, never zero-memory samples. */
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
    /* rusage may return an exited process, which does not satisfy this tool's live observation-window contract. */
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

/* Encode filesystem bytes without guessing UTF-8; hex remains private local identity data, not publishable anonymization. */
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

/* Match every selected identity coordinate; sharing a UID does not establish product membership. */
static bool same_identity(const struct identity *observed, const struct identity *expected) {
  return observed->pid == expected->pid && observed->uid == expected->uid &&
         observed->birth_sec == expected->birth_sec && observed->birth_usec == expected->birth_usec &&
         observed->start_ticks == expected->start_ticks &&
         !strcmp(observed->executable, expected->executable);
}

/* Estimate sample time at the continuous-clock bracket midpoint while retaining bracket width as read uncertainty. */
static uint64_t midpoint(const struct point *point) {
  return point->before_ticks + (point->before_end_ticks - point->before_ticks) / 2;
}

/* Validate raw clock order before midpoint's unsigned subtraction. */
static bool ordered_clock(const struct point *point) {
  return point->before_ticks <= point->before_end_ticks &&
         point->before_end_ticks <= point->after_begin_ticks &&
         point->after_begin_ticks <= point->after_ticks &&
         point->awake_before_ticks <= point->awake_after_ticks;
}

/* Subtract the full read uncertainty from same-domain deltas; only the remaining excess is a suspension lower bound. */
static bool suspended_ns(uint64_t continuous_delta, uint64_t awake_delta, uint64_t uncertainty,
                         const mach_timebase_info_data_t *base, uint64_t *ns) {
  uint64_t lower = 0;
  if (continuous_delta > awake_delta && continuous_delta - awake_delta > uncertainty) {
    /* Keep only excess beyond all bracket uncertainty, avoiding false suspension from ordinary read ordering. */
    lower = continuous_delta - awake_delta - uncertainty;
  }
  return ticks_to_ns(lower, base, ns);
}

/* Validate identity, clocks, gaps, and peak invariants from supplied observations without performing system queries. */
static const char *validate_point(const struct point *point, const struct point *previous,
                                  const struct identity *expected, uint64_t origin,
                                  uint64_t planned_ns, uint64_t interval_ns,
                                  const mach_timebase_info_data_t *base,
                                  uint64_t *elapsed, uint64_t *gap, uint64_t *span,
                                  uint64_t *suspension) {
  /* Any path, birth-coordinate, or UID change ends the selection; do not reacquire a similarly named process. */
  if (!same_identity(&point->identity, expected)) return "identity_changed";
  /* Ordered bracket widths sum to no more than the whole query, preventing overflow in their sum. */
  if (!ordered_clock(point) || (previous && !ordered_clock(previous))) return "clock_order";
  /* Every output duration must fit u64 nanoseconds; saturation would conceal an unsupported conversion. */
  if (!delta_ns(origin, midpoint(point), base, elapsed) ||
      !delta_ns(point->before_ticks, point->after_ticks, base, span)) return "clock_conversion";
  /* XNU reads lifetime peak before fill_task_rusage reads current; these fields are not an atomic snapshot.
   * Intervening allocations can make current exceed this record's peak; neither raw value may be rewritten or discarded.
   * Peak monotonicity is checked below, while current and peak independently obey the strict 200 MB boundary. */
  uint64_t query_uncertainty = (point->before_end_ticks - point->before_ticks) +
                               (point->after_ticks - point->after_begin_ticks);
  /* Check suspension inside this query; same-domain deltas separately check the interval between queries below. */
  if (!suspended_ns(point->after_ticks - point->before_ticks,
                    point->awake_after_ticks - point->awake_before_ticks,
                    query_uncertainty, base, suspension)) return "clock_conversion";
  *gap = 0;
  if (previous) {
    /* The previous valid sample anchors gaps, clock order, and lifetime monotonicity within the same process epoch. */
    if (midpoint(point) < midpoint(previous) ||
        point->awake_before_ticks < previous->awake_before_ticks) return "clock_order";
    /* A decreasing lifetime peak in one live epoch contradicts the counter's semantics. */
    if (point->lifetime_peak_bytes < previous->lifetime_peak_bytes) return "peak_moved_backwards";
    uint64_t current_width = point->before_end_ticks - point->before_ticks;
    uint64_t previous_width = previous->before_end_ticks - previous->before_ticks;
    /* Extremely wide supplied brackets must also fail; overflow cannot shrink estimated read uncertainty. */
    if (UINT64_MAX - current_width < previous_width) return "clock_conversion";
    uint64_t uncertainty = current_width + previous_width;
    uint64_t gap_suspension = 0;
    /* Conversion failure or incomparable cross-sample clocks cannot leave a valid zero gap. */
    if (!delta_ns(midpoint(previous), midpoint(point), base, gap) ||
        !suspended_ns(midpoint(point) - midpoint(previous),
                      point->awake_before_ticks - previous->awake_before_ticks,
                      uncertainty, base, &gap_suspension)) return "clock_conversion";
    /* Report the larger established suspension lower bound from within and between queries. */
    if (gap_suspension > *suspension) *suspension = gap_suspension;
  }
  /* Any established suspension invalidates the window; zero means only that uncertainty hides no provable excess. */
  if (*suspension) return "suspension_observed";
  /* A query longer than its interval cannot support the declared sampling frequency. */
  if (*span > interval_ns) return "query_exceeds_interval";
  /* Missing points are not interpolated; the absolute schedule also rejects missed deadlines below the gap ceiling. */
  if (*gap > interval_ns * 2) return "sampling_gap";
  /* Each sample must start at its declared deadline or later; early reads cannot certify endpoint coverage. */
  if (*elapsed < planned_ns) return "sample_before_deadline";
  /* A full interval of lateness loses a scheduled point; burst catch-up cannot establish continuous coverage. */
  if (*elapsed > planned_ns && *elapsed - planned_ns >= interval_ns) return "missed_deadline";
  return NULL;
}

/* Fixed buffers and a total byte budget bound formatting storage and output growth. */
static bool write_record(struct writer *writer, bool footer, const char *format, ...) {
  char line[MAX_LINE_BYTES];
  va_list arguments;
  va_start(arguments, format);
  int size = vsnprintf(line, sizeof(line), format, arguments);
  va_end(arguments);
  uint64_t limit = MAX_OUTPUT_BYTES - (footer ? 0 : FOOTER_RESERVE);
  /* Reject formatting truncation or budget exhaustion before writing any part of this record. */
  if (size < 0 || (size_t)size >= sizeof(line) || writer->bytes + (uint64_t)size > limit) return false;
  /* Real I/O can leave a partial file; any short write or flush failure prevents completion certification. */
  if (fwrite(line, 1, (size_t)size, writer->file) != (size_t)size || fflush(writer->file)) return false;
  writer->bytes += (uint64_t)size;
  return true;
}

/* The header distinguishes evidence validity from product acceptance; arguments cannot certify process membership. */
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
    "\"timebase_numer\":%u,\"timebase_denom\":%u,\"budget_bytes\":\"%" PRIu64 "\","
    "\"api\":\"SDK_declared_libproc_private_subject_to_change\",\"clock\":\"mach_continuous_time\"}\n",
    options->expected.pid, (unsigned)options->expected.uid, options->expected.birth_sec,
    options->expected.birth_usec, options->expected.start_ticks, encoded, origin,
    options->duration_ns, options->interval_ns, options->planned_records, base->numer, base->denom,
    BUDGET_BYTES);
}

/* Memory counters and Mach ticks use decimal strings, preserving integer precision in downstream JSON consumers. */
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

/* Preserve failed clocks and gaps in a separate record, excluded from valid-sample aggregates and acceptance. */
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

/* Counters below budget cannot produce a product pass without complete membership and workload evidence. */
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
    complete && summary->max_current < BUDGET_BYTES ? "true" : "false",
    complete && summary->max_lifetime < BUDGET_BYTES ? "true" : "false");
}

/* Wait against an absolute continuous-clock deadline; observation checks still expose suspension and late scheduling. */
static bool wait_until(uint64_t origin, uint64_t target_ns, const mach_timebase_info_data_t *base) {
  for (;;) {
    uint64_t elapsed = 0;
    /* A backward clock or unsupported conversion must not continue waiting against an invalid deadline. */
    if (!delta_ns(origin, mach_continuous_time(), base, &elapsed)) return false;
    /* The one absolute deadline has arrived; EINTR never restarts the full declared duration. */
    if (elapsed >= target_ns) return true;
    uint64_t remaining = target_ns - elapsed;
    struct timespec request = { .tv_sec = (time_t)(remaining / NS_PER_SECOND),
                               .tv_nsec = (long)(remaining % NS_PER_SECOND) };
    /* Recompute absolute remaining time after signals; other wait errors invalidate observation. */
    if (nanosleep(&request, NULL) && errno != EINTR) return false;
  }
}

/* Exclusively create a new 0600 regular file, without overwriting, deleting, or following an existing final path. */
static FILE *create_output(const char *path) {
  int fd = open(path, O_WRONLY | O_CREAT | O_EXCL | O_NOFOLLOW | O_CLOEXEC, 0600);
  /* Creation failure acquires no new file owner; never delete the caller's preexisting path. */
  if (fd < 0) return NULL;
  struct stat info;
  if (fstat(fd, &info) || !S_ISREG(info.st_mode) || info.st_nlink != 1) {
    /* A nonexclusive regular-file identity cannot be written; close only the acquired descriptor, without pathname cleanup. */
    close(fd);
    return NULL;
  }
  FILE *output = fdopen(fd, "w");
  /* Close the descriptor if fdopen has not taken ownership; a new empty file left on failure is invalid evidence. */
  if (!output) close(fd);
  return output;
}

/* Advance scalar aggregates only after writing one complete identity-validated sample. */
static void accumulate(struct summary *summary, const struct point *point, uint64_t gap, uint64_t span) {
  ++summary->records;
  /* Keep current, lifetime, and sampling-quality maxima separate; they represent different observations. */
  if (point->current_bytes > summary->max_current) summary->max_current = point->current_bytes;
  if (point->lifetime_peak_bytes > summary->max_lifetime) summary->max_lifetime = point->lifetime_peak_bytes;
  if (gap > summary->max_gap_ns) summary->max_gap_ns = gap;
  if (span > summary->max_query_ns) summary->max_query_ns = span;
}

/* This entry point never elevates permissions, obtains task ports, guesses coalitions, opens GUI, or starts its target. */
int main(int argc, char **argv) {
  if (argc == 2 && !strcmp(argv[1], "--help")) {
    /* Help returns before identity, clock, and permission checks, with no target-process query. */
    usage();
    return 0;
  }
  struct options options;
  if (!parse_options(argc, argv, &options)) {
    /* Argument rejection precedes every target query and output creation. */
    fputs("invalid memory sampler arguments; use --help\n", stderr);
    return EXIT_USAGE;
  }
  if (getuid() == 0 || geteuid() != getuid()) {
    /* Root and setuid execution cannot substitute for an ordinary-user observation. */
    fputs("memory sampler requires an ordinary non-elevated user\n", stderr);
    return EXIT_INVALID;
  }
  options.expected.uid = getuid();
  mach_timebase_info_data_t base;
  struct point first;
  if (mach_timebase_info(&base) != KERN_SUCCESS || !base.numer || !base.denom ||
      !capture_point(options.expected.pid, &first) || !ordered_clock(&first)) {
    /* Initial identity or clock failure occurs before acquiring an output file. */
    fputs("target identity or rusage observation unavailable\n", stderr);
    return EXIT_INVALID;
  }
  if (options.identify) {
    /* Identity output establishes no memory acceptance and creates no observation file. */
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
    /* PID reuse, exec, or UID change between identify and sample cannot automatically authorize a replacement target. */
    fputs("selected target identity does not match\n", stderr);
    return EXIT_INVALID;
  }
  FILE *output = create_output(options.output);
  if (!output) {
    /* Failed exclusive creation stops the run, without selecting a silent alternate path or overwriting an existing file. */
    fputs("cannot exclusively create private output\n", stderr);
    return EXIT_IO;
  }
  struct writer writer = { .file = output, .bytes = 0 };
  struct summary summary = {0};
  const char *failure = NULL;
  uint64_t origin = midpoint(&first);
  /* The header must complete first; failure stops sampling but still attempts a bounded failure footer. */
  if (!write_header(&writer, &options, &base, origin)) failure = "output_failure";
  struct point previous = {0};
  for (uint64_t index = 0; !failure && index < options.planned_records; ++index) {
    /* The final deadline equals the declared duration instead of rounding up into a different workload window. */
    uint64_t planned = index * options.interval_ns;
    if (planned > options.duration_ns) planned = options.duration_ns;
    struct point point = index ? (struct point){0} : first;
    uint64_t elapsed = 0, gap = 0, span = 0, suspension = 0;
    if (index && (!wait_until(origin, planned, &base) ||
                  !capture_point(options.expected.pid, &point))) {
      /* Preserve fields actually obtained on missing samples; zero marks an unfinished capture stage, not zero memory. */
      failure = "sample_unavailable";
      (void)write_failure(&writer, &point, index, planned, failure, elapsed, gap, span, suspension);
      break;
    }
    failure = validate_point(&point, index ? &previous : NULL, &options.expected,
                             origin, planned, options.interval_ns, &base,
                             &elapsed, &gap, &span, &suspension);
    if (failure) {
      /* Invalid samples cannot advance maxima or counts; the footer remains incomplete. */
      (void)write_failure(&writer, &point, index, planned, failure, elapsed, gap, span, suspension);
      break;
    }
    if (!write_sample(&writer, &point, index, planned, elapsed, gap, span, suspension)) {
      /* A partly written point cannot enter aggregates; offline verification rejects the possible partial line. */
      failure = "output_failure";
      break;
    }
    accumulate(&summary, &point, gap, span);
    previous = point;
  }
  bool complete = !failure && summary.records == options.planned_records;
  /* Publish scoped exit status only after both footer and close finish; this does not certify crash durability. */
  bool footer_written = write_footer(&writer, &summary, complete, failure);
  bool closed = fclose(output) == 0;
  /* I/O failure outranks apparently complete counters, so a written footer cannot conceal fclose failure. */
  if (!footer_written || !closed) return EXIT_IO;
  /* Missing samples and semantic failures cannot treat an unseen peak as within budget. */
  if (!complete) return EXIT_INVALID;
  /* A known lifetime exceedance fails the budget without claiming that its peak occurred inside this observation window. */
  if (summary.max_current >= BUDGET_BYTES || summary.max_lifetime >= BUDGET_BYTES) return EXIT_BUDGET;
  /* A complete single-PID sequence still cannot establish whole-product membership. */
  return EXIT_MEMBERSHIP_UNVERIFIED;
}
