//! Session field-editor/undo ownership and public cache-detachment facts; explicit editors still require release.

mod session_editor;
pub(super) use session_editor::SessionFieldEditorProvider;

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use objc2::rc::{Retained, Weak};
use objc2::{MainThreadMarker, msg_send};
use objc2_app_kit::{NSTextField, NSTextView, NSWindow};
use objc2_foundation::NSUndoManager;

use super::native_payload::NativeOwnedLedger;

#[derive(Default)]
/// Allocate a manager only when undo is requested; never recreate session history after close.
pub(super) struct SessionUndo {
    manager: RefCell<Option<Retained<NSUndoManager>>>,
    providing: Cell<bool>,
    closed: Cell<bool>,
    failure: RefCell<Option<String>>,
}

impl SessionUndo {
    /// Lend only this session's lazy native history, never a shared responder-chain manager.
    pub(super) fn manager(&self, ledger: &NativeOwnedLedger) -> Option<Retained<NSUndoManager>> {
        if self.closed.get() || self.failure.borrow().is_some() {
            // Accepted close and the first ownership failure permanently revoke publication authority.
            return None;
        }
        if self.providing.replace(true) {
            // A nested native getter cannot mistake the temporarily empty slot for absent history.
            return None;
        }
        struct ProvidingGuard<'a>(&'a Cell<bool>);
        impl Drop for ProvidingGuard<'_> {
            /// Clear scalar busy state after every return or unwind; never send native messages from Drop.
            fn drop(&mut self) {
                self.0.set(false);
            }
        }
        let _providing = ProvidingGuard(&self.providing);
        // Move the real owner out before initialization, ledger capture or retain can reenter AppKit.
        let cached = self.manager.take();
        let is_new = cached.is_none();
        let owner = match cached {
            Some(owner) => {
                // Cached live history remains the same manager and is never registered a second time.
                owner
            }
            None => {
                let Some(mtm) = MainThreadMarker::new() else {
                    // A foreign thread creates no manager and leaves the first concrete reason for preflight.
                    self.record_failure("Session undo requires the AppKit main thread".into());
                    return None;
                };
                // This is the native session's only creation boundary, still guarded against reentrant lending.
                NSUndoManager::new(mtm)
            }
        };
        if !self.can_publish_owner() {
            // Initialization may reenter close; an unpublished candidate has no authority to enter the ledger.
            self.reject_owner(owner, None, is_new);
            return None;
        }
        if is_new && let Err(error) = ledger.include("session.undo-manager", &owner) {
            // Untracked history must never become visible; preserve an earlier cause if registration reentered.
            self.record_failure(error);
        }
        if !self.can_publish_owner() {
            // Weak registration can reenter too; do not lend a candidate whose authority changed during capture.
            self.reject_owner(owner, None, is_new);
            return None;
        }
        // Native retain is outside the RefCell borrow and may itself reenter close or ownership observation.
        let returned = owner.clone();
        if !self.can_publish_owner() {
            // Before accepted close, restore the cached owner intact; after close, release both actual claims.
            self.reject_owner(owner, Some(returned), is_new);
            return None;
        }
        let previous = self.manager.replace(Some(owner));
        // Providing excludes competing writers. End the slot borrow before even an unexpected prior owner drops.
        drop(previous);
        Some(returned)
    }

    /// Check around normal resignation and before admission; an in-flight provider must finish before close is accepted.
    pub(super) fn prepare_close(&self) -> Result<(), String> {
        if let Some(error) = self.failure.borrow().as_ref() {
            // Preserve an existing ownership failure before considering later Busy or closed state.
            return Err(error.clone());
        }
        if self.providing.get() {
            // Busy neither poisons the session nor clears history; editing and close can resume after the provider returns.
            return Err("Session undo manager is being provided".into());
        }
        if self.closed.get() {
            // Another preflight cannot reactivate an owner whose close was already accepted.
            return Err("Session undo is already closed".into());
        }
        Ok(())
    }

    /// Recheck authority after every reentrant native phase; a first failure prevents owner publication.
    fn can_publish_owner(&self) -> bool {
        if self.closed.get() {
            // Record direct reentrant closure explicitly; a temporarily empty slot does not prove complete cleanup.
            self.record_failure("Session undo closed while manager was being provided".into());
        }
        !self.closed.get() && self.failure.borrow().is_none()
    }

    /// Preserve cached live history; only a new candidate or closed session permits owned-action cleanup.
    fn reject_owner(
        &self,
        owner: Retained<NSUndoManager>,
        returned: Option<Retained<NSUndoManager>>,
        is_new: bool,
    ) {
        if !is_new && !self.closed.get() {
            // A failed return before admission revokes this loan only; preserve the user's actual cached history.
            let previous = self.manager.replace(Some(owner));
            drop(previous);
            // Restore the original owner before dropping the extra return claim; the busy guard still rejects reentry.
            drop(returned);
            return;
        }
        if let Err(error) = Self::clear_owned_manager(&owner) {
            // Only a new unpublished candidate or an accepted-close owner permits action cleanup.
            self.record_failure(error);
        }
        // Both claims end outside state borrows; cleanup failure cannot keep an owner hidden in the session slot.
        drop(returned);
        drop(owner);
    }

    /// Preserve the first delegate failure for close preflight because Objective-C has no Result return channel.
    fn record_failure(&self, error: String) {
        self.failure.borrow_mut().get_or_insert(error);
    }

    /// Clean only the exclusive project slot; inspect undo/redo on that owner, not an unknown responder.
    fn clear_owned_manager(manager: &NSUndoManager) -> Result<(), String> {
        manager.removeAllActions();
        if manager.canUndo() || manager.canRedo() {
            // Remaining actions can retain old text or targets, so they prevent a successful resource receipt.
            return Err("Session undo history did not clear".into());
        }
        Ok(())
    }

    /// After close acceptance, clear undo/redo and remove the owner; Drop performs no fallible business cleanup.
    fn finish(&self) -> Result<(), String> {
        self.closed.set(true);
        if self.providing.get() {
            // Normal preflight rejects this state; direct reentry seals it without claiming outstanding stack owners were released.
            self.record_failure("Session undo closed while manager was being provided".into());
        }
        let manager = self.manager.take();
        if let Some(manager) = manager {
            // A local strong claim protects this public cleanup only and never enters callbacks or a resident cache.
            if let Err(error) = Self::clear_owned_manager(&manager) {
                // Cleanup failure preserves earlier ownership failure; drop the local owner outside state borrows.
                self.record_failure(error);
            }
        }
        if let Some(error) = self.failure.borrow().as_ref() {
            // Clearing actions cannot hide an earlier registration failure.
            return Err(error.clone());
        }
        Ok(())
    }
}

