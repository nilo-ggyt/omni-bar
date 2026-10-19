//! Customize only the standard field editor's undo owner; AppKit retains text, IME and command behavior.

use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send};
use objc2_app_kit::NSTextView;
use objc2_foundation::{NSObjectProtocol, NSRange, NSRect, NSUndoManager, ns_string};

use super::{BorrowedEditorSnapshot, NativeOwnedLedger, SessionUndo};

/// Native ivars hold only weak Rust session owners, avoiding a responder-to-controller retain cycle.
struct SessionEditorIvars {
    undo: Weak<SessionUndo>,
    ledger: Weak<NativeOwnedLedger>,
}

define_class!(
    /// Preserve NSTextView editing semantics while selecting the session-owned undoManager.
    // SAFETY: Construct and use NSTextView on the main thread; the override preserves undoManager's nullable +0 ABI.
    #[unsafe(super = NSTextView)]
    #[thread_kind = MainThreadOnly]
    #[ivars = SessionEditorIvars]
    struct SessionTextFieldEditor;

    // SAFETY: Preserve NSObject identity/reference counting and initialize Rust ivars before designated initialization.
    unsafe impl NSObjectProtocol for SessionTextFieldEditor {}

    impl SessionTextFieldEditor {
        #[unsafe(method_id(undoManager))]
        fn session_undo_manager(&self) -> Option<Retained<NSUndoManager>> {
            // Ended sessions return nil; never recreate history or obtain shared history through an old responder chain.
            match (self.ivars().undo.upgrade(), self.ivars().ledger.upgrade()) {
                (Some(undo), Some(ledger)) => {
                    // Both Rust owners must remain live before returning this session's explicitly registered history.
                    undo.manager(&ledger)
                }
                _ => {
                    // Losing either owner revokes history access without searching the old responder chain for another manager.
                    None
                }
            }
        }
    }
);

impl SessionTextFieldEditor {
    /// Set weak ivars before public initialization, without constructing a custom TextKit graph or private classes.
    fn new(
        undo: &Rc<SessionUndo>,
        ledger: &Rc<NativeOwnedLedger>,
        mtm: MainThreadMarker,
    ) -> Retained<Self> {
        let allocated = Self::alloc(mtm).set_ivars(SessionEditorIvars {
            undo: Rc::downgrade(undo),
            ledger: Rc::downgrade(ledger),
        });
        // SAFETY: Ivars are initialized; initWithFrame: consumes the +1 allocation to construct a standard NSTextView.
        let editor: Retained<Self> =
            unsafe { msg_send![super(allocated), initWithFrame: NSRect::ZERO] };
        // Field-editor mode preserves Return/Tab submission and keeps native undo/redo enabled during the session.
        editor.setFieldEditor(true);
        editor.setAllowsUndo(true);
        editor.setRichText(false);
        editor.setImportsGraphics(false);
        editor
    }
}

/// The controller owns one lazy editor; the native object receives only weak ledger/history links.
pub(in crate::platform::macos) struct SessionFieldEditorProvider {
    undo: Weak<SessionUndo>,
    ledger: Weak<NativeOwnedLedger>,
    borrowed: BorrowedEditorSnapshot,
    editor: RefCell<Option<Retained<SessionTextFieldEditor>>>,
    creating: Cell<bool>,
    closed: Cell<bool>,
}

impl SessionFieldEditorProvider {
    /// Store activated-session weak owners and observation metadata without eagerly allocating an editor or manager.
    pub(in crate::platform::macos) fn new(
        undo: &Rc<SessionUndo>,
        ledger: &Rc<NativeOwnedLedger>,
        borrowed: &BorrowedEditorSnapshot,
    ) -> Self {
        Self {
            undo: Rc::downgrade(undo),
            ledger: Rc::downgrade(ledger),
            borrowed: borrowed.clone(),
            editor: RefCell::new(None),
            creating: Cell::new(false),
            closed: Cell::new(false),
        }
    }

