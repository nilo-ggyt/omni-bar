/*
 * Observe one explicitly selected process through the SDK-declared proc_pid_rusage ABI.
 * libproc declares its interfaces private and subject to change; header availability is not a public stability promise.
 * Counters are per-PID deltas, not a process-set or coalition attribution claim.
 * Physical footprint differs from resident size; the lifetime peak is not an
 * interval peak. Paired awake/continuous clocks expose suspension contamination,
 * but do not certify display, power, thermal, or uninterrupted benchmark state.
 */
#include <errno.h>
#include <inttypes.h>
#include <libproc.h>
#include <limits.h>
#include <mach/mach_time.h>
#include <math.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/resource.h>
#include <time.h>

/* Keep argument failure distinct from runtime observation failure. */
enum {
  EXIT_USAGE = 64,
  NANOSECONDS_PER_SECOND = 1000000000,
};

/* Bracket one awake-clock read with continuous reads to bound read-order skew. */
struct sample_clock_pair {
  uint64_t continuous_ticks;
  uint64_t awake_ticks;
  uint64_t uncertainty_ticks;
};

/* Accept only a complete positive PID representable by macOS's pid_t ABI. */
static int parse_target_pid(const char *value, pid_t *target_pid) {
  /* Reset conversion state so an unrelated earlier error cannot reject input. */
  char *end = NULL;
  errno = 0;
  long parsed = strtol(value, &end, 10);
  if (errno != 0 || value[0] == '\0' || end == NULL || end[0] != '\0' ||
      parsed <= 0 || parsed > INT_MAX) {
    /* Reject empty, partial, overflowing, and nonpositive selections before any
     * process query; process existence is checked separately by proc_pid_rusage. */
    return -1;
  }

  /* Publish output only after validation, preserving the caller's value on error. */
  *target_pid = (pid_t)parsed;
  return 0;
}

/* Bound a finite interval before it can reach a floating-to-integer conversion. */
static int parse_sample_seconds(const char *value, double *sample_seconds) {
  /* strtod accepts NaN/Infinity syntax; ordered comparisons alone cannot reject NaN. */
  char *end = NULL;
  errno = 0;
  double parsed = strtod(value, &end);
  if (errno != 0 || value[0] == '\0' || end == NULL || end[0] != '\0' ||
      !isfinite(parsed) || parsed < 0.01 || parsed > 3600.0) {
    /* Invalid durations never reach time_t/long conversion or start a measurement. */
    return -1;
  }

  /* Keep the validated duration available without permitting partial conversion. */
  *sample_seconds = parsed;
  return 0;
}

/* Initialize all result bytes and request exactly the SDK-declared version-4 layout. */
static int capture_process_usage(pid_t target_pid,
                                 struct rusage_info_v4 *usage) {
  memset(usage, 0, sizeof(*usage));
  return proc_pid_rusage(target_pid, RUSAGE_INFO_V4,
                         (rusage_info_t *)usage);
}

/* Convert an ordered tick difference with the already validated Mach timebase. */
static int elapsed_clock_seconds(uint64_t start_ticks, uint64_t end_ticks,
                                 const mach_timebase_info_data_t *timebase,
                                 double *seconds) {
  if (end_ticks < start_ticks) {
    /* A backwards reading cannot produce a valid unsigned interval or rate. */
    return -1;
  }

  /* Widen before multiplication so scaling a long interval cannot overflow a
   * uint64_t. Zero is valid here for a clock-read uncertainty, but not for rates. */
  long double elapsed_nanoseconds =
      (long double)(end_ticks - start_ticks) * (long double)timebase->numer /
      (long double)timebase->denom;
  *seconds =
      (double)(elapsed_nanoseconds / (long double)NANOSECONDS_PER_SECOND);
  if (!isfinite(*seconds) || *seconds < 0.0) {
    /* Reject invalid numeric scaling rather than emitting non-finite time values. */
    return -1;
  }
  return 0;
}

/* Sample documented sleep-inclusive and sleep-exclusive clocks without changing
 * system sleep policy. Continuous-read brackets quantify acquisition skew. */
static int capture_clock_pair(struct sample_clock_pair *pair) {
  uint64_t before = mach_continuous_time();
  pair->awake_ticks = mach_absolute_time();
  uint64_t after = mach_continuous_time();
  if (after < before) {
    /* Preserve a clock failure instead of wrapping its midpoint or uncertainty. */
    return -1;
  }

  /* An overflow-free midpoint approximates the instant of the enclosed awake
   * read. The full bracket width is conservative; it is not clock resolution. */
  pair->uncertainty_ticks = after - before;
  pair->continuous_ticks = before + pair->uncertainty_ticks / 2;
  return 0;
}

