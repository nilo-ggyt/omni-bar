//! Foreground clipboard state; persistent history outlives a session, transient bodies do not.

use std::cell::Cell;
use std::path::PathBuf;

pub(crate) use super::clipboard_storage::{
    ClipboardFileEntry as ClipboardEntry, MAX_ENTRY_BYTES, MAX_ENTRY_COUNT, MAX_QUERY_CHARACTERS,
};
use super::clipboard_storage::{ClipboardHistoryFile, ClipboardHistoryFileError};

/// Nominal foreground capture period; the platform timer may coalesce within its leeway.
pub(crate) const CHECK_INTERVAL_MS: u64 = 2_000;
/// Twenty-percent timer leeway avoids unnecessary wakeups while the panel is visible.
pub(crate) const CHECK_LEEWAY_MS: u64 = 400;

/// Native search reads this finite UTF-16 prefix before applying storage's Unicode-scalar limit.
pub(crate) const MAX_QUERY_UTF16: usize = MAX_QUERY_CHARACTERS * 2;

/// Acquire the baseline's scalar prefix without copying an unbounded native query or splitting pairs.
pub(crate) fn clipboard_query_prefix(units: &[u16]) -> String {
    // A scalar uses at most two units; the second bound prevents a pure caller from scanning arbitrary input.
    char::decode_utf16(units.iter().copied().take(MAX_QUERY_UTF16))
        .take(MAX_QUERY_CHARACTERS)
        .map(|decoded| {
            // Invalid native UTF-16 remains a finite searchable replacement, never an unchecked Rust string.
            decoded.unwrap_or(char::REPLACEMENT_CHARACTER)
        })
        .collect()
}

/// User navigation and result replacement share one bounded, deterministic row-selection contract.
#[derive(Clone, Copy)]
pub(crate) enum ClipboardSelectionAction {
    QueryChanged,
    Refresh,
    Previous,
    Next,
    First,
    Last,
}

/// Preserve baseline reset/wrap/Home/End behavior without requiring native controls in its tests.
pub(crate) fn clipboard_selection(
    selected: usize,
    count: usize,
    action: ClipboardSelectionAction,
) -> usize {
    if count == 0 {
        // Empty history has no restore target; keeping zero also prevents underflow in boundary commands.
        return 0;
    }
    match action {
        // A new query or explicit Home always starts from the first result, independent of prior selection.
        ClipboardSelectionAction::QueryChanged | ClipboardSelectionAction::First => 0,
        // Updating the same result list preserves selection where possible without an out-of-range row.
        ClipboardSelectionAction::Refresh => selected.min(count - 1),
        // Previous wraps from the first result while avoiding overflowing index-plus-count arithmetic.
        ClipboardSelectionAction::Previous => match selected % count {
            // The first row cycles to the final actual result.
            0 => count - 1,
            // Any later valid row has a directly preceding result.
            current => current - 1,
        },
        // The modulo input is below count, so adding one cannot overflow even for a maximal usize count.
        ClipboardSelectionAction::Next => (selected % count + 1) % count,
        // End chooses the last result, including the same sole row for a one-item history.
        ClipboardSelectionAction::Last => count - 1,
    }
}

/// Exact source names fit existing general/private pasteboards without an idle heap allocation.
pub(crate) const MAX_SOURCE_NAME_UTF16: usize = 256;

/// A bounded exact namespace value; it owns neither a pasteboard nor any clipboard content.
#[derive(Clone, Copy, Eq, PartialEq)]
pub(crate) struct ClipboardSourceIdentity {
    units: [u16; MAX_SOURCE_NAME_UTF16],
    length: usize,
}

impl ClipboardSourceIdentity {
    /// Reject oversized names rather than allowing truncation or hash collisions across sources.
    pub(crate) fn from_utf16(name: &[u16]) -> Option<Self> {
        if name.len() > MAX_SOURCE_NAME_UTF16 {
            // An identity must describe the whole namespace before capture or storage mutation.
            return None;
        }
        let mut units = [0; MAX_SOURCE_NAME_UTF16];
        units[..name.len()].copy_from_slice(name);
        Some(Self {
            units,
            length: name.len(),
        })
    }
}

/// Namespace and change count observed together from one transient native authority.
#[derive(Clone, Copy)]
pub(crate) struct ClipboardSourceGeneration {
    pub(crate) source: ClipboardSourceIdentity,
    pub(crate) change_count: isize,
}

