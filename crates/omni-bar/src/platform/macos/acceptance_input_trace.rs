//! Explicit fixture-only source-input controls and bounded scalar JSONL export.
//!
//! The fixture retains the returned menu target until its event loop ends. AppKit's menu targets
//! are weak, and this controller retains no editor, trace, document, event or callback. Export is
//! an explicit post-measurement action; no input hook, timer, observer or destructor writes an artifact.

use std::cell::Cell;
use std::ffi::CString;
use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, Sel};
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{NSApplication, NSMenu, NSMenuItem};
use objc2_foundation::{NSObject, NSObjectProtocol, NSString};
use serde_json::{Value, json};

use crate::composition::plugins::input_trace::{
    self, AccessError, CAPACITY, Document, Kind, Outcome, Record, Rejection, Selector, Snapshot,
};

const MAX_ARTIFACT_BYTES: usize = 256 * 1024;
const MAX_LINE_BYTES: usize = 4096;
const MAX_EXPORTS: u16 = 64;
const SCHEMA: &str = "omni.native-source-input-trace";

/// Only finite install categories cross the fixture boundary; paths/native error strings never do.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum InstallError {
    MissingMainMenu,
    AlreadyInstalled,
    RootUnavailable,
}

/// Export failure is deliberately distinct from an invalid but completely exported scalar trace.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ExportError {
    InvalidShape,
    Encoding,
    RootChanged,
    Exhausted,
    Storage,
}

/// The opened directory is the write authority; the original pathname is only an identity guard.
struct ExportRoot {
    directory: File,
    path: PathBuf,
    device: u64,
    inode: u64,
    uid: u32,
    next: Cell<u16>,
}

impl ExportRoot {
    /// Acquire the caller's fresh, private real directory without following its final component.
    fn open(path: &Path) -> Result<Self, InstallError> {
        // The fixture supplies its owned root, never an editor workspace or a user-selected export path.
        let directory = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(path)
            .map_err(|_| InstallError::RootUnavailable)?;
        let metadata = directory
            .metadata()
            .map_err(|_| InstallError::RootUnavailable)?;
        // SAFETY: geteuid has no pointer/ownership preconditions and returns the current process uid.
        let uid = unsafe { libc::geteuid() };
        if !path.is_absolute()
            || !metadata.is_dir()
            || metadata.uid() != uid
            || metadata.mode() & 0o077 != 0
        {
            // A shared/non-directory authority is not the isolated fixture root promised by the caller.
            return Err(InstallError::RootUnavailable);
        }
        let root = Self {
            directory,
            path: path.to_owned(),
            device: metadata.dev(),
            inode: metadata.ino(),
            uid,
            next: Cell::new(0),
        };
        root.check_identity()
            .map_err(|_| InstallError::RootUnavailable)?;
        Ok(root)
    }

    /// Reject replacement/unlink/symlink of the declared root without opening the replacement for writing.
    fn check_identity(&self) -> Result<(), ExportError> {
        let metadata =
            std::fs::symlink_metadata(&self.path).map_err(|_| ExportError::RootChanged)?;
        if !metadata.is_dir()
            || metadata.file_type().is_symlink()
            || metadata.dev() != self.device
            || metadata.ino() != self.inode
            || metadata.uid() != self.uid
            || metadata.mode() & 0o077 != 0
        {
            // Descriptor-relative writes cannot escape into a new root; a changed name also invalidates publication.
            return Err(ExportError::RootChanged);
        }
        Ok(())
    }