#[derive(Clone, Copy)]
/// Keep public scalar facts; invisibility or elapsed time cannot prove a safe cache boundary.
struct BorrowedEditorFacts {
    text_empty: bool,
    unmarked: bool,
    delegate_detached: bool,
    hierarchy_detached: bool,
    selection_empty: bool,
}

impl BorrowedEditorFacts {
    /// Every fact concerns data or callback authority; a framework class name cannot waive any requirement.
    fn verify(self) -> bool {
        self.text_empty
            && self.unmarked
            && self.delegate_detached
            && self.hierarchy_detached
            && self.selection_empty
    }
}

#[derive(Default)]
/// Share completion facts between preflight and named probes without owning native objects.
struct BorrowedEditorState {
    editors: Vec<Weak<NSTextView>>,
    observed_undo: Vec<Weak<NSUndoManager>>,
    finished: bool,
    controls_detached: bool,
    failure: Option<String>,
}

#[derive(Clone, Default)]
/// Record at most four before/after editor weak identities without observing deallocation of borrowed caches.
pub(super) struct BorrowedEditorSnapshot(Rc<RefCell<BorrowedEditorState>>);

impl BorrowedEditorSnapshot {
    /// Observe the actual manager only during editing begin/change, never by creating history during close observation.
    pub(super) fn observe_editing(
        &self,
        control: &NSTextField,
        undo: &SessionUndo,
        ledger: &NativeOwnedLedger,
    ) -> Result<(), String> {
        let result = self.observe_active_editor(control, undo, ledger);
        if let Err(error) = &result {
            // Preserve identity mismatch in shared state so later preflight rejects close with the original input intact.
            self.record_failure(error.clone());
        }
        result
    }

