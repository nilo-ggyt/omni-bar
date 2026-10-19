"""离线反例只构造真实serializer形状和显式proposal fixture；不采集、不启动产品。"""

import copy
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import struct
import subprocess
import sys
import tempfile
import unittest

SCRIPT = Path(__file__).resolve().parents[1] / "join-input-frame-upper-bounds.py"
SPEC = importlib.util.spec_from_file_location("input_frame_upper_bounds", SCRIPT)
JOIN = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(JOIN)


def encode_lines(rows):
    """保持生产trace要求的最终换行；witness仍显式是提案fixture。"""
    return b"".join(json.dumps(row, separators=(",", ":")).encode() + b"\n" for row in rows)


def trace_fixture(offset=0, numer=1, denom=1):
    """按Host record_value/encode真实字段手写五条独立fixture，不伪造Snapshot或实际输入。"""
    before = {"generation": "9", "utf8_bytes": "1048576", "utf16_units": "1048576"}
    after = {**before, "generation": "10"}
    header = {"schema": "omni.native-source-input-trace", "schema_version": 1, "kind": "header",
              "record_count": 5, "record_capacity": 256,
              "completion_contract": "newline_terminated_complete_footer_required",
              "clock_domain": "mach_absolute_ticks", "nsevent_clock": "raw_f64_bits_unconverted",
              "armed_at_mach_ticks": str(offset + 100), "timebase_numer": numer, "timebase_denom": denom,
              "initial_document": before, "overflowed": False, "invalid_scope": False,
              "aborted_scope": False, "trace_valid": True, "latency_acceptance": False}
    kinds = ("key_down_begin", "selector_begin", "canonical_accepted", "selector_end", "key_down_end")
    records = []
    for index, (kind, ticks) in enumerate(zip(kinds, (1000, 1010, 1050, 1100, 1200))):
        # 一次替换单ASCII字符，初始/最终长度相同；默认标量值来自serializer的实际默认规则。
        scoped = index in (1, 2, 3)
        record = {"kind": "record", "record_kind": kind, "serial": str(index + 1),
                  "mach_ticks": str(offset + ticks), "event_serial": "1",
                  "selector_serial": "1" if scoped else None, "composition_serial": None,
                  "selector": "insert_text" if scoped else None,
                  "nsevent_timestamp_bits": str(struct.unpack(">Q", struct.pack(">d", 123.25))[0]) if index == 0 else None,
                  "document": before if index < 2 else after, "removed_utf16_units": "1" if index == 2 else "0",
                  "inserted_utf16_units": "1" if index == 2 else "0", "native_marked": None if index == 2 else False,
                  "outcome": "committed_changed" if index == 3 else "not_applicable", "rejection": None,
                  "accepted_count": 1 if index == 3 else 0, "rejected_count": 0}
        records.append(record)
    footer = {"schema": "omni.native-source-input-trace", "schema_version": 1, "kind": "complete",
              "export_complete": True, "record_count": 5, "trace_valid": True, "latency_acceptance": False,
              "footer_durability_certified": False}
    return copy.deepcopy([header, *records, footer])