    /// Consume at most 64 fixed names; existing regular files, hardlinks and symlinks are never overwritten.
    fn create_exclusive(&self) -> Result<(File, CString), ExportError> {
        self.check_identity()?;
        while self.next.get() < MAX_EXPORTS {
            // Consume each candidate once, including collisions; no unbounded retry or directory scan exists.
            let next = self.next.get() + 1;
            self.next.set(next);
            let name = CString::new(format!("source-input-trace-{next:04}.jsonl"))
                .map_err(|_| ExportError::Encoding)?;
            // SAFETY: the retained directory fd is live, name is one fixed NUL-terminated component,
            // and O_EXCL/O_NOFOLLOW cannot open an existing alias. The promoted mode argument is 0600.
            let descriptor = unsafe {
                libc::openat(
                    self.directory.as_raw_fd(),
                    name.as_ptr(),
                    libc::O_WRONLY
                        | libc::O_CREAT
                        | libc::O_EXCL
                        | libc::O_NOFOLLOW
                        | libc::O_CLOEXEC,
                    0o600 as libc::c_int,
                )
            };
            if descriptor >= 0 {
                // SAFETY: openat returned a new +1 descriptor; this File is its sole Rust close owner.
                return Ok((unsafe { File::from_raw_fd(descriptor) }, name));
            }
            if io::Error::last_os_error().kind() != io::ErrorKind::AlreadyExists {
                // Only a name collision advances; permission/resource failures preserve the root and all old files.
                return Err(ExportError::Storage);
            }
        }
        // Exhaustion preserves every prior artifact and never wraps a counter into an old filename.
        Err(ExportError::Exhausted)
    }

    /// Recheck descriptor/path identity before the last completion write, without following an output link.
    fn check_output(&self, file: &File, name: &CString) -> Result<(), ExportError> {
        self.check_identity()?;
        let metadata = file.metadata().map_err(|_| ExportError::Storage)?;
        let mut published = std::mem::MaybeUninit::<libc::stat>::uninit();
        // SAFETY: both the directory fd and NUL-terminated name remain live; fstatat initializes
        // exactly one stat on success. AT_SYMLINK_NOFOLLOW inspects rather than opens a replacement link.
        if unsafe {
            libc::fstatat(
                self.directory.as_raw_fd(),
                name.as_ptr(),
                published.as_mut_ptr(),
                libc::AT_SYMLINK_NOFOLLOW,
            )
        } != 0
        {
            // A removed/replaced output cannot acquire a successful completion footer through this helper.
            return Err(ExportError::RootChanged);
        }
        // SAFETY: the successful public fstatat call initialized every stat field read below.
        let published = unsafe { published.assume_init() };
        if !metadata.is_file()
            || metadata.nlink() != 1
            || metadata.uid() != self.uid
            || metadata.mode() & 0o077 != 0
            || metadata.dev() != published.st_dev as u64
            || metadata.ino() != published.st_ino
            || published.st_mode & libc::S_IFMT != libc::S_IFREG
            || published.st_nlink != 1
            || published.st_uid != self.uid
            || published.st_mode & 0o077 != 0
        {
            // Writes stay on the exclusively created fd; changed pathname/hardlink publication still fails closed.
            return Err(ExportError::RootChanged);
        }
        Ok(())
    }

    /// Write prefix first and the completion footer last; no fallible operation follows that final write.
    fn publish(&self, encoded: &EncodedTrace) -> Result<(), ExportError> {
        let (mut file, name) = self.create_exclusive()?;
        write_artifact(&mut file, encoded, |file| {
            // Sync prefix/entry and verify publication before the final completion authority is written.
            file.sync_all().map_err(|_| ExportError::Storage)?;
            self.directory
                .sync_all()
                .map_err(|_| ExportError::Storage)?;
            self.check_output(file, &name)
        })
    }
}

/// A complete footer includes its final newline; no fallible operation follows that last write.
fn write_artifact<W: Write>(
    writer: &mut W,
    encoded: &EncodedTrace,
    before_footer: impl FnOnce(&mut W) -> Result<(), ExportError>,
) -> Result<(), ExportError> {
    writer
        .write_all(&encoded.prefix)
        .map_err(|_| ExportError::Storage)?;
    // Identity/sync failures leave only a prefix. A failed footer write lacks its complete newline-terminated footer.
    before_footer(writer)?;
    // The footer explicitly declines crash-durability certification: syncing afterward could fail post-commit.
    writer
        .write_all(&encoded.footer)
        .map_err(|_| ExportError::Storage)
}