    /// Admit only this control's real session editor and independently record its actual undo identity.
    fn observe_active_editor(
        &self,
        control: &NSTextField,
        undo: &SessionUndo,
        ledger: &NativeOwnedLedger,
    ) -> Result<(), String> {
        let Some(editor) = control.currentEditor() else {
            // A nonediting native value update has no actual editor to observe; do not create one for the probe.
            return Ok(());
        };
        let editor = editor
            .downcast::<NSTextView>()
            .map_err(|_| "Host active field editor does not implement NSTextView".to_owned())?;
        if !session_editor::is_session_editor(&editor, undo, ledger) {
            // An NSCell bypass or wrong window cannot authorize an unknown editor; preserve input and reject close.
            return Err("Host active field editor bypassed its session provider".into());
        }
        self.record_editor(&editor)?;
        let expected = undo
            .manager(ledger)
            .ok_or("Host session undo manager is unavailable during actual editing")?;
        // This is the actual editing undo path; the owned standard editor must return this session's manager.
        let actual = editor
            .undoManager()
            .ok_or("Host active editor has no session undo manager")?;
        if Retained::as_ptr(&expected) != Retained::as_ptr(&actual) {
            // Never clear unknown/shared history or treat it as exclusively owned session history.
            return Err("Host active editor uses an unexpected undo manager".into());
        }
        let known = self.0.borrow().observed_undo.clone();
        if !known.iter().any(|manager| {
            // Deduplicate live manager identities only; stale addresses grant no ownership authority.
            manager
                .load()
                .is_some_and(|manager| std::ptr::eq(&*manager, &*actual))
        }) {
            if known.len() == 4 {
                // Bound manager replacements during editing; preserve failure rather than discarding previous evidence.
                return Err("Host observed undo-manager identity budget exceeded".into());
            }
            self.0
                .borrow_mut()
                .observed_undo
                .push(Weak::from_retained(&actual));
        }
        Ok(())
    }

    /// Observe an existing default editor without creation, including replacements made during resignation.
    pub(super) fn capture(&self, panel: &NSWindow) -> Result<(), String> {
        if let Some(error) = self.failure() {
            // Ownership failures found during editing must reject close before input can be cleared.
            return Err(error);
        }
        // SAFETY: false forbids creation; a nil client queries only the live main-thread window's existing editor.
        let editor = unsafe { panel.fieldEditor_forObject(false, None) };
        if let Some(editor) = editor {
            // Inspect only the existing window cache and retain its weak identity; no default editor is requested.
            let editor = editor.downcast::<NSTextView>().map_err(|_| {
                "Host default field editor does not implement NSTextView".to_owned()
            })?;
            self.record_editor(&editor)?;
        }
        Ok(())
    }

    /// Share weak facts between owned-editor construction and cache observation without creating a default editor.
    fn record_editor(&self, editor: &Retained<NSTextView>) -> Result<(), String> {
        let known_editors = self.0.borrow().editors.clone();
        let already_known = known_editors.iter().any(|known| {
            // Deduplicate live weak identities; nil entries neither hide address reuse nor discard surviving old editors.
            known
                .load()
                .is_some_and(|known| std::ptr::eq(&*known, &**editor))
        });
        if !already_known {
            // New observations append bounded zeroing weak evidence; old live editors remain in the proof set.
            if self.0.borrow().editors.len() == 4 {
                // Reject replacements beyond the fixed budget instead of discarding evidence to report success.
                return Err("Host observed field-editor identity budget exceeded".into());
            }
            self.0
                .borrow_mut()
                .editors
                .push(Weak::from_retained(editor));
        }
        Ok(())
    }

    /// Preserve the first constructor/delegate failure for normal preflight to protect editable input.
    fn record_failure(&self, error: String) {
        self.0.borrow_mut().failure.get_or_insert(error);
    }

    /// Run only after validation and close admission; retain failure as a nonretryable release-proof failure.
    pub(super) fn finish_accepted(
        &self,
        panel: &NSWindow,
        controls: &[&NSTextField],
        undo: &SessionUndo,
        provider: &SessionFieldEditorProvider,
    ) {
        let result = self.finish_in_place(panel, controls, undo, provider);
        self.0.borrow_mut().finished = true;
        if let Err(error) = result {
            // Accepted close cannot be rolled back or retried here; the barrier must remain Closing on failure.
            self.record_failure(error);
        }
    }

