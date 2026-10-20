#!/usr/bin/env python3
"""显式 opt-in 的真实 OS 控制；只观察本次创建并拥有的非 GUI 合成子进程。"""

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
    """固定的阶段失败标签，不把任意 OS 错误或私有路径当作协议输出。"""


def require(condition, reason):
    """失败即保留原始证据并停止验收，不自动重试或更改权限。"""
    if not condition:
        # 调用者的 finally 仍负责回收本 harness 拥有的子进程。
        raise ControlFailure(reason)


def sha256(path):
    """有界源码/二进制输入保留完整 hash，不复用旧采样器。"""
    require(path.stat().st_size <= 8 * 1024 * 1024, "provenance_input_too_large")
    return hashlib.sha256(path.read_bytes()).hexdigest()


def private_file(path, mode="wb"):
    """仅创建本次新目录内的全新 0600 文件，不覆盖已有证据。"""
    fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW | os.O_CLOEXEC, 0o600)
    return os.fdopen(fd, mode)


def save_json(path, value):
    """证据写入只在外部控制器发生，不修改被测程序或用户文件。"""
    with private_file(path) as output:
        output.write(json.dumps(value, indent=2, sort_keys=True).encode() + b"\n")


def remaining(deadline, limit=10):
    """每个阻塞操作均受全局期限和局部更小期限约束。"""
    value = min(limit, deadline - time.monotonic())
    require(value > 0, "harness_deadline")
    return value


def stop_owned(process):
    """只清理保留 Popen owner 的子进程；绝不搜索或终止任意 PID。"""
    if process is None:
        # 尚未成功启动时没有外部资源可回收。
        return
    if process.poll() is None:
        # 异常路径先请求终止，最多两秒后只对同一子进程执行 kill。
        process.terminate()
        try:
            process.wait(timeout=2)
        except subprocess.TimeoutExpired:
            # 合成程序不能拖延整个 harness，也不能让映射留在后台。
            process.kill()
            process.wait(timeout=2)
    for stream in (process.stdin, process.stdout):
        # 管道生命周期在 wait 后结束，避免把测量控制器留给下一次运行。
        if stream is not None:
            stream.close()


def acknowledgement(process, stage, deadline):
    """有界读取一条 ack；真实阶段完成由显式协议确认，不用固定 sleep 猜测。"""
    data = bytearray()
    while not data.endswith(b"\n"):
        # 控制程序每条 ack 小于 512 bytes；EOF、过长或超时都不继续采样。
        ready, _, _ = select.select([process.stdout], [], [], remaining(deadline, 3))
        require(ready, "control_ack_timeout")
        block = os.read(process.stdout.fileno(), 513 - len(data))
        require(block and len(data) + len(block) <= 512, "control_ack_unavailable")
        data.extend(block)
    record = json.loads(data)
    require(record.get("stage") == stage, "wrong_control_stage")
    return record


def command(process, value, stage, deadline, events):
    """命令只来自本文件固定有限阶段，不接收任意分配或 shell 输入。"""
    process.stdin.write(value.encode() + b"\n")
    process.stdin.flush()
    record = acknowledgement(process, stage, deadline)
    events.append({"command": value, "ack": record, "orchestration_monotonic_ns": str(time.monotonic_ns())})


def sample_arguments(binary, identity, output, duration=500):
    """所有目标字段只来自本 harness 创建的 control 的 identify，不能传入现有 PID。"""
    executable = os.fsdecode(bytes.fromhex(identity["executable_hex"]))
    return [str(binary), "sample", "--pid", str(identity["pid"]), "--birth-sec", identity["birth_sec"],
            "--birth-usec", identity["birth_usec"], "--start-abstime", identity["start_abstime"],
            "--executable", executable, "--duration-ms", str(duration), "--interval-ms", "100",
            "--output", str(output)]


def collect(binary, identity, root, name, deadline, cases, expected_exit, expected_valid=True):
    """原始 JSONL、stderr、退出码和离线复核各自保留，不用测试期望覆盖真实结果。"""
    artifact = root / (name + ".jsonl")
    args = sample_arguments(binary, identity, artifact)
    with private_file(root / (name + ".stderr")) as stderr:
        result = subprocess.run(args, stdout=subprocess.DEVNULL, stderr=stderr,
                                timeout=remaining(deadline, 5), check=False)
    report = None
    failure = None
    if artifact.exists():
        # 原始文件存在也要独立复核，不接受 footer 的自报结果。
        try:
            report = REPORT.inspect_file(artifact, result.returncode)
        except (OSError, REPORT.InvalidObservation, ValueError):
            # 失败保留原始文件；固定标记避免任意异常内容进入摘要。
            failure = "offline_validation_failed"
    case = {"name": name, "arguments": args, "observer_exit_code": result.returncode,
            "artifact_exists": artifact.exists(), "offline_report": report, "failure": failure}
    cases.append(case)
    require(result.returncode == expected_exit, "unexpected_observer_exit_" + name)
    require((report is not None) == expected_valid, "unexpected_offline_result_" + name)
    if report is not None:
        # 有效单 PID 控制依然不能升级成 whole-product 预算通过。
        require(report["whole_product_budget_pass"] is False and report["membership"] == "unverified",
                "invalid_whole_product_claim")
    return report


