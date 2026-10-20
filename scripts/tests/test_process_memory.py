"""只编译工具、解析合成记录并操作隔离临时文件，不查询或启动产品进程。"""

import copy
import importlib.util
import io
import json
import os
from pathlib import Path
import stat
import subprocess
import sys
import tempfile
import unittest

SCRIPTS = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location("memory_report", SCRIPTS / "macos-process-memory-report.py")
REPORT = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(REPORT)


class ProcessMemoryTests(unittest.TestCase):
    """独立生产序列化器和离线复核器交叉验证，真实 libproc 路径保持未执行。"""

    @classmethod
    def setUpClass(cls):
        # 测试拥有整个输出目录；编译不会写入仓库或复用历史二进制。
        cls.directory = tempfile.TemporaryDirectory(prefix="omni-memory-pure-")
        cls.root = Path(cls.directory.name)
        cls.fixture = cls.root / "fixture"
        cls.sampler = cls.root / "sampler"
        if sys.platform != "darwin":
            # 此工具依赖 macOS SDK；其他平台明确跳过，而不伪造 ABI 编译结果。
            cls.directory.cleanup()
            raise unittest.SkipTest("requires the macOS SDK for compile-only/pure fixtures")
        for source, output in ((SCRIPTS / "macos-process-memory-sampler.c", cls.sampler),
                               (SCRIPTS / "tests" / "process_memory_fixture.c", cls.fixture)):
            # Wall/Wextra/Werror 和产品相同的 macOS 14 build floor 均为强制参数。
            subprocess.run(["xcrun", "clang", "-std=c17", "-Wall", "-Wextra", "-Werror",
                            "-mmacosx-version-min=14.0", str(source), "-o", str(output)],
                           check=True, capture_output=True, timeout=30)
        raw = subprocess.run([str(cls.fixture), "fixture"], check=True, capture_output=True, timeout=5).stdout
        cls.fixture_records = [json.loads(line) for line in raw.splitlines()]
        cls.raw = raw

    @classmethod
    def tearDownClass(cls):
        # TemporaryDirectory 只清理本测试创建的内容，不接触用户文件。
        cls.directory.cleanup()

    def inspect(self, records=None, exit_code=78):
        """辅助函数只从 bytes 流读取；不会调用 CLI 的真实进程采样入口。"""
        raw = self.raw if records is None else self.encode(records)
        return REPORT.inspect_stream(io.BytesIO(raw), exit_code)

    @staticmethod
    def encode(records):
        """逐行 JSON fixture 与生产边界一致，保留最后一个换行。"""
        return b"".join(json.dumps(item, separators=(",", ":")).encode() + b"\n" for item in records)

    def changed(self):
        """每个反例拥有自己的副本，避免测试之间共享被修改的证据。"""
        return copy.deepcopy(self.fixture_records)

    def test_c_policy_identities_clocks_limits_and_writer_reserve(self):
        result = subprocess.run([str(self.fixture), "policy"], capture_output=True, timeout=5, check=False)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout, b"synthetic policy assertions passed\n")

    def test_valid_single_pid_never_claims_whole_product_pass(self):
        result = self.inspect()
        self.assertTrue(result["observation_valid"])
        self.assertEqual(result["sampled_max_current_bytes"], "149999999")
        self.assertEqual(result["max_lifetime_peak_bytes"], "149999999")
        self.assertEqual(result["exit_code"], 78)
        self.assertFalse(result["whole_product_budget_pass"])
        self.assertFalse(result["steady_growth_assessed"])
        self.assertEqual(result["membership"], "unverified")

    def test_lifetime_only_exceedance_is_distinct_from_sampled_current(self):
        raw = subprocess.run([str(self.fixture), "fixture-over-budget"], check=True,
                             capture_output=True, timeout=5).stdout
        result = REPORT.inspect_stream(io.BytesIO(raw), 1)
        self.assertTrue(result["single_pid_current_within_budget"])
        self.assertFalse(result["single_pid_lifetime_within_budget"])
        self.assertEqual(result["max_lifetime_peak_bytes"], "150000000")
        self.assertEqual(result["exit_code"], 1)

    def test_current_exactly_at_limit_also_fails(self):
        records = self.changed()
        # 真实非原子记录可在读peak之后升至150MB；较小peak不能使已经超限的current通过。
        records[2]["physical_footprint_bytes"] = "150000000"
        footer = records[-1]
        footer["max_current_bytes"] = "150000000"
        footer["single_pid_current_within_budget"] = False
        result = self.inspect(records, 1)
        self.assertFalse(result["single_pid_current_within_budget"])
        self.assertTrue(result["single_pid_lifetime_within_budget"])
        self.assertEqual(result["max_lifetime_peak_bytes"], "149999999")
        self.assertEqual(result["budget_bytes"], "150000000")

    def test_full_u64_counters_never_pass_through_float(self):
        records = self.changed()
        for sample in records[1:-1]:
            # 超出 JavaScript 精确整数范围的字符串仍保持原值，并明确超限。
            sample["physical_footprint_bytes"] = str((1 << 64) - 1)
            sample["lifetime_peak_bytes"] = str((1 << 64) - 1)
        footer = records[-1]
        footer["max_current_bytes"] = footer["max_lifetime_peak_bytes"] = str((1 << 64) - 1)
        footer["single_pid_current_within_budget"] = footer["single_pid_lifetime_within_budget"] = False
        result = self.inspect(records, 1)
        self.assertEqual(result["sampled_max_current_bytes"], "18446744073709551615")

    def test_epoch_uid_and_anchor_claim_each_must_match(self):
        changes = {"pid": 43, "uid": 502, "birth_sec": "1001", "birth_usec": "100",
                   "start_abstime": "12346", "identity_matches_anchor": False}
        for field, value in changes.items():
            # 只改变一个身份分量；其余记录仍完整，拒绝不能依赖别的格式错误。
            with self.subTest(field=field):
                records = self.changed()
                records[2][field] = value
                with self.assertRaises(REPORT.InvalidObservation):
                    self.inspect(records)

    def test_invalid_path_encodings_and_root_uid_are_rejected(self):
        for path in ("00", "2f00", "zz", "2F", "61", "", "2f" * 4096):
            # 十六进制只是文件系统字节编码，不允许 NUL、相对路径或超长路径。
            with self.subTest(path=path[:20]):
                records = self.changed()
                records[0]["executable_hex"] = path
                with self.assertRaises(REPORT.InvalidObservation):
                    self.inspect(records)
        records = self.changed()
        records[0]["uid"] = 0
        with self.assertRaises(REPORT.InvalidObservation):
            self.inspect(records)

    def test_timebase_is_not_assumed_to_be_one_to_one(self):
        records = self.changed()
        records[0]["timebase_numer"], records[0]["timebase_denom"] = 125, 3
        for index, sample in enumerate(records[1:-1]):
            # 240000 ticks 在 125:3 下恰为 10ms；读取括号原始宽度仍为 100 ticks。
            adjustment = index * (10000000 - 240000)
            for field in ("continuous_before_ticks", "continuous_before_end_ticks", "awake_before_ticks",
                          "continuous_after_begin_ticks", "continuous_after_ticks", "awake_after_ticks"):
                sample[field] = str(int(sample[field]) - adjustment)
            sample["query_span_ns"] = "4166"
        records[-1]["max_query_span_ns"] = "4166"
        self.assertEqual(self.inspect(records)["max_gap_ns"], "10000000")

    def test_raw_clock_order_gap_suspension_and_deadline_are_recomputed(self):
        changes = (("continuous_before_end_ticks", "0"), ("awake_after_ticks", "0"),
                   ("elapsed_ns", "0"), ("gap_ns", "0"), ("query_span_ns", "0"),
                   ("suspension_lower_bound_ns", "1"))
        for field, value in changes:
            # 摘要字段也必须由原始时钟重新验证，不能信任采样器声称 valid。
            with self.subTest(field=field):
                records = self.changed()
                records[2][field] = value
                with self.assertRaises(REPORT.InvalidObservation):
                    self.inspect(records)
        records = self.changed()
        for field in ("awake_before_ticks", "awake_after_ticks"):
            records[2][field] = str(int(records[2][field]) - 100)
        with self.assertRaises(REPORT.InvalidObservation):
            self.inspect(records)
        records = self.changed()
        for field in ("continuous_before_ticks", "continuous_before_end_ticks", "awake_before_ticks",
                      "continuous_after_begin_ticks", "continuous_after_ticks", "awake_after_ticks"):
            records[2][field] = str(int(records[2][field]) + 20000000)
        with self.assertRaises(REPORT.InvalidObservation):
            self.inspect(records)

    def test_nonatomic_current_keeps_original_peak_and_monotonicity(self):
        records = self.changed()
        # 先读到较小peak、再读到较大current是合法外部输入，仍保原样本而非重采或填充值。
        records[1]["lifetime_peak_bytes"] = "149999997"
        result = self.inspect(records)
        self.assertEqual(result["sampled_max_current_bytes"], "149999999")
        self.assertEqual(result["max_lifetime_peak_bytes"], "149999999")
        records = self.changed()
        records[3]["lifetime_peak_bytes"] = "99999999"
        with self.assertRaises(REPORT.InvalidObservation):
            self.inspect(records)

    def test_missing_middle_duplicate_and_reordered_samples_are_rejected(self):
        records = self.changed()
        for candidate in (records[:2] + records[3:], records[:2] + records[1:],
                          [records[0], records[2], records[1], records[3], records[4]]):
            # 缺少一次采样不通过插值、零填充或重排恢复。
            with self.assertRaises(REPORT.InvalidObservation):
                self.inspect(candidate)

    def test_missing_false_and_followed_footer_are_rejected(self):
        with self.assertRaises(REPORT.InvalidObservation):
            self.inspect(self.fixture_records[:-1])
        records = self.changed()
        records[-1]["collection_complete"] = False
        with self.assertRaises(REPORT.InvalidObservation):
            self.inspect(records)
        with self.assertRaises(REPORT.InvalidObservation):
            self.inspect(self.fixture_records + [self.fixture_records[1]])

    def test_failure_record_and_footer_cannot_be_skipped(self):
        records = self.changed()
        records.insert(2, {"type": "failure", "failure": "sample_unavailable"})
        with self.assertRaises(REPORT.InvalidObservation):
            self.inspect(records)
        records = self.changed()
        records[-1]["failure"] = "sample_unavailable"
        with self.assertRaises(REPORT.InvalidObservation):
            self.inspect(records)

    def test_footer_max_flags_and_membership_cannot_be_forged(self):
        changes = {"max_current_bytes": "99999999", "max_lifetime_peak_bytes": "99999999",
                   "max_gap_ns": "1", "max_query_span_ns": "1", "records": 2,
                   "single_pid_current_within_budget": False, "single_pid_lifetime_within_budget": False,
                   "whole_product_budget_pass": True, "membership": "proven", "steady_growth_assessed": True}
        for field, value in changes.items():
            # footer 只是可复核摘要，不是允许覆盖原始数据或归属状态的授权。
            with self.subTest(field=field):
                records = self.changed()
                records[-1][field] = value
                with self.assertRaises(REPORT.InvalidObservation):
                    self.inspect(records)

    def test_exit_code_must_confirm_observer_finished_without_io_error(self):
        for exit_code in (0, 1, 64, 65, 74, -15):
            # 文件已经含 complete footer，fclose/进程退出失败仍不能接受。
            with self.subTest(exit_code=exit_code):
                with self.assertRaises(REPORT.InvalidObservation):
                    self.inspect(exit_code=exit_code)

    def test_decimal_json_types_unknown_fields_and_duplicate_keys_fail_closed(self):
        for value in (100000000, 100000000.0, True, "NaN", "1e8", "00", "-1", str(1 << 64)):
            # 不接受 Python/JSON 的隐式数值转换或非规范数字串。
            with self.subTest(value=value):
                records = self.changed()
                records[1]["physical_footprint_bytes"] = value
                with self.assertRaises(REPORT.InvalidObservation):
                    self.inspect(records)
        raw = self.raw.replace(b'"pid":42', b'"pid":43,"pid":42', 1)
        with self.assertRaises(REPORT.InvalidObservation):
            REPORT.inspect_stream(io.BytesIO(raw), 78)
        records = self.changed()
        records[0]["coalition_members"] = [42]
        with self.assertRaises(REPORT.InvalidObservation):
            self.inspect(records)

    def test_byte_line_record_and_newline_limits_are_enforced(self):
        for raw in (b"", self.raw[:-1], b"x" * REPORT.MAX_LINE_BYTES + b"\n",
                    b'{"x":NaN}\n', b"{}\n" * (REPORT.MAX_RECORDS + 5)):
            # 所有反例都来自内存，且解析器读取本身保留字节和行预算。
            with self.assertRaises(REPORT.InvalidObservation):
                REPORT.inspect_stream(io.BytesIO(raw), 78)

    def test_identify_and_sample_bad_cli_never_create_output(self):
        with tempfile.TemporaryDirectory(dir=self.root) as directory:
            path = Path(directory) / "must-not-exist"
            base = [str(self.sampler), "sample", "--pid", "42", "--birth-sec", "1000",
                    "--birth-usec", "99", "--start-abstime", "12345", "--executable", "/Synthetic",
                    "--duration-ms", "20", "--interval-ms", "10", "--output", str(path)]
            bad_args = ([str(self.sampler), "identify", "--pid", "0"],
                        [str(self.sampler), "identify", "--pid", "42", "--output", str(path)],
                        base + ["--pid", "42"], base[:-4] + ["--interval-ms", "nan", "--output", str(path)])
            for args in bad_args:
                # 每个形状在 parse_options 阶段拒绝；没有执行任何合法 PID 查询。
                result = subprocess.run(args, capture_output=True, timeout=5, check=False)
                self.assertEqual(result.returncode, 64)
                self.assertFalse(path.exists())
            result = subprocess.run([str(self.sampler), "--help"], capture_output=True, timeout=5, check=False)
            self.assertEqual(result.returncode, 0)
            self.assertIn(b"single PID only", result.stdout)

    def test_output_exclusive_creation_preserves_existing_hardlink_and_symlink_victim(self):
        with tempfile.TemporaryDirectory(dir=self.root) as directory:
            root = Path(directory)
            victim, link, alias, fresh = (root / name for name in ("victim", "link", "alias", "fresh"))
            victim.write_bytes(b"synthetic unchanged victim")
            link.symlink_to(victim)
            os.link(victim, alias)
            for path in (victim, link, alias):
                # O_EXCL 在任何 truncate/write 前拒绝；工具不删除失败路径。
                result = subprocess.run([str(self.fixture), "create-only", str(path)],
                                        capture_output=True, timeout=5, check=False)
                self.assertEqual(result.returncode, 74)
                self.assertEqual(victim.read_bytes(), b"synthetic unchanged victim")
                self.assertTrue(path.exists())
            result = subprocess.run([str(self.fixture), "create-only", str(fresh)],
                                    capture_output=True, timeout=5, check=False)
            self.assertEqual(result.returncode, 0)
            self.assertEqual(stat.S_IMODE(fresh.stat().st_mode), 0o600)
            self.assertEqual(fresh.read_bytes(), b"synthetic-private-output\n")

    def test_report_cli_requires_exit_status_and_never_returns_success(self):
        with tempfile.TemporaryDirectory(dir=self.root) as directory:
            path = Path(directory) / "synthetic.jsonl"
            path.write_bytes(self.raw)
            result = subprocess.run([sys.executable, "-B", str(SPEC.origin), str(path),
                                     "--observer-exit-code", "78"], capture_output=True, timeout=5, check=False)
            self.assertEqual(result.returncode, 78)
            self.assertFalse(json.loads(result.stdout)["whole_product_budget_pass"])
            result = subprocess.run([sys.executable, "-B", str(SPEC.origin), str(path),
                                     "--observer-exit-code", "74"], capture_output=True, timeout=5, check=False)
            self.assertEqual(result.returncode, 65)
            self.assertFalse(json.loads(result.stdout)["observation_valid"])

    def test_report_read_rejects_symlink_and_oversized_artifact(self):
        with tempfile.TemporaryDirectory(dir=self.root) as directory:
            path, alias = Path(directory) / "synthetic.jsonl", Path(directory) / "link"
            path.write_bytes(self.raw)
            alias.symlink_to(path)
            with self.assertRaises(OSError):
                REPORT.inspect_file(alias, 78)
            # 用本测试独占的真实稀疏文件越过生产上限，不改常量、替换解析器或制造并发钩子。
            # fstat 必须在读取记录前拒绝它；稀疏扩展不分配同样大小的内存或磁盘数据块。
            with path.open("wb") as output:
                output.truncate(REPORT.MAX_BYTES + 1)
            with self.assertRaises(REPORT.InvalidObservation):
                REPORT.inspect_file(path, 78)


if __name__ == "__main__":
    # 标准测试入口不包含真实 identify/sample 或任何产品原生进程启动。
    unittest.main()
