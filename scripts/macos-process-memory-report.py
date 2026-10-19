#!/usr/bin/env python3
"""复核有界单 PID 原始记录；任何结果都不代替整产品进程集合归属。"""

import argparse
import hashlib
import json
import os
import re
import stat
import sys

MAX_BYTES = 64 * 1024 * 1024
MAX_LINE_BYTES = 16384
MAX_RECORDS = 65536
BUDGET_BYTES = 100000000
U64_MAX = (1 << 64) - 1
UNVERIFIED = 78
INVALID = 65


class InvalidObservation(ValueError):
    """原始证据不完整、语义不支持或出现身份/时间不一致。"""


def require(condition, reason):
    """失败原因限定为固定标签，避免把私有路径或内容写入报告。"""
    if not condition:
        # 不从部分观测恢复成低内存样本，也不输出失败输入本身。
        raise InvalidObservation(reason)


def integer(value, minimum=0, maximum=U64_MAX):
    """JSON 计数只能是有限范围整数；bool 不能冒充 0/1。"""
    require(type(value) is int and minimum <= value <= maximum, "invalid_integer")
    return value


def decimal(value, minimum=0, maximum=U64_MAX):
    """原始 u64 必须为规范十进制字符串，不经浮点舍入。"""
    require(type(value) is str and len(value) <= 20 and
            re.fullmatch(r"0|[1-9][0-9]*", value) is not None, "invalid_decimal")
    return integer(int(value), minimum, maximum)


def exact_keys(value, names):
    """唯一 schema 不静默忽略新字段，以免误解释不同版本。"""
    require(type(value) is dict and set(value) == set(names.split()), "unsupported_fields")


def unique_object(pairs):
    """拒绝重复 JSON key，防止阅读者和解析器选中不同值。"""
    value = {}
    for key, item in pairs:
        # 每个对象只接受第一次且唯一的字段声明。
        require(key not in value, "duplicate_field")
        value[key] = item
    return value


def records(stream):
    """流式读取固定大小行和总字节，不持有整个原始文件。"""
    digest = hashlib.sha256()
    consumed = 0
    for index in range(MAX_RECORDS + 4):
        # 多读一字节区分恰好上限与被截断的长行，不使用无界 readline。
        raw = stream.readline(MAX_LINE_BYTES + 1)
        if not raw:
            # EOF 交由状态机验证最后一条确实是完整 footer。
            return
        consumed += len(raw)
        require(len(raw) < MAX_LINE_BYTES and raw.endswith(b"\n"), "invalid_line_boundary")
        require(consumed <= MAX_BYTES, "byte_budget_exceeded")
        digest.update(raw)
        try:
            # 严格 JSON 不接受 NaN/Infinity，且每行必须独立形成对象。
            value = json.loads(raw, object_pairs_hook=unique_object,
                               parse_constant=lambda unused: require(False, "nonfinite_json"))
        except (UnicodeError, json.JSONDecodeError, RecursionError) as error:
            # 解析器异常只转换为固定类型，不传播输入片段。
            raise InvalidObservation("invalid_json") from error
        yield index, value, digest.hexdigest(), consumed
    # 即使每行很短，记录数量也不能无限增长。
    raise InvalidObservation("record_budget_exceeded")