/// Serialization borrows only the already drained scalar snapshot, never an editor or native owner.
struct Evidence<'a> {
    records: &'a [Record],
    overflowed: bool,
    invalid_scope: bool,
    aborted_scope: bool,
    armed_at: u64,
    numer: u32,
    denom: u32,
    initial: Document,
}

impl<'a> From<&'a Snapshot> for Evidence<'a> {
    fn from(snapshot: &'a Snapshot) -> Self {
        // This view contains one fixed-array slice and Copy counters; it cannot prolong foreground ownership.
        Self {
            records: snapshot.records(),
            overflowed: snapshot.overflowed,
            invalid_scope: snapshot.invalid_scope,
            aborted_scope: snapshot.aborted_scope,
            armed_at: snapshot.armed_at_mach_ticks,
            numer: snapshot.timebase_numer,
            denom: snapshot.timebase_denom,
            initial: snapshot.initial_document,
        }
    }
}

/// A complete serialized artifact is bounded before file creation; footer ownership stays separate until last write.
struct EncodedTrace {
    prefix: Vec<u8>,
    footer: Vec<u8>,
    valid: bool,
}

/// Stable lowercase protocol labels are exhaustive; Debug formatting never becomes an artifact schema.
fn kind_label(value: Kind) -> &'static str {
    match value {
        // An unused entry should never be exported, but retains a distinct label for shape validation/tests.
        Kind::Empty => "empty",
        // Real responder-stack entry starts actual dispatch provenance.
        Kind::KeyDownBegin => "key_down_begin",
        // The matching return closes that same synchronous event scope.
        Kind::KeyDownEnd => "key_down_end",
        // Standard native input callbacks have their own nested scope.
        Kind::SelectorBegin => "selector_begin",
        // Settled callback return carries its explicit interpretation.
        Kind::SelectorEnd => "selector_end",
        // Canonical Ok is separate from whether generation changed.
        Kind::CanonicalAccepted => "canonical_accepted",
        // Domain/admission rejection is finite evidence, not an error body.
        Kind::CanonicalRejected => "canonical_rejected",
    }
}

/// Native input callback kinds have fixed labels without spelling or retaining their text arguments.
fn selector_label(value: Selector) -> &'static str {
    match value {
        // User/IME insertion can finalize a changed canonical revision.
        Selector::InsertText => "insert_text",
        // Marked updates are provisional even when they mutate canonical text.
        Selector::SetMarkedText => "set_marked_text",
        // Unmark can accept existing characters without another primitive.
        Selector::UnmarkText => "unmark_text",
    }
}

/// Commit interpretation remains distinct from the fact that a canonical primitive returned Ok.
fn outcome_label(value: Outcome) -> &'static str {
    match value {
        // Event/canonical records have no standalone commit interpretation.
        Outcome::NotApplicable => "not_applicable",
        // Nested callbacks cannot complete their enclosing selector early.
        Outcome::Nested => "nested",
        // Visible marked text is not a completed user commit.
        Outcome::Provisional => "provisional",
        // An outer accepted mutation returned with native marking clear.
        Outcome::CommittedChanged => "committed_changed",
        // Existing marked text finalized without another canonical change.
        Outcome::CommittedExisting => "committed_existing",
        // No accepted completed insertion was established.
        Outcome::NoCommit => "no_commit",
        // At least one rejection vetoed the outer commit claim.
        Outcome::Rejected => "rejected",
    }
}

/// Rejection labels retain fixed domain meaning and exclude arbitrary error descriptions or paths.
fn rejection_label(value: Rejection) -> &'static str {
    match value {
        // A sealed foreground graph no longer admits input.
        Rejection::Closing => "closing",
        // The native scalar/range ABI cannot represent this operation.
        Rejection::CoordinateOverflow => "coordinate_overflow",
        // The old adapter has no canonical document authority.
        Rejection::DocumentUnavailable => "document_unavailable",
        // The actual domain operation returned an error.
        Rejection::CanonicalError => "canonical_error",
    }
}

