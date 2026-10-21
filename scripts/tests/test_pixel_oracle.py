"""Build and run pure C pixel/sequence counterexamples without macOS SDK, product processes, or capture permission."""

import json
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest


class PixelOracleTests(unittest.TestCase):
    """Own all temporary outputs and rebuild current sources, without reusing historical binary evidence."""

    def test_exact_pixel_and_600_frame_replay_contracts(self):
        # clang and a regular C17 compiler are both supported; a missing compiler explicitly skips rather than passing.
        compiler = shutil.which("clang") or shutil.which("cc")
        if compiler is None:
            # A missing build prerequisite says nothing about pixel, native-runtime, or capture success.
            self.skipTest("requires a C17 compiler for the portable pure fixtures")
        source = Path(__file__).resolve().parents[1] / "macos_frame_probe"
        with tempfile.TemporaryDirectory(prefix="omni-pixel-oracle-pure-") as directory:
            # Strict warnings use portable C checks without linking an Apple framework.
            binary = Path(directory) / "pixel-oracle-tests"
            platform_flags = []
            if sys.platform == "darwin":
                # Keep the product's macOS 14.0 build floor; execution on this host is not macOS 14 runtime acceptance.
                platform_flags.append("-mmacosx-version-min=14.0")
            built = subprocess.run(
                [compiler, "-std=c17", "-Wall", "-Wextra", "-Werror", "-Wconversion",
                 "-Wsign-conversion", "-pedantic", *platform_flags, str(source / "pixel_oracle.c"),
                 str(source / "pixel_oracle_tests.c"), "-o", str(binary)],
                capture_output=True, timeout=30, check=False,
            )
            self.assertEqual(built.returncode, 0, built.stderr.decode(errors="replace"))
            # Finite operations and an external timeout bound execution; output explicitly declares zero capture invocations.
            run = subprocess.run([str(binary)], capture_output=True, timeout=10, check=False)
            self.assertEqual(run.returncode, 0, run.stderr.decode(errors="replace"))
            summary = json.loads(run.stdout)
            self.assertEqual(summary["pure_cases"], 16)
            self.assertGreaterEqual(summary["pure_checks"], 5000)
            self.assertEqual(summary["max_replay_frames"], 600)
            self.assertGreater(summary["owned_scalar_bytes"], 0)
            self.assertEqual(summary["peak_owned_pixel_bytes"], 261120)
            self.assertEqual(summary["capture_invocations"], 0)
            self.assertEqual(summary["native_audits"], 0)
            self.assertFalse(summary["latency_acceptance"])


if __name__ == "__main__":
    # Standard unittest runs these pure cases only, without launching the product or another native harness.
    unittest.main()