def witness_fixture(trial, trace, candidate_delta=1000, offset=0):
    """未来collector尚未实现：所有模板/可见性/audit事实都是本测试显式构造的模型。"""
    geometry = {"display_id": 1, "display_points": ["0", "0", "1440", "900"],
                "filter_points": ["0", "0", "1440", "900"],
                "source_roi_points": ["100", "100", "128", "24"],
                "content_points": ["0", "0", "128", "24"], "pixel_scale": "2", "content_scale": "1",
                "width": 256, "height": 48, "mask_enabled": False}
    candidate_ticks = offset + 1000 + candidate_delta
    deadline = candidate_ticks + 100000
    header = {"schema": "omni.pixel-witness.proposal", "schema_version": 1, "kind": "header",
              "evidence_mode": "fixture_only", "producer_implemented": False, "actual_capture": False,
              "trial_id": trial["trial_id"], "run_id": trial["run_id"],
              "input_trace_sha256": trial["input_sha256"], "target": trial["target"],
              "initial_document": trial["initial_document"], "old_token_id": trial["old_token_id"],
              "new_token_id": trial["new_token_id"], "clock_domain": "mach_absolute_ticks",
              "timebase_numer": trace[0]["timebase_numer"], "timebase_denom": trace[0]["timebase_denom"],
              "start_ticks": str(offset + 200), "deadline_ticks": str(deadline),
              "required_baseline_frames": 2, "frame_limit": 600, "geometry": geometry,
              "scope": "synthetic_app_display_filtered", "authorization_mode": "existing_screen_recording",
              "recognition": {"revision": 3, "language": "en-US", "level": "accurate",
                              "language_correction": False, "automatic_language_detection": False,
                              "preferred_words": False},
              "prerequisites": {"template_source": "fixture_model", "old_calibrated": True,
                                "new_calibrated": True, "reset_old_verified": True, "glyphs_complete": True,
                                "same_document_and_layout": True, "new_state_unique": True,
                                "target_scope_verified": True, "unlocked": True, "unoccluded": True}}
    frames = []
    for index, (ticks, state) in enumerate(((offset + 600, "old"), (offset + 800, "old"),
                                            (offset + 1500, "old"), (candidate_ticks, "new")), 1):
        # 输入后仍OLD是正常渲染等待；不把这个时钟错当输入前baseline的时间。
        frames.append({"kind": "frame", "run_id": trial["run_id"], "index": index,
                       "display_ticks": str(ticks), "callback_absolute_ticks": str(ticks + 10),
                       "state": state, "baseline_ready": index >= 2, "earliest_observed_candidate": index == 4,
                       "geometry": geometry, "target": trial["target"], "unoccluded": True,
                       "sample_status": "complete"})
    binding = {"run_id": trial["run_id"], "frame_index": 4, "display_ticks": str(candidate_ticks)}
    footer = {"kind": "complete", "run_id": trial["run_id"], "recorded_frames": 4, "failure": "none",
              "witness_valid": True, "baseline_ready": True, "candidate_present": True,
              "earliest_observed": binding, "audit_binding": binding, "audit_complete": True,
              "audit_passed": True, "audit_loan_released": True, "audit_expected_certain": True,
              "audit_baseline_seen": False, "audit_native_objects_released": True,
              "audit_complete_ticks": str(candidate_ticks + 5000), "audit_release_ticks": str(candidate_ticks + 5100),
              "cleanup_complete_ticks": str(candidate_ticks + 6000), "combined_owned_pixel_peak": "262144",
              "owned_pixel_bytes_after_drain": "0", "output_removed": True, "stop_completed": True,
              "capture_completeness_verified": False, "latency_acceptance": False, "physical_scanout_verified": False,
              "observer_exit_code": 0, "known_dropped_frames": 0, "overruns": 0, "deadline_failed": False,
              "picker_client_released": None}
    return copy.deepcopy([header, *frames, footer])


