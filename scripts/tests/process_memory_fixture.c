/* 只用合成标量测试采样策略；此程序不调用 capture_point 或任何 PID 查询。 */
#define main memory_sampler_entry
#include "../macos-process-memory-sampler.c"
#undef main
#include <assert.h>

/* 固定测试身份不来自运行中的进程，也不声称任何真实产品归属。 */
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

/* 参数测试只到纯解析边界；即使构造完整 sample 形状也不会调用其 main。 */
static void test_parameters(void) {
  uint64_t value = 77;
  const char *bad[] = {"", "-1", "+1", " 1", "1 ", "1.0", "nan", "inf",
                       "0x10", "18446744073709551616"};
  for (size_t index = 0; index < sizeof(bad) / sizeof(bad[0]); ++index) {
    /* 不合法输入不改变 out，防止错误路径发布默认零身份。 */
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
  assert(!parse_options(18, args, &options)); /* 10ms 下会超过 65536 个样本。 */
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

/* 单一失败出口保证每个反例验证的是指定原因，而非任意拒绝。 */
static void expect_failure(struct point point, const struct point *previous,
                            uint64_t planned, const char *reason) {
  struct point anchor = synthetic_point(0);
  mach_timebase_info_data_t base = {1, 1};
  uint64_t elapsed = 0, gap = 0, span = 0, suspension = 0;
  const char *failure = validate_point(&point, previous, &anchor.identity, midpoint(&anchor),
                                      planned, 10000000, &base, &elapsed, &gap, &span, &suspension);
  assert(failure && !strcmp(failure, reason));
}

/* 时间和身份边界在内存中构造，不依赖当前系统 uptime、调度或正在运行的程序。 */
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
    /* 每个身份分量变化都独立拒绝，而不是只验证 PID 或路径。 */
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

  /* 写入器的字节边界必须在任何输出前拒绝，footer 仍有保留空间。 */
  FILE *file = tmpfile();
  assert(file);
  struct writer writer = {file, MAX_OUTPUT_BYTES - FOOTER_RESERVE};
  assert(!write_record(&writer, false, "x") && ftell(file) == 0);
  assert(write_record(&writer, true, "x") && ftell(file) == 1);
  assert(fclose(file) == 0);
}

/* 用生产序列化器产生三点 fixture，供独立 Python 复核器交叉验证格式和边界。 */
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
    /* 最后一点下降的 current 与不下降的 lifetime 区分两个计数器的语义。 */
    struct point point = synthetic_point(index * options.interval_ns);
    point.current_bytes = index == 0 ? BUDGET_BYTES - 2 : (index == 1 ? BUDGET_BYTES - 1 : 50000000);
    if (over_budget && index == 2) {
      /* 恰好达到上限也失败，不能把“低于”误实现为“小于等于”。 */
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

/* 这些入口只操作合成文件和内存；明确不提供真实 PID 查询入口。 */
int main(int argc, char **argv) {
  if (argc == 2 && !strcmp(argv[1], "policy")) {
    /* 每次运行独立完成固定次数的纯策略断言。 */
    test_policy();
    puts("synthetic policy assertions passed");
    return 0;
  }
  if (argc == 2 && !strcmp(argv[1], "fixture")) return emit_fixture(false);
  if (argc == 2 && !strcmp(argv[1], "fixture-over-budget")) return emit_fixture(true);
  if (argc == 3 && !strcmp(argv[1], "create-only")) {
    /* 文件 fixture 仅测试独占创建边界；既存路径不能被覆盖或删除。 */
    FILE *file = create_output(argv[2]);
    if (!file) return EXIT_IO;
    bool written = fputs("synthetic-private-output\n", file) >= 0;
    bool closed = fclose(file) == 0;
    return written && closed ? 0 : EXIT_IO;
  }
  return EXIT_USAGE;
}
