#!/usr/bin/env python3
"""Bounded operator-assisted raw attribution capture; unknown powermetrics schemas never certify metrics."""

import argparse
import ctypes
import hashlib
import json
import os
from pathlib import Path
import plistlib
import selectors
import signal
import subprocess
import sys
import tempfile
import time
from xml.parsers.expat import ExpatError

# Every parser, inventory, output stream and sampling loop has a finite budget independent of sampler semantics.
MAX_RAW_BYTES = 64 * 1024 * 1024
MAX_PROCESSES = 16384
MAX_INVENTORY_BYTES = 64 * 1024 * 1024
MAX_STDERR_BYTES = 1024 * 1024
MAX_SAMPLES = 60
UNKNOWN_SCHEMA = 78
# These exact constants are declared by the inspected macOS 27 SDK sys/proc_info.h, not guessed private flavors.
PROC_PIDTBSDINFO = 3
PROC_PIDPATHINFO_MAXSIZE = 4096


class CaptureError(Exception):
    """An explicit input, identity, framing or observer failure leaves attribution unverified."""


class ProcBsdInfo(ctypes.Structure):
    """Installed SDK PROC_PIDTBSDINFO layout; libproc explicitly does not promise stable interfaces."""

    # Preserve the installed declaration's field order, widths and fixed names; only identity coordinates are consumed.
    _fields_ = [(name, ctypes.c_uint32) for name in (
        "flags", "status", "xstatus", "pid", "ppid", "uid", "gid", "ruid", "rgid",
        "svuid", "svgid", "reserved",
    )] + [("comm", ctypes.c_char * 16), ("name", ctypes.c_char * 32)] + [
        (name, ctypes.c_uint32) for name in ("nfiles", "pgid", "pjobc", "tdev", "tpgid")
    ] + [("nice", ctypes.c_int32), ("start_sec", ctypes.c_uint64), ("start_usec", ctypes.c_uint64)]


def read_bounded(path, limit):
    """Reject oversized artifacts before or during reading, including concurrent file growth."""
    with Path(path).open("rb") as source:
        # A bounded extra byte detects growth without materializing an arbitrary operator-supplied file.
        data = source.read(limit + 1)
    if len(data) > limit:
        # Partial or oversized data cannot become parsing or attribution authority.
        raise CaptureError("artifact exceeds the configured byte limit")
    return data


def bundle_identity(executable):
    """Record a nearest actual .app Info.plist identity without treating that label as process ownership proof."""
    path = Path(executable)
    for parent in path.parents:
        # App extensions/helpers may have their own nearest bundle; preserve its own label for later review.
        if parent.suffix != ".app":
            # Ordinary ancestor directories provide no declared application bundle identity.
            continue
        info_path = parent / "Contents" / "Info.plist"
        try:
            # Bound metadata and hash the exact bytes used; bundle contents remain external mutable evidence.
            raw = read_bounded(info_path, 1024 * 1024)
            info = plistlib.loads(raw)
        except (OSError, ValueError, plistlib.InvalidFileException, ExpatError, CaptureError):
            # An absent/unreadable/invalid bundle label cannot satisfy an explicit operator selection.
            return None
        if not isinstance(info, dict) or not isinstance(info.get("CFBundleIdentifier"), str):
            # A malformed metadata root or non-string label cannot become a declared bundle identity.
            return None
        return {
            "path": str(parent), "identifier": info.get("CFBundleIdentifier"),
            "executable_matches": info.get("CFBundleExecutable") == path.name,
            "info_plist_sha256": hashlib.sha256(raw).hexdigest(),
        }
    return None


def bundle_path_hint(executable):
    """Find an app-looking ancestor without interpreting a path label as process-set membership."""
    for parent in Path(executable).parents:
        # The nearest app-looking path is metadata only; no filesystem traversal or ownership inference occurs here.
        if parent.suffix == ".app":
            # Keep the process's own nearest label rather than charging every nested helper to an outer bundle.
            return str(parent)
    return None


