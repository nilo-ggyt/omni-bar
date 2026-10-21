#!/usr/bin/env python3
"""Recheck bounded single-PID observations without certifying whole-product process membership."""

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
# This protocol shares the sampler's strict 200 MB boundary; a passing PID does not establish a product pass.
BUDGET_BYTES = 200000000
U64_MAX = (1 << 64) - 1
UNVERIFIED = 78
INVALID = 65


class InvalidObservation(ValueError):
    """Reject incomplete evidence, unsupported semantics, or inconsistent process identity and clocks."""


def require(condition, reason):
    """Report only finite failure labels, excluding private paths and observation contents."""
    if not condition:
        # Partial observations cannot be repaired into low-memory evidence or echoed into the report.
        raise InvalidObservation(reason)


def integer(value, minimum=0, maximum=U64_MAX):
    """Accept bounded JSON integers while rejecting bool, even though Python treats it as an int subclass."""
    require(type(value) is int and minimum <= value <= maximum, "invalid_integer")
    return value


def decimal(value, minimum=0, maximum=U64_MAX):
    """Parse canonical decimal u64 text without an intermediate floating-point conversion."""
    require(type(value) is str and len(value) <= 20 and
            re.fullmatch(r"0|[1-9][0-9]*", value) is not None, "invalid_decimal")
    return integer(int(value), minimum, maximum)


def exact_keys(value, names):
    """Require the one supported schema, so unfamiliar fields cannot silently acquire old semantics."""
    require(type(value) is dict and set(value) == set(names.split()), "unsupported_fields")


def unique_object(pairs):
    """Reject duplicate JSON keys instead of letting readers and parsers choose different values."""
    value = {}
    for key, item in pairs:
        # Every field must have one declaration; a later duplicate cannot override the first value.
        require(key not in value, "duplicate_field")
        value[key] = item
    return value


def records(stream):
    """Stream bounded lines and total bytes without retaining the complete observation file."""
    digest = hashlib.sha256()
    consumed = 0
    for index in range(MAX_RECORDS + 4):
        # Read one extra byte to distinguish an allowed line boundary from truncation, without unbounded readline.
        raw = stream.readline(MAX_LINE_BYTES + 1)
        if not raw:
            # The consuming state machine must still establish that EOF follows a complete footer.
            return
        consumed += len(raw)
        require(len(raw) < MAX_LINE_BYTES and raw.endswith(b"\n"), "invalid_line_boundary")
        require(consumed <= MAX_BYTES, "byte_budget_exceeded")
        digest.update(raw)
        try:
            # Each line is independent strict JSON; nonfinite constants cannot bypass integer checks.
            value = json.loads(raw, object_pairs_hook=unique_object,
                               parse_constant=lambda unused: require(False, "nonfinite_json"))
        except (UnicodeError, json.JSONDecodeError, RecursionError) as error:
            # Convert parser exceptions to a finite category without propagating snippets of source evidence.
            raise InvalidObservation("invalid_json") from error
        yield index, value, digest.hexdigest(), consumed
    # Short lines must still obey the finite record budget.
    raise InvalidObservation("record_budget_exceeded")


def inspect_stream(stream, observer_exit_code):
    """Recompute clocks and maxima while preserving single-PID and lifetime-counter semantics."""
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
        # Python's wide intermediate matches the sampler's uint128 calculation; the result must still fit u64.
        return integer(ticks * numer // denom)

    count, max_current, max_peak, max_gap, max_span = 0, 0, 0, 0, 0
    previous = None
    footer = None
    final_digest, final_bytes = None, 0
    for _, record, digest, consumed in iterator:
        # Completion must be the final record, with no trailing samples or concatenated second report.
        require(footer is None, "trailing_record")
        require(type(record) is dict, "invalid_record")
        if record.get("type") == "complete":
            # Retain completion provisionally; all samples and the actual observer exit must still agree.
            footer, final_digest, final_bytes = record, digest, consumed
            continue
        if record.get("type") == "failure":
            # A recorded collection failure cannot be skipped to manufacture a complete sequence.
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
        # XNU reads peak before current, so intervening allocations can make current larger; preserve both raw values.
        # Substituting max(current, peak) would invent a kernel peak. Budget checks independently use both counters.
        if previous is not None:
            # A lifetime peak cannot decrease within one process epoch; also compare sleep-clock deltas across samples.
            previous_mid, previous_awake, previous_width, previous_peak = previous
            require(middle >= previous_mid and awake >= previous_awake, "clock_order")
            require(peak >= previous_peak, "peak_moved_backwards")
            width = before_end - before + previous_width
            integer(width)
            gap = ns(middle - previous_mid)
            gap_suspension = ns(max(0, middle - previous_mid - (awake - previous_awake) - width))
            suspension = max(suspension, gap_suspension)
        else:
            # The first midpoint must equal the declared origin; removing early samples and relabeling indices fails.
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
    require(footer["single_pid_current_within_budget"] is (max_current < BUDGET_BYTES) and
            footer["single_pid_lifetime_within_budget"] is (max_peak < BUDGET_BYTES), "wrong_budget_result")
    expected_exit = 1 if max(max_current, max_peak) >= BUDGET_BYTES else UNVERIFIED
    require(observer_exit_code == expected_exit, "observer_exit_mismatch")
    return {"schema": "omni-process-memory-report-v1", "observation_valid": True,
            "scope": "single_pid", "membership": "unverified", "pid": epoch[0], "records": count,
            "budget_bytes": str(BUDGET_BYTES), "sampled_max_current_bytes": str(max_current),
            "max_lifetime_peak_bytes": str(max_peak), "duration_ns": str(duration),
            "interval_ns": str(interval), "max_gap_ns": str(max_gap), "max_query_span_ns": str(max_span),
            "single_pid_current_within_budget": max_current < BUDGET_BYTES,
            "single_pid_lifetime_within_budget": max_peak < BUDGET_BYTES,
            "steady_growth_assessed": False, "whole_product_budget_pass": False,
            "source_sha256": final_digest, "source_bytes": final_bytes, "exit_code": expected_exit}


def inspect_file(path, observer_exit_code):
    """Inspect a regular file and detect mutation during reading without querying processes or permissions."""
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
    """Require the observer's exit code so a written footer cannot hide a later close failure."""
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("artifact")
    parser.add_argument("--observer-exit-code", type=int, required=True)
    args = parser.parse_args()
    try:
        # Verify existing local evidence only; do not implicitly identify, sample, install, or elevate permissions.
        result = inspect_file(args.artifact, args.observer_exit_code)
    except (OSError, InvalidObservation, ValueError) as error:
        # Failure publishes a finite category; private paths and raw evidence remain in the caller's local file.
        failure = str(error) if isinstance(error, InvalidObservation) else "artifact_io_or_parse_failure"
        print(json.dumps({"observation_valid": False, "whole_product_budget_pass": False,
                          "membership": "unverified", "failure": failure}))
        return INVALID
    print(json.dumps(result, sort_keys=True))
    return result["exit_code"]


if __name__ == "__main__":
    # Import is side-effect free; only explicit CLI execution opens the selected observation file.
    sys.exit(main())