/// Every potentially wide counter is decimal text, so JSON consumers cannot silently round it to f64.
fn document_value(document: Document) -> Value {
    json!({
        "generation": document.generation.to_string(),
        "utf8_bytes": document.utf8_bytes.to_string(),
        "utf16_units": document.utf16_units.to_string(),
    })
}

/// Format only Copy scalar fields; absent identities/marking/selector decisions stay JSON null.
fn record_value(record: &Record) -> Value {
    json!({
        "kind": "record",
        "record_kind": kind_label(record.kind),
        "serial": record.serial.to_string(),
        "mach_ticks": record.mach_ticks.to_string(),
        "event_serial": record.event_serial.map(|value| value.to_string()),
        "selector_serial": record.selector_serial.map(|value| value.to_string()),
        "composition_serial": record.composition_serial.map(|value| value.to_string()),
        "selector": record.selector.map(selector_label),
        "nsevent_timestamp_bits": record.nsevent_timestamp_bits.map(|value| value.to_string()),
        "document": record.document.map(document_value),
        "removed_utf16_units": record.removed_utf16_units.to_string(),
        "inserted_utf16_units": record.inserted_utf16_units.to_string(),
        "native_marked": record.native_marked,
        "outcome": outcome_label(record.outcome),
        "rejection": record.rejection.map(rejection_label),
        "accepted_count": record.accepted_count,
        "rejected_count": record.rejected_count,
    })
}

/// Append one bounded JSON object/newline, rejecting growth before extending retained artifact storage.
fn append_line(target: &mut Vec<u8>, value: &Value, maximum: usize) -> Result<(), ExportError> {
    let line = serde_json::to_vec(value).map_err(|_| ExportError::Encoding)?;
    if line.len() > MAX_LINE_BYTES
        || target.len().saturating_add(line.len()).saturating_add(1) > maximum
    {
        // Neither a schema expansion nor an oversized source array may silently exceed the export byte budget.
        return Err(ExportError::Encoding);
    }
    target.extend_from_slice(&line);
    target.push(b'\n');
    Ok(())
}

/// Serialize a finite run after drain; invalidity is explicit evidence and never becomes latency acceptance.
fn encode(evidence: Evidence<'_>) -> Result<EncodedTrace, ExportError> {
    if evidence.records.len() > CAPACITY
        || evidence
            .records
            .iter()
            .any(|record| record.kind == Kind::Empty)
    {
        // A malformed populated slice is not repaired by serializing unused entries or truncating evidence.
        return Err(ExportError::InvalidShape);
    }
    let valid = !evidence.overflowed && !evidence.invalid_scope && !evidence.aborted_scope;
    let mut footer = Vec::new();
    append_line(
        &mut footer,
        &json!({
            "schema": SCHEMA,
            "schema_version": 1,
            "kind": "complete",
            "export_complete": true,
            "record_count": evidence.records.len(),
            "trace_valid": valid,
            "latency_acceptance": false,
            "footer_durability_certified": false,
        }),
        MAX_LINE_BYTES,
    )?;
    let mut prefix = Vec::new();
    append_line(
        &mut prefix,
        &json!({
            "schema": SCHEMA,
            "schema_version": 1,
            "kind": "header",
            "record_count": evidence.records.len(),
            "record_capacity": CAPACITY,
            "completion_contract": "newline_terminated_complete_footer_required",
            "clock_domain": "mach_absolute_ticks",
            "nsevent_clock": "raw_f64_bits_unconverted",
            "armed_at_mach_ticks": evidence.armed_at.to_string(),
            "timebase_numer": evidence.numer,
            "timebase_denom": evidence.denom,
            "initial_document": document_value(evidence.initial),
            "overflowed": evidence.overflowed,
            "invalid_scope": evidence.invalid_scope,
            "aborted_scope": evidence.aborted_scope,
            "trace_valid": valid,
            "latency_acceptance": false,
        }),
        MAX_ARTIFACT_BYTES - footer.len(),
    )?;
    for record in evidence.records {
        // At most CAPACITY records are copied, with no string bodies/native handles in their format adapter.
        append_line(
            &mut prefix,
            &record_value(record),
            MAX_ARTIFACT_BYTES - footer.len(),
        )?;
    }
    Ok(EncodedTrace {
        prefix,
        footer,
        valid,
    })
}