/* Wait for a finite request while preserving only its unfinished part after a
 * signal. This wait does not prevent machine suspension or prove uptime. */
static int wait_for_sample(double sample_seconds) {
  /* parse_sample_seconds guarantees [0.01, 3600], making both integer casts
   * representable and keeping fractional nanoseconds within timespec bounds. */
  time_t whole_seconds = (time_t)sample_seconds;
  double fractional_seconds = sample_seconds - (double)whole_seconds;
  struct timespec remaining = {
      .tv_sec = whole_seconds,
      .tv_nsec = (long)(fractional_seconds * (double)NANOSECONDS_PER_SECOND),
  };

  while (nanosleep(&remaining, &remaining) == -1) {
    /* EINTR consumes only the remaining interval; other errors invalidate the run. */
    if (errno != EINTR) {
      /* A failed wait must not emit a partial sample as a completed observation. */
      return -1;
    }
  }

  return 0;
}

/* Reject counter reset/wrap rather than manufacturing a huge unsigned delta. */
static int subtract_counter(uint64_t start, uint64_t end, uint64_t *delta) {
  if (end < start) {
    /* Process identity checks do not replace monotonicity checks for each counter. */
    return -1;
  }

  /* Publish a nonnegative delta without changing output on the rejection path. */
  *delta = end - start;
  return 0;
}

