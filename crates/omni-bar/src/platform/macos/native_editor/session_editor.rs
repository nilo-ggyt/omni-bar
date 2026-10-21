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
        /// Lend only the still-live session's registered history to AppKit's focused responder.
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
    providing: Cell<bool>,
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
            providing: Cell::new(false),
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
        if self.providing.replace(true) {
            // A nested getter sees no editor while the sole owner is temporarily local. It may neither
            // construct a replacement nor turn a busy loan into a permanent ownership failure.
            return None;
        }
        struct Reset<'a>(&'a Cell<bool>);
        impl Drop for Reset<'_> {
            /// End the loan on every exit; native cleanup belongs to the concrete local editor owner.
            fn drop(&mut self) {
                self.0.set(false);
            }
        }
        let _reset = Reset(&self.providing);
        // Move the actual owner out of the slot before any Objective-C retain or initialization.
        // Accepted close can take the empty slot without colliding with a live RefCell borrow.
        let editor = match self.editor.take() {
            Some(editor) => editor,
            None => match self.create(mtm) {
                Ok(editor) => editor,
                Err(error) => {
                    // Objective-C cannot return Result; retain the first reason for normal close preflight.
                    self.borrowed.record_failure(error);
                    return None;
                }
            },
        };
        if self.closed.get() {
            // Close may have run during construction while the slot was empty. The local owner must
            // clear its own data and callbacks; it cannot rely on release_accepted having found it.
            Self::release_editor(editor);
            return None;
        }
        let returned = editor.clone();
        if self.closed.get() {
            // Native retain may reenter close too. Clear the same editor and release both claims
            // without republishing it or returning a now-revoked editor to AppKit.
            Self::release_editor(editor);
            drop(returned);
            return None;
        }
        let previous = self.editor.replace(Some(editor));
        // The providing guard excludes another writer; even an unexpected previous owner drops only
        // after the slot borrow ends. The final cast transfers the already-retained claim unchanged.
        drop(previous);
        Some(as_object(returned))
    }

    /// Refuse close before admission while a local editor loan has not returned to its slot.
    pub(in crate::platform::macos) fn prepare_close(&self) -> Result<(), String> {
        if self.providing.get() {
            // Busy preserves active text and history. It neither authorizes accepted cleanup nor
            // poisons the original session with a permanent ownership failure.
            return Err("Host session field editor is being provided".into());
        }
        if self.closed.get() {
            // A later preflight cannot reactivate an editor after its close authority was consumed.
            return Err("Host session field editor is already closed".into());
        }
        Ok(())
    }

    /// Register the explicitly constructed view and weak cache facts before any input callback can occur.
    fn create(&self, mtm: MainThreadMarker) -> Result<Retained<SessionTextFieldEditor>, String> {
        // Upgrading both weak links establishes this construction's owners without extending their TLS borrows.
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
        // Register the same upcast native identity before handing it to AppKit; failure drops the local editor.
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
            Self::release_editor(editor);
        }
    }

    /// Clear only this provider's actual editor after close revoked publication, including a local loan.
    fn release_editor(editor: Retained<SessionTextFieldEditor>) {
        // Normal resignation and public endEditing own the accepted-close boundary. No slot borrow
        // survives these native calls, and closed prevents callbacks from recreating an editor.
        editor.setAllowsUndo(false);
        // SAFETY: nil clears weak delegate/nextResponder links without installing unretained references.
        unsafe {
            editor.setDelegate(None);
            editor.setNextResponder(None);
        }
        editor.setString(ns_string!(""));
        editor.setSelectedRange(NSRange::new(0, 0));
        // Framework claims on this explicit editor still require actual weak nil; it has no cache exemption.
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
    // Both weak identities must name the original pair; a same-type editor from another session is foreign.
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
