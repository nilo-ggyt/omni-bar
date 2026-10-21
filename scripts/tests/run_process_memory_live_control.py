#!/usr/bin/env python3
"""Explicit opt-in OS controls observe only the non-GUI synthetic child created and owned by this run."""

import argparse
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import platform
import select
import subprocess
import sys
import time

SCRIPTS = Path(__file__).resolve().parents[1]
SOURCE = Path(__file__).with_name("process_memory_control.c")
SPEC = importlib.util.spec_from_file_location("memory_report", SCRIPTS / "macos-process-memory-report.py")
REPORT = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(REPORT)
MAX_RUN_SECONDS = 60


class ControlFailure(RuntimeError):
    """Carry finite stage-failure labels, excluding arbitrary OS errors and private paths from protocol output."""


def require(condition, reason):
    """Stop acceptance on failure while preserving evidence, without automatic retry or permission changes."""
    if not condition:
        # The caller's finally block still owns cleanup of this harness's children.
        raise ControlFailure(reason)


def sha256(path):
    """Hash bounded source/binary inputs fully instead of reusing an unidentified historical sampler."""
    require(path.stat().st_size <= 8 * 1024 * 1024, "provenance_input_too_large")
    return hashlib.sha256(path.read_bytes()).hexdigest()


def private_file(path, mode="wb"):
    """Create a new 0600 file in this run's fresh directory without overwriting existing evidence."""
    fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW | os.O_CLOEXEC, 0o600)
    return os.fdopen(fd, mode)


def save_json(path, value):
    """Write evidence in the external controller only, without modifying the observed program or user files."""
    with private_file(path) as output:
        output.write(json.dumps(value, indent=2, sort_keys=True).encode() + b"\n")


def remaining(deadline, limit=10):
    """Bound blocking operations by both the global deadline and a smaller local limit."""
    value = min(limit, deadline - time.monotonic())
    require(value > 0, "harness_deadline")
    return value


def stop_owned(process):
    """Reap only a child with a retained Popen owner, never a searched or arbitrary PID."""
    if process is None:
        # An unsuccessful launch acquired no child process to reclaim.
        return
    if process.poll() is None:
        # Request termination first; after at most two seconds, kill only that same owned child.
        process.terminate()
        try:
            process.wait(timeout=2)
        except subprocess.TimeoutExpired:
            # A synthetic workload must not extend the harness indefinitely or leave its mappings in a background child.
            process.kill()
            process.wait(timeout=2)
    for stream in (process.stdin, process.stdout):
        # End pipe ownership after wait, without carrying control channels into another run.
        if stream is not None:
            stream.close()


def acknowledgement(process, stage, deadline):
    """Read one bounded acknowledgement, synchronizing stage completion through protocol instead of guessed sleeps."""
    data = bytearray()
    while not data.endswith(b"\n"):
        # Each acknowledgement fits 512 bytes; EOF, overflow, or timeout stops further sampling.
        ready, _, _ = select.select([process.stdout], [], [], remaining(deadline, 3))
        require(ready, "control_ack_timeout")
        block = os.read(process.stdout.fileno(), 513 - len(data))
        require(block and len(data) + len(block) <= 512, "control_ack_unavailable")
        data.extend(block)
    record = json.loads(data)
    require(record.get("stage") == stage, "wrong_control_stage")
    return record


def command(process, value, stage, deadline, events):
    """Send only this file's fixed finite stages, never arbitrary allocation sizes or shell input."""
    process.stdin.write(value.encode() + b"\n")
    process.stdin.flush()
    record = acknowledgement(process, stage, deadline)
    events.append({"command": value, "ack": record, "orchestration_monotonic_ns": str(time.monotonic_ns())})


def sample_arguments(binary, identity, output, duration=500):
    """Use only identify results from this harness's control child; callers cannot select an existing external PID."""
    executable = os.fsdecode(bytes.fromhex(identity["executable_hex"]))
    return [str(binary), "sample", "--pid", str(identity["pid"]), "--birth-sec", identity["birth_sec"],
            "--birth-usec", identity["birth_usec"], "--start-abstime", identity["start_abstime"],
            "--executable", executable, "--duration-ms", str(duration), "--interval-ms", "100",
            "--output", str(output)]