class ProcessReader:
    """Use installed SDK-declared identity ABI, without task ports or undeclared coalition/responsibility flavors."""

    def __init__(self):
        if sys.platform != "darwin":
            # Parsing synthetic fixtures is portable; actual process observation requires macOS.
            raise CaptureError("process identity collection requires macOS")
        if ctypes.sizeof(ProcBsdInfo) != 136:
            # An unexpected local ABI cannot be silently decoded as PID/birth evidence.
            raise CaptureError("unsupported installed proc_bsdinfo ABI size")
        # These declarations ship in libproc.h, whose own notice marks its interfaces private and subject to change.
        # Their presence supports this explicit diagnostic ABI, not a stable public coalition or permission guarantee.
        self.library = ctypes.CDLL("/usr/lib/libproc.dylib", use_errno=True)
        self.library.proc_pidinfo.argtypes = [ctypes.c_int, ctypes.c_int, ctypes.c_uint64, ctypes.c_void_p, ctypes.c_int]
        self.library.proc_pidinfo.restype = ctypes.c_int
        self.library.proc_pidpath.argtypes = [ctypes.c_int, ctypes.c_void_p, ctypes.c_uint32]
        self.library.proc_pidpath.restype = ctypes.c_int
        self.library.proc_listallpids.argtypes = [ctypes.c_void_p, ctypes.c_int]
        self.library.proc_listallpids.restype = ctypes.c_int

    def bsd(self, pid):
        """Require a complete SDK-declared record and requested PID before accepting birth coordinates."""
        value = ProcBsdInfo()
        size = ctypes.sizeof(value)
        if self.library.proc_pidinfo(pid, PROC_PIDTBSDINFO, 0, ctypes.byref(value), size) != size or value.pid != pid:
            # Exit, permission refusal and unsupported record shape all remain explicit failed observations.
            raise CaptureError("pid identity unavailable: {} errno={}".format(pid, ctypes.get_errno()))
        if value.start_sec == 0 or value.start_usec >= 1000000:
            # Implausible timeval coordinates indicate unavailable or unsupported identity decoding.
            raise CaptureError("unsupported pid birth record: {}".format(pid))
        return value

    def identity(self, pid, include_bundle=True):
        """Double-read birth around path acquisition to reject PID replacement during one observation."""
        started = time.monotonic_ns()
        first = self.bsd(pid)
        path = ctypes.create_string_buffer(PROC_PIDPATHINFO_MAXSIZE)
        if self.library.proc_pidpath(pid, path, len(path)) <= 0:
            # Missing executable identity must not be replaced with a guessed process name or parent path.
            raise CaptureError("pid executable unavailable: {} errno={}".format(pid, ctypes.get_errno()))
        second = self.bsd(pid)
        if (first.start_sec, first.start_usec, first.uid) != (second.start_sec, second.start_usec, second.uid):
            # A replacement process cannot inherit the previous PID's bundle or attribution identity.
            raise CaptureError("pid birth changed during observation: {}".format(pid))
        executable = os.fsdecode(path.value)
        return {
            "monotonic_start_ns": started, "monotonic_end_ns": time.monotonic_ns(),
            "pid": pid, "birth": "{}:{}".format(second.start_sec, second.start_usec),
            "uid": second.uid, "ppid": second.ppid, "executable": executable,
            "bundle": bundle_identity(executable) if include_bundle else None,
            "bundle_path_hint": bundle_path_hint(executable),
        }

    def snapshot(self, deadline):
        """Retain one bounded PID inventory with explicit inaccessible/exited rows and observation timestamps."""
        start = time.monotonic_ns()
        storage = (ctypes.c_int * MAX_PROCESSES)()
        count = self.library.proc_listallpids(storage, ctypes.sizeof(storage))
        if count <= 0 or count >= MAX_PROCESSES:
            # A full/truncated or failed inventory is not complete membership evidence.
            raise CaptureError("process inventory failed or exceeded its finite capacity")
        processes = []
        for pid in sorted(set(storage[:count])):
            # Inventory live executable identities only; argv/environment and user document content are never requested.
            if pid <= 0:
                # Kernel/sentinel entries have no user-process bundle identity.
                continue
            if time.monotonic() >= deadline:
                # A large inventory cannot extend the collection indefinitely while the privileged sampler is running.
                raise CaptureError("process inventory exceeded collection deadline")
            try:
                # Member bundle paths remain hints; only the selected anchor reads/hashes a declared Info.plist.
                processes.append(self.identity(pid, include_bundle=False))
            except CaptureError as error:
                # Preserve the missing observation so later review cannot assume the unseen process was absent.
                processes.append({"pid": pid, "unavailable": str(error)})
        return {"monotonic_start_ns": start, "monotonic_end_ns": time.monotonic_ns(),
                "wall_time_ns": time.time_ns(), "processes": processes}