/// A successful fixed-file clear is the only producer of suppression publication authority.
#[derive(Clone, Copy)]
pub(crate) struct ClipboardClearReceipt(ClipboardSourceGeneration);

impl ClipboardClearReceipt {
    /// Copy the successful boundary so the active monitor can avoid rereading its suppressed body.
    pub(crate) fn generation(self) -> ClipboardSourceGeneration {
        self.0
    }
}

/// Decide body acquisition from exact source/activity/generation metadata before reading text.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ClipboardCaptureDecision {
    Skip,
    Read,
    SourceChanged,
}

/// Process-only value for the most recently cleared namespace; replacing it retains no history.
#[derive(Clone, Copy)]
pub(crate) struct ClipboardClearSuppression {
    cleared: Option<ClipboardSourceGeneration>,
}

impl ClipboardClearSuppression {
    /// A new application process has no suppression, even when persisted history already exists.
    pub(crate) const fn new() -> Self {
        Self { cleared: None }
    }

    /// Apply a clear barrier only to its exact namespace, never to an unrelated fixture source.
    fn ignored_through(self, source: ClipboardSourceIdentity) -> isize {
        match self.cleared {
            // Reopening the same source keeps its successful process-lifetime clear boundary.
            Some(cleared) if cleared.source == source => cleared.change_count,
            // Opening another namespace leaves this one-entry state intact but cannot inherit its barrier.
            Some(_) | None => isize::MIN,
        }
    }

    /// Skip materializing an initial body that is already covered by this source's clear barrier.
    pub(crate) fn ignores(self, generation: ClipboardSourceGeneration) -> bool {
        generation.change_count <= self.ignored_through(generation.source)
    }

    /// Remember only successful clear receipts; a different cleared namespace replaces this entry.
    pub(crate) fn remember(&mut self, receipt: ClipboardClearReceipt) {
        self.cleared = Some(receipt.0);
    }
}

/// Bounded failures exposed to native controls without revealing filesystem paths or bodies.
#[derive(Debug, thiserror::Error)]
pub(crate) enum ClipboardError {
    #[error("剪贴板历史文件不可用")]
    Storage(#[source] ClipboardHistoryFileError),
    #[error("找不到这条剪贴板记录")]
    EntryNotFound,
    #[error("剪贴板来源已改变，请重新打开面板")]
    SourceChanged,
}

/// Owns paths and scalar foreground state; no timer, descriptor, map, or body is retained here.
pub(crate) struct ClipboardSession {
    storage: ClipboardHistoryFile,
    active: Cell<bool>,
    ignore_through: Cell<isize>,
    source: ClipboardSourceIdentity,
}

impl ClipboardSession {
    /// Construct inactive metadata without creating a file before explicit foreground use.
    pub(crate) fn new(
        application_data: PathBuf,
        source: ClipboardSourceIdentity,
        suppression: ClipboardClearSuppression,
    ) -> Self {
        // Interior scalar cells support callback cancellation without borrowing a native object graph.
        Self {
            storage: ClipboardHistoryFile::new(application_data),
            active: Cell::new(false),
            ignore_through: Cell::new(suppression.ignored_through(source)),
            source,
        }
    }

    /// Prepare the fixed file only when the clipboard surface is explicitly opened.
    pub(crate) fn begin(&self) -> Result<(), ClipboardError> {
        // Failed preparation leaves the session inactive and cannot create a monitor.
        self.storage.prepare().map_err(map_error)?;
        self.active.set(true);
        Ok(())
    }

    /// Record an already-authorized stable pasteboard generation without keeping its body.
    pub(crate) fn record(
        &self,
        current: ClipboardSourceGeneration,
        content: &str,
        timestamp_ms: i64,
    ) -> Result<bool, ClipboardError> {
        if current.source != self.source {
            // Even a correctly bounded body cannot be attributed to a different clipboard authority.
            return Err(ClipboardError::SourceChanged);
        }
        if !self.active.get() || current.change_count <= self.ignore_through.get() {
            // Closed sessions and captures that predate clear cannot repopulate persistent history.
            return Ok(false);
        }
        // Storage owns validation, deduplication and operation-scoped descriptors/owned buffers.
        self.storage
            .record(content, timestamp_ms)
            .map(|outcome| outcome.is_some())
            .map_err(map_error)
    }