    /// Serve windowWillReturnFieldEditor:toObject: directly; nil-client cache observation never creates an editor.
    pub(in crate::platform::macos) fn provide(
        &self,
        client: Option<&AnyObject>,
        mtm: MainThreadMarker,
    ) -> Option<Retained<AnyObject>> {
        if client.is_none() || self.closed.get() {
            // Cache queries and closed sessions receive no project editor; late cleanup cannot recreate resources.
            return None;
        }
        let cached = self.editor.borrow().as_ref().cloned();
        if let Some(editor) = cached {
            // Return the same session editor and perform its native retain outside any RefCell borrow.
            return Some(as_object(editor));
        }
        if self.creating.replace(true) {
            // Reentrant initialization cannot allocate another editor; preserve the failure for normal close preflight.
            self.borrowed
                .record_failure("Host session field-editor construction reentered".into());
            return None;
        }
        struct Reset<'a>(&'a Cell<bool>);
        impl Drop for Reset<'_> {
            fn drop(&mut self) {
                // Reset only the stack-local construction flag; Drop performs no user-text or history cleanup.
                self.0.set(false);
            }
        }
        let _reset = Reset(&self.creating);
        let result = self.create(mtm);
        match result {
            Ok(editor) => {
                // Recheck close authority after initialization; do not publish an editor created during reentrant close.
                if self.closed.get() {
                    self.borrowed
                        .record_failure("Host field editor closed during construction".into());
                    return None;
                }
                self.editor.replace(Some(editor.clone()));
                Some(as_object(editor))
            }
            Err(error) => {
                // Objective-C cannot return Result here; preserve the first failure in shared preflight-visible state.
                self.borrowed.record_failure(error);
                None
            }
        }
    }

    /// Register the explicitly constructed view and weak cache facts before any input callback can occur.
    fn create(&self, mtm: MainThreadMarker) -> Result<Retained<SessionTextFieldEditor>, String> {
        let undo = self
            .undo
            .upgrade()
            .ok_or("Host field-editor undo owner expired")?;
        let ledger = self
            .ledger
            .upgrade()
            .ok_or("Host field-editor ledger owner expired")?;
        let editor = SessionTextFieldEditor::new(&undo, &ledger, mtm);
        ledger.include("session.field-editor", &editor)?;
        let standard = editor.clone().into_super();
        self.borrowed.record_editor(&standard)?;
        Ok(editor)
    }

    /// Freeze creation after close admission so teardown-time delegate calls cannot reacquire the editor.
    pub(super) fn freeze(&self) {
        self.closed.set(true);
    }

    /// Clean only the editor created by this provider; never mutate an unknown shared editor or manager.
    pub(super) fn release_accepted(&self) {
        self.freeze();
        let editor = self.editor.take();
        if let Some(editor) = editor {
            // Normal resignation and public endEditing have completed; detach owned data/callbacks before releasing the owner.
            editor.setAllowsUndo(false);
            // SAFETY: nil clears existing weak delegate/nextResponder links without installing unretained references.
            unsafe {
                editor.setDelegate(None);
                editor.setNextResponder(None);
            }
            editor.setString(ns_string!(""));
            editor.setSelectedRange(NSRange::new(0, 0));
            // Framework claims on the explicit editor still require actual weak nil; this editor has no cache exemption.
        }
    }
}

/// Return the protocol's id type by upcasting the same object, preserving its owner and retain convention.
fn as_object(editor: Retained<SessionTextFieldEditor>) -> Retained<AnyObject> {
    // SAFETY: SessionTextFieldEditor derives from NSObject, so upcasting its object to AnyObject is valid.
    unsafe { Retained::cast_unchecked(editor) }
}

/// Require this session's provider identity; matching contents cannot authorize an NSCell bypass or foreign editor.
pub(super) fn is_session_editor(
    editor: &NSTextView,
    undo: &SessionUndo,
    ledger: &NativeOwnedLedger,
) -> bool {
    let Some(editor) = editor.downcast_ref::<SessionTextFieldEditor>() else {
        // Another native editor may function normally but does not prove this session's undo ownership.
        return false;
    };
    editor
        .ivars()
        .undo
        .upgrade()
        .is_some_and(|owner| std::ptr::eq(&*owner, undo))
        && editor
            .ivars()
            .ledger
            .upgrade()
            .is_some_and(|owner| std::ptr::eq(&*owner, ledger))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_and_future_editor_links_do_not_retain_session_owners() {
        // Check Rust ownership only; no AppKit construction or native Undo behavior is claimed by this pure test.
        let undo = Rc::new(SessionUndo::default());
        let ledger = Rc::new(NativeOwnedLedger::default());
        let borrowed = BorrowedEditorSnapshot::default();
        let provider = SessionFieldEditorProvider::new(&undo, &ledger, &borrowed);
        assert_eq!(Rc::strong_count(&undo), 1);
        assert_eq!(Rc::strong_count(&ledger), 1);
        assert!(provider.editor.borrow().is_none());
        drop(undo);
        drop(ledger);
        assert!(provider.undo.upgrade().is_none());
        assert!(provider.ledger.upgrade().is_none());
        // Accepted cleanup of an empty provider freezes scalar state without constructing an editor or history.
        provider.release_accepted();
        assert!(provider.closed.get());
    }

    #[test]
    fn provider_failure_keeps_first_preflight_reason() {
        let borrowed = BorrowedEditorSnapshot::default();
        borrowed.record_failure("first editor ownership failure".into());
        borrowed.record_failure("later delegate failure".into());
        // Later callbacks cannot replace or clear the original ownership failure to authorize close.
        assert_eq!(
            borrowed.failure().as_deref(),
            Some("first editor ownership failure")
        );
    }
}
