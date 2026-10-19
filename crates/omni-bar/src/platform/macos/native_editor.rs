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
    /// Share exclusive history between the window delegate and owned editor override, without assuming default ownership.
    pub(super) fn manager(&self, ledger: &NativeOwnedLedger) -> Option<Retained<NSUndoManager>> {
        self.provide_owner(
            &self.manager,
            || {
                // Reject a non-main-thread call before constructing any native candidate or owner.
                let mtm = MainThreadMarker::new()
                    .ok_or("Session undo requires the AppKit main thread")?;
                Ok(NSUndoManager::new(mtm))
            },
            |manager| {
                // Register a new manager once; runtime registration remains inside the exclusive providing phase.
                ledger.include("session.undo-manager", manager)
            },
            |manager| {
                // objc retain may reenter; keep the sole project owner local and release the state borrow first.
                manager.clone()
            },
            |manager| {
                // Borrow the typed known owner directly rather than discovering other history through a responder chain.
                Self::clear_owned_manager(manager)
            },
        )
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

    /// Run the same provider ordering with pure Rust owners to test reentrant retain/drop boundaries.
    fn provide_owner<T>(
        &self,
        slot: &RefCell<Option<T>>,
        create: impl FnOnce() -> Result<T, String>,
        register: impl FnOnce(&T) -> Result<(), String>,
        retain: impl FnOnce(&T) -> T,
        clear: impl Fn(&T) -> Result<(), String>,
    ) -> Option<T> {
        if self.closed.get() || self.failure.borrow().is_some() {
            // Late requests and recorded failures neither construct a candidate nor lend the cached owner.
            return None;
        }
        if self.providing.replace(true) {
            // Reject only the nested getter; do not poison the outer provider or clear live session history.
            return None;
        }
        struct ProvidingGuard<'a>(&'a Cell<bool>);
        impl Drop for ProvidingGuard<'_> {
            fn drop(&mut self) {
                // Reset a scalar only; Drop sends no native messages and clears no user history.
                self.0.set(false);
            }
        }
        let _providing = ProvidingGuard(&self.providing);
        let cached = slot.take();
        let is_new = cached.is_none();
        let owner = match cached {
            Some(owner) => {
                // End the RefCell borrow after take; native retain, cleanup and drop operate only on the local owner.
                owner
            }
            None => match create() {
                Ok(owner) => {
                    // Native initialization may reenter close; recheck authority before registration and publication.
                    owner
                }
                Err(error) => {
                    // Preserve construction failure even without a candidate; a later delegate request cannot replace its cause.
                    self.record_failure(error);
                    return None;
                }
            },
        };
        if !self.can_publish_owner() {
            // After construction reentry, stop registration/retain and distinguish cached history from an unpublished candidate.
            self.reject_owner(slot, owner, None, is_new, &clear);
            return None;
        }
        if is_new && let Err(error) = register(&owner) {
            // Ledger failure must not publish untracked history or overwrite an earlier construction failure.
            self.record_failure(error);
        }
        if !self.can_publish_owner() {
            // Weak registration can reenter; reject a closed or failed candidate again after registration returns.
            self.reject_owner(slot, owner, None, is_new, &clear);
            return None;
        }
        let returned = retain(&owner);
        if !self.can_publish_owner() {
            // Lend no new claim; before accepted close, restore the cached owner and preserve its history.
            self.reject_owner(slot, owner, Some(returned), is_new, &clear);
            return None;
        }
        // Providing excludes other writers and finish only takes; restore the empty slot and drop returns outside its borrow.
        let previous = slot.replace(Some(owner));
        drop(previous);
        Some(returned)
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
    fn reject_owner<T>(
        &self,
        slot: &RefCell<Option<T>>,
        owner: T,
        returned: Option<T>,
        is_new: bool,
        clear: impl FnOnce(&T) -> Result<(), String>,
    ) {
        if !is_new && !self.closed.get() {
            // A real failure revokes this return only; restore the original owner without accepted-close history clearing.
            let previous = slot.replace(Some(owner));
            drop(previous);
            // Drop the extra return claim after restoring the cached owner; providing still rejects drop-time reentry.
            drop(returned);
            return;
        }
        if let Err(error) = clear(&owner) {
            // New candidates have no live history and closed owners permit cleanup; cleanup failure preserves the first cause.
            self.record_failure(error);
        }
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

/// Separate accepted-cleanup ordering from AppKit objects so injected failures exercise every cleanup stage.
fn complete_accepted_editor_cleanup(
    freeze: impl FnOnce(),
    capture_before: impl FnOnce() -> Result<(), String>,
    end_editing: impl FnOnce(),
    finish_owned_undo: impl FnOnce() -> Result<(), String>,
    release_owned_payload: impl FnOnce(),
    verify_after: impl FnOnce() -> Result<(), String>,
) -> Result<(), String> {
    // Freeze creation first; an initial capture failure cannot leave an accepted owner able to allocate resources.
    freeze();
    let mut first_failure = capture_before().err();
    // Normal resignation and admission succeeded; end editing publicly even after observation failure, without retry.
    end_editing();
    if let Err(error) = finish_owned_undo() {
        // Clean only the known session manager; undo failure cannot skip later owned-payload release.
        first_failure.get_or_insert(error);
    }
    release_owned_payload();
    if let Err(error) = verify_after() {
        // Final verification preserves the first cause; completed cleanup cannot erase failure or authorize a receipt.
        first_failure.get_or_insert(error);
    }
    match first_failure {
        Some(error) => {
            // The caller remains Closing; cleanup alone cannot restore Active or consume the barrier.
            Err(error)
        }
        None => {
            // Successful stages still require the actual cohort and weak-release checks before a final receipt.
            Ok(())
        }
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
        complete_accepted_editor_cleanup(
            || {
                // Freeze immediately after admission so teardown reentry cannot create an editor or history.
                provider.freeze();
                undo.closed.set(true);
            },
            || {
                // Capture remains noncreating; preserve failure while continuing cleanup of known owned resources.
                self.capture(panel)
            },
            || {
                // SAFETY: Public endEditing accepts nil; validation and normal resignation succeeded before admission.
                unsafe { panel.endEditingFor(None) };
            },
            || {
                // Remove and clear only SessionUndo's exclusive slot; never query or mutate an unknown editor's manager.
                undo.finish()
            },
            || {
                for control in controls {
                    // Clear business fields only after admission; undo/capture failure cannot retain active payload.
                    control.setStringValue(objc2_foundation::ns_string!(""));
                }
                // Default caches are borrowed; clear data/callbacks and release only this provider's owned editor.
                provider.release_accepted();
            },
            || {
                // Even failed post-capture must check known weak identities and relationships; empty text alone is insufficient.
                let captured = self.capture(panel);
                let controls_detached = controls.iter().all(|control| {
                    // currentEditor is noncreating; any surviving old editing relationship prevents success.
                    control.currentEditor().is_none()
                });
                self.0.borrow_mut().controls_detached = controls_detached;
                let editors_detached = self.editors_are_detached();
                let undo_empty = self.observed_undo_is_empty();
                if !controls_detached || !editors_detached || !undo_empty {
                    // Preserve capture failure first; otherwise report the uncleared boundary and remain Closing.
                    return captured.and(Err(
                        "Host borrowed field editor is not empty and detached".into(),
                    ));
                }
                captured
            },
        )
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

    fn observed_undo_is_empty(&self) -> bool {
        let managers = self.0.borrow().observed_undo.clone();
        managers.iter().all(|manager| {
            // Inspect only managers recorded during actual editing; never invoke a potentially creating responder getter.
            manager
                .load()
                .is_none_or(|manager| !manager.canUndo() && !manager.canRedo())
        })
    }

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

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};
    use std::rc::{Rc, Weak};

    use super::{BorrowedEditorFacts, SessionUndo, complete_accepted_editor_cleanup};

    /// Fake native claims test provider ordering; retain/drop actively detect an outstanding state-slot borrow.
    struct FakeUndoOwner(Rc<FakeUndoState>);

    struct FakeUndoState {
        slot: Weak<RefCell<Option<FakeUndoOwner>>>,
        live_claims: Cell<usize>,
        cleared: Cell<usize>,
        text: RefCell<String>,
        undo: RefCell<Vec<String>>,
        redo: RefCell<Vec<String>>,
    }

    impl FakeUndoOwner {
        fn new(state: &Rc<FakeUndoState>) -> Self {
            // Simulated construction/retain reenters external code; the real path must release its manager-slot borrow first.
            assert!(state.slot.upgrade().unwrap().try_borrow_mut().is_ok());
            state.live_claims.set(state.live_claims.get() + 1);
            Self(state.clone())
        }

        fn retain(&self) -> Self {
            // Model native retain explicitly instead of hiding its reentry behind a derived Rust Clone.
            Self::new(&self.0)
        }

        fn clear(&self) -> Result<(), String> {
            // Cleanup can reenter too; operate only on this known owner after ending the slot borrow.
            assert!(self.0.slot.upgrade().unwrap().try_borrow_mut().is_ok());
            self.0.cleared.set(self.0.cleared.get() + 1);
            self.0.undo.borrow_mut().clear();
            self.0.redo.borrow_mut().clear();
            Ok(())
        }

        fn undo(&self) {
            // Preserve real synthetic text history; demonstrate restoration rather than checking a has-actions flag alone.
            let previous = self.0.undo.borrow_mut().pop().unwrap();
            let current = self.0.text.replace(previous);
            self.0.redo.borrow_mut().push(current);
        }

        fn redo(&self) {
            // Restore the new text too, proving Busy preserved usable history rather than an empty manager identity.
            let next = self.0.redo.borrow_mut().pop().unwrap();
            let current = self.0.text.replace(next);
            self.0.undo.borrow_mut().push(current);
        }
    }

    impl Drop for FakeUndoOwner {
        fn drop(&mut self) {
            // A clone/drop inside the slot borrow fails directly through the same provider helper.
            assert!(self.0.slot.upgrade().unwrap().try_borrow_mut().is_ok());
            self.0.live_claims.set(self.0.live_claims.get() - 1);
        }
    }

    fn fake_undo_slot() -> (Rc<RefCell<Option<FakeUndoOwner>>>, Rc<FakeUndoState>) {
        let slot = Rc::new(RefCell::new(None));
        let state = Rc::new(FakeUndoState {
            slot: Rc::downgrade(&slot),
            live_claims: Cell::new(0),
            cleared: Cell::new(0),
            text: RefCell::new("draft after input".into()),
            undo: RefCell::new(vec!["draft before input".into()]),
            redo: RefCell::new(Vec::new()),
        });
        (slot, state)
    }

    #[test]
    fn undo_provider_busy_preflight_preserves_active_owner_and_native_claims() {
        let undo = SessionUndo::default();
        let (slot, state) = fake_undo_slot();
        let created = Cell::new(0);
        let registered = Cell::new(0);
        let returned = undo
            .provide_owner(
                &slot,
                || {
                    created.set(created.get() + 1);
                    Ok(FakeUndoOwner::new(&state))
                },
                |_| {
                    // Ledger registration shares the providing phase; a temporarily empty owner slot cannot authorize close.
                    registered.set(registered.get() + 1);
                    assert!(undo.prepare_close().is_err());
                    Ok(())
                },
                |owner| {
                    // Busy rejects this close only; it neither freezes the session, clears history nor records a lasting failure.
                    assert!(undo.prepare_close().is_err());
                    assert!(!undo.closed.get());
                    assert!(undo.failure.borrow().is_none());
                    assert!(slot.borrow().is_none());
                    owner.retain()
                },
                FakeUndoOwner::clear,
            )
            .unwrap();
        assert!(undo.prepare_close().is_ok());
        assert_eq!(created.get(), 1);
        assert_eq!(registered.get(), 1);
        assert_eq!(state.live_claims.get(), 2);
        assert_eq!(*state.undo.borrow(), ["draft before input"]);
        let second = undo
            .provide_owner(
                &slot,
                || {
                    // Reuse the cached owner instead of rebuilding live history on the second query.
                    panic!("cached manager must not be recreated")
                },
                |_| {
                    // The live owner is already registered; do not create a second identity or repeat construction.
                    panic!("cached manager must not be registered again")
                },
                FakeUndoOwner::retain,
                FakeUndoOwner::clear,
            )
            .unwrap();
        assert!(Rc::ptr_eq(&returned.0, &second.0));
        assert_eq!(state.live_claims.get(), 3);
        drop(returned);
        drop(second);
        let owner = slot.take();
        drop(owner);
        assert_eq!(state.live_claims.get(), 0);
        assert_eq!(state.cleared.get(), 0);
    }

    #[test]
    fn undo_nested_native_getter_keeps_same_cached_owner_and_working_history() {
        let undo = SessionUndo::default();
        let (slot, state) = fake_undo_slot();
        slot.replace(Some(FakeUndoOwner::new(&state)));
        let nested_creations = Cell::new(0);
        let returned = undo.provide_owner(
            &slot,
            || {
                // Enter through retain of an existing manager; first-time construction must not run.
                panic!("cached manager must not be recreated")
            },
            |_| {
                // The cached identity is registered; isolate native-retain reentry in this counterexample.
                panic!("cached manager must not be registered again")
            },
            |owner| {
                assert!(slot.borrow().is_none());
                let nested = undo.provide_owner(
                    &slot,
                    || {
                        // Without the guard, the temporary empty slot would allocate another owner and change this construction count.
                        nested_creations.set(nested_creations.get() + 1);
                        Ok(FakeUndoOwner::new(&state))
                    },
                    |_| Ok(()),
                    FakeUndoOwner::retain,
                    FakeUndoOwner::clear,
                );
                assert!(nested.is_none());
                owner.retain()
            },
            FakeUndoOwner::clear,
        );
        let returned = returned.unwrap();
        let owner = slot.take().unwrap();
        assert!(Rc::ptr_eq(&returned.0, &owner.0));
        assert!(!undo.providing.get());
        assert_eq!(nested_creations.get(), 0);
        assert_eq!(state.live_claims.get(), 2);
        assert_eq!(state.cleared.get(), 0);
        assert!(undo.failure.borrow().is_none());
        assert!(undo.prepare_close().is_ok());
        assert_eq!(*state.text.borrow(), "draft after input");
        owner.undo();
        assert_eq!(*state.text.borrow(), "draft before input");
        owner.redo();
        assert_eq!(*state.text.borrow(), "draft after input");
        drop(returned);
        drop(owner);
        assert_eq!(state.live_claims.get(), 0);
    }

    #[test]
    fn undo_close_during_each_native_stage_discards_without_publication() {
        for (cached, close_at) in [
            (false, "create"),
            (false, "register"),
            (false, "retain"),
            (true, "retain"),
        ] {
            // Recheck all new-candidate phases and cached retain; only closed state permits clearing existing history.
            let undo = SessionUndo::default();
            let (slot, state) = fake_undo_slot();
            if cached {
                // Cached mode starts with live history; do not confuse close cleanup with failed first allocation.
                slot.replace(Some(FakeUndoOwner::new(&state)));
            }
            let returned = undo.provide_owner(
                &slot,
                || {
                    let owner = FakeUndoOwner::new(&state);
                    if close_at == "create" {
                        // Direct close bypassing preflight must fail and seal state without claiming stack-local owners were released.
                        assert!(undo.finish().is_err());
                    }
                    Ok(owner)
                },
                |_| {
                    if close_at == "register" {
                        // Close during registration must prevent both candidate publication and its return to AppKit.
                        assert!(undo.finish().is_err());
                    }
                    Ok(())
                },
                |owner| {
                    let returned = owner.retain();
                    if close_at == "retain" {
                        // Release both outstanding claims, not merely the owner held by the state slot.
                        assert!(undo.finish().is_err());
                    }
                    returned
                },
                FakeUndoOwner::clear,
            );
            assert!(returned.is_none(), "stage={close_at}");
            assert!(slot.borrow().is_none());
            assert!(!undo.providing.get());
            assert!(undo.closed.get());
            assert_eq!(state.live_claims.get(), 0);
            assert_eq!(state.cleared.get(), 1);
            assert!(state.undo.borrow().is_empty());
            assert!(state.redo.borrow().is_empty());
            assert_eq!(
                undo.prepare_close().unwrap_err(),
                "Session undo closed while manager was being provided"
            );
        }
    }

    #[test]
    fn undo_registration_failure_cleans_candidate_and_keeps_first_reason() {
        let undo = SessionUndo::default();
        let (slot, state) = fake_undo_slot();
        let returned = undo.provide_owner(
            &slot,
            || Ok(FakeUndoOwner::new(&state)),
            |_| Err("ledger failed first".into()),
            |_| {
                // Registration failure must prevent creation of the return claim.
                panic!("failed manager must not be retained for publication")
            },
            |owner| {
                owner.clear()?;
                Err("clear failed later".into())
            },
        );
        assert!(returned.is_none());
        assert!(slot.borrow().is_none());
        assert_eq!(state.live_claims.get(), 0);
        assert_eq!(state.cleared.get(), 1);
        assert_eq!(undo.prepare_close().unwrap_err(), "ledger failed first");
    }

    #[test]
    fn undo_real_failure_before_admission_restores_cached_owner_without_clearing_history() {
        let undo = SessionUndo::default();
        let (slot, state) = fake_undo_slot();
        slot.replace(Some(FakeUndoOwner::new(&state)));
        let returned = undo.provide_owner(
            &slot,
            || {
                // A real ownership failure on the cached owner cannot trigger replacement allocation.
                panic!("cached manager must not be recreated")
            },
            |_| {
                // Rejecting the return claim must not register the cached identity a second time.
                panic!("cached manager must not be registered again")
            },
            |owner| {
                let returned = owner.retain();
                // Keep real ownership failure distinct from Busy; later close sees the first cause without cleanup admission.
                undo.record_failure("real ownership failure first".into());
                undo.record_failure("later observation failure".into());
                returned
            },
            |_| {
                // A live cached owner has no accepted-cleanup authority; reaching this clear would destroy user history.
                panic!("active cached history must not be cleared")
            },
        );
        assert!(returned.is_none());
        assert!(!undo.closed.get());
        assert!(!undo.providing.get());
        assert_eq!(state.live_claims.get(), 1);
        assert_eq!(state.cleared.get(), 0);
        assert_eq!(
            undo.prepare_close().unwrap_err(),
            "real ownership failure first"
        );
        let owner = slot.take().unwrap();
        assert!(Rc::ptr_eq(&owner.0, &state));
        owner.undo();
        assert_eq!(*state.text.borrow(), "draft before input");
        owner.redo();
        assert_eq!(*state.text.borrow(), "draft after input");
        drop(owner);
        assert_eq!(state.live_claims.get(), 0);
    }

    /// Pure Rust guards model exclusive resources; their Drop count does not prove AppKit deallocation.
    struct OwnedResource<'a>(&'a Cell<usize>);

    impl Drop for OwnedResource<'_> {
        fn drop(&mut self) {
            // Each fake resource contributes one owner so failures expose any skipped release path.
            self.0.set(self.0.get() - 1);
        }
    }

    #[test]
    fn accepted_failures_freeze_first_release_known_payload_and_keep_first_reason() {
        for fail_capture in [true, false] {
            // Inject capture and undo failures separately; later failures must neither abort cleanup nor replace the cause.
            let frozen = Cell::new(false);
            let calls = RefCell::new(Vec::new());
            let live_resources = Cell::new(2);
            let own_undo = RefCell::new(Some(OwnedResource(&live_resources)));
            let own_editor = RefCell::new(Some(OwnedResource(&live_resources)));
            let control_payload = RefCell::new(Some(vec![7_u8; 512]));
            let result = complete_accepted_editor_cleanup(
                || {
                    // Subsequent simulated native reentry must already observe frozen creation authority.
                    frozen.set(true);
                    calls.borrow_mut().push("freeze");
                },
                || {
                    assert!(frozen.get());
                    calls.borrow_mut().push("capture");
                    if fail_capture {
                        // This capture failure previously returned before freeze/release; exercise that actual control-flow boundary.
                        Err("capture failed".to_owned())
                    } else {
                        // Successful initial observation must not hide a later exclusive-undo cleanup failure.
                        Ok(())
                    }
                },
                || {
                    // Accepted close still calls public endEditing exactly once.
                    assert!(frozen.get());
                    calls.borrow_mut().push("end-editing");
                },
                || {
                    // Clean the known exclusive slot only; even on failure, the manager's project owner must end.
                    calls.borrow_mut().push("undo");
                    own_undo.borrow_mut().take();
                    Err("undo failed".to_owned())
                },
                || {
                    // Both earlier failures must still release the editor owner and control payload, not merely set a flag.
                    calls.borrow_mut().push("release-payload");
                    own_editor.borrow_mut().take();
                    control_payload.borrow_mut().take();
                },
                || {
                    // Earlier errors must not skip final facts; resource guards have actually dropped by this verification.
                    calls.borrow_mut().push("verify");
                    assert_eq!(live_resources.get(), 0);
                    assert!(control_payload.borrow().is_none());
                    Err("verification failed".to_owned())
                },
            );
            let expected = if fail_capture {
                // Initial capture failure takes precedence over later undo and final-verification failures.
                "capture failed"
            } else {
                // When capture succeeds, preserve undo failure rather than replacing it with later verification failure.
                "undo failed"
            };
            assert_eq!(result.unwrap_err(), expected);
            assert_eq!(
                *calls.borrow(),
                [
                    "freeze",
                    "capture",
                    "end-editing",
                    "undo",
                    "release-payload",
                    "verify"
                ]
            );
        }
    }

    #[test]
    fn completed_cleanup_does_not_override_failed_final_boundary() {
        let released = Cell::new(false);
        let result = complete_accepted_editor_cleanup(
            || {
                // This pure test verifies failure authority without constructing an AppKit editor or manager.
            },
            || {
                // Initial capture succeeds to isolate the final boundary-verification failure.
                Ok(())
            },
            || {
                // This native-free fixture does not perform real endEditing.
            },
            || {
                // Exclusive undo cleanup succeeds, so the final failure cannot be attributed to it.
                Ok(())
            },
            || {
                // Record owned-payload release explicitly for the final verification stage.
                released.set(true);
            },
            || {
                // Owned-resource release cannot turn a failed cache boundary into a successful receipt.
                assert!(released.get());
                Err("boundary still active".into())
            },
        );
        assert_eq!(result.unwrap_err(), "boundary still active");
    }

    #[test]
    /// Residual text, composition, delegate, hierarchy or selection invalidates the public cache-detachment proof.
    fn every_borrowed_editor_data_or_authority_edge_must_be_empty() {
        let empty = BorrowedEditorFacts {
            text_empty: true,
            unmarked: true,
            delegate_detached: true,
            hierarchy_detached: true,
            selection_empty: true,
        };
        assert!(empty.verify());
        for facts in [
            BorrowedEditorFacts {
                text_empty: false,
                ..empty
            },
            BorrowedEditorFacts {
                unmarked: false,
                ..empty
            },
            BorrowedEditorFacts {
                delegate_detached: false,
                ..empty
            },
            BorrowedEditorFacts {
                hierarchy_detached: false,
                ..empty
            },
            BorrowedEditorFacts {
                selection_empty: false,
                ..empty
            },
        ] {
            // An otherwise empty shell cannot hide any remaining business state or callback capability.
            assert!(!facts.verify());
        }
    }
}