/* Collect one before/after observation and print machine-readable, scoped units. */
int main(int argc, const char *argv[]) {
  if (argc != 3) {
    /* A strict argument shape prevents silently ignoring an unintended target. */
    fprintf(stderr, "usage: %s <pid> <sample-seconds>\n", argv[0]);
    return EXIT_USAGE;
  }

  /* Parse both user-controlled fields before acquiring process measurements. */
  pid_t target_pid = -1;
  double requested_seconds = 0.0;
  if (parse_target_pid(argv[1], &target_pid) != 0 ||
      parse_sample_seconds(argv[2], &requested_seconds) != 0) {
    /* Name finiteness explicitly because NaN otherwise bypasses ordered bounds. */
    fprintf(stderr,
            "pid must be positive and sample-seconds must be finite and in [0.01, 3600]\n");
    return EXIT_USAGE;
  }

  /* Resolve scaling before observing the process; both public clocks use Mach
   * tick units. No private clock or guessed coalition API enters the sample. */
  mach_timebase_info_data_t timebase;
  if (mach_timebase_info(&timebase) != KERN_SUCCESS || timebase.denom == 0 ||
      timebase.numer == 0) {
    /* Missing scaling prevents reporting meaningful seconds or normalized rates. */
    fprintf(stderr, "cannot resolve Mach clock timebase\n");
    return 1;
  }

  /* SDK-declared rusage snapshots bracket the wait but are not atomic with clock reads.
   * Query overhead lies outside the clock interval; these are finite process
   * observations, not scheduler or complete-product acceptance measurements. */
  struct rusage_info_v4 start_usage;
  struct rusage_info_v4 end_usage;
  struct sample_clock_pair start_clock;
  struct sample_clock_pair end_clock;
  if (capture_process_usage(target_pid, &start_usage) != 0) {
    /* An unreadable/missing process cannot supply an initial identity or counter base. */
    fprintf(stderr, "cannot read pid %d before sample: %s\n", target_pid,
            strerror(errno));
    return 1;
  }
  if (capture_clock_pair(&start_clock) != 0) {
    /* A broken start bracket invalidates both clock interpretations of the run. */
    fprintf(stderr, "cannot capture ordered start clocks\n");
    return 1;
  }
  if (wait_for_sample(requested_seconds) != 0) {
    /* A failed wait cannot be reported as the completed requested interval. */
    fprintf(stderr, "sample wait failed: %s\n", strerror(errno));
    return 1;
  }
  if (capture_clock_pair(&end_clock) != 0) {
    /* A broken end bracket cannot be repaired by trusting only one clock. */
    fprintf(stderr, "cannot capture ordered end clocks\n");
    return 1;
  }
  if (capture_process_usage(target_pid, &end_usage) != 0) {
    /* An exited or newly unreadable process cannot supply a trustworthy delta. */
    fprintf(stderr, "cannot read pid %d after sample: %s\n", target_pid,
            strerror(errno));
    return 1;
  }
  if (start_usage.ri_proc_start_abstime != end_usage.ri_proc_start_abstime) {
    /* PID reuse must not combine counters from two different process lifetimes. */
    fprintf(stderr, "pid %d changed identity during sample\n", target_pid);
    return 1;
  }
  if (start_usage.ri_proc_exit_abstime != 0 ||
      end_usage.ri_proc_exit_abstime != 0) {
    /* proc_pid_rusage also accepts zombies. A terminated process cannot prove
     * resident activity or idle behavior for the whole requested live interval. */
    fprintf(stderr, "pid %d exited before the live sample completed\n", target_pid);
    return 1;
  }

  /* Derive only this process's monotonic counters. Child/coalition ownership is
   * not inferred from a PID, process name, resident size, or these resource fields. */
  uint64_t user_ticks = 0;
  uint64_t system_ticks = 0;
  uint64_t interrupt_wakeups = 0;
  uint64_t package_idle_wakeups = 0;
  uint64_t pageins = 0;
  uint64_t bytes_read = 0;
  uint64_t bytes_written = 0;
  uint64_t logical_writes = 0;
  if (subtract_counter(start_usage.ri_user_time, end_usage.ri_user_time,
                       &user_ticks) != 0 ||
      subtract_counter(start_usage.ri_system_time, end_usage.ri_system_time,
                       &system_ticks) != 0 ||
      subtract_counter(start_usage.ri_interrupt_wkups,
                       end_usage.ri_interrupt_wkups, &interrupt_wakeups) != 0 ||
      subtract_counter(start_usage.ri_pkg_idle_wkups,
                       end_usage.ri_pkg_idle_wkups, &package_idle_wakeups) != 0 ||
      subtract_counter(start_usage.ri_pageins, end_usage.ri_pageins,
                       &pageins) != 0 ||
      subtract_counter(start_usage.ri_diskio_bytesread,
                       end_usage.ri_diskio_bytesread, &bytes_read) != 0 ||
      subtract_counter(start_usage.ri_diskio_byteswritten,
                       end_usage.ri_diskio_byteswritten, &bytes_written) != 0 ||
      subtract_counter(start_usage.ri_logical_writes,
                       end_usage.ri_logical_writes, &logical_writes) != 0) {
    /* Any backwards counter rejects the entire observation rather than a subset. */
    fprintf(stderr, "pid %d resource counters moved backwards\n", target_pid);
    return 1;
  }

  /* Preserve the continuous denominator while correcting CPU tick units, and
   * expose awake alternatives so suspension cannot silently dilute idle rates. */
  double measured_seconds = 0.0;
  double awake_seconds = 0.0;
  double user_seconds = 0.0;
  double system_seconds = 0.0;
  double start_uncertainty_seconds = 0.0;
  double end_uncertainty_seconds = 0.0;
  if (elapsed_clock_seconds(start_clock.continuous_ticks,
                            end_clock.continuous_ticks, &timebase,
                            &measured_seconds) != 0 ||
      elapsed_clock_seconds(start_clock.awake_ticks, end_clock.awake_ticks,
                            &timebase, &awake_seconds) != 0 ||
      elapsed_clock_seconds(0, start_clock.uncertainty_ticks, &timebase,
                            &start_uncertainty_seconds) != 0 ||
      elapsed_clock_seconds(0, end_clock.uncertainty_ticks, &timebase,
                            &end_uncertainty_seconds) != 0 ||
      elapsed_clock_seconds(0, user_ticks, &timebase, &user_seconds) != 0 ||
      elapsed_clock_seconds(0, system_ticks, &timebase, &system_seconds) != 0 ||
      measured_seconds <= 0.0 || awake_seconds <= 0.0) {
    /* Zero or invalid denominators invalidate rates even when raw counters exist. */
    fprintf(stderr, "paired clocks produced an invalid interval\n");
    return 1;
  }

  /* Discount both complete clock brackets for a conservative suspension lower
   * bound. A zero result cannot certify sleep-free operation: smaller suspension
   * or other uncontrolled display/power/thermal changes are not excluded. */
  double clock_uncertainty_seconds =
      start_uncertainty_seconds + end_uncertainty_seconds;
  double continuous_minus_awake_seconds = measured_seconds - awake_seconds;
  double suspended_seconds_lower_bound =
      fmax(0.0, continuous_minus_awake_seconds - clock_uncertainty_seconds);
  /* ri_user_time/ri_system_time are Mach absolute ticks, not nanoseconds. XNU's
   * fill_task_rusage copies task_power_info's recount Mach-time values. Convert
   * both deltas through the validated timebase before reporting time or rates;
   * a 1:1 timebase can conceal this mistake on a different architecture.
   * One logical core corresponds to 100%; concurrent work may exceed that value. */
  double cpu_percent = (user_seconds + system_seconds) / measured_seconds * 100.0;
  double interrupt_wakeups_per_second =
      (double)interrupt_wakeups / measured_seconds;
  double package_idle_wakeups_per_second =
      (double)package_idle_wakeups / measured_seconds;
  const double bytes_per_mib = 1024.0 * 1024.0;

  /* Emit scope and both time interpretations before resources so downstream
   * collection can reject contaminated intervals rather than relabel them as idle. */
  printf("measurement_scope=single_pid\n");
  printf("measurement_api=proc_pid_rusage_v4\n");
  printf("coalition_membership=unmeasured\n");
  printf("rate_clock=continuous\n");
  printf("cpu_counter_unit=mach_absolute_ticks\n");
  printf("mach_timebase_numer=%" PRIu32 "\n", timebase.numer);
  printf("mach_timebase_denom=%" PRIu32 "\n", timebase.denom);
  printf("pid=%d\n", target_pid);
  printf("process_start_abstime=%" PRIu64 "\n", end_usage.ri_proc_start_abstime);
  printf("requested_sample_seconds=%.9f\n", requested_seconds);
  printf("sample_seconds=%.6f\n", measured_seconds);
  printf("sample_continuous_seconds=%.9f\n", measured_seconds);
  printf("sample_awake_seconds=%.9f\n", awake_seconds);
  printf("continuous_minus_awake_seconds=%.9f\n", continuous_minus_awake_seconds);
  printf("clock_pair_uncertainty_seconds=%.9f\n", clock_uncertainty_seconds);
  printf("suspended_seconds_lower_bound=%.9f\n", suspended_seconds_lower_bound);
  /* Existing rate names keep their continuous denominator; new names state awake. */
  printf("cpu_percent_one_core=%.6f\n", cpu_percent);
  printf("cpu_percent_one_core_awake=%.6f\n",
         (user_seconds + system_seconds) / awake_seconds * 100.0);
  printf("user_time_mach_ticks=%" PRIu64 "\n", user_ticks);
  printf("system_time_mach_ticks=%" PRIu64 "\n", system_ticks);
  printf("user_time_ms=%.6f\n", user_seconds * 1000.0);
  printf("system_time_ms=%.6f\n", system_seconds * 1000.0);
  printf("interrupt_wakeups=%" PRIu64 "\n", interrupt_wakeups);
  printf("interrupt_wakeups_per_second=%.6f\n",
         interrupt_wakeups_per_second);
  printf("package_idle_wakeups=%" PRIu64 "\n", package_idle_wakeups);
  printf("package_idle_wakeups_per_second=%.6f\n",
         package_idle_wakeups_per_second);
  printf("interrupt_wakeups_per_awake_second=%.6f\n",
         (double)interrupt_wakeups / awake_seconds);
  printf("package_idle_wakeups_per_awake_second=%.6f\n",
         (double)package_idle_wakeups / awake_seconds);
  /* Raw deltas remain available independently of either rate denominator. */
  printf("pageins=%" PRIu64 "\n", pageins);
  printf("disk_bytes_read=%" PRIu64 "\n", bytes_read);
  printf("disk_bytes_written=%" PRIu64 "\n", bytes_written);
  printf("logical_writes=%" PRIu64 "\n", logical_writes);
  /* Footprint/resident are end snapshots; the peak covers the process lifetime
   * and must never be described as a maximum within this sampling interval. */
  printf("resident_mib=%.6f\n",
         (double)end_usage.ri_resident_size / bytes_per_mib);
  printf("physical_footprint_mib=%.6f\n",
         (double)end_usage.ri_phys_footprint / bytes_per_mib);
  printf("lifetime_peak_physical_footprint_mib=%.6f\n",
         (double)end_usage.ri_lifetime_max_phys_footprint / bytes_per_mib);
  return 0;
}
