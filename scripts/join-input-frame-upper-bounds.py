#!/usr/bin/env python3
"""Validate real trace v1 and explicitly proposed fixtures offline; no real pixel-witness producer exists.

Usage: this script manifest.json --allow-proposal-fixtures. Only fixture calculations are supported.
Exit 78 means the contract/calculation holds while real capture remains unverified; 1 rejects the
fixture bound, and 65 rejects evidence. --describe-contract reads no files. New witness/manifest fields
are proposal v1, never the current SCK collector's schema1 output. This initial profile accepts exactly
five real-serializer-shaped records for one unmarked ASCII keyDown/insert/accept/commit/keyDown-end;
IME, nested callbacks, and multiple edits are explicitly unsupported.
"""

import argparse
from fractions import Fraction
import hashlib
import json
import math
import os
from pathlib import Path
import re
import stat
import struct
import sys

U64 = (1 << 64) - 1
U32 = (1 << 32) - 1
MAX_TRIALS = 1024
MAX_TOTAL_BYTES = 64 * 1024 * 1024
MAX_MANIFEST_BYTES = 2 * 1024 * 1024
MAX_TRACE_BYTES = 256 * 1024
MAX_WITNESS_BYTES = 3 * 1024 * 1024
MAX_LINE_BYTES = 4096
TRACE_SCHEMA = "omni.native-source-input-trace"
MANIFEST_SCHEMA = "omni.input-frame-trial-set.proposal"
WITNESS_SCHEMA = "omni.pixel-witness.proposal"
REPORT_SCHEMA = "omni.input-frame-upper-bound-report.proposal"
PROFILE = "single_unmarked_ascii_insert_v1"
THRESHOLDS = {"p95_ns": "16700000", "p99_ns": "33400000"}
INVALID, UNVERIFIED = 65, 78


class InvalidEvidence(ValueError):
    """Carry finite failure labels without propagating paths, input contents, or underlying exception text."""


def require(condition, reason):
    """Reject each unmet prerequisite without repairing, filling, or dropping unfavorable records."""
    if not condition:
        raise InvalidEvidence(reason)


def keys(value, names):
    """Match fields exactly; unfamiliar versions and extensions require an explicit contract update."""
    require(type(value) is dict and set(value) == set(names.split()), "unsupported_fields")


def integer(value, lower=0, upper=U64):
    """Bound each input integer and reject bool as a counter."""
    require(type(value) is int and lower <= value <= upper, "invalid_integer")
    return value


def decimal(value, lower=0, upper=U64):
    """Parse wide clocks and identities from canonical decimal text without floating-point or exponent repair."""
    require(type(value) is str and len(value) <= 20 and
            re.fullmatch(r"0|[1-9][0-9]*", value) is not None, "invalid_decimal")
    return integer(int(value), lower, upper)


def identifier(value):
    """Allow only a finite caller-declared trial nonce in reports, excluding paths and arbitrary text."""
    require(type(value) is str and re.fullmatch(r"[A-Za-z0-9_-]{1,64}", value) is not None,
            "invalid_identifier")
    return value


def digest(value):
    """Require full lowercase SHA256 artifact/token identities instead of matching truncated prefixes."""
    require(type(value) is str and re.fullmatch(r"[0-9a-f]{64}", value) is not None,
            "invalid_digest")
    return value


def unique_object(pairs):
    """Reject duplicate keys so different parsers cannot select different identities or completion facts."""
    value = {}
    for name, item in pairs:
        # A later duplicate must not replace an earlier failure or schema-version declaration.
        require(name not in value, "duplicate_field")
        value[name] = item
    return value


def decode(raw):
    """Reject floating-point/NaN JSON; the real trace already encodes wide values as strings."""
    try:
        return json.loads(raw, object_pairs_hook=unique_object,
                          parse_float=lambda _: require(False, "floating_json"),
                          parse_constant=lambda _: require(False, "nonfinite_json"))
    except (UnicodeError, json.JSONDecodeError, RecursionError, ValueError) as error:
        # Preserve recognized finite labels; other parser exceptions must not expose raw evidence bytes.
        if isinstance(error, InvalidEvidence):
            raise
        raise InvalidEvidence("invalid_json") from error