    /// Native capture must consult this decision before asking its pasteboard for any body or types.
    pub(crate) fn capture_decision(
        &self,
        current: ClipboardSourceGeneration,
        last_seen: isize,
    ) -> ClipboardCaptureDecision {
        if current.source != self.source {
            // Equal change counts do not establish namespace identity and cannot bypass this rejection.
            return ClipboardCaptureDecision::SourceChanged;
        }
        if !self.active.get()
            || current.change_count <= self.ignore_through.get()
            || current.change_count == last_seen
        {
            // Closed, cleared or unchanged metadata must never request materialization of the current body.
            return ClipboardCaptureDecision::Skip;
        }
        ClipboardCaptureDecision::Read
    }

    /// Return at most twenty bounded previews; never retain complete clipboard bodies.
    pub(crate) fn search(&self, query: &str) -> Result<Vec<ClipboardEntry>, ClipboardError> {
        // Search does not create missing storage, so status remains lazy on a new installation.
        self.storage
            .search(query, MAX_ENTRY_COUNT)
            .map_err(map_error)
    }

    /// Lend one validated, bounded owned body to the platform write boundary and promote recency.
    pub(crate) fn with_entry<T>(
        &self,
        id: i64,
        consume: impl FnOnce(&str) -> T,
    ) -> Result<T, ClipboardError> {
        // The callback cannot return a borrow of storage's temporary buffer; its lock and file close on return.
        self.storage
            .with_entry_content(id, consume)
            .map_err(map_error)
    }

    /// Clear all visible records and reject a capture already read before this clear operation.
    pub(crate) fn clear(
        &self,
        current: ClipboardSourceGeneration,
    ) -> Result<ClipboardClearReceipt, ClipboardError> {
        if current.source != self.source {
            // An authority replacement cannot clear this session or publish another source's generation.
            return Err(ClipboardError::SourceChanged);
        }
        // Commit metadata invalidation/payload clearing before advancing the generation barrier.
        self.storage.clear().map_err(map_error)?;
        let change_count = self.ignore_through.get().max(current.change_count);
        self.ignore_through.set(change_count);
        Ok(ClipboardClearReceipt(ClipboardSourceGeneration {
            source: self.source,
            change_count,
        }))
    }

