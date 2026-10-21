/* Exercise sampling policy with synthetic scalars only; this program never calls capture_point or queries a PID. */
#define main memory_sampler_entry
#include "../macos-process-memory-sampler.c"
#undef main
#include <assert.h>

/* Fixed test identity does not represent a running process or establish real product membership. */
static struct point synthetic_point(uint64_t offset) {
  struct point point = {0};
  point.identity.pid = 42;
  point.identity.uid = 501;
  point.identity.birth_sec = 1000;
  point.identity.birth_usec = 99;
  point.identity.start_ticks = 12345;
  strcpy(point.identity.executable, "/Synthetic.app/Contents/MacOS/Synthetic");
  point.current_bytes = BUDGET_BYTES - 2;
  point.lifetime_peak_bytes = BUDGET_BYTES - 1;
  point.before_ticks = 1000000000 + offset;
  point.before_end_ticks = point.before_ticks + 2;
  point.awake_before_ticks = 800000000 + offset;
  point.after_begin_ticks = point.before_ticks + 98;
  point.after_ticks = point.before_ticks + 100;
  point.awake_after_ticks = point.awake_before_ticks + 98;
  return point;
}

/* Stop argument tests at pure parsing, even when they construct the complete sample command shape. */
static void test_parameters(void) {
  uint64_t value = 77;
  const char *bad[] = {"", "-1", "+1", " 1", "1 ", "1.0", "nan", "inf",
                       "0x10", "18446744073709551616"};
  for (size_t index = 0; index < sizeof(bad) / sizeof(bad[0]); ++index) {
    /* Invalid input must leave out unchanged, preventing failure from publishing a default zero identity. */
    assert(!parse_u64(bad[index], 0, UINT64_MAX, &value) && value == 77);
  }
  assert(parse_u64("18446744073709551615", 0, UINT64_MAX, &value) && value == UINT64_MAX);
  assert(!parse_u64("9", 10, 100, &value));
  char *args[] = {"fixture", "sample", "--pid", "42", "--birth-sec", "1000",
    "--birth-usec", "99", "--start-abstime", "12345", "--executable", "/Synthetic",
    "--duration-ms", "20", "--interval-ms", "10", "--output", "/synthetic-never-created"};
  struct options options;
  assert(parse_options(18, args, &options) && options.planned_records == 3);
  args[13] = "3600000";
  assert(!parse_options(18, args, &options)); /* At 10 ms this would exceed 65536 records. */
  args[15] = "10000";
  assert(parse_options(18, args, &options) && options.planned_records == 361);
  args[13] = "3600001";
  assert(!parse_options(18, args, &options));
  args[13] = "20";
  args[15] = "9";
  assert(!parse_options(18, args, &options));
  args[15] = "10";
  args[3] = "0";
  assert(!parse_options(18, args, &options));
  args[3] = "42";
  args[16] = "--pid";
  assert(!parse_options(18, args, &options));
}

/* Require each counterexample's specific rejection reason rather than accepting any unrelated failure. */
static void expect_failure(struct point point, const struct point *previous,
                            uint64_t planned, const char *reason) {
  struct point anchor = synthetic_point(0);
  mach_timebase_info_data_t base = {1, 1};
  uint64_t elapsed = 0, gap = 0, span = 0, suspension = 0;
  const char *failure = validate_point(&point, previous, &anchor.identity, midpoint(&anchor),
                                      planned, 10000000, &base, &elapsed, &gap, &span, &suspension);
  assert(failure && !strcmp(failure, reason));
}