def json_lines(raw, maximum_records):
    """Require the footer's final newline and bound line count and per-line bytes before decoding."""
    require(raw.endswith(b"\n"), "incomplete_newline")
    lines = raw.splitlines(keepends=True)
    require(2 <= len(lines) <= maximum_records, "record_budget")
    result = []
    for line in lines:
        # CRLF, empty lines, and oversized records fall outside the real newline-only serializer contract.
        require(len(line) <= MAX_LINE_BYTES + 1 and line.endswith(b"\n") and
                not line.endswith(b"\r\n") and line.strip(), "invalid_line")
        result.append(decode(line))
    return result


def document(value):
    """Read the real serializer's three canonical counters without claiming proof of document contents."""
    keys(value, "generation utf8_bytes utf16_units")
    return tuple(decimal(value[field]) for field in ("generation", "utf8_bytes", "utf16_units"))


def validate_trace(raw, expected_initial):
    """Validate the real v1 schema, then limit evidence to one synchronous unmarked ASCII commit."""
    rows = json_lines(raw, 258)
    header, footer, records = rows[0], rows[-1], rows[1:-1]
    keys(header, "schema schema_version kind record_count record_capacity completion_contract clock_domain "
         "nsevent_clock armed_at_mach_ticks timebase_numer timebase_denom initial_document overflowed "
         "invalid_scope aborted_scope trace_valid latency_acceptance")
    keys(footer, "schema schema_version kind export_complete record_count trace_valid latency_acceptance "
         "footer_durability_certified")
    require(header["schema"] == footer["schema"] == TRACE_SCHEMA and
            type(header["schema_version"]) is int and header["schema_version"] == 1 and
            type(footer["schema_version"]) is int and footer["schema_version"] == 1 and
            header["kind"] == "header" and footer["kind"] == "complete", "unsupported_trace_schema")
    require(header["completion_contract"] == "newline_terminated_complete_footer_required" and
            header["clock_domain"] == "mach_absolute_ticks" and
            header["nsevent_clock"] == "raw_f64_bits_unconverted", "unsupported_clock")
    require(all(header[field] is False for field in
                ("overflowed", "invalid_scope", "aborted_scope", "latency_acceptance")) and
            header["trace_valid"] is True and footer["trace_valid"] is True and
            footer["export_complete"] is True and footer["latency_acceptance"] is False and
            footer["footer_durability_certified"] is False, "invalid_trace_completion")
    require(integer(header["record_capacity"]) == 256 and
            integer(header["record_count"], 0, 256) == len(records) ==
            integer(footer["record_count"], 0, 256), "trace_count_mismatch")
    initial = document(header["initial_document"])
    require(initial == document(expected_initial), "initial_document_mismatch")
    numer = integer(header["timebase_numer"], 1, U32)
    denom = integer(header["timebase_denom"], 1, U32)
    armed = decimal(header["armed_at_mach_ticks"], 1)
    kinds = ["key_down_begin", "selector_begin", "canonical_accepted", "selector_end", "key_down_end"]
    require(len(records) == len(kinds), "unsupported_input_sequence")
    previous = armed
    for index, (record, kind) in enumerate(zip(records, kinds)):
        # The exact five-record profile cannot reattribute asynchronous, nested, or additional edits to the last event.
        keys(record, "kind record_kind serial mach_ticks event_serial selector_serial composition_serial selector "
             "nsevent_timestamp_bits document removed_utf16_units inserted_utf16_units native_marked outcome "
             "rejection accepted_count rejected_count")
        require(record["kind"] == "record" and record["record_kind"] == kind and
                decimal(record["serial"], 1) == index + 1, "unsupported_input_sequence")
        ticks = decimal(record["mach_ticks"], 1)
        require(ticks >= previous, "trace_clock_order")
        previous = ticks
        require(decimal(record["event_serial"], 1) == 1 and record["composition_serial"] is None,
                "unproven_event_causality")
        scoped = index in (1, 2, 3)
        require(record["selector_serial"] == ("1" if scoped else None) and
                record["selector"] == ("insert_text" if scoped else None), "unproven_selector_causality")
        require(record["native_marked"] is (None if index == 2 else False) and
                record["rejection"] is None and
                record["outcome"] == ("committed_changed" if index == 3 else "not_applicable"),
                "unproven_unmarked_commit")
        require(integer(record["accepted_count"], 0, U32) == (1 if index == 3 else 0) and
                integer(record["rejected_count"], 0, U32) == 0, "canonical_count_mismatch")
        removed = decimal(record["removed_utf16_units"])
        inserted = decimal(record["inserted_utf16_units"])
        require((removed in (0, 1) and inserted == 1) if index == 2 else (removed == inserted == 0),
                "unsupported_ascii_edit")
        if index == 0:
            # Raw NSEvent seconds must be finite and nonnegative; do not convert them into a Mach timestamp.
            bits = decimal(record["nsevent_timestamp_bits"])
            timestamp = struct.unpack(">d", struct.pack(">Q", bits))[0]
            require(math.isfinite(timestamp) and timestamp >= 0, "invalid_nsevent_bits")
        else:
            # Only key-down entry carries the original NSEvent clock; later records cannot invent another one.
            require(record["nsevent_timestamp_bits"] is None, "invented_event_timestamp")
        require(document(record["document"]) == (initial if index < 2 else document(records[2]["document"])),
                "document_changed_outside_commit")
    after = document(records[2]["document"])
    delta = 1 - decimal(records[2]["removed_utf16_units"])
    require(after == (initial[0] + 1, initial[1] + delta, initial[2] + delta) and after[0] <= U64,
            "canonical_ascii_transition_mismatch")
    handler, commit = (decimal(records[index]["mach_ticks"], 1) for index in (0, 3))
    require(armed <= handler < commit, "trace_clock_order")
    return {"armed": armed, "handler": handler, "commit": commit, "after": after,
            "numer": numer, "denom": denom, "event_serial": "1", "selector_serial": "1"}