class UpperBoundTests(unittest.TestCase):
    """临时目录是本测试唯一文件权限；所有接受结果仍标记fixture且不证明实际capture。"""

    def setUp(self):
        self.directory = tempfile.TemporaryDirectory(prefix="omni-frame-join-pure-")
        self.root = Path(self.directory.name)
        self.addCleanup(self.directory.cleanup)
        self.manifest = {"schema": "omni.input-frame-trial-set.proposal", "schema_version": 1,
                         "evidence_mode": "fixture_only", "profile": "single_unmarked_ascii_insert_v1",
                         "endpoint": "handler", "planned_trial_count": 0, "trial_ids": [], "trials": [],
                         "thresholds": {"p95_ns": "16700000", "p99_ns": "33400000"}}

    def write(self, name, raw):
        """写入只由测试创建的0600普通文件；工具本身只读。"""
        path = self.root / name
        path.write_bytes(raw)
        path.chmod(0o600)
        return hashlib.sha256(raw).hexdigest()

    def add_trial(self, candidate_delta=1000, offset=0, numer=1, denom=1):
        """每个试次有独立ID/run/file，避免用重复artifact凑样本数量。"""
        index = len(self.manifest["trials"]) + 1
        trace = trace_fixture(offset, numer, denom)
        trial = {"trial_id": f"trial-{index}", "run_id": str(index), "input_file": f"input-{index}.jsonl",
                 "input_sha256": "", "witness_file": f"frame-{index}.jsonl", "witness_sha256": "",
                 "target": {"pid": 4242, "uid": 501, "birth_sec": "100", "birth_usec": "123",
                            "boot_epoch_id": "a" * 64, "bundle_id": "com.example.OmniSynthetic"},
                 "initial_document": trace[0]["initial_document"], "old_token_id": "b" * 64, "new_token_id": "c" * 64}
        trial["input_sha256"] = self.write(trial["input_file"], encode_lines(trace))
        witness = witness_fixture(trial, trace, candidate_delta, offset)
        trial["witness_sha256"] = self.write(trial["witness_file"], encode_lines(witness))
        self.manifest["trial_ids"].append(trial["trial_id"])
        self.manifest["trials"].append(trial)
        self.manifest["planned_trial_count"] = index
        return trial, trace, witness

    def update_trace(self, trial, trace, witness):
        """反例重新绑定正确SHA，确保失败来自语义验证而非无关的hash不符。"""
        trial["input_sha256"] = self.write(trial["input_file"], encode_lines(trace))
        witness[0]["input_trace_sha256"] = trial["input_sha256"]
        self.update_witness(trial, witness)

    def update_witness(self, trial, witness):
        """更新fixture文件而不改变其其余声明。"""
        trial["witness_sha256"] = self.write(trial["witness_file"], encode_lines(witness))

    def inspect(self):
        """显式opt-in仅给proposal fixture，不隐式启用任何captured模式。"""
        self.write("manifest.json", json.dumps(self.manifest).encode())
        return JOIN.inspect_manifest(self.root / "manifest.json", True)

    def assert_invalid_trial(self, expected_reason=None):
        """全组失败不能计算好样本子集的分位或阈值通过。"""
        report = self.inspect()
        self.assertFalse(report["all_declared_trials_valid"])
        self.assertIsNone(report["quantiles"])
        self.assertIsNone(report["fixture_thresholds_pass"])
        if expected_reason:
            self.assertEqual(report["trial_results"][0]["reason"], expected_reason)
        return report

    def test_exact_trace_and_late_frame_produce_only_fixture_upper_bounds(self):
        self.add_trial()
        report = self.inspect()
        self.assertTrue(report["all_declared_trials_valid"])
        self.assertTrue(report["fixture_thresholds_pass"])
        self.assertEqual(report["trial_results"][0]["handler_upper_bound_ns"]["ceil_ns"], "1000")
        self.assertEqual(report["trial_results"][0]["commit_upper_bound_ns"]["ceil_ns"], "900")
        for name in ("actual_capture_verified", "capture_completeness_verified", "physical_scanout_verified",
                     "latency_acceptance", "producer_implemented"):
            self.assertIs(report[name], False)
        self.assertTrue(report["fixture_only"])

    def test_wide_mach_values_survive_without_float_rounding(self):
        self.add_trial(offset=(1 << 63) + 123)
        self.assertEqual(self.inspect()["trial_results"][0]["handler_upper_bound_ns"]["numerator"], "1000")

    def test_fractional_over_threshold_is_not_floored_into_pass(self):
        self.add_trial(candidate_delta=16_700_000 * 3 + 1, denom=3)
        report = self.inspect()
        self.assertFalse(report["fixture_thresholds_pass"])
        p95 = report["quantiles"]["p95"]
        self.assertEqual((p95["numerator"], p95["denominator"], p95["ceil_ns"]),
                         ("50100001", "3", "16700001"))
        self.assertFalse(p95["within_threshold"])
        self.assertTrue(report["quantiles"]["p99"]["within_threshold"])

    def test_nearest_rank_keeps_the_declared_tail(self):
        values = [JOIN.Fraction(number, 3) for number in range(1, 102)]
        self.assertEqual(JOIN.nearest_rank(values, 95), (JOIN.Fraction(96, 3), 96))
        self.assertEqual(JOIN.nearest_rank(values, 99), (JOIN.Fraction(100, 3), 100))
        # 迟到帧扩大观测上界；不能用较小的commit值替换已声明handler端点。
        self.add_trial(candidate_delta=16_700_050)
        self.assertFalse(self.inspect()["fixture_thresholds_pass"])
        self.manifest["endpoint"] = "commit"
        self.assertTrue(self.inspect()["fixture_thresholds_pass"])

    def test_one_failed_trial_preserves_every_declared_result_without_quantiles(self):
        trial, _, witness = self.add_trial()
        self.add_trial()
        witness[2]["state"] = "unknown"
        self.update_witness(trial, witness)
        report = self.assert_invalid_trial("unknown_pixel_state")
        self.assertEqual([row["trial_id"] for row in report["trial_results"]], ["trial-1", "trial-2"])
        self.assertTrue(report["trial_results"][1]["valid_fixture"])

    def test_missing_duplicate_reordered_and_reused_trials_are_rejected(self):
        self.add_trial()
        self.add_trial()
        original = copy.deepcopy(self.manifest)
        mutations = [lambda m: m["trials"].pop(), lambda m: m["trial_ids"].__setitem__(1, "trial-1"),
                     lambda m: m["trials"].reverse(),
                     lambda m: m["trials"][1].__setitem__("run_id", "1"),
                     lambda m: m["trials"][1].__setitem__("input_file", "input-1.jsonl")]
        for mutation in mutations:
            with self.subTest(mutation=mutation):
                self.manifest = copy.deepcopy(original)
                mutation(self.manifest)
                with self.assertRaises(JOIN.InvalidEvidence):
                    self.inspect()

    def test_trace_scope_commit_clock_and_counter_counterfactuals(self):
        trial, trace, witness = self.add_trial()
        changes = [(0, "schema_version", 2), (0, "overflowed", True), (0, "record_count", 6),
                   (0, "clock_domain", "mach_continuous_ticks"), (6, "export_complete", False),
                   (6, "trace_valid", False), (1, "event_serial", None), (2, "selector_serial", "2"),
                   (2, "selector", "set_marked_text"), (3, "rejection", "canonical_error"),
                   (3, "native_marked", False), (4, "outcome", "committed_existing"),
                   (4, "native_marked", True), (4, "accepted_count", 0), (4, "rejected_count", 1),
                   (5, "mach_ticks", "1000"), (1, "nsevent_timestamp_bits", str(0x7ff0000000000000)),
                   (2, "nsevent_timestamp_bits", "0"), (3, "inserted_utf16_units", "2")]
        for index, field, value in changes:
            with self.subTest(index=index, field=field):
                changed = copy.deepcopy(trace)
                changed[index][field] = value
                current_witness = copy.deepcopy(witness)
                self.update_trace(trial, changed, current_witness)
                self.assert_invalid_trial()
        # generation只在canonical处变化；只改结尾来伪装commit不能通过。
        changed = copy.deepcopy(trace)
        changed[3]["document"]["generation"] = "9"
        self.update_trace(trial, changed, copy.deepcopy(witness))
        self.assert_invalid_trial()

    def test_trace_extra_record_and_unknown_field_are_not_silently_ignored(self):
        trial, trace, witness = self.add_trial()
        changed = copy.deepcopy(trace)
        changed.insert(3, copy.deepcopy(changed[2]))
        changed[0]["record_count"] = changed[-1]["record_count"] = 6
        self.update_trace(trial, changed, copy.deepcopy(witness))
        self.assert_invalid_trial("unsupported_input_sequence")
        changed = copy.deepcopy(trace)
        changed[1]["future_causal_hint"] = True
        self.update_trace(trial, changed, copy.deepcopy(witness))
        self.assert_invalid_trial("unsupported_fields")

    def test_witness_captured_schema_and_all_prerequisites_fail_closed(self):
        trial, _, witness = self.add_trial()
        changes = [("evidence_mode", "captured"), ("actual_capture", True), ("producer_implemented", True),
                   ("schema", "current-collector-schema-1"), ("clock_domain", "nsevent_seconds"),
                   ("timebase_numer", 125), ("run_id", "2"), ("new_token_id", "d" * 64),
                   ("scope", "whole_desktop"), ("authorization_mode", "fallback")]
        for field, value in changes:
            with self.subTest(field=field):
                changed = copy.deepcopy(witness)
                changed[0][field] = value
                self.update_witness(trial, changed)
                self.assert_invalid_trial()
        for field in witness[0]["prerequisites"]:
            with self.subTest(prerequisite=field):
                changed = copy.deepcopy(witness)
                changed[0]["prerequisites"][field] = "coretext" if field == "template_source" else False
                self.update_witness(trial, changed)
                self.assert_invalid_trial("unproven_capture_prerequisite")

    def test_target_birth_scope_geometry_and_occlusion_each_veto(self):
        trial, _, witness = self.add_trial()
        mutations = [lambda w: w[0]["target"].__setitem__("birth_usec", "124"),
                     lambda w: w[2].__setitem__("unoccluded", False),
                     lambda w: w[2]["geometry"].__setitem__("pixel_scale", "1.5"),
                     lambda w: w[0]["geometry"].__setitem__("display_points", ["1", "0", "1440", "900"]),
                     lambda w: w[0]["geometry"].__setitem__("content_scale", "2"),
                     lambda w: w[0]["geometry"].__setitem__("mask_enabled", True),
                     lambda w: w[0]["geometry"].__setitem__("height", True)]
        for mutation in mutations:
            with self.subTest(mutation=mutation):
                changed = copy.deepcopy(witness)
                mutation(changed)
                self.update_witness(trial, changed)
                self.assert_invalid_trial()

    def test_preexisting_precommit_and_first_candidate_cannot_be_replaced(self):
        trial, _, witness = self.add_trial()
        changed = copy.deepcopy(witness)
        changed[1]["state"] = "new"
        self.update_witness(trial, changed)
        self.assert_invalid_trial("preexisting_new")
        changed = copy.deepcopy(witness)
        changed[3]["state"] = "new"
        changed[3]["display_ticks"] = "1090"
        changed[3]["callback_absolute_ticks"] = "1095"
        changed[3]["earliest_observed_candidate"] = True
        self.update_witness(trial, changed)
        self.assert_invalid_trial("preexisting_or_precommit_new")
        changed = copy.deepcopy(witness)
        changed[-1]["audit_binding"]["frame_index"] = 3
        self.update_witness(trial, changed)
        self.assert_invalid_trial("audit_identity_mismatch")

    def test_slow_audit_and_new_old_new_keep_original_candidate_time(self):
        trial, _, witness = self.add_trial()
        tail = []
        for index, (ticks, state) in enumerate(((3000, "new"), (4000, "old"), (5000, "new")), 5):
            frame = copy.deepcopy(witness[4])
            frame.update(index=index, display_ticks=str(ticks), callback_absolute_ticks=str(ticks + 10),
                         state=state, earliest_observed_candidate=False)
            tail.append(frame)
        witness[5:5] = tail
        witness[-1]["recorded_frames"] = 7
        self.update_witness(trial, witness)
        result = self.inspect()["trial_results"][0]
        self.assertEqual(result["candidate_frame_index"], 4)
        self.assertEqual(result["handler_upper_bound_ns"]["ceil_ns"], "1000")

    def test_audit_release_stop_deadline_and_false_claims_each_veto(self):
        trial, _, witness = self.add_trial()
        changes = {"audit_complete": False, "audit_passed": False, "audit_loan_released": False,
                   "audit_expected_certain": False, "audit_baseline_seen": True,
                   "audit_native_objects_released": False, "owned_pixel_bytes_after_drain": "1",
                   "combined_owned_pixel_peak": "262145", "output_removed": False, "stop_completed": False,
                   "capture_completeness_verified": True, "latency_acceptance": True,
                   "physical_scanout_verified": True, "observer_exit_code": 1, "known_dropped_frames": 1,
                   "overruns": 1, "deadline_failed": True, "audit_complete_ticks": "2000",
                   "audit_release_ticks": "999999999", "cleanup_complete_ticks": "1"}
        for field, value in changes.items():
            with self.subTest(field=field):
                changed = copy.deepcopy(witness)
                changed[-1][field] = value
                self.update_witness(trial, changed)
                self.assert_invalid_trial()
        changed = copy.deepcopy(witness)
        changed[0]["authorization_mode"] = "picker_application_session"
        self.update_witness(trial, changed)
        self.assert_invalid_trial("picker_cleanup_unverified")
        changed[-1]["picker_client_released"] = True
        self.update_witness(trial, changed)
        self.assertTrue(self.inspect()["all_declared_trials_valid"])

    def test_frame_order_status_and_601st_record_never_get_repaired(self):
        trial, _, witness = self.add_trial()
        changes = {"index": 1, "display_ticks": "600", "callback_absolute_ticks": "700",
                   "state": "unknown", "sample_status": "idle", "baseline_ready": False,
                   "earliest_observed_candidate": True, "run_id": "9"}
        for field, value in changes.items():
            with self.subTest(field=field):
                changed = copy.deepcopy(witness)
                changed[2][field] = value
                self.update_witness(trial, changed)
                self.assert_invalid_trial()
        changed = [witness[0], *([witness[1]] * 601), witness[-1]]
        self.update_witness(trial, changed)
        self.assert_invalid_trial("record_budget")

    def test_digest_incomplete_footer_duplicate_key_and_numeric_types(self):
        trial, _, witness = self.add_trial()
        trial["input_sha256"] = "0" * 64
        self.assert_invalid_trial("input_digest_mismatch")
        trial["input_sha256"] = witness[0]["input_trace_sha256"]
        for raw in (encode_lines(witness)[:-1], encode_lines(witness[:-1]),
                    encode_lines(witness) + b'{}\n',
                    encode_lines(witness).replace(b'"actual_capture":false', b'"actual_capture":false,"actual_capture":false'),
                    encode_lines(witness).replace(b'"index":2', b'"index":2.0'),
                    encode_lines(witness).replace(b'"index":2', b'"index":NaN')):
            with self.subTest(raw_length=len(raw)):
                trial["witness_sha256"] = self.write(trial["witness_file"], raw)
                self.assert_invalid_trial()

    def test_unsafe_files_budgets_and_opt_in_are_explicit(self):
        trial, _, _ = self.add_trial()
        with self.assertRaisesRegex(JOIN.InvalidEvidence, "proposal_fixture_requires_opt_in"):
            JOIN.inspect_manifest(self.root / "manifest.json")
        source = self.root / trial["witness_file"]
        source.chmod(0o644)
        self.assert_invalid_trial("invalid_artifact_file")
        source.chmod(0o600)
        original = source.read_bytes()
        source.unlink()
        source.symlink_to(self.root / trial["input_file"])
        self.assert_invalid_trial("artifact_io_failure")
        source.unlink()
        self.write(trial["witness_file"], original)
        trial["witness_file"] = "../outside.jsonl"
        self.assert_invalid_trial("invalid_artifact_name")

    def test_cli_contract_fixture_success_and_actual_capture_never_pass(self):
        self.add_trial()
        self.inspect()
        command = [sys.executable, "-B", str(SCRIPT)]
        contract = subprocess.run([*command, "--describe-contract"], capture_output=True, timeout=5, check=False)
        self.assertEqual(contract.returncode, 0)
        self.assertEqual(json.loads(contract.stdout)["capture_modes_accepted"], [])
        simulated = subprocess.run([*command, str(self.root / "manifest.json"), "--allow-proposal-fixtures"],
                                   capture_output=True, timeout=5, check=False)
        self.assertEqual(simulated.returncode, 78, simulated.stderr)
        report = json.loads(simulated.stdout)
        self.assertFalse(report["latency_acceptance"])
        self.assertTrue(report["fixture_thresholds_pass"])
        refused = subprocess.run([*command, str(self.root / "manifest.json")],
                                 capture_output=True, timeout=5, check=False)
        self.assertEqual(refused.returncode, 65)
        self.assertEqual(json.loads(refused.stdout)["reason"], "proposal_fixture_requires_opt_in")


if __name__ == "__main__":
    # 普通unittest discovery仅解析自有合成文件，不运行任何原生采集路径。
    unittest.main()
