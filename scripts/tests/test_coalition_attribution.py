"""No-privilege parser/identity regressions; synthetic dictionaries are deliberately not a claimed tool schema."""

import importlib.util
import io
import json
import subprocess
import sys
import plistlib
from pathlib import Path
import tempfile
from types import SimpleNamespace
import unittest
from unittest import mock
from contextlib import redirect_stdout

SPEC = importlib.util.spec_from_file_location("attribution", Path(__file__).parents[1] / "macos_coalition_attribution.py")
ATTRIBUTION = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(ATTRIBUTION)


class AttributionTests(unittest.TestCase):
    """Exercise fail-closed semantics and file/identity limits without invoking libproc, sudo or powermetrics."""

    def test_valid_nul_framing_still_refuses_unknown_semantics(self):
        # A syntactically valid pair contains only synthetic labels, never invented measured field definitions.
        frame = plistlib.dumps({"synthetic_test_only": "not a powermetrics schema"})
        result = ATTRIBUTION.inspect_plist(frame + b"\0\n" + frame + b"\0")
        self.assertEqual(result["frame_count"], 2)
        self.assertEqual(result["schema_status"], "unknown_unapproved")
        self.assertEqual(result["membership_status"], "unverified")
        self.assertEqual(result["whole_product_physical_footprint"], "unmeasured")
        self.assertNotIn("coalition_members", result)

    def test_inspect_cli_returns_non_success_for_valid_unknown_schema(self):
        with tempfile.TemporaryDirectory() as directory:
            # A pipeline must not mistake valid plist framing for a successful attribution gate.
            path = Path(directory) / "synthetic.plist"
            path.write_bytes(plistlib.dumps({"synthetic_test_only": "unapproved"}))
            result = subprocess.run([sys.executable, str(Path(SPEC.origin)), "inspect", str(path)],
                                    capture_output=True, timeout=5, check=False)
            self.assertEqual(result.returncode, ATTRIBUTION.UNKNOWN_SCHEMA)
            self.assertEqual(json.loads(result.stdout)["membership_status"], "unverified")

    def test_incomplete_diagnostic_and_binary_frames_are_rejected(self):
        # Neither privilege diagnostics nor unsupported binary NULs may be accepted as partial sample records.
        for raw in (b"", b"sudo: a password is required\n", b"<?xml version='1.0'?><plist>",
                    plistlib.dumps({"synthetic_test_only": 1}, fmt=plistlib.FMT_BINARY)):
            # Each failure is independent and never invokes any privileged process.
            with self.subTest(raw=raw[:32]):
                with self.assertRaises(ATTRIBUTION.CaptureError):
                    ATTRIBUTION.inspect_plist(raw)

    def test_record_and_byte_budgets_fail_closed(self):
        # A valid-looking oversized record sequence cannot drive an unbounded parsing loop.
        frame = plistlib.dumps({"synthetic_test_only": []})
        with self.assertRaises(ATTRIBUTION.CaptureError):
            ATTRIBUTION.inspect_plist((frame + b"\0") * (ATTRIBUTION.MAX_SAMPLES + 5))
        with tempfile.TemporaryDirectory() as directory:
            # Bounded file reading detects the first extra byte, including a concurrently extended artifact.
            path = Path(directory) / "oversized"
            path.write_bytes(b"12345")
            with self.assertRaises(ATTRIBUTION.CaptureError):
                ATTRIBUTION.read_bounded(path, 4)

    def test_target_epoch_executable_and_bundle_are_all_required(self):
        # Numeric PID equality alone must never reuse a prior process epoch or a different executable's label.
        observed = {"pid": 42, "birth": "1000:99", "executable": "/Synthetic.app/Contents/MacOS/Synthetic",
                    "bundle": {"identifier": "test.synthetic", "executable_matches": True}}
        expected = {"pid": 42, "birth": "1000:99", "executable": observed["executable"], "bundle_id": "test.synthetic"}
        ATTRIBUTION.require_anchor(observed, expected)
        for field, replacement in (("birth", "1000:100"), ("executable", "/different"), ("bundle_id", "different")):
            # Every changed explicit authority independently invalidates the selection.
            changed = dict(expected, **{field: replacement})
            with self.subTest(field=field):
                with self.assertRaises(ATTRIBUTION.CaptureError):
                    ATTRIBUTION.require_anchor(observed, changed)

    def test_inventory_byte_cap_writes_no_partial_record(self):
        # Record rejection occurs before the stream changes, preserving a parseable bounded history.
        stream = io.BytesIO()
        stream.seek(ATTRIBUTION.MAX_INVENTORY_BYTES)
        with self.assertRaises(ATTRIBUTION.CaptureError):
            ATTRIBUTION.append_inventory(stream, {"synthetic_test_only": 1})
        self.assertEqual(stream.getvalue(), b"")

    def test_malformed_bundle_metadata_is_not_identity(self):
        with tempfile.TemporaryDirectory() as directory:
            # An app-looking path needs actual dictionary metadata and a declared string identifier.
            executable = Path(directory) / "Synthetic.app" / "Contents" / "MacOS" / "Synthetic"
            executable.parent.mkdir(parents=True)
            info = executable.parents[1] / "Info.plist"
            info.write_bytes(plistlib.dumps(["synthetic malformed root"]))
            self.assertIsNone(ATTRIBUTION.bundle_identity(str(executable)))

    def test_provenance_failure_preserves_result_without_launching_observer(self):
        # Failing before sampler startup must still expose a private attempt and never invoke sudo as recovery.
        observed = {"pid": 42, "birth": "1000:99", "executable": "/Synthetic.app/Contents/MacOS/Synthetic",
                    "bundle": {"identifier": "test.synthetic", "executable_matches": True}}
        args = SimpleNamespace(pid=42, birth="1000:99", executable=observed["executable"],
                               bundle_id="test.synthetic", samples=1)
        with tempfile.TemporaryDirectory() as directory:
            # Redirect only acquisition seams: the actual failure/result code and its exclusive JSON write execute.
            output = io.StringIO()
            with mock.patch.object(ATTRIBUTION.os, "geteuid", return_value=501), \
                 mock.patch.object(ATTRIBUTION, "ProcessReader") as reader, \
                 mock.patch.object(ATTRIBUTION.tempfile, "mkdtemp", return_value=directory), \
                 mock.patch.object(ATTRIBUTION, "read_bounded", side_effect=OSError("synthetic provenance failure")), \
                 mock.patch.object(ATTRIBUTION.subprocess, "Popen") as launch, redirect_stdout(output):
                reader.return_value.identity.return_value = observed
                self.assertEqual(ATTRIBUTION.collect(args), 1)
            # A reviewer can find the retained failed attempt even though no capabilities or raw sample exist.
            result = json.loads((Path(directory) / "result.json").read_text())
            self.assertEqual(result["collection_status"], "failed")
            self.assertIsNone(result["observer_pid"])
            self.assertIn("synthetic provenance failure", result["failure"])
            self.assertEqual(json.loads(output.getvalue())["directory"], directory)
            launch.assert_not_called()

    def test_observer_cleanup_timeout_remains_an_explicit_failure(self):
        # An unresponsive observer cannot become a successful release claim or trigger a guessed privileged kill.
        observer = mock.Mock()
        observer.poll.return_value = None
        observer.wait.side_effect = subprocess.TimeoutExpired("synthetic observer", 5)
        self.assertFalse(ATTRIBUTION.stop_observer(observer))
        observer.terminate.assert_called_once_with()
        observer.wait.assert_called_once_with(timeout=5)
        observer.kill.assert_not_called()


if __name__ == "__main__":
    # Standard unittest runs only portable pure cases, never an operator collection.
    unittest.main()