def target_identity(value):
    """Check the proposal's external manifest/witness binding; the real trace contains no native process identity."""
    keys(value, "pid uid birth_sec birth_usec boot_epoch_id bundle_id")
    integer(value["pid"], 2, (1 << 31) - 1)
    integer(value["uid"], 1, U32)
    decimal(value["birth_sec"], 1)
    decimal(value["birth_usec"], 0, 999999)
    digest(value["boot_epoch_id"])
    require(type(value["bundle_id"]) is str and len(value["bundle_id"]) <= 255 and
            re.fullmatch(r"[A-Za-z0-9_-]+(?:\.[A-Za-z0-9_-]+)+", value["bundle_id"]) is not None,
            "invalid_bundle_identity")
    return value


def point(value):
    """Parse bounded canonical decimal geometry exactly, without guessing pixel rounding."""
    require(type(value) is str and len(value) <= 16 and
            re.fullmatch(r"(?:0|[1-9][0-9]*)(?:\.[0-9]{1,6})?", value) is not None,
            "invalid_geometry_decimal")
    parsed = Fraction(value)
    require(0 <= parsed <= 16384, "geometry_range")
    return parsed


def rectangle(value):
    """Require all four rectangle components; cropping cannot silently turn them into a different valid ROI."""
    require(type(value) is list and len(value) == 4, "invalid_rectangle")
    result = tuple(point(component) for component in value)
    require(result[2] > 0 and result[3] > 0, "empty_rectangle")
    return result