def collect(binary, identity, root, name, deadline, cases, expected_exit, expected_valid=True):
    """Preserve raw JSONL, stderr, exit status, and offline review independently, without substituting expected outcomes."""
    artifact = root / (name + ".jsonl")
    args = sample_arguments(binary, identity, artifact)
    with private_file(root / (name + ".stderr")) as stderr:
        result = subprocess.run(args, stdout=subprocess.DEVNULL, stderr=stderr,
                                timeout=remaining(deadline, 5), check=False)
    report = None
    failure = None
    if artifact.exists():
        # File existence still requires independent verification; the footer cannot certify itself.
        try:
            report = REPORT.inspect_file(artifact, result.returncode)
        except (OSError, REPORT.InvalidObservation, ValueError):
            # Retain the failed artifact; a finite category excludes arbitrary exception contents from the summary.
            failure = "offline_validation_failed"
    case = {"name": name, "arguments": args, "observer_exit_code": result.returncode,
            "artifact_exists": artifact.exists(), "offline_report": report, "failure": failure}
    cases.append(case)
    require(result.returncode == expected_exit, "unexpected_observer_exit_" + name)
    require((report is not None) == expected_valid, "unexpected_offline_result_" + name)
    if report is not None:
        # A valid single-PID control remains insufficient for whole-product budget acceptance.
        require(report["whole_product_budget_pass"] is False and report["membership"] == "unverified",
                "invalid_whole_product_claim")
    return report


def exit_during_sample(binary, identity, control, root, deadline, cases, events):
    """Exit the sole control after two observed samples, verifying that a missing target cannot become zero-byte evidence."""
    artifact = root / "exit-during-sample.jsonl"
    args = sample_arguments(binary, identity, artifact, duration=1500)
    process = None
    with private_file(root / "exit-during-sample.stderr") as stderr:
        try:
            # The sampler also has one Popen owner; exceptional cleanup targets only that observer child.
            process = subprocess.Popen(args, stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=stderr)
            while True:
                require(process.poll() is None, "sampler_finished_before_control_exit")
                remaining(deadline, 3)
                if artifact.exists():
                    # Read only this run's private artifact, with an explicit byte ceiling on each record inspection.
                    require(artifact.stat().st_size < 65536, "exit_fixture_artifact_too_large")
                    lines = artifact.read_bytes().splitlines()
                    if sum(b'"type":"sample"' in line for line in lines) >= 2:
                        # Synchronize on records already flushed by the sampler, without inferring start from elapsed time.
                        break
                time.sleep(0.01)
            command(control, "EXIT", "exiting", deadline, events)
            require(control.wait(timeout=remaining(deadline, 3)) == 0, "control_exit_failed")
            status = process.wait(timeout=remaining(deadline, 5))
        finally:
            # Normal completion also uses the shared cleanup path, leaving no owned child or pipe behind.
            stop_owned(process)
    invalid = False
    try:
        # Expect a complete failure footer, never acceptance of a shorter observation window.
        REPORT.inspect_file(artifact, status)
    except (OSError, REPORT.InvalidObservation, ValueError):
        invalid = True
    cases.append({"name": "exit-during-sample", "arguments": args, "observer_exit_code": status,
                  "offline_rejected": invalid})
    require(status == 65 and invalid, "exited_target_was_not_invalid")