/// This fixture-only controller owns root authority and a status label, never an armed snapshot cache.
pub(super) struct ControllerIvars {
    root: ExportRoot,
    status: Retained<NSMenuItem>,
}

define_class!(
    /// The fixture retains this target; NSMenuItem.target is a public zeroing weak property.
    // SAFETY: this main-thread NSObject subclass initializes all ivars before init and exposes only
    // standard one-object menu action ABIs. All AppKit access remains on the main thread.
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    #[ivars = ControllerIvars]
    pub(super) struct AcceptanceInputTraceController;

    // SAFETY: the NSObject subclass inherits the protocol's normal lifetime and identity behavior.
    unsafe impl NSObjectProtocol for AcceptanceInputTraceController {}

    impl AcceptanceInputTraceController {
        #[unsafe(method(omniAcceptanceArmSourceInputTrace:))]
        fn arm_trace(&self, _sender: Option<&AnyObject>) {
            let status = match input_trace::arm(self.mtm()) {
                // Enter recording only after explicit preparation succeeds; this action starts no output or modal UI.
                Ok(()) => "已开始记录；测量结束后导出",
                // Preparation failure preserves evidence and displays a bounded error category only.
                Err(error) => access_status(error),
            };
            self.ivars().status.setTitle(&NSString::from_str(status));
        }

        #[unsafe(method(omniAcceptanceExportSourceInputTrace:))]
        fn export_trace(&self, _sender: Option<&AnyObject>) {
            let status = match input_trace::drain(self.mtm()) {
                // Only owned scalars leave the plugin borrow; formatting and file I/O begin after drain returns.
                Ok(snapshot) => match encode(Evidence::from(&snapshot)).and_then(|encoded| {
                    // Serialize bounded data locally; publishing neither calls back into the editor nor retains its owner.
                    self.ivars().root.publish(&encoded)?;
                    Ok(encoded.valid)
                }) {
                    // Complete export does not establish input, presentation or latency acceptance.
                    Ok(true) => "导出完成；请单独核对画面证据",
                    // Invalid samples remain inspectable, but their footer must explicitly reject validity.
                    Ok(false) => "导出完成；记录未通过有效性检查",
                    // Preserve incomplete files for inspection; a missing complete footer triggers neither deletion nor automatic retry.
                    Err(_) => "导出未完成；未生成可接受的记录",
                },
                // Busy, unstarted or missing-editor states neither consume evidence nor perform file I/O.
                Err(error) => access_status(error),
            };
            self.ivars().status.setTitle(&NSString::from_str(status));
        }
    }
);

/// Access failures are public finite statuses, never plugin error formatting or source/private-path logging.
fn access_status(error: AccessError) -> &'static str {
    match error {
        // Explicitly start recording first; an empty state cannot be exported as an executed measurement.
        AccessError::NotArmed => "尚未开始记录",
        // A closing or missing editor cannot provide a valid recording source.
        AccessError::NoEditor => "请先打开合成笔记并切换到源码模式",
        // Preserve ownership on reentry without retrying the action inside a callback.
        AccessError::Busy => "编辑器忙碌，本次操作未执行",
        // Explicitly export existing records before another start can replace them.
        AccessError::AlreadyArmed => "正在记录；请先导出",
        // Export cannot split an in-progress input transaction into two records.
        AccessError::ActiveScope => "输入尚未结束；请在测量完成后导出",
        // Composition already in progress lacks a recorded start and cannot be certified as a complete sample.
        AccessError::CompositionActive => "请先完成组合输入，再开始记录",
        // This fixture measures source editing only; preview mode has no recording authority.
        AccessError::SourceRequired => "请先切换到源码模式，再开始记录",
        // Reject recording when the clock is unavailable instead of inventing time units for frame comparison.
        AccessError::ClockUnavailable => "记录时钟不可用",
    }
}