def require_anchor(observed, expected):
    """Compare the operator's exact PID epoch, executable and declared bundle before and throughout collection."""
    for field in ("pid", "birth", "executable"):
        # Every supplied coordinate must agree; a reused PID or another app cannot satisfy the original selection.
        if observed[field] != expected[field]:
            # A changed target invalidates this window even when its numeric PID remains the same.
            raise CaptureError("selected target {} changed".format(field))
    bundle = observed.get("bundle")
    if not bundle or not bundle["executable_matches"] or bundle["identifier"] != expected["bundle_id"]:
        # A naked developer executable or mismatched bundle needs a new explicit selection, never an automatic fallback.
        raise CaptureError("selected target bundle identity does not match")


def inspect_plist(data):
    """Validate bounded documented XML-plist/NUL framing and report schema shape without interpreting unknown fields."""
    if not data or len(data) > MAX_RAW_BYTES:
        # Empty, denied or oversized output cannot establish a sampler schema.
        raise CaptureError("empty or oversized powermetrics artifact")
    frames = [frame.strip() for frame in data.split(b"\0") if frame.strip()]
    if not frames or len(frames) > MAX_SAMPLES + 4:
        # Finite sample collection cannot authorize an unbounded number of record parses.
        raise CaptureError("unsupported or excessive plist framing")
    shapes = []
    for frame in frames:
        # Binary plists contain internal NULs; do not invent an alternative framing contract for an unseen schema.
        if not frame.startswith(b"<?xml") and not frame.startswith(b"<plist"):
            # Tool diagnostics, binary streams and non-plist records remain explicit unsupported input.
            raise CaptureError("unsupported plist record encoding")
        try:
            value = plistlib.loads(frame)
        except (ValueError, plistlib.InvalidFileException, ExpatError) as error:
            # Truncation and malformed frames invalidate the collection instead of yielding partial accepted samples.
            raise CaptureError("invalid plist record") from error
        if not isinstance(value, dict) or len(value) > 256:
            # No observed output schema authorizes another root layout or an unbounded key inventory.
            raise CaptureError("unsupported plist root shape")
        # Expose only bounded root shape for later schema review; no key becomes an interpreted measurement field.
        shapes.append({str(key): type(item).__name__ for key, item in value.items()})
    # No privileged sample has been reviewed: the approved semantic schema registry is intentionally empty.
    return {"schema_status": "unknown_unapproved", "frame_count": len(frames), "root_key_types": shapes,
            "raw_sha256": hashlib.sha256(data).hexdigest(), "membership_status": "unverified",
            "whole_product_physical_footprint": "unmeasured"}


def write_json(path, value):
    """Create one private sidecar without overwriting any existing file or following an output symlink."""
    with open(path, "x", encoding="utf-8") as destination:
        # JSON escaping preserves arbitrary filesystem bytes without logging executable arguments or source contents.
        json.dump(value, destination, ensure_ascii=True, indent=2)
        destination.write("\n")


def append_inventory(destination, snapshot):
    """Stream one timestamped inventory and reject its output budget before another record is written."""
    encoded = (json.dumps(snapshot, ensure_ascii=True) + "\n").encode("ascii")
    if destination.tell() + len(encoded) > MAX_INVENTORY_BYTES:
        # No partial record or unbounded in-memory inventory history is retained on rejection.
        raise CaptureError("inventory output exceeded its finite byte budget")
    destination.write(encoded)
    destination.flush()


def stop_observer(process):
    """Signal only the launched sudo observer; sudo forwards termination to its own privileged child."""
    if process.poll() is not None:
        # The launched wrapper has exited; this observation alone does not prove its privileged child's deallocation.
        return True
    try:
        # The launched sudo process retains the invoking user's real UID and forwards signals to its command.
        process.terminate()
    except OSError:
        # Permission/race failures are not a successful cleanup claim and never trigger a guessed privileged kill.
        return process.poll() is not None
    try:
        # This finite grace period never kills the selected product or guesses a root child PID.
        process.wait(timeout=5)
        return True
    except subprocess.TimeoutExpired:
        # Report an unreaped observer honestly; no arbitrary privileged kill or indefinite wait is attempted.
        return False