    /// Stop accepting callbacks before the platform cancels its foreground timer.
    pub(crate) fn close(&self) {
        // Persistent history remains untouched so a later fresh session can display it.
        self.active.set(false);
    }
}

/// Translate storage failures to bounded presentation errors while preserving missing-entry intent.
fn map_error(error: ClipboardHistoryFileError) -> ClipboardError {
    match error {
        // The selected row may disappear after a clear or a newer bounded history update.
        ClipboardHistoryFileError::EntryNotFound => ClipboardError::EntryNotFound,
        // Keep the typed storage cause for the owning error boundary while Display remains the
        // fixed Chinese message above. Losing the cause here would erase the actual failed I/O.
        cause @ (ClipboardHistoryFileError::StorageUnavailable(_)
        | ClipboardHistoryFileError::InvalidFormat) => ClipboardError::Storage(cause),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    /// Construct finite synthetic namespace metadata without acquiring AppKit or a native clipboard.
    fn generation(name: &str, change_count: isize) -> ClipboardSourceGeneration {
        let units: Vec<_> = name.encode_utf16().collect();
        ClipboardSourceGeneration {
            source: ClipboardSourceIdentity::from_utf16(&units).unwrap(),
            change_count,
        }
    }

    /// Closing blocks late callbacks while retaining the user's fixed-file history across sessions.
    #[test]
    fn close_and_clear_have_independent_generation_boundaries() {
        // The test supplies synthetic generations and data under a disposable root.
        let root = tempdir().unwrap();
        let source = generation("general", 0).source;
        let session = ClipboardSession::new(
            root.path().to_owned(),
            source,
            ClipboardClearSuppression::new(),
        );
        assert!(session.search("").unwrap().is_empty());
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
        session.begin().unwrap();
        assert!(
            session
                .record(generation("general", 7), "first", 1)
                .unwrap()
        );
        let receipt = session.clear(generation("general", 8)).unwrap();
        assert!(
            !session
                .record(generation("general", 8), "late before clear", 2)
                .unwrap()
        );
        assert!(
            session
                .record(generation("general", 9), "fresh", 3)
                .unwrap()
        );
        session.close();
        assert!(
            !session
                .record(generation("general", 10), "late after close", 4)
                .unwrap()
        );
        let mut suppression = ClipboardClearSuppression::new();
        suppression.remember(receipt);
        let reopened = ClipboardSession::new(root.path().to_owned(), source, suppression);
        assert_eq!(reopened.search("").unwrap()[0].preview, "fresh");
    }

    /// Reopening must reject the initial unchanged body after the original session is entirely dropped.
    #[test]
    fn successful_clear_survives_close_reopen_but_not_process_state_reset() {
        let root = tempdir().unwrap();
        let current = generation("general", 7);
        let mut suppression = ClipboardClearSuppression::new();
        let session = ClipboardSession::new(root.path().to_owned(), current.source, suppression);
        // The first user-triggered activation captures the present item without resident monitoring.
        session.begin().unwrap();
        assert!(
            session
                .record(generation("general", 7), "current body", 1)
                .unwrap()
        );
        suppression.remember(session.clear(current).unwrap());
        session.close();
        drop(session);

        // Copy-only metadata, rather than the old session, supplies the new activation's clear boundary.
        assert!(suppression.ignores(current));
        let reopened = ClipboardSession::new(root.path().to_owned(), current.source, suppression);
        reopened.begin().unwrap();
        assert!(
            !reopened
                .record(generation("general", 7), "current body", 2)
                .unwrap()
        );
        assert!(reopened.search("").unwrap().is_empty());
        // A genuinely newer pasteboard generation is ordinary new input, even with identical bytes.
        assert!(
            reopened
                .record(generation("general", 8), "current body", 3)
                .unwrap()
        );
        assert_eq!(reopened.search("").unwrap().len(), 1);
        reopened.close();
        drop(reopened);

        // Starting a new process policy deliberately permits the initial item; suppression is never persisted.
        let restarted = ClipboardSession::new(
            root.path().to_owned(),
            current.source,
            ClipboardClearSuppression::new(),
        );
        restarted.begin().unwrap();
        assert!(
            restarted
                .record(generation("general", 7), "current body", 4)
                .unwrap()
        );
    }

    /// Source and clear admission must reject a body read, rather than merely rejecting its later storage write.
    #[test]
    fn capture_admission_precedes_body_reads_for_clear_close_and_source_changes() {
        let root = tempdir().unwrap();
        let current = generation("general", 7);
        let mut suppression = ClipboardClearSuppression::new();
        let session = ClipboardSession::new(root.path().to_owned(), current.source, suppression);
        // Inactive sessions cannot authorize body acquisition even for an unseen source generation.
        assert_eq!(
            session.capture_decision(current, 1),
            ClipboardCaptureDecision::Skip
        );
        session.begin().unwrap();
        assert_eq!(
            session.capture_decision(current, 1),
            ClipboardCaptureDecision::Read
        );
        assert_eq!(
            session.capture_decision(current, 7),
            ClipboardCaptureDecision::Skip
        );
        assert_eq!(
            session.capture_decision(generation("private", 7), 7),
            ClipboardCaptureDecision::SourceChanged
        );
        // Clearing a newly copied generation before its timer tick forbids reading that body with the stale seen count.
        let cleared = generation("general", 8);
        let receipt = session.clear(cleared).unwrap();
        assert_eq!(receipt.generation().change_count, 8);
        suppression.remember(receipt);
        assert_eq!(
            session.capture_decision(cleared, 7),
            ClipboardCaptureDecision::Skip
        );
        session.close();
        drop(session);
        // A fresh activation cannot bypass the source-specific suppression with a reset local seen count.
        let reopened = ClipboardSession::new(root.path().to_owned(), current.source, suppression);
        reopened.begin().unwrap();
        assert_eq!(
            reopened.capture_decision(cleared, isize::MIN),
            ClipboardCaptureDecision::Skip
        );
        assert_eq!(
            reopened.capture_decision(generation("general", 9), 8),
            ClipboardCaptureDecision::Read
        );
        assert!(matches!(
            reopened.record(generation("private", 9), "wrong source", 1),
            Err(ClipboardError::SourceChanged)
        ));
        reopened.close();
        assert_eq!(
            reopened.capture_decision(generation("general", 9), 8),
            ClipboardCaptureDecision::Skip
        );
    }

    /// The single process entry is exact-source scoped and replaces only when another clear succeeds.
    #[test]
    fn different_namespaces_do_not_inherit_suppression_or_replace_it_on_open() {
        let root = tempdir().unwrap();
        let general = generation("general", 20);
        let private = generation("private-fixture", 3);
        let mut suppression = ClipboardClearSuppression::new();
        let session = ClipboardSession::new(root.path().to_owned(), general.source, suppression);
        session.begin().unwrap();
        suppression.remember(session.clear(general).unwrap());
        drop(session);

        // Opening a lower-generation private source neither inherits nor destroys the general barrier.
        assert!(!suppression.ignores(private));
        let other = ClipboardSession::new(root.path().to_owned(), private.source, suppression);
        other.begin().unwrap();
        assert!(
            other
                .record(generation("private-fixture", 3), "private body", 1)
                .unwrap()
        );
        assert!(suppression.ignores(general));
        // Clearing that distinct source replaces the bounded one-entry policy instead of retaining a namespace collection.
        suppression.remember(other.clear(private).unwrap());
        assert!(suppression.ignores(private));
        assert!(!suppression.ignores(general));
    }

    /// Disk refusal cannot advance either the live boundary or the process-only successful-clear entry.
    #[test]
    fn failed_clear_preserves_previous_suppression_and_accepts_a_later_capture() {
        use std::os::unix::fs::FileExt;

        let root = tempdir().unwrap();
        let current = generation("general", 7);
        let mut suppression = ClipboardClearSuppression::new();
        let session = ClipboardSession::new(root.path().to_owned(), current.source, suppression);
        session.begin().unwrap();
        suppression.remember(session.clear(current).unwrap());
        // Corrupt only the fixed magic, preserving extent and payloads so validation rejects before mutation.
        let file = std::fs::OpenOptions::new()
            .write(true)
            .open(root.path().join("Clipboard/history-v1.mmap"))
            .unwrap();
        file.write_all_at(b"INVALID!", 0).unwrap();
        let newer = generation("general", 8);
        assert!(session.clear(newer).is_err());
        assert!(suppression.ignores(current));
        assert!(!suppression.ignores(newer));
        // Restore the test-owned header and prove failure did not silently suppress the pending generation.
        file.write_all_at(b"OMNICLP1", 0).unwrap();
        drop(file);
        assert!(
            session
                .record(generation("general", 8), "still capturable", 1)
                .unwrap()
        );
        assert_eq!(session.search("").unwrap()[0].preview, "still capturable");
    }

    /// An authority switch is rejected before the original history or clear policy can change.
    #[test]
    fn clear_rejects_another_source_before_mutating_history() {
        let root = tempdir().unwrap();
        let current = generation("general", 7);
        let session = ClipboardSession::new(
            root.path().to_owned(),
            current.source,
            ClipboardClearSuppression::new(),
        );
        session.begin().unwrap();
        session
            .record(generation("general", 7), "preserved body", 1)
            .unwrap();
        assert!(matches!(
            session.clear(generation("private-fixture", 7)),
            Err(ClipboardError::SourceChanged)
        ));
        assert_eq!(session.search("").unwrap()[0].preview, "preserved body");
    }

    /// Exact bounded UTF-16 preserves suffixes, embedded NUL and surrogate units without hash/truncation aliasing.
    #[test]
    fn source_identity_keeps_the_entire_bounded_namespace() {
        let full = [1; MAX_SOURCE_NAME_UTF16];
        let mut different = full;
        different[MAX_SOURCE_NAME_UTF16 - 1] = 2;
        assert!(ClipboardSourceIdentity::from_utf16(&full).is_some());
        assert!(
            ClipboardSourceIdentity::from_utf16(&full)
                != ClipboardSourceIdentity::from_utf16(&different)
        );
        assert!(ClipboardSourceIdentity::from_utf16(&[1; MAX_SOURCE_NAME_UTF16 + 1]).is_none());
        assert!(
            ClipboardSourceIdentity::from_utf16(&[b'a' as u16])
                != ClipboardSourceIdentity::from_utf16(&[b'a' as u16, 0])
        );
        assert!(
            ClipboardSourceIdentity::from_utf16(&[0xd83d, 0xde00])
                != ClipboardSourceIdentity::from_utf16(&[0xd83d, 0xde01])
        );
    }

    /// Prefix acquisition must preserve complete pairs and the baseline's scalar, rather than grapheme, count.
    #[test]
    fn query_prefix_matches_scalar_bound_for_ascii_pairs_and_combining_marks() {
        // Compare all three sides of the exact storage boundary without native string or IME APIs.
        for count in [127, 128, 129] {
            // Emoji consume two UTF-16 units each; ASCII and combining scalars consume one.
            for value in [
                "a".repeat(count),
                "😀".repeat(count),
                "\u{301}".repeat(count),
            ] {
                let units: Vec<_> = value.encode_utf16().collect();
                let expected: String = value.chars().take(MAX_QUERY_CHARACTERS).collect();
                assert_eq!(clipboard_query_prefix(&units), expected);
            }
        }
        // A mixed prefix can end its native acquisition window halfway through an irrelevant 129th scalar.
        let mixed = format!("{}a😀tail", "😀".repeat(127));
        assert_eq!(
            clipboard_query_prefix(&mixed.encode_utf16().collect::<Vec<_>>()),
            format!("{}a", "😀".repeat(127))
        );
        // Combining marks remain independent scalars exactly as storage's preexisting chars().take() rule requires.
        let combining = "a\u{301}".repeat(129);
        assert_eq!(
            clipboard_query_prefix(&combining.encode_utf16().collect::<Vec<_>>()),
            "a\u{301}".repeat(64)
        );
    }

    /// Unpaired UTF-16 units produce finite replacements instead of invalid Rust UTF-8 or a panic.
    #[test]
    fn query_prefix_replaces_invalid_surrogates_within_the_scalar_budget() {
        assert_eq!(
            clipboard_query_prefix(&[0xd800, b'x' as u16, 0xdc00]),
            "�x�"
        );
        // A final invalid unit inside the admitted 128th scalar is retained as one replacement.
        let mut units = vec![b'a' as u16; MAX_QUERY_CHARACTERS - 1];
        units.push(0xd800);
        assert_eq!(
            clipboard_query_prefix(&units),
            format!("{}�", "a".repeat(127))
        );
        // Invalid suffix units beyond the scalar prefix cannot change the bounded query.
        units.insert(0, b'a' as u16);
        assert_eq!(clipboard_query_prefix(&units), "a".repeat(128));
    }

    /// A real filesystem conflict keeps its diagnostic cause without exposing it to the user.
    #[test]
    fn failed_storage_activation_preserves_its_cause_and_existing_file() {
        use std::error::Error;

        // A regular file occupies the required private directory. This exercises actual production
        // acquisition failure without injected I/O, permission changes, or access to a pasteboard.
        let root = tempdir().unwrap();
        let occupant = root.path().join("Clipboard");
        std::fs::write(&occupant, b"preserve existing bytes").unwrap();
        let session = ClipboardSession::new(
            root.path().to_owned(),
            generation("isolated-storage-failure", 1).source,
            ClipboardClearSuppression::new(),
        );
        let error = session.begin().unwrap_err();
        assert_eq!(error.to_string(), "剪贴板历史文件不可用");
        let storage_cause = error.source().expect("original typed storage error");
        assert!(
            storage_cause
                .source()
                .and_then(|cause| cause.downcast_ref::<std::io::Error>())
                .is_some()
        );
        assert_eq!(
            std::fs::read(&occupant).unwrap(),
            b"preserve existing bytes"
        );
    }

    /// Result refresh preserves a valid target; explicit query edits and Home/End have their own baseline meanings.
    #[test]
    fn selection_policy_preserves_refresh_resets_query_and_wraps_navigation() {
        use ClipboardSelectionAction::*;

        // Every action on empty history remains a non-restorable zero selection, without underflow.
        for action in [QueryChanged, Refresh, Previous, Next, First, Last] {
            assert_eq!(clipboard_selection(usize::MAX, 0, action), 0);
        }
        assert_eq!(clipboard_selection(9, 20, Refresh), 9);
        assert_eq!(clipboard_selection(9, 3, Refresh), 2);
        assert_eq!(clipboard_selection(9, 20, QueryChanged), 0);
        assert_eq!(clipboard_selection(9, 20, First), 0);
        assert_eq!(clipboard_selection(2, 20, Last), 19);
        assert_eq!(clipboard_selection(0, 20, Previous), 19);
        assert_eq!(clipboard_selection(19, 20, Next), 0);
        assert_eq!(clipboard_selection(5, 20, Previous), 4);
        assert_eq!(clipboard_selection(5, 20, Next), 6);
        // Arbitrary stale indices and maximal count inputs cannot overflow the pure navigation policy.
        assert_eq!(clipboard_selection(usize::MAX, 1, Next), 0);
        assert_eq!(clipboard_selection(usize::MAX - 1, usize::MAX, Next), 0);
        assert_eq!(
            clipboard_selection(usize::MAX, usize::MAX, Previous),
            usize::MAX - 1
        );
    }
}
