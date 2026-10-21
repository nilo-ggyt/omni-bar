"""Compile tools, parse synthetic records, and use isolated files without querying or starting product processes."""

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
    """Cross-check the production serializer against offline verification without executing live libproc capture."""

    @classmethod
    def setUpClass(cls):
        # This class owns all compiler output; neither repository files nor historical binaries are reused.
        cls.directory = tempfile.TemporaryDirectory(prefix="omni-memory-pure-")
        # Class cleanups also run when setUpClass raises or skips; no compiler/fixture failure can bypass directory cleanup.
        cls.addClassCleanup(cls.directory.cleanup)
        cls.root = Path(cls.directory.name)
        cls.fixture = cls.root / "fixture"
        cls.sampler = cls.root / "sampler"
        if sys.platform != "darwin":
            # This tool requires the macOS SDK; other platforms skip instead of claiming an ABI compile result.
            raise unittest.SkipTest("requires the macOS SDK for compile-only/pure fixtures")
        for source, output in ((SCRIPTS / "macos-process-memory-sampler.c", cls.sampler),
                               (SCRIPTS / "tests" / "process_memory_fixture.c", cls.fixture)):
            # Compiler warnings are errors, and the macOS 14 deployment floor matches the product build.
            subprocess.run(["xcrun", "clang", "-std=c17", "-Wall", "-Wextra", "-Werror",
                            "-mmacosx-version-min=14.0", str(source), "-o", str(output)],
                           check=True, capture_output=True, timeout=30)
        raw = subprocess.run([str(cls.fixture), "fixture"], check=True, capture_output=True, timeout=5).stdout
        cls.fixture_records = [json.loads(line) for line in raw.splitlines()]
        cls.raw = raw

    def inspect(self, records=None, exit_code=78):
        """Read a byte stream directly, without invoking the CLI's live process-sampling entry point."""
        raw = self.raw if records is None else self.encode(records)
        return REPORT.inspect_stream(io.BytesIO(raw), exit_code)

    @staticmethod
    def encode(records):
        """Preserve the serializer's JSONL boundary, including the final newline."""
        return b"".join(json.dumps(item, separators=(",", ":")).encode() + b"\n" for item in records)

    def changed(self):
        """Give each malformed-input case its own copy instead of sharing mutable evidence between cases."""
        return copy.deepcopy(self.fixture_records)

    def test_c_policy_identities_clocks_limits_and_writer_reserve(self):
        result = subprocess.run([str(self.fixture), "policy"], capture_output=True, timeout=5, check=False)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout, b"synthetic policy assertions passed\n")

    def test_valid_single_pid_never_claims_whole_product_pass(self):
        result = self.inspect()
        self.assertTrue(result["observation_valid"])
        self.assertEqual(result["sampled_max_current_bytes"], "199999999")
        self.assertEqual(result["max_lifetime_peak_bytes"], "199999999")
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
        self.assertEqual(result["max_lifetime_peak_bytes"], "200000000")
        self.assertEqual(result["exit_code"], 1)

    def test_current_exactly_at_limit_also_fails(self):
        records = self.changed()
        # A non-atomic read can reach exactly 200 MB after reading peak; a smaller peak cannot excuse current at budget.
        records[2]["physical_footprint_bytes"] = "200000000"
        footer = records[-1]
        footer["max_current_bytes"] = "200000000"
        footer["single_pid_current_within_budget"] = False
        result = self.inspect(records, 1)
        self.assertFalse(result["single_pid_current_within_budget"])
        self.assertTrue(result["single_pid_lifetime_within_budget"])
        self.assertEqual(result["max_lifetime_peak_bytes"], "199999999")
        self.assertEqual(result["budget_bytes"], "200000000")

    def test_full_u64_counters_never_pass_through_float(self):
        records = self.changed()
        for sample in records[1:-1]:
            # Decimal strings beyond JavaScript's exact integer range retain their values and explicitly exceed budget.
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
            # Alter one identity coordinate while preserving other records, so rejection cannot depend on malformed JSON.
            with self.subTest(field=field):
                records = self.changed()
                records[2][field] = value
                with self.assertRaises(REPORT.InvalidObservation):
                    self.inspect(records)

    def test_invalid_path_encodings_and_root_uid_are_rejected(self):
        for path in ("00", "2f00", "zz", "2F", "61", "", "2f" * 4096):
            # Hex represents filesystem bytes; embedded NUL, relative paths, and oversized paths remain invalid.
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
            # At 125:3, 240000 ticks is exactly 10 ms; the raw query bracket remains 100 ticks wide.
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
            # Recompute summary fields from raw clocks instead of trusting the sampler's validity claim.
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
        # Peak may be read before a larger current value; preserve that observation without retry or invented counters.
        records[1]["lifetime_peak_bytes"] = "199999997"
        result = self.inspect(records)
        self.assertEqual(result["sampled_max_current_bytes"], "199999999")
        self.assertEqual(result["max_lifetime_peak_bytes"], "199999999")
        records = self.changed()
        records[3]["lifetime_peak_bytes"] = "99999999"
        with self.assertRaises(REPORT.InvalidObservation):
            self.inspect(records)

    def test_missing_middle_duplicate_and_reordered_samples_are_rejected(self):
        records = self.changed()
        for candidate in (records[:2] + records[3:], records[:2] + records[1:],
                          [records[0], records[2], records[1], records[3], records[4]]):
            # A missing observation cannot be repaired through interpolation, zero fill, or record reordering.
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
            # The footer is a verifiable summary, not authority to override raw counters or process membership.
            with self.subTest(field=field):
                records = self.changed()
                records[-1][field] = value
                with self.assertRaises(REPORT.InvalidObservation):
                    self.inspect(records)

    def test_exit_code_must_confirm_observer_finished_without_io_error(self):
        for exit_code in (0, 1, 64, 65, 74, -15):
            # A complete footer cannot hide fclose failure or an unsuccessful observer exit.
            with self.subTest(exit_code=exit_code):
                with self.assertRaises(REPORT.InvalidObservation):
                    self.inspect(exit_code=exit_code)

    def test_decimal_json_types_unknown_fields_and_duplicate_keys_fail_closed(self):
        for value in (100000000, 100000000.0, True, "NaN", "1e8", "00", "-1", str(1 << 64)):
            # Reject implicit Python/JSON numeric conversion and noncanonical decimal strings.
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
            # Malformed evidence stays in memory; parser reads still obey finite byte and line budgets.
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
                # Each shape fails parse_options before executing any valid PID query.
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
                # O_EXCL rejects an existing node before truncation or writing; the tool never deletes that path.
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
            # Cross the real byte ceiling with a class-owned sparse file, without altering constants or parser behavior.
            # fstat must reject it before record reads; sparse growth does not allocate matching memory or disk blocks.
            with path.open("wb") as output:
                output.truncate(REPORT.MAX_BYTES + 1)
            with self.assertRaises(REPORT.InvalidObservation):
                REPORT.inspect_file(path, 78)


if __name__ == "__main__":
    # This standard test entry never invokes live identify/sample or starts a native product process.
    unittest.main()