def collect(args):
    """Acquire bounded raw evidence using preauthorized sudo -n only; unknown schema exits non-successfully."""
    if os.geteuid() == 0:
        # Do not run the collector or metadata parser as root; elevate only the fixed system sampling tool.
        raise CaptureError("run this collector as your ordinary user; only powermetrics uses sudo -n")
    reader = ProcessReader()
    expected = {"pid": args.pid, "birth": args.birth, "executable": args.executable, "bundle_id": args.bundle_id}
    anchor = reader.identity(args.pid)
    require_anchor(anchor, expected)
    # A new owner-only directory and preopened file descriptors avoid privileged writes through caller paths.
    directory = Path(tempfile.mkdtemp(prefix="omni-coalition-attribution-", dir="/private/tmp"))
    os.chmod(directory, 0o700)
    command = ["/usr/bin/sudo", "-n", "/usr/bin/powermetrics", "--samplers", "tasks",
               "--show-process-coalition", "--show-responsible-pid", "--format", "plist",
               "--sample-rate", "1000", "--sample-count", str(args.samples)]
    manifest = {"format_version": 1, "anchor": anchor, "expected": expected, "command": command,
                "observer_pid": None,
                "samples_requested": args.samples, "interval_ms": 1000, "membership_status": "unverified",
                "whole_product_physical_footprint": "unmeasured", "collection_status": "incomplete"}
    # Tool/OS provenance identifies this capability attempt without recording host names, argv or environment.
    manifest["os"] = {"sysname": os.uname().sysname, "release": os.uname().release,
                      "version": os.uname().version, "machine": os.uname().machine}
    observer = None
    failure = None
    try:
        # Provenance failures are part of this collection attempt and must still publish its private result location.
        manifest["powermetrics_sha256"] = hashlib.sha256(read_bounded("/usr/bin/powermetrics", MAX_RAW_BYTES)).hexdigest()
        write_json(directory / "selection.json", manifest)
        # Preserve installed primary capability text; disagreement or a failed help probe cannot authorize semantics.
        help_result = subprocess.run(["/usr/bin/powermetrics", "--help"], capture_output=True, timeout=5, check=False)
        (directory / "powermetrics-help.txt").write_bytes(help_result.stdout + help_result.stderr)
        (directory / "powermetrics-man.1").write_bytes(read_bounded("/usr/share/man/man1/powermetrics.1", 1024 * 1024))
        # Each inventory is streamed and dropped immediately; collection retains no process-history heap mirror.
        deadline = time.monotonic() + args.samples + 10
        with open(directory / "inventories.jsonl", "xb") as inventories, \
             open(directory / "raw.plist", "xb") as output, \
             open(directory / "observer.stderr", "xb") as errors:
            append_inventory(inventories, reader.snapshot(deadline))
            # Pipes let the ordinary-user collector enforce disk limits without a privileged output-file path.
            observer = subprocess.Popen(command, stdout=subprocess.PIPE, stderr=subprocess.PIPE, stdin=subprocess.DEVNULL)
            manifest["observer_pid"] = observer.pid
            with selectors.DefaultSelector() as channels:
                channels.register(observer.stdout, selectors.EVENT_READ, (output, MAX_RAW_BYTES))
                channels.register(observer.stderr, selectors.EVENT_READ, (errors, MAX_STDERR_BYTES))
                next_inventory = time.monotonic()
                while channels.get_map() or observer.poll() is None:
                    # Every wait is finite; exact anchor identity and per-record inventories repeat at most once per second.
                    now = time.monotonic()
                    if now >= deadline:
                        # Partial evidence remains inspectable, but cannot authorize an attribution or resource result.
                        raise CaptureError("observer exceeded its finite collection deadline")
                    if now >= next_inventory:
                        # PID replacement or exit invalidates the selected measurement window immediately.
                        require_anchor(reader.identity(args.pid), expected)
                        append_inventory(inventories, reader.snapshot(deadline))
                        next_inventory = time.monotonic() + 1
                    for key, _ in channels.select(timeout=min(0.25, deadline - now)):
                        # Read only a fixed chunk and check its destination limit before any private file write.
                        chunk = os.read(key.fileobj.fileno(), 65536)
                        if not chunk:
                            # EOF ends this channel without confusing process exit with completed valid sampling.
                            channels.unregister(key.fileobj)
                            continue
                        destination, limit = key.data
                        if destination.tell() + len(chunk) > limit:
                            # The raw/stderr byte cap is enforced by this unprivileged writer, not a root tool policy.
                            raise CaptureError("observer output exceeded its finite byte budget")
                        destination.write(chunk)
                manifest["observer_exit_code"] = observer.wait(timeout=1)
            if observer.returncode != 0:
                # Noninteractive privilege denial and unsupported flags are preserved in the bounded private stderr artifact.
                raise CaptureError("powermetrics failed; inspect observer.stderr (no credential prompt was opened)")
            require_anchor(reader.identity(args.pid), expected)
            append_inventory(inventories, reader.snapshot(deadline))
        manifest["collection_status"] = "raw_complete_semantics_unverified"
        manifest["schema"] = inspect_plist(read_bounded(directory / "raw.plist", MAX_RAW_BYTES))
    except KeyboardInterrupt:
        # Operator cancellation preserves artifacts and still terminates only this script's launched observer.
        failure = "operator_cancelled"
        manifest["collection_status"] = "failed"
    except (CaptureError, OSError, subprocess.SubprocessError) as error:
        # Keep every owned artifact for operator inspection; never delete paths or accept a partial process window.
        failure = str(error)
        manifest["collection_status"] = "failed"
    finally:
        if observer is not None:
            # Cancellation affects only this script's observer, never the explicitly selected application.
            manifest["observer_wrapper_reaped"] = stop_observer(observer)
            if observer.stdout is not None:
                # Pipe ownership belongs to the collector and ends even when parsing or cancellation fails.
                observer.stdout.close()
            if observer.stderr is not None:
                # Closing a failed observer's output reader does not claim its privileged child was reaped.
                observer.stderr.close()
        manifest["failure"] = failure
        write_json(directory / "result.json", manifest)
    print(json.dumps({"directory": str(directory), "status": manifest["collection_status"],
                      "membership": "unverified", "physical_footprint": "unmeasured"}))
    return 1 if failure else UNKNOWN_SCHEMA


