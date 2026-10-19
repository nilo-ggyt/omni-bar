#!/usr/bin/env python3
"""Exercise only help/validation/argument rejection; never run a valid capture invocation."""

import argparse
import hashlib
import json
import os
from pathlib import Path
import subprocess


def run(binary: Path, arguments: list[str]) -> subprocess.CompletedProcess[str]:
    """Bound every CLI child so an unintended native acquisition cannot hang this pure check."""
    # Each caller uses help, validation, or intentionally invalid arguments before any native route.
    return subprocess.run(
        [str(binary), *arguments], capture_output=True, text=True, timeout=5, check=False
    )


def replace_value(arguments: list[str], option: str, value: str) -> list[str]:
    """Change one named fixture value without rebuilding unrelated authorization requirements."""
    copied = arguments.copy()
    copied[copied.index(option) + 1] = value
    return copied


def main() -> None:
    """Verify exclusive authorization policies and finite argument bounds without UI/TCC/capture."""
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    options = parser.parse_args()
    binary = options.binary.resolve(strict=True)
    # The parser only examines the external PID spelling; no identity or permission check is allowed.
    fixture_pid = str(os.getpid())
    base = [
        "--validate", "--synthetic-unlocked", "--pid", fixture_pid,
        "--bundle-id", "com.example.OmniSynthetic", "--display-index", "0",
    ]
    help_result = run(binary, ["--help"])
    assert help_result.returncode == 0 and "--picker" in help_result.stdout
    assert "--existing-authorization" in help_result.stdout
    assert "--expect-token" in help_result.stdout and "--baseline-token" in help_result.stdout

    accepted = 0
    rejected = 0
    for authorization, expected in [
        ("--existing-authorization", "existing_screen_recording"),
        ("--picker", "picker_application_session"),
    ]:
        # Both authorization routes must validate without requiring a live synthetic GUI application.
        valid = [*base, authorization]
        semantic = [*valid, "--expect-token", "FRAMEZX729B", "--baseline-token", "FRAMEZX729A", "--source-roi", "10,20,256,64"]
        positive_cases = [
            valid,
            [*valid, "--frames", "1", "--duration", "0.1"],
            [*replace_value(valid, "--display-index", "15"), "--frames", "600", "--duration", "30"],
            replace_value(valid, "--pid", "2147483647"),
            semantic,
            [*semantic, "--baseline-frames", "10"],
            [*semantic, "--continuous-semantic"],
            replace_value(semantic, "--expect-token", "N" * 64),
            replace_value(semantic, "--expect-token", "测" * 21 + "A"),
        ]
        for arguments in positive_cases:
            # A successful parse record explicitly disclaims all native/permission/identity work.
            result = run(binary, arguments)
            assert result.returncode == 0, result.stderr
            record = json.loads(result.stdout)
            assert record["kind"] == "validated_options"
            assert record["authorization_mode"] == expected
            assert record["capture_minimum_macos"] == ("15.2" if authorization == "--picker" else "14.2")
            for field in [
                "capture_started", "permission_checked", "identity_checked",
                "unlock_checked", "picker_presented", "pixels_accessed", "vision_performed",
                "latency_acceptance",
            ]:
                # Regressions must not relabel a preflight, picker, or capture as harmless validation.
                assert record[field] is False, (field, record)
            semantic_enabled = "--expect-token" in arguments
            assert record["semantic_enabled"] is semantic_enabled
            assert record["continuous_semantic"] is ("--continuous-semantic" in arguments)
            assert len(result.stdout.encode()) <= 2049
            if semantic_enabled:
                # Validation may hash caller-supplied nonces; it must never return their plaintext or perform OCR.
                expected_token = arguments[arguments.index("--expect-token") + 1]
                old_token = arguments[arguments.index("--baseline-token") + 1]
                assert record["expected_token_id"] == hashlib.sha256(expected_token.encode()).hexdigest()
                assert record["baseline_token_id"] == hashlib.sha256(old_token.encode()).hexdigest()
                assert record["source_roi_points"] == [10, 20, 256, 64]
                assert expected_token not in result.stdout and old_token not in result.stdout
            else:
                # Default parsing declares no semantic baseline, token or ROI, preserving metadata-only behavior.
                assert record["expected_token_id"] is None and record["baseline_token_id"] is None
                assert record["required_baseline_frames"] == 0 and record["source_roi_points"] is None
            accepted += 1

        negative_cases = [
            [*valid, authorization],
            [*valid, "--picker" if authorization == "--existing-authorization" else "--existing-authorization"],
            [*valid, "--capture"],
            [*valid, "--validate"],
            [*valid, "--synthetic-unlocked"],
            [*valid, "--unknown"],
            [*valid, "--frames"],
            replace_value(valid, "--pid", "0"),
            replace_value(valid, "--pid", "1"),
            replace_value(valid, "--pid", "-2"),
            replace_value(valid, "--pid", "2147483648"),
            replace_value(valid, "--pid", "+2"),
            replace_value(valid, "--bundle-id", ""),
            replace_value(valid, "--bundle-id", "com.example invalid"),
            replace_value(valid, "--bundle-id", "a" * 256),
            replace_value(valid, "--display-index", "16"),
            replace_value(valid, "--display-index", " 0"),
            [*valid, "--frames", "0"],
            [*valid, "--frames", "601"],
            [*valid, "--duration", "nan"],
            [*valid, "--duration", "inf"],
            [*valid, "--duration", "0.0999"],
            [*valid, "--duration", "30.0001"],
            [*valid, "--duration", "1e309"],
            [*valid, "--duration", "1suffix"],
            [*valid, "--expect-token", "FRAMEZX729B"],
            [*valid, "--baseline-token", "FRAMEZX729A"],
            [*valid, "--source-roi", "0,0,256,64"],
            [*valid, "--baseline-frames", "1"],
            [*valid, "--continuous-semantic"],
            [*semantic, "--continuous-semantic", "--continuous-semantic"],
            replace_value(semantic, "--expect-token", "FRAMEZX729A"),
            replace_value(semantic, "--expect-token", "short"),
            replace_value(semantic, "--expect-token", "N" * 65),
            replace_value(semantic, "--expect-token", "测" * 22),
            replace_value(semantic, "--expect-token", "FRAME ZX729B"),
            replace_value(semantic, "--expect-token", "--------"),
            replace_value(semantic, "--expect-token", "\u0301" * 4),
            replace_value(semantic, "--expect-token", "\udcffFRAMEZX729B"),
            replace_value(semantic, "--baseline-token", "short"),
            replace_value(semantic, "--source-roi", "nan,0,256,64"),
            replace_value(semantic, "--source-roi", "0,0,inf,64"),
            replace_value(semantic, "--source-roi", "0,0,256,0"),
            replace_value(semantic, "--source-roi", "-1,0,256,64"),
            replace_value(semantic, "--source-roi", "0,0,1e2,64"),
            replace_value(semantic, "--source-roi", "0,0,0x100,64"),
            replace_value(semantic, "--source-roi", "0,0,256,64,1"),
            replace_value(semantic, "--source-roi", "0,0,16385,64"),
            [*semantic, "--baseline-frames", "0"],
            [*semantic, "--baseline-frames", "11"],
            [*semantic, "--expect-token", "FRAMEZX729B"],
            [*semantic, "--baseline-token", "FRAMEZX729A"],
            [*semantic, "--source-roi", "0,0,256,64"],
        ]
        for arguments in negative_cases:
            # These invocations are invalid before main can select an authorization or native lifecycle.
            result = run(binary, arguments)
            assert result.returncode == 64, (result.returncode, result.stdout, result.stderr)
            assert result.stdout == ""
            rejected += 1
    print(json.dumps({
        "help_cases": 1, "accepted_validation_cases": accepted,
        "rejected_argument_cases": rejected, "valid_capture_invocations": 0,
    }))


if __name__ == "__main__":
    # Explicit execution runs only the pure CLI contract above; importing this module has no side effects.
    main()