/* Supplied clock and identity boundaries do not depend on current uptime, scheduler timing, or running processes. */
static void test_policy(void) {
  test_parameters();
  uint64_t converted = 0;
  mach_timebase_info_data_t base = {125, 3}, zero = {0, 1};
  assert(ticks_to_ns(3, &base, &converted) && converted == 125);
  assert(ticks_to_ns(1, &base, &converted) && converted == 41);
  assert(!ticks_to_ns(UINT64_MAX, &base, &converted));
  assert(!ticks_to_ns(1, &zero, &converted));
  assert(!delta_ns(2, 1, &base, &converted));
  struct point first = synthetic_point(0), next = synthetic_point(10000000), changed;
  for (unsigned kind = 0; kind < 6; ++kind) {
    /* Each identity-coordinate change must independently reject, rather than checking only PID or path. */
    changed = next;
    switch (kind) {
      case 0: ++changed.identity.pid; break;
      case 1: ++changed.identity.uid; break;
      case 2: ++changed.identity.birth_sec; break;
      case 3: ++changed.identity.birth_usec; break;
      case 4: ++changed.identity.start_ticks; break;
      default: strcpy(changed.identity.executable, "/different"); break;
    }
    expect_failure(changed, &first, 10000000, "identity_changed");
  }
  changed = next; changed.before_end_ticks = changed.before_ticks - 1;
  expect_failure(changed, &first, 10000000, "clock_order");
  changed = next; --changed.lifetime_peak_bytes;
  expect_failure(changed, &first, 10000000, "peak_moved_backwards");
  changed = next; changed.awake_before_ticks -= 100; changed.awake_after_ticks -= 100;
  expect_failure(changed, &first, 10000000, "suspension_observed");
  changed = synthetic_point(30000000);
  expect_failure(changed, &first, 10000000, "sampling_gap");
  changed = synthetic_point(20000000);
  expect_failure(changed, &first, 10000000, "missed_deadline");
  expect_failure(first, NULL, 10000000, "sample_before_deadline");
  changed = next; changed.after_begin_ticks += 10000001; changed.after_ticks += 10000001;
  changed.awake_after_ticks += 10000001;
  expect_failure(changed, &first, 10000000, "query_exceeds_interval");

  /* The writer rejects byte-budget exhaustion before output while retaining reserved footer capacity. */
  FILE *file = tmpfile();
  assert(file);
  struct writer writer = {file, MAX_OUTPUT_BYTES - FOOTER_RESERVE};
  assert(!write_record(&writer, false, "x") && ftell(file) == 0);
  assert(write_record(&writer, true, "x") && ftell(file) == 1);
  assert(fclose(file) == 0);
}

/* Emit three points with the production serializer for independent Python format and boundary verification. */
static int emit_fixture(bool over_budget) {
  struct point first = synthetic_point(0);
  struct options options = {.expected = first.identity, .duration_ns = 20000000,
                            .interval_ns = 10000000, .planned_records = 3};
  mach_timebase_info_data_t base = {1, 1};
  struct writer writer = {stdout, 0};
  struct summary summary = {0};
  assert(write_header(&writer, &options, &base, midpoint(&first)));
  struct point previous = {0};
  for (uint64_t index = 0; index < 3; ++index) {
    /* Falling current at the last point, with unchanged lifetime peak, distinguishes the two counters. */
    struct point point = synthetic_point(index * options.interval_ns);
    point.current_bytes = index == 0 ? BUDGET_BYTES - 2 : (index == 1 ? BUDGET_BYTES - 1 : 50000000);
    if (over_budget && index == 2) {
      /* Equality must fail: strictly below is not less than or equal to budget. */
      point.lifetime_peak_bytes = BUDGET_BYTES;
    }
    uint64_t elapsed = 0, gap = 0, span = 0, suspension = 0;
    assert(!validate_point(&point, index ? &previous : NULL, &options.expected, midpoint(&first),
                            index * options.interval_ns, options.interval_ns, &base,
                            &elapsed, &gap, &span, &suspension));
    assert(write_sample(&writer, &point, index, index * options.interval_ns, elapsed, gap, span, suspension));
    accumulate(&summary, &point, gap, span);
    previous = point;
  }
  assert(write_footer(&writer, &summary, true, NULL));
  return ferror(stdout) ? 1 : 0;
}

/* Entry points operate on synthetic files and memory only, with no live PID-query mode. */
int main(int argc, char **argv) {
  if (argc == 2 && !strcmp(argv[1], "policy")) {
    /* Each invocation performs a finite, independent set of pure policy assertions. */
    test_policy();
    puts("synthetic policy assertions passed");
    return 0;
  }
  if (argc == 2 && !strcmp(argv[1], "fixture")) return emit_fixture(false);
  if (argc == 2 && !strcmp(argv[1], "fixture-over-budget")) return emit_fixture(true);
  if (argc == 3 && !strcmp(argv[1], "create-only")) {
    /* The file fixture exercises exclusive creation; existing paths cannot be overwritten or deleted. */
    FILE *file = create_output(argv[2]);
    if (!file) return EXIT_IO;
    bool written = fputs("synthetic-private-output\n", file) >= 0;
    bool closed = fclose(file) == 0;
    return written && closed ? 0 : EXIT_IO;
  }
  return EXIT_USAGE;
}