/// Add one explicit fixture menu after product menus exist, returning its required strong target owner.
///
/// The caller supplies its fresh private synthetic root and retains the returned controller until
/// the application event loop ends. The root's final component and every export filename must be
/// real owned nodes; descriptor-relative IO never targets an existing file or replacement root.
///
/// # Errors
/// Rejects missing/already installed menus and unavailable, shared, symlinked or replaced root authority.
pub(super) fn install(
    root_path: &Path,
    mtm: MainThreadMarker,
) -> Result<Retained<AcceptanceInputTraceController>, InstallError> {
    let main = NSApplication::sharedApplication(mtm)
        .mainMenu()
        .ok_or(InstallError::MissingMainMenu)?;
    if main
        .itemWithTitle(&NSString::from_str("源码输入记录"))
        .is_some()
    {
        // Repeated installation cannot replace another live target or create a second export counter.
        return Err(InstallError::AlreadyInstalled);
    }
    let root = ExportRoot::open(root_path)?;
    let status = NSMenuItem::new(mtm);
    status.setTitle(&NSString::from_str("尚未开始记录"));
    status.setEnabled(false);
    let allocated = AcceptanceInputTraceController::alloc(mtm).set_ivars(ControllerIvars {
        root,
        status: status.clone(),
    });
    // SAFETY: the allocated main-thread NSObject subclass has initialized ivars; init consumes its +1
    // allocation and returns the same declared subclass ownership with no escaping borrowed argument.
    let controller: Retained<AcceptanceInputTraceController> =
        unsafe { msg_send![super(allocated), init] };
    let submenu = NSMenu::initWithTitle(NSMenu::alloc(mtm), &NSString::from_str("源码输入记录"));
    submenu.setAutoenablesItems(false);
    for (title, selector) in [
        ("开始记录", sel!(omniAcceptanceArmSourceInputTrace:)),
        (
            "测量结束后导出",
            sel!(omniAcceptanceExportSourceInputTrace:),
        ),
    ] {
        // These actions have no key equivalents, so they do not intercept the actual editor typing being measured.
        add_action(&submenu, &controller, title, selector, mtm);
    }
    submenu.addItem(&NSMenuItem::separatorItem(mtm));
    submenu.addItem(&status);
    let item = NSMenuItem::new(mtm);
    item.setTitle(&NSString::from_str("源码输入记录"));
    item.setSubmenu(Some(&submenu));
    // Publication happens only after controller initialization; no TLS/global strong target is needed.
    main.addItem(&item);
    Ok(controller)
}

/// Bind one private fixture action through AppKit's weak target while the fixture retains its controller.
fn add_action(
    menu: &NSMenu,
    controller: &AcceptanceInputTraceController,
    title: &str,
    selector: Sel,
    mtm: MainThreadMarker,
) {
    // SAFETY: both supplied selectors are declared above with the standard nullable-object argument;
    // the main-thread controller remains retained by the fixture and NSMenuItem.target is zeroing weak.
    let item = unsafe {
        NSMenuItem::initWithTitle_action_keyEquivalent(
            NSMenuItem::alloc(mtm),
            &NSString::from_str(title),
            Some(selector),
            &NSString::from_str(""),
        )
    };
    // SAFETY: this live controller implements the exact selectors; no ownership is transferred to the weak target.
    unsafe { item.setTarget(Some(controller)) };
    menu.addItem(&item);
}

#[cfg(test)]
#[path = "acceptance_input_trace_tests.rs"]
mod tests;