def geometry(value):
    """Check the primary-display/native-resolution contract and the initial five-buffer pixel budget."""
    keys(value, "display_id display_points filter_points source_roi_points content_points pixel_scale "
         "content_scale width height mask_enabled")
    integer(value["display_id"], 1, U32)
    display, source, content = (rectangle(value[name]) for name in
                                ("display_points", "source_roi_points", "content_points"))
    require(display[:2] == (0, 0) and rectangle(value["filter_points"]) == display,
            "unsupported_display_transform")
    x, y, width_points, height_points = source
    require(x + width_points <= display[2] and y + height_points <= display[3] and
            content == (0, 0, width_points, height_points), "unproven_roi_geometry")
    scale = point(value["pixel_scale"])
    width, height = integer(value["width"], 1, 512), integer(value["height"], 1, 128)
    require(1 <= scale <= 4 and point(value["content_scale"]) == 1 and
            width_points * scale == width and height_points * scale == height and
            (x * scale).denominator == (y * scale).denominator == 1 and
            value["mask_enabled"] is False, "unproven_roi_geometry")
    require(width * height * 4 * 5 <= 262144, "combined_pixel_budget")
    return width * height * 4


def validate_witness(raw, trial, trace):
    """Accept only explicit proposal fixtures, rejecting real collector schema1 and all captured-evidence claims."""
    rows = json_lines(raw, 602)
    header, footer, frames = rows[0], rows[-1], rows[1:-1]
    keys(header, "schema schema_version kind evidence_mode producer_implemented actual_capture trial_id run_id "
         "input_trace_sha256 target initial_document old_token_id new_token_id clock_domain timebase_numer "
         "timebase_denom start_ticks deadline_ticks required_baseline_frames frame_limit geometry prerequisites "
         "scope authorization_mode recognition")
    require(header["schema"] == WITNESS_SCHEMA and type(header["schema_version"]) is int and
            header["schema_version"] == 1 and header["kind"] == "header", "unsupported_witness_schema")
    require(header["evidence_mode"] == "fixture_only" and header["producer_implemented"] is False and
            header["actual_capture"] is False, "capture_producer_unimplemented")
    require(header["scope"] == "synthetic_app_display_filtered" and header["authorization_mode"] in
            ("existing_screen_recording", "picker_application_session"), "unsupported_capture_scope")
    recognition = header["recognition"]
    keys(recognition, "revision language level language_correction automatic_language_detection preferred_words")
    require(integer(recognition["revision"]) == 3 and recognition["language"] == "en-US" and
            recognition["level"] == "accurate" and all(recognition[name] is False for name in
            ("language_correction", "automatic_language_detection", "preferred_words")),
            "unsupported_recognition_contract")
    require(header["trial_id"] == trial["trial_id"] and header["run_id"] == trial["run_id"] and
            header["input_trace_sha256"] == trial["input_sha256"] and
            target_identity(header["target"]) == trial["target"] and
            document(header["initial_document"]) == document(trial["initial_document"]), "witness_identity_mismatch")
    require(digest(header["old_token_id"]) == trial["old_token_id"] and
            digest(header["new_token_id"]) == trial["new_token_id"], "token_identity_mismatch")
    require(header["clock_domain"] == "mach_absolute_ticks" and
            integer(header["timebase_numer"], 1, U32) == trace["numer"] and
            integer(header["timebase_denom"], 1, U32) == trace["denom"], "clock_domain_mismatch")
    keys(header["prerequisites"], "template_source old_calibrated new_calibrated reset_old_verified "
         "glyphs_complete same_document_and_layout new_state_unique target_scope_verified unlocked unoccluded")
    prerequisites = header["prerequisites"]
    require(prerequisites["template_source"] == "fixture_model" and
            all(value is True for name, value in prerequisites.items() if name != "template_source"),
            "unproven_capture_prerequisite")
    pixel_bytes = geometry(header["geometry"])
    start, deadline = decimal(header["start_ticks"], 1), decimal(header["deadline_ticks"], 1)
    require(trace["armed"] <= start < deadline and
            (deadline - start) * trace["numer"] <= 30_000_000_000 * trace["denom"], "invalid_capture_interval")
    baseline_needed = integer(header["required_baseline_frames"], 1, 10)
    require(1 <= len(frames) <= integer(header["frame_limit"], 1, 600), "frame_count_mismatch")
    baseline_count, baseline_ready, baseline_ticks, candidate = 0, False, None, None
    previous_display = previous_callback = start
    for index, frame in enumerate(frames, 1):
        # Preserve each frame's timing, geometry, and identity; UNKNOWN is neither absence evidence nor a skippable frame.
        keys(frame, "kind run_id index display_ticks callback_absolute_ticks state baseline_ready "
             "earliest_observed_candidate geometry target unoccluded sample_status")
        require(frame["kind"] == "frame" and frame["run_id"] == trial["run_id"] and
                integer(frame["index"], 1, 600) == index and frame["sample_status"] == "complete",
                "frame_identity_order")
        require(frame["geometry"] == header["geometry"] and frame["target"] == trial["target"] and
                frame["unoccluded"] is True, "frame_scope_geometry_visibility")
        geometry(frame["geometry"])
        target_identity(frame["target"])
        displayed = decimal(frame["display_ticks"], 1)
        callback = decimal(frame["callback_absolute_ticks"], 1)
        require(previous_display < displayed <= callback <= deadline and previous_callback < callback,
                "frame_clock_order")
        previous_display, previous_callback = displayed, callback
        require(frame["state"] in ("old", "new"), "unknown_pixel_state")
        if not baseline_ready:
            # NEW before an established OLD baseline is preexisting; a later OLD frame cannot repair that trial.
            require(frame["state"] == "old", "preexisting_new")
            baseline_count += 1
            baseline_ready = baseline_count >= baseline_needed
            if baseline_ready:
                # Fix the first established baseline time; normal post-input OLD waiting frames cannot replace it.
                baseline_ticks = displayed
        is_candidate = baseline_ready and frame["state"] == "new" and candidate is None
        if is_candidate:
            # The first candidate must follow input and commit; rejection cannot be repaired using a later NEW frame.
            require(baseline_ticks is not None and baseline_ticks < trace["handler"] and
                    displayed > trace["commit"], "preexisting_or_precommit_new")
            candidate = {"run_id": trial["run_id"], "frame_index": index,
                         "display_ticks": frame["display_ticks"]}
            candidate_callback = callback
        require(frame["baseline_ready"] is baseline_ready and
                frame["earliest_observed_candidate"] is is_candidate, "frame_summary_mismatch")
    require(candidate is not None, "missing_new_witness")
    keys(footer, "kind run_id recorded_frames failure witness_valid baseline_ready candidate_present "
         "earliest_observed audit_binding audit_complete audit_passed audit_loan_released audit_expected_certain "
         "audit_baseline_seen audit_native_objects_released audit_complete_ticks audit_release_ticks "
         "cleanup_complete_ticks combined_owned_pixel_peak owned_pixel_bytes_after_drain output_removed "
         "stop_completed capture_completeness_verified latency_acceptance physical_scanout_verified "
         "observer_exit_code known_dropped_frames overruns deadline_failed picker_client_released")
    require(footer["kind"] == "complete" and footer["run_id"] == trial["run_id"] and
            integer(footer["recorded_frames"], 1, 600) == len(frames), "witness_completion_mismatch")
    for name in ("earliest_observed", "audit_binding"):
        # Run, index, and display ticks must all identify the first candidate; later-frame audits cannot backdate it.
        keys(footer[name], "run_id frame_index display_ticks")
        require(footer[name] == candidate, "audit_identity_mismatch")
    true_fields = ("witness_valid", "baseline_ready", "candidate_present", "audit_complete", "audit_passed",
                   "audit_loan_released", "audit_expected_certain", "audit_native_objects_released",
                   "output_removed", "stop_completed")
    false_fields = ("audit_baseline_seen", "capture_completeness_verified", "latency_acceptance",
                    "physical_scanout_verified", "deadline_failed")
    require(footer["failure"] == "none" and all(footer[name] is True for name in true_fields) and
            all(footer[name] is False for name in false_fields) and
            integer(footer["observer_exit_code"]) == integer(footer["known_dropped_frames"]) ==
            integer(footer["overruns"]) == 0, "unproven_audit_or_cleanup")
    require(footer["picker_client_released"] is
            (True if header["authorization_mode"] == "picker_application_session" else None),
            "picker_cleanup_unverified")
    audit, released, cleaned = (decimal(footer[name], 1) for name in
                               ("audit_complete_ticks", "audit_release_ticks", "cleanup_complete_ticks"))
    require(candidate_callback <= audit <= released <= deadline and cleaned >= max(released, previous_callback) and
            (cleaned - deadline) * trace["numer"] <= 3_000_000_000 * trace["denom"], "audit_cleanup_clock_order")
    require(5 * pixel_bytes <= decimal(footer["combined_owned_pixel_peak"]) <= 262144 and
            decimal(footer["owned_pixel_bytes_after_drain"]) == 0, "pixel_release_budget")
    return decimal(candidate["display_ticks"]), candidate["frame_index"]