    /// Public endEditing detaches shared editing relationships after normal resignation and close admission.
    fn finish_in_place(
        &self,
        panel: &NSWindow,
        controls: &[&NSTextField],
        undo: &SessionUndo,
        provider: &SessionFieldEditorProvider,
    ) -> Result<(), String> {
        // Freeze both native editor and history creation first; an observation failure cannot leave an
        // accepted-close owner able to reacquire resources through a synchronous teardown callback.
        provider.freeze();
        undo.closed.set(true);
        let mut first_failure = self.capture(panel).err();
        // SAFETY: public endEditing accepts nil. Normal resignation and admission completed before
        // this cleanup, so invoke it once even when initial capture failed; never retry the old edit.
        unsafe { panel.endEditingFor(None) };
        if let Err(error) = undo.finish() {
            // Preserve the first cause while continuing to release every known editor/control payload.
            first_failure.get_or_insert(error);
        }
        for control in controls {
            // Only accepted close may clear business fields; earlier capture/history errors do not retain payloads.
            control.setStringValue(objc2_foundation::ns_string!(""));
        }
        // Default caches are borrowed; release only this provider's editor after clearing its data and callbacks.
        provider.release_accepted();
        // Final capture is noncreating. Inspect all already-known identities even when it fails, because
        // neither empty text nor earlier cleanup success can waive a remaining history or editing relationship.
        let captured = self.capture(panel);
        let controls_detached = controls.iter().all(|control| {
            // currentEditor observes the existing relationship without constructing another native editor.
            control.currentEditor().is_none()
        });
        self.0.borrow_mut().controls_detached = controls_detached;
        let editors_detached = self.editors_are_detached();
        let undo_empty = self.observed_undo_is_empty();
        let verified = if !controls_detached || !editors_detached || !undo_empty {
            // A capture error has priority over this later boundary failure; neither authorizes a close receipt.
            captured.and(Err(
                "Host borrowed field editor is not empty and detached".into()
            ))
        } else {
            // Every public relationship is empty, but a failed capture still remains a concrete failure.
            captured
        };
        if let Err(error) = verified {
            // Release and final observation cannot replace the original error or restore accepted close to Active.
            first_failure.get_or_insert(error);
        }
        match first_failure {
            Some(error) => {
                // The caller remains Closing; the native cohort/barrier must never publish success from failed cleanup.
                Err(error)
            }
            None => {
                // Actual deallocation cohorts and weak-release checks still own the later completion receipt.
                Ok(())
            }
        }
    }

    /// Check cache detachment once after the project cohort completes; borrowed-cache survival alone is not a leak.
    pub(super) fn is_detached(&self) -> bool {
        let admitted = {
            let state = self.0.borrow();
            // Copy scalar state and end its borrow before invoking potentially reentrant native getters.
            state.finished && state.failure.is_none() && state.controls_detached
        };
        admitted && self.editors_are_detached() && self.observed_undo_is_empty()
    }

    /// Check already-observed histories without invoking a responder getter that could allocate a new manager.
    fn observed_undo_is_empty(&self) -> bool {
        let managers = self.0.borrow().observed_undo.clone();
        managers.iter().all(|manager| {
            // Inspect only managers recorded during actual editing; never invoke a potentially creating responder getter.
            manager
                .load()
                .is_none_or(|manager| !manager.canUndo() && !manager.canRedo())
        })
    }

    /// Verify borrowed framework caches carry no old text, composition, callbacks, selection or hierarchy links.
    fn editors_are_detached(&self) -> bool {
        let editors = self.0.borrow().editors.clone();
        editors.iter().all(|editor| {
            let Some(editor) = editor.load() else {
                // A cache already released by AppKit cannot retain the old session's data.
                return true;
            };
            // SAFETY: The dynamically verified main-thread NSTextView implements the public NSTextInputClient getter.
            let marked: bool = unsafe { msg_send![&*editor, hasMarkedText] };
            // SAFETY: A weak load protects the live main-thread view during this synchronous parent-view query.
            let no_superview = unsafe { editor.superview().is_none() };
            BorrowedEditorFacts {
                text_empty: editor.string().length() == 0,
                unmarked: !marked,
                delegate_detached: editor.delegate().is_none(),
                hierarchy_detached: no_superview && editor.window().is_none(),
                selection_empty: editor.selectedRange().length == 0,
            }
            .verify()
        })
    }

    /// Report failure and cache-instance metadata without reading or emitting user text.
    pub(super) fn failure(&self) -> Option<String> {
        self.0.borrow().failure.clone()
    }
}