def execute(root, include_over_budget):
    """Run all stages on one freshly built control, with a global deadline and finally-owned cleanup."""
    deadline = time.monotonic() + MAX_RUN_SECONDS
    result = {"schema": "omni-memory-live-control-v1", "control_only": True,
              "whole_product_budget_pass": False, "cases": [], "events": [], "passed": False,
              "system": platform.system(), "machine": platform.machine(), "macos": platform.mac_ver()[0]}
    paths = [SCRIPTS / "macos-process-memory-sampler.c", SCRIPTS / "macos-process-memory-report.py",
             SOURCE, Path(__file__).resolve()]
    result["source_sha256"] = {str(path.relative_to(SCRIPTS.parent)): sha256(path) for path in paths}
    control, binary, controller = root / "control", root / "sampler", None
    try:
        with private_file(root / "build.log") as log:
            # Build current sources at the macOS 14 floor; do not execute or reuse an old binary.
            for source, target in ((paths[0], binary), (SOURCE, control)):
                args = ["xcrun", "--sdk", "macosx", "clang", "-std=c17", "-Wall", "-Wextra", "-Werror",
                        "-mmacosx-version-min=14.0", str(source), "-o", str(target)]
                completed = subprocess.run(args, stdout=log, stderr=log,
                                           timeout=remaining(deadline, 15), check=False)
                require(completed.returncode == 0, "control_build_failed")
        result["binary_sha256"] = {"sampler": sha256(binary), "control": sha256(control)}
        with private_file(root / "control.stderr") as stderr:
            # The sole target comes from this Popen call; no external PID/path selection argument exists.
            controller = subprocess.Popen([str(control)], stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                          stderr=stderr, bufsize=0)
            result["owned_control_pid"] = controller.pid
            result["events"].append({"ack": acknowledgement(controller, "ready", deadline)})
            identity_run = subprocess.run([str(binary), "identify", "--pid", str(controller.pid)],
                                          capture_output=True, timeout=remaining(deadline, 5), check=False)
            with private_file(root / "identify.stdout") as output:
                output.write(identity_run.stdout)
            with private_file(root / "identify.stderr") as output:
                output.write(identity_run.stderr)
            result["identify_exit_code"] = identity_run.returncode
            require(identity_run.returncode == 0, "owned_control_identity_unavailable")
            identity = json.loads(identity_run.stdout)
            require(identity["pid"] == controller.pid and identity["uid"] == os.getuid() and
                    bytes.fromhex(identity["executable_hex"]) == os.fsencode(control), "wrong_owned_control_identity")
            cases, events = result["cases"], result["events"]
            idle = collect(binary, identity, root, "idle", deadline, cases, 78)
            command(controller, "ALLOC32", "allocated32", deadline, events)
            allocated = collect(binary, identity, root, "allocated32", deadline, cases, 78)
            command(controller, "FREE", "freed", deadline, events)
            freed = collect(binary, identity, root, "freed32", deadline, cases, 78)
            # Require observed growth, reduction, and lifetime retention; physical-footprint deltas need not equal 32 MiB.
            require(int(allocated["sampled_max_current_bytes"]) > int(idle["sampled_max_current_bytes"]),
                    "touch_did_not_raise_observed_footprint")
            require(int(freed["sampled_max_current_bytes"]) < int(allocated["sampled_max_current_bytes"]),
                    "unmap_did_not_lower_observed_footprint")
            require(int(freed["max_lifetime_peak_bytes"]) >= int(allocated["max_lifetime_peak_bytes"]),
                    "lifetime_peak_was_not_preserved")
            if include_over_budget:
                # The over-budget stage is explicit opt-in and remains confined to this bounded synthetic child.
                command(controller, "ALLOC192", "allocated192", deadline, events)
                collect(binary, identity, root, "allocated192", deadline, cases, 1)
                command(controller, "FREE", "freed", deadline, events)
                over_freed = collect(binary, identity, root, "freed192", deadline, cases, 1)
                require(over_freed["single_pid_current_within_budget"] is True and
                        over_freed["single_pid_lifetime_within_budget"] is False,
                        "post_release_lifetime_was_not_distinct")
            wrong_epoch = dict(identity, birth_usec=str((int(identity["birth_usec"]) + 1) % 1000000))
            collect(binary, wrong_epoch, root, "wrong-birth", deadline, cases, 65, expected_valid=False)
            require(not (root / "wrong-birth.jsonl").exists(), "wrong_epoch_created_output")
            wrong_start = dict(identity, start_abstime=str(int(identity["start_abstime"]) + 1))
            collect(binary, wrong_start, root, "wrong-start", deadline, cases, 65, expected_valid=False)
            require(not (root / "wrong-start.jsonl").exists(), "wrong_start_created_output")
            exit_during_sample(binary, identity, controller, root, deadline, cases, events)
            require(all(sha256(path) == result["source_sha256"][str(path.relative_to(SCRIPTS.parent))]
                        for path in paths), "source_changed_during_control")
            result["passed"] = True
    except (OSError, ValueError, ControlFailure, subprocess.TimeoutExpired) as error:
        # Preserve OS capability/timing failure without switching ABI, retrying, or measuring another PID.
        result["failure"] = str(error) if isinstance(error, ControlFailure) else "control_io_or_timeout_failure"
    finally:
        stop_owned(controller)
        result["owned_control_exit_code"] = controller.returncode if controller is not None else None
        save_json(root / "result.json", result)
    return result


def main():
    """Help and missing opt-in perform no compilation, directory creation, target launch, or process query."""
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--run-live-controls", action="store_true")
    parser.add_argument("--include-over-budget", action="store_true")
    parser.add_argument("--output-dir", type=Path, required=True)
    args = parser.parse_args()
    if not args.run_live_controls:
        # Prevent unittest discovery or accidental invocation from treating live controls as a default lightweight test.
        parser.error("explicit --run-live-controls is required")
    require(sys.platform == "darwin" and os.getuid() != 0 and os.getuid() == os.geteuid(),
            "requires_non_elevated_macos")
    require(args.output_dir.is_absolute(), "output_must_be_absolute")
    os.mkdir(args.output_dir, mode=0o700)
    root = args.output_dir.resolve(strict=True)
    result = execute(root, args.include_over_budget)
    print(json.dumps({"control_only": True, "passed": result["passed"],
                      "whole_product_budget_pass": False, "private_output": str(root)}))
    return 0 if result["passed"] else 1


if __name__ == "__main__":
    # The filename does not match test_*.py, so ordinary pure-test discovery cannot implicitly run live OS controls.
    sys.exit(main())