def main():
    """Expose explicit identity selection, bounded collection and read-only schema inspection commands."""
    # Keep identity discovery and semantic inspection usable without starting the explicitly selected collector path.
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="operation", required=True)
    identify = commands.add_parser("identify", help="read one PID's exact SDK-declared birth/path/bundle identity")
    identify.add_argument("--pid", type=int, required=True)
    capture = commands.add_parser("collect", help="capture private raw evidence with preauthorized sudo -n only")
    for name in ("birth", "executable", "bundle-id"):
        # These explicit coordinates prevent a convenient PID-only lookup from silently selecting another owner.
        capture.add_argument("--" + name, required=True)
    capture.add_argument("--pid", type=int, required=True)
    capture.add_argument("--samples", type=int, default=5, choices=range(1, MAX_SAMPLES + 1), metavar="1..60")
    inspect = commands.add_parser("inspect", help="validate framing and report unknown schema without attribution")
    inspect.add_argument("artifact", type=Path)
    args = parser.parse_args()
    os.umask(0o077)
    def interrupted(_signal, _frame):
        # The process owns this handler only for its explicit CLI lifetime; cancellation never touches product signals.
        raise KeyboardInterrupt
    signal.signal(signal.SIGTERM, interrupted)
    try:
        if args.operation == "identify":
            # Identity discovery requires no privileged sampler or process command arguments.
            print(json.dumps(ProcessReader().identity(args.pid), ensure_ascii=True, indent=2))
            return 0
        if args.operation == "collect":
            # All target selection and bounded operator-only collection lives in one explicit branch.
            return collect(args)
        # Unknown semantic schemas always return a non-success status, even when their plist framing is valid.
        print(json.dumps(inspect_plist(read_bounded(args.artifact, MAX_RAW_BYTES)), indent=2))
        return UNKNOWN_SCHEMA
    except (CaptureError, OSError, subprocess.SubprocessError) as error:
        # Fail closed without printing unbounded raw tool output or any target argv/environment.
        print("attribution unavailable: " + str(error), file=sys.stderr)
        return 1


if __name__ == "__main__":
    # Exit status is intentionally nonzero until actual observed semantic membership fields are reviewed and supported.
    sys.exit(main())