def upper_bound(delta, numer, denom):
    """Return exact rational nanoseconds and their ceiling; never floor a bound before comparing thresholds."""
    require(0 <= delta <= U64, "negative_or_overflowed_delta")
    exact = Fraction(delta * numer, denom)
    return exact, {"numerator": str(exact.numerator), "denominator": str(exact.denominator),
                   "ceil_ns": str((exact.numerator + exact.denominator - 1) // exact.denominator)}


def nearest_rank(values, percent):
    """Sort the same complete sample set by exact fractions, without interpolation or removal of slow trials."""
    require(bool(values) and percent in (95, 99), "invalid_quantile")
    rank = (len(values) * percent + 99) // 100
    return sorted(values)[rank - 1], rank


class ArtifactReader:
    """Use a read-only directory descriptor to bound reads and require stable file identity throughout them."""

    def __init__(self, directory_fd):
        # The caller owns this fd; the reader tracks only cumulative bytes and already-consumed file identities.
        self.directory_fd = directory_fd
        self.consumed = 0
        self.identities = set()

    def read(self, name, maximum):
        """Read regular files in one private directory, rejecting traversal, symlinks, FIFOs, and identity reuse."""
        require(type(name) is str and re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9_.-]{0,79}", name) is not None and
                ".." not in name, "invalid_artifact_name")
        try:
            fd = os.open(name, os.O_RDONLY | os.O_NOFOLLOW | os.O_CLOEXEC | os.O_NONBLOCK,
                         dir_fd=self.directory_fd)
            with os.fdopen(fd, "rb") as stream:
                before = os.fstat(stream.fileno())
                identity = (before.st_dev, before.st_ino)
                require(stat.S_ISREG(before.st_mode) and before.st_uid == os.getuid() and
                        before.st_mode & 0o077 == 0 and 0 < before.st_size <= maximum,
                        "invalid_artifact_file")
                require(identity not in self.identities, "artifact_reused")
                require(self.consumed + before.st_size <= MAX_TOTAL_BYTES, "total_byte_budget")
                raw = stream.read(maximum + 1)
                after = os.fstat(stream.fileno())
                stamp = lambda item: (item.st_dev, item.st_ino, item.st_size, item.st_mtime_ns, item.st_ctime_ns)
                require(stamp(before) == stamp(after) and len(raw) == before.st_size, "artifact_changed_during_read")
                self.consumed += len(raw)
                self.identities.add(identity)
                return raw, hashlib.sha256(raw).hexdigest()
        except OSError as error:
            # Filesystem details stay in the private artifact context, without copying paths or arbitrary errors to stdout.
            raise InvalidEvidence("artifact_io_failure") from error


def validate_manifest(value):
    """Check the complete predeclared trial set first; missing or duplicate trials cannot disappear from statistics."""
    keys(value, "schema schema_version evidence_mode profile endpoint planned_trial_count trial_ids trials thresholds")
    require(value["schema"] == MANIFEST_SCHEMA and type(value["schema_version"]) is int and
            value["schema_version"] == 1 and value["evidence_mode"] == "fixture_only" and
            value["profile"] == PROFILE and value["endpoint"] in ("handler", "commit") and
            value["thresholds"] == THRESHOLDS, "unsupported_manifest_contract")
    count = integer(value["planned_trial_count"], 1, MAX_TRIALS)
    require(type(value["trial_ids"]) is list and type(value["trials"]) is list and
            len(value["trial_ids"]) == len(value["trials"]) == count, "incomplete_trial_set")
    ids = [identifier(item) for item in value["trial_ids"]]
    require(len(set(ids)) == count, "duplicate_trial")
    names, runs = set(), set()
    for index, trial in enumerate(value["trials"]):
        # Files and runs must also be independent; relabeling a single observation cannot give it repeated statistical weight.
        keys(trial, "trial_id run_id input_file input_sha256 witness_file witness_sha256 target initial_document "
             "old_token_id new_token_id")
        require(trial["trial_id"] == ids[index], "trial_order_mismatch")
        run = decimal(trial["run_id"], 1)
        require(run not in runs, "duplicate_run")
        runs.add(run)
        for name in ("input_file", "witness_file"):
            require(type(trial[name]) is str and trial[name] not in names, "duplicate_artifact")
            names.add(trial[name])
        for name in ("input_sha256", "witness_sha256", "old_token_id", "new_token_id"):
            digest(trial[name])
        require(trial["old_token_id"] != trial["new_token_id"], "identical_token_ids")
        target_identity(trial["target"])
        document(trial["initial_document"])
    return value


def inspect_manifest(path, allow_proposal_fixtures=False):
    """Inspect files only; retain failed trials and withhold passing quantiles when any declared trial is invalid."""
    require(allow_proposal_fixtures is True, "proposal_fixture_requires_opt_in")
    directory_fd = None
    try:
        directory_fd = os.open(Path(path).parent, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW | os.O_CLOEXEC)
        root = os.fstat(directory_fd)
        require(root.st_uid == os.getuid() and root.st_mode & 0o077 == 0, "nonprivate_artifact_directory")
        reader = ArtifactReader(directory_fd)
        raw, manifest_sha = reader.read(Path(path).name, MAX_MANIFEST_BYTES)
        manifest = validate_manifest(decode(raw))
        results, bounds = [], []
        for trial in manifest["trials"]:
            # Every declared ID gets a valid/invalid result; successful trials cannot hide the first failing trial.
            try:
                trace_raw, trace_sha = reader.read(trial["input_file"], MAX_TRACE_BYTES)
                require(trace_sha == trial["input_sha256"], "input_digest_mismatch")
                trace = validate_trace(trace_raw, trial["initial_document"])
                witness_raw, witness_sha = reader.read(trial["witness_file"], MAX_WITNESS_BYTES)
                require(witness_sha == trial["witness_sha256"], "witness_digest_mismatch")
                displayed, frame_index = validate_witness(witness_raw, trial, trace)
                handler, handler_json = upper_bound(displayed - trace["handler"], trace["numer"], trace["denom"])
                commit, commit_json = upper_bound(displayed - trace["commit"], trace["numer"], trace["denom"])
                bounds.append(handler if manifest["endpoint"] == "handler" else commit)
                results.append({"trial_id": trial["trial_id"], "valid_fixture": True,
                                "candidate_frame_index": frame_index, "handler_upper_bound_ns": handler_json,
                                "commit_upper_bound_ns": commit_json})
            except InvalidEvidence as error:
                # Preserve failure and leave all quantiles null; never aggregate only the successful bounds prefix.
                results.append({"trial_id": trial["trial_id"], "valid_fixture": False, "reason": str(error)})
        valid = len(bounds) == manifest["planned_trial_count"]
        report = {"schema": REPORT_SCHEMA, "schema_version": 1, "fixture_only": True,
                  "producer_implemented": False, "actual_capture_verified": False,
                  "capture_completeness_verified": False, "physical_scanout_verified": False,
                  "latency_acceptance": False, "endpoint": manifest["endpoint"],
                  "endpoint_description": "input_handler_or_commit_to_audited_WindowServer_frame_upper_bound",
                  "all_declared_trials_valid": valid, "planned_trial_count": manifest["planned_trial_count"],
                  "trial_results": results, "quantiles": None, "fixture_thresholds_pass": None,
                  "manifest_sha256": manifest_sha, "source_bytes": reader.consumed}
        if valid:
            # Empirical quantiles require all declared trials; they establish neither future populations nor real capture/scanout.
            quantiles = {}
            for percent, threshold in ((95, 16700000), (99, 33400000)):
                exact, rank = nearest_rank(bounds, percent)
                quantiles[f"p{percent}"] = {"rank": rank, "numerator": str(exact.numerator),
                                            "denominator": str(exact.denominator),
                                            "ceil_ns": str((exact.numerator + exact.denominator - 1) // exact.denominator),
                                            "threshold_ns": str(threshold), "within_threshold": exact <= threshold}
            report["quantiles"] = quantiles
            report["fixture_thresholds_pass"] = all(item["within_threshold"] for item in quantiles.values())
        return report
    except OSError as error:
        raise InvalidEvidence("artifact_io_failure") from error
    finally:
        # Close only this function's acquired directory descriptor, regardless of parsing or validation failure.
        if directory_fd is not None:
            os.close(directory_fd)


def main():
    """Print one JSON report without writing product/capture files; passing fixtures still exit UNVERIFIED."""
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("manifest", nargs="?")
    parser.add_argument("--allow-proposal-fixtures", action="store_true")
    parser.add_argument("--describe-contract", action="store_true")
    args = parser.parse_args()
    if args.describe_contract:
        # Contract description reports versions and limits without reading fixtures, processes, or permission state.
        require(args.manifest is None and not args.allow_proposal_fixtures, "conflicting_cli_mode")
        print(json.dumps({"manifest_schema": MANIFEST_SCHEMA, "witness_schema": WITNESS_SCHEMA,
                          "schema_version": 1, "real_trace_schema": TRACE_SCHEMA, "profile": PROFILE,
                          "max_trials": MAX_TRIALS, "max_trace_records": 256, "max_frame_records": 600,
                          "capture_producer_implemented": False, "capture_modes_accepted": [],
                          "proposal_fixture_requires_opt_in": True}, sort_keys=True))
        return 0
    require(args.manifest is not None, "missing_manifest")
    report = inspect_manifest(args.manifest, args.allow_proposal_fixtures)
    print(json.dumps(report, separators=(",", ":"), sort_keys=True))
    if not report["all_declared_trials_valid"]:
        # Incomplete evidence outranks any favorable bounds computed from individual valid trials.
        return INVALID
    # Fixture threshold failure does not prove a slow product; success does not prove real performance acceptance.
    return UNVERIFIED if report["fixture_thresholds_pass"] else 1


if __name__ == "__main__":
    try:
        sys.exit(main())
    except InvalidEvidence as error:
        print(json.dumps({"schema": REPORT_SCHEMA, "valid": False, "reason": str(error),
                          "actual_capture_verified": False, "latency_acceptance": False,
                          "capture_completeness_verified": False, "physical_scanout_verified": False}))
        sys.exit(INVALID)
    except (OSError, BrokenPipeError):
        # A broken output pipe is failure; do not attempt another error write through the same damaged pipe.
        sys.exit(INVALID)