def inspect_stream(stream, observer_exit_code):
    """重算时钟和 MAX，保留 single-PID 与 lifetime 的准确语义。"""
    iterator = iter(records(stream))
    first = next(iterator, None)
    require(first is not None, "missing_header")
    _, header, _, _ = first
    exact_keys(header, "type schema scope membership whole_product_budget_pass pid uid birth_sec "
               "birth_usec start_abstime executable_hex origin_continuous_ticks duration_ns interval_ns "
               "planned_records timebase_numer timebase_denom budget_bytes api clock")
    require(header["type"] == "header" and header["schema"] == "omni-process-memory-v1" and
            header["scope"] == "single_pid" and header["membership"] == "unverified" and
            header["whole_product_budget_pass"] is False, "unsupported_scope")
    require(header["api"] == "SDK_declared_libproc_private_subject_to_change" and
            header["clock"] == "mach_continuous_time", "unsupported_counter_source")
    require(decimal(header["budget_bytes"]) == BUDGET_BYTES, "wrong_budget")
    epoch = (integer(header["pid"], 1, (1 << 31) - 1), integer(header["uid"], 1, (1 << 32) - 1),
             decimal(header["birth_sec"], 1), decimal(header["birth_usec"], 0, 999999),
             decimal(header["start_abstime"], 1))
    path = header["executable_hex"]
    require(type(path) is str and len(path) <= 8190 and
            re.fullmatch(r"(?:[0-9a-f]{2})+", path) is not None, "invalid_executable")
    path_bytes = bytes.fromhex(path)
    require(path_bytes.startswith(b"/") and b"\0" not in path_bytes, "invalid_executable")
    origin = decimal(header["origin_continuous_ticks"])
    duration = decimal(header["duration_ns"], 10000000, 3600000000000)
    interval = decimal(header["interval_ns"], 10000000, 10000000000)
    planned_count = integer(header["planned_records"], 2, MAX_RECORDS)
    require(duration >= interval and duration % 1000000 == 0 and interval % 1000000 == 0 and
            planned_count == (duration + interval - 1) // interval + 1, "invalid_schedule")
    numer = integer(header["timebase_numer"], 1, (1 << 32) - 1)
    denom = integer(header["timebase_denom"], 1, (1 << 32) - 1)

    def ns(ticks):
        # Python 宽整数与 C 的 uint128 换算保持一致，仍显式验证 u64 结果。
        return integer(ticks * numer // denom)

    count, max_current, max_peak, max_gap, max_span = 0, 0, 0, 0, 0
    previous = None
    footer = None
    final_digest, final_bytes = None, 0
    for _, record, digest, consumed in iterator:
        # footer 必须最后出现，不能藏入尾随样本或第二份报告。
        require(footer is None, "trailing_record")
        require(type(record) is dict, "invalid_record")
        if record.get("type") == "complete":
            # 收集结束状态仅在所有样本和 observer exit 均通过后接受。
            footer, final_digest, final_bytes = record, digest, consumed
            continue
        if record.get("type") == "failure":
            # 原始失败记录用于诊断，不能被跳过后继续制造完整序列。
            raise InvalidObservation("recorded_collection_failure")
        exact_keys(record, "type index planned_ns elapsed_ns gap_ns query_span_ns suspension_lower_bound_ns "
                   "physical_footprint_bytes lifetime_peak_bytes identity_matches_anchor pid uid birth_sec birth_usec "
                   "start_abstime continuous_before_ticks continuous_before_end_ticks awake_before_ticks "
                   "continuous_after_begin_ticks continuous_after_ticks awake_after_ticks")
        require(record["type"] == "sample" and integer(record["index"]) == count and
                count < planned_count and record["identity_matches_anchor"] is True, "invalid_sample_sequence")
        current_epoch = (integer(record["pid"]), integer(record["uid"]), decimal(record["birth_sec"]),
                         decimal(record["birth_usec"]), decimal(record["start_abstime"]))
        require(current_epoch == epoch, "identity_changed")
        before, before_end, awake, after_begin, after, awake_after = (
            decimal(record[field]) for field in ("continuous_before_ticks", "continuous_before_end_ticks",
                "awake_before_ticks", "continuous_after_begin_ticks", "continuous_after_ticks", "awake_after_ticks"))
        require(before <= before_end <= after_begin <= after and awake <= awake_after, "clock_order")
        middle = before + (before_end - before) // 2
        require(middle >= origin, "clock_order")
        elapsed, span = ns(middle - origin), ns(after - before)
        planned = min(count * interval, duration)
        require(decimal(record["planned_ns"]) == planned and planned <= elapsed < planned + interval,
                "missed_deadline")
        suspension = ns(max(0, after - before - (awake_after - awake) -
                             (before_end - before) - (after - after_begin)))
        gap = 0
        peak = decimal(record["lifetime_peak_bytes"])
        current = decimal(record["physical_footprint_bytes"])
        require(peak >= current, "unsupported_peak_counter")
        if previous is not None:
            # 同一 epoch 的 lifetime peak 不可倒退；不同采样点也要比较休眠时钟差值。
            previous_mid, previous_awake, previous_width, previous_peak = previous
            require(middle >= previous_mid and awake >= previous_awake, "clock_order")
            require(peak >= previous_peak, "peak_moved_backwards")
            width = before_end - before + previous_width
            integer(width)
            gap = ns(middle - previous_mid)
            gap_suspension = ns(max(0, middle - previous_mid - (awake - previous_awake) - width))
            suspension = max(suspension, gap_suspension)
        else:
            # 首个中点必须精确等于 header 原点，不能删除早期样本后重贴 index。
            require(middle == origin and elapsed == 0, "wrong_origin")
        require(span <= interval and gap <= interval * 2 and suspension == 0, "invalid_sampling_window")
        require(decimal(record["elapsed_ns"]) == elapsed and decimal(record["gap_ns"]) == gap and
                decimal(record["query_span_ns"]) == span and
                decimal(record["suspension_lower_bound_ns"]) == suspension, "summary_clock_mismatch")
        count += 1
        max_current, max_peak = max(max_current, current), max(max_peak, peak)
        max_gap, max_span = max(max_gap, gap), max(max_span, span)
        previous = (middle, awake, before_end - before, peak)
    require(footer is not None, "missing_footer")
    exact_keys(footer, "type collection_complete failure records max_current_bytes max_lifetime_peak_bytes "
               "max_gap_ns max_query_span_ns single_pid_current_within_budget single_pid_lifetime_within_budget "
               "membership steady_growth_assessed whole_product_budget_pass durability_certified")
    require(footer["collection_complete"] is True and footer["failure"] == "none" and
            count == planned_count and integer(footer["records"]) == count, "incomplete_collection")
    require(footer["membership"] == "unverified" and footer["steady_growth_assessed"] is False and
            footer["whole_product_budget_pass"] is False and footer["durability_certified"] is False,
            "unsupported_acceptance_claim")
    require((decimal(footer["max_current_bytes"]), decimal(footer["max_lifetime_peak_bytes"]),
             decimal(footer["max_gap_ns"]), decimal(footer["max_query_span_ns"])) ==
            (max_current, max_peak, max_gap, max_span), "summary_counter_mismatch")
    require(footer["single_pid_current_within_budget"] is (max_current <= BUDGET_BYTES) and
            footer["single_pid_lifetime_within_budget"] is (max_peak <= BUDGET_BYTES), "wrong_budget_result")
    expected_exit = 1 if max(max_current, max_peak) > BUDGET_BYTES else UNVERIFIED
    require(observer_exit_code == expected_exit, "observer_exit_mismatch")
    return {"schema": "omni-process-memory-report-v1", "observation_valid": True,
            "scope": "single_pid", "membership": "unverified", "pid": epoch[0], "records": count,
            "budget_bytes": str(BUDGET_BYTES), "sampled_max_current_bytes": str(max_current),
            "max_lifetime_peak_bytes": str(max_peak), "duration_ns": str(duration),
            "interval_ns": str(interval), "max_gap_ns": str(max_gap), "max_query_span_ns": str(max_span),
            "single_pid_current_within_budget": max_current <= BUDGET_BYTES,
            "single_pid_lifetime_within_budget": max_peak <= BUDGET_BYTES,
            "steady_growth_assessed": False, "whole_product_budget_pass": False,
            "source_sha256": final_digest, "source_bytes": final_bytes, "exit_code": expected_exit}


def inspect_file(path, observer_exit_code):
    """只读独立文件，并检测读取期间的替换/增长；不访问进程或操作系统权限。"""
    fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_CLOEXEC)
    with os.fdopen(fd, "rb") as stream:
        before = os.fstat(stream.fileno())
        require(stat.S_ISREG(before.st_mode) and before.st_size <= MAX_BYTES, "invalid_artifact_file")
        result = inspect_stream(stream, observer_exit_code)
        after = os.fstat(stream.fileno())
        require((before.st_dev, before.st_ino, before.st_size, before.st_mtime_ns, before.st_ctime_ns) ==
                (after.st_dev, after.st_ino, after.st_size, after.st_mtime_ns, after.st_ctime_ns),
                "artifact_changed_during_read")
        return result


def main():
    """显式要求 observer 退出码，避免已写 footer 但关闭失败的文件被误接受。"""
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("artifact")
    parser.add_argument("--observer-exit-code", type=int, required=True)
    args = parser.parse_args()
    try:
        # 仅验证已有本地记录，不隐式 identify、采样、安装或提升权限。
        result = inspect_file(args.artifact, args.observer_exit_code)
    except (OSError, InvalidObservation, ValueError) as error:
        # 任意失败只输出有限固定分类；私有路径和原始证据留在操作者选定的本地文件。
        failure = str(error) if isinstance(error, InvalidObservation) else "artifact_io_or_parse_failure"
        print(json.dumps({"observation_valid": False, "whole_product_budget_pass": False,
                          "membership": "unverified", "failure": failure}))
        return INVALID
    print(json.dumps(result, sort_keys=True))
    return result["exit_code"]


if __name__ == "__main__":
    # import 用于纯 fixture；只有显式 CLI 执行才读取指定报告文件。
    sys.exit(main())