def exit_during_sample(binary, identity, control, root, deadline, cases, events):
    """已看到两个 sample 后让唯一 control 正常退出，验证缺样不变成零字节。"""
    artifact = root / "exit-during-sample.jsonl"
    args = sample_arguments(binary, identity, artifact, duration=1500)
    process = None
    with private_file(root / "exit-during-sample.stderr") as stderr:
        try:
            # sampler 本身也有 Popen owner；异常时只清理这一个观察子进程。
            process = subprocess.Popen(args, stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=stderr)
            while True:
                require(process.poll() is None, "sampler_finished_before_control_exit")
                remaining(deadline, 3)
                if artifact.exists():
                    # 只读本次私有原始文件；记录读取也有明确字节上界。
                    require(artifact.stat().st_size < 65536, "exit_fixture_artifact_too_large")
                    lines = artifact.read_bytes().splitlines()
                    if sum(b'"type":"sample"' in line for line in lines) >= 2:
                        # 使用采样器已 fflush 的记录同步，不用 elapsed 猜测它是否已开始。
                        break
                time.sleep(0.01)
            command(control, "EXIT", "exiting", deadline, events)
            require(control.wait(timeout=remaining(deadline, 3)) == 0, "control_exit_failed")
            status = process.wait(timeout=remaining(deadline, 5))
        finally:
            # 正常结束同样经过统一回收，避免留下子进程或管道。
            stop_owned(process)
    invalid = False
    try:
        # 预期是完整的失败 footer，而不是一个被接受的短窗口。
        REPORT.inspect_file(artifact, status)
    except (OSError, REPORT.InvalidObservation, ValueError):
        invalid = True
    cases.append({"name": "exit-during-sample", "arguments": args, "observer_exit_code": status,
                  "offline_rejected": invalid})
    require(status == 65 and invalid, "exited_target_was_not_invalid")


def execute(root, include_over_budget):
    """一个新编译控制进程完成全部阶段；全局期限与 finally 共同限定资源。"""
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
            # 每次从当前源码构建，固定 mac14 floor；不运行或复用历史 binary。
            for source, target in ((paths[0], binary), (SOURCE, control)):
                args = ["xcrun", "--sdk", "macosx", "clang", "-std=c17", "-Wall", "-Wextra", "-Werror",
                        "-mmacosx-version-min=14.0", str(source), "-o", str(target)]
                completed = subprocess.run(args, stdout=log, stderr=log,
                                           timeout=remaining(deadline, 15), check=False)
                require(completed.returncode == 0, "control_build_failed")
        result["binary_sha256"] = {"sampler": sha256(binary), "control": sha256(control)}
        with private_file(root / "control.stderr") as stderr:
            # 唯一目标必须来自本次 Popen，外部没有 PID/路径选择参数。
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
            # 只断言观察到增长/回落和 lifetime 保留，不断言增量恰等于 32MiB。
            require(int(allocated["sampled_max_current_bytes"]) > int(idle["sampled_max_current_bytes"]),
                    "touch_did_not_raise_observed_footprint")
            require(int(freed["sampled_max_current_bytes"]) < int(allocated["sampled_max_current_bytes"]),
                    "unmap_did_not_lower_observed_footprint")
            require(int(freed["max_lifetime_peak_bytes"]) >= int(allocated["max_lifetime_peak_bytes"]),
                    "lifetime_peak_was_not_preserved")
            if include_over_budget:
                # 超预算是显式额外阶段，仍只占用这个有上限的合成进程。
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
        # OS 能力/时序失败同样留证，不自动换 ABI、重试或对其他 PID 继续测量。
        result["failure"] = str(error) if isinstance(error, ControlFailure) else "control_io_or_timeout_failure"
    finally:
        stop_owned(controller)
        result["owned_control_exit_code"] = controller.returncode if controller is not None else None
        save_json(root / "result.json", result)
    return result


def main():
    """help 和未 opt-in 路径不编译、不创建目录、不启动或查询任何目标。"""
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--run-live-controls", action="store_true")
    parser.add_argument("--include-over-budget", action="store_true")
    parser.add_argument("--output-dir", type=Path, required=True)
    args = parser.parse_args()
    if not args.run_live_controls:
        # 防止被 unittest discovery 或无意执行当成默认轻量测试。
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
    # 文件名不匹配 test_*.py；现有纯 CI 不会隐式执行 opt-in OS 控制。
    sys.exit(main())
