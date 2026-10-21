//! Launcher native controls, responder routing and explicit graph teardown.

use std::cell::{Cell, OnceCell, RefCell};
use std::rc::Rc;

use dispatch2::DispatchQueue;
use objc2::rc::{Retained, autoreleasepool};
use objc2::runtime::{AnyObject, ProtocolObject};
use objc2::{DefinedClass, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{
    NSApplication, NSBackingStoreType, NSBox, NSBoxType, NSButton, NSColor,
    NSControlTextEditingDelegate, NSEvent, NSEventType, NSFloatingWindowLevel, NSFocusRingType,
    NSFont, NSPanel, NSTextAlignment, NSTextField, NSTextFieldDelegate, NSTextView,
    NSTitlePosition, NSWindow, NSWindowCollectionBehavior, NSWindowDelegate, NSWindowStyleMask,
};
use objc2_foundation::{
    MainThreadMarker, NSNotification, NSObject, NSObjectProtocol, NSPoint, NSRect, NSSize,
    NSString, NSUndoManager, ns_string,
};

use super::dismissal_shortcut::is_dismissal_event;
use super::launcher_results::{
    LauncherSelectionDirection, NativeLauncherResults, NativeLauncherSelection,
    is_launcher_activation_key, launcher_selection_navigation_direction,
    move_launcher_selection_index, resolve_launcher_results,
};
use super::native_editor::{BorrowedEditorSnapshot, SessionFieldEditorProvider, SessionUndo};
use super::native_payload::NativeOwnedLedger;
use super::{
    LAUNCHER_HEIGHT, LAUNCHER_WIDTH, MAX_VISIBLE_LAUNCHER_RESULTS, activate_active_launcher_query,
    finalize_launcher_close, move_active_launcher_selection, request_active_launcher_dismissal,
    shortcuts,
};
use crate::LauncherCatalog;

define_class!(
    /// A foreground-only panel routes actual key events without intercepting input-method cancellation.
    // SAFETY: NSPanel has no additional subclassing invariants for these key-window
    // capability overrides, and this class owns no Rust ivars.
    #[unsafe(super = NSPanel)]
    #[thread_kind = MainThreadOnly]
    pub(super) struct NativeLauncherPanel;

    impl NativeLauncherPanel {
        #[unsafe(method(canBecomeKeyWindow))]
        fn can_become_key_window(&self) -> bool {
            // A nonactivating panel still needs keyboard focus for its search field.
            true
        }

        #[unsafe(method(canBecomeMainWindow))]
        fn can_become_main_window(&self) -> bool {
            // Floating Launcher must not replace another application's main document window.
            false
        }

        #[unsafe(method(sendEvent:))]
        fn send_event(&self, event: &NSEvent) {
            if event.r#type() == NSEventType::KeyDown && is_current_panel(self) {
                // Snapshot immutable event/composition state once before choosing a Launcher command.
                let key_code = event.keyCode();
                let modifiers = event.modifierFlags();
                let has_marked_text = self.field_editor_has_marked_text();
                if is_dismissal_event(event, has_marked_text) {
                    // Escape preserves input-method ownership; logical Command-W uses the same fallible close path.
                    request_active_launcher_dismissal();
                    return;
                }
                if let Some(direction) = launcher_selection_navigation_direction(
                    key_code,
                    modifiers,
                    has_marked_text,
                ) {
                    // Unmodified arrows outside composition navigate results instead of text lines.
                    move_active_launcher_selection(direction);
                    return;
                }
                if is_launcher_activation_key(
                    key_code,
                    modifiers,
                    has_marked_text,
                ) {
                    // Plain Return commits the current result only after IME composition ends.
                    activate_active_launcher_query();
                    return;
                }
            }

            // SAFETY: the unconsumed event is a live AppKit borrow, and this main-thread
            // NSPanel subclass forwards it exactly once using the public superclass signature.
            unsafe {
                let _: () = msg_send![super(self), sendEvent: event];
            }
        }
    }
);

impl NativeLauncherPanel {
    /// Construct a main-thread panel with manual release ownership in the enclosing surface.
    pub(super) fn new(mtm: MainThreadMarker) -> Retained<Self> {
        // Establish the empty subclass payload before entering the superclass designated initializer.
        let panel = Self::alloc(mtm).set_ivars(());
        // SAFETY: the allocated subclass has initialized unit ivars; this public initializer
        // consumes its +1 allocation with the declared NSPanel parameter and return types.
        unsafe {
            msg_send![
                super(panel),
                initWithContentRect: NSRect::new(
                    NSPoint::new(0.0, 0.0),
                    NSSize::new(LAUNCHER_WIDTH, LAUNCHER_HEIGHT),
                ),
                styleMask: NSWindowStyleMask::Borderless | NSWindowStyleMask::NonactivatingPanel,
                backing: NSBackingStoreType::Buffered,
                defer: false
            ]
        }
    }

    /// Ask the live shared field editor, without retaining it across responder transitions.
    fn field_editor_has_marked_text(&self) -> bool {
        // Only a dynamically validated live field editor can own native text-input composition.
        self.firstResponder()
            .and_then(|responder| responder.downcast::<NSTextView>().ok())
            .is_some_and(|text_view| {
                // SAFETY: the dynamically checked NSTextView implements NSTextInputClient;
                // its retained local borrow is live on the main thread for this Boolean message.
                unsafe { msg_send![&*text_view, hasMarkedText] }
            })
    }
}

/// One foreground controller owns its catalog snapshot, controls and pending close action.
pub(super) struct LauncherControllerIvars {
    /// Construction capture owns weak metadata only; default-editor detachment facts are shared separately.
    owned: Rc<NativeOwnedLedger>,
    borrowed: BorrowedEditorSnapshot,
    undo: Rc<SessionUndo>,
    field_editor: SessionFieldEditorProvider,
    catalog: LauncherCatalog,
    result_views: OnceCell<LauncherResultViews>,
    dismiss_button: OnceCell<Retained<NSButton>>,
    results: RefCell<NativeLauncherResults>,
    selected_index: Cell<usize>,
    /// Content/selection projections advance a revision; synchronous reentry revokes the old setter sequence.
    projection_revision: Cell<u64>,
    /// Reentry during partial content/appearance updates requires the new projection to repair every row.
    results_projection_complete: Cell<bool>,
    pending_action: RefCell<Option<NativeLauncherSelection>>,
    close_finalizer_scheduled: Cell<bool>,
}

define_class!(
    /// The unique session's retained delegate owns bounded results and weak native callback targets.
    // SAFETY: Controller methods and retained AppKit controls are only used on the main thread.
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    #[ivars = LauncherControllerIvars]
    pub(super) struct LauncherController;

    // SAFETY: this initialized NSObject subclass inherits the protocol's standard identity methods.
    unsafe impl NSObjectProtocol for LauncherController {}

    // SAFETY: declared selectors match AppKit's delegate ABI; the surface retains this
    // main-thread controller until editing callbacks are detached before close.
    unsafe impl NSControlTextEditingDelegate for LauncherController {
        #[unsafe(method(controlTextDidBeginEditing:))]
        fn control_text_did_begin_editing(&self, notification: &NSNotification) {
            if !is_current_controller(self) {
                // A closed or stale controller cannot reacquire session-history authority.
                return;
            }
            if let Some(field) = notification.object().and_then(|object| object.downcast::<NSTextField>().ok())
                && is_current_query_field(self, &field)
            {
                // Actual editing-begin records the editor's manager instead of creating an observation object during close.
                if let Err(error) = self.ivars().borrowed.observe_editing(&field, &self.ivars().undo, &self.ivars().owned) {
                    eprintln!("Launcher undo ownership observation failed: {error}");
                }
                #[cfg(feature = "native-product-acceptance")]
                {
                    // Acquiring the field editor authorizes a later observation only, not window close on the native callback stack.
                    super::acceptance_product::launcher_did_begin_editing(notification);
                }
            }
        }

        #[unsafe(method(controlTextDidChange:))]
        fn control_text_did_change(&self, notification: &NSNotification) {
            if !is_current_controller(self) {
                // A closing or expired delegate must not perform application scans or native result work.
                return;
            }
            let Some(field) = notification
                .object()
                .and_then(|object| object.downcast::<NSTextField>().ok())
            else {
                // Unrelated notifications cannot become query input or access a guessed control type.
                return;
            };
            if !is_current_query_field(self, &field) {
                // A foreign field notification cannot inject a query into this foreground search surface.
                return;
            }
            // Recheck history identity on actual edits, outside result-model and global-owner borrows.
            if let Err(error) = self.ivars().borrowed.observe_editing(&field, &self.ivars().undo, &self.ivars().owned) {
                eprintln!("Launcher undo ownership observation failed: {error}");
            }
            autoreleasepool(|pool| {
                // Borrow the current text only for this synchronous search and pool lifetime.
                let value = field.stringValue();
                if value.length() > 256 {
                    // A 128-scalar query needs at most 256 UTF-16 units. Reject larger native values
                    // before Foundation creates a second unbounded UTF-8 representation for matching.
                    self.install_results(NativeLauncherResults::empty());
                    return;
                }
                // SAFETY: NSString's UTF-8 view is consumed before its local owner and pool end;
                // no result stores a reference to the field editor's backing bytes.
                self.update_selection_for_query(unsafe { value.to_str(pool) });
            });
        }


    }

    // SAFETY: inherited text-editing selectors already use their declared protocol signatures.
    unsafe impl NSTextFieldDelegate for LauncherController {}

    // SAFETY: the panel's weak delegate remains strongly owned by the active/closing surface;
    // callbacks execute on the main thread and do not outlive that owner.
    unsafe impl NSWindowDelegate for LauncherController {
        #[cfg(feature = "native-product-acceptance")]
        #[unsafe(method(windowDidBecomeKey:))]
        fn window_did_become_key(&self, notification: &NSNotification) {
            if is_current_controller(self) {
                // Match controller and notification panel against original weak identities; stale delegates cannot wake a successor.
                super::acceptance_product::launcher_did_become_key(notification);
            }
        }

        #[unsafe(method_id(windowWillReturnFieldEditor:toObject:))]
        fn field_editor(&self, window: &NSWindow, client: Option<&AnyObject>) -> Option<Retained<AnyObject>> {
            if !owns_field_editor_window(self, window) {
                // Require this Active/Closing owner's window; normal preflight resignation must not switch to an unknown editor.
                None
            } else {
                // objc2 method_id wraps the final expression to preserve the required +0 return ABI.
                self.ivars().field_editor.provide(client, self.mtm())
            }
        }

        #[unsafe(method_id(windowWillReturnUndoManager:))]
        fn undo_manager(&self, _window: &NSWindow) -> Option<Retained<NSUndoManager>> {
            // Create the session manager only for actual undo requests; close permanently revokes new history authority.
            self.ivars().undo.manager(&self.ivars().owned)
        }

        #[unsafe(method(windowShouldClose:))]
        fn window_should_close(&self, _window: &NSWindow) -> bool {
            // Standard native close actions must enter the host's ownership transition first.
            // Programmatic close after that claim bypasses this advisory delegate selector.
            self.enqueue_owned_close();
            false
        }

        #[unsafe(method(windowWillClose:))]
        fn window_will_close(&self, _notification: &NSNotification) {
            if self.ivars().close_finalizer_scheduled.replace(true) {
                // Duplicate will-close notifications cannot schedule a second ownership release.
                return;
            }
            // Finalize after the native close stack returns, while the closing owner retains
            // the weak delegate and content graph needed by the current AppKit callback.
            DispatchQueue::main().exec_async(finalize_launcher_close);
        }
    }

    impl LauncherController {
        #[unsafe(method(activateSelection:))]
        fn activate_selection(&self, sender: &AnyObject) {
            let Some(button) = sender.downcast_ref::<NSButton>() else {
                // Ignore a foreign action sender instead of reading an unrelated object's tag.
                return;
            };
            let Ok(index) = usize::try_from(button.tag()) else {
                // Negative tags cannot identify an initialized result slot.
                return;
            };
            self.enqueue_action_at(index);
        }

        #[unsafe(method(dismissLauncher:))]
        fn dismiss_launcher(&self, _sender: &AnyObject) {
            // Mouse dismissal and Escape use the same queue and foreground transition owner.
            self.enqueue_launcher_dismissal();
        }
    }
);

impl LauncherController {
    /// Check Rust history state without native retain, rejecting in-flight lending/construction before logical close.
    pub(super) fn prepare_history_for_close(&self) -> Result<(), String> {
        self.ivars().undo.prepare_close()
    }

    /// Initialize state before registering this object as any native callback target.
    pub(super) fn new(
        catalog: LauncherCatalog,
        initial_results: NativeLauncherResults,
        mtm: MainThreadMarker,
    ) -> Result<Retained<Self>, String> {
        // The editor weakly references these Rust owners; the controller is the active session's sole strong aggregate owner.
        let owned = Rc::new(NativeOwnedLedger::default());
        let undo = Rc::new(SessionUndo::default());
        let borrowed = BorrowedEditorSnapshot::default();
        let field_editor = SessionFieldEditorProvider::new(&undo, &owned, &borrowed);
        let controller = Self::alloc(mtm).set_ivars(LauncherControllerIvars {
            owned,
            borrowed,
            undo,
            field_editor,
            catalog,
            result_views: OnceCell::new(),
            dismiss_button: OnceCell::new(),
            results: RefCell::new(initial_results),
            selected_index: Cell::new(0),
            projection_revision: Cell::new(0),
            results_projection_complete: Cell::new(false),
            pending_action: RefCell::new(None),
            close_finalizer_scheduled: Cell::new(false),
        });
        // SAFETY: all Rust ivars are initialized and the public NSObject initializer
        // consumes the subclass allocation, returning an owned reference of the same class.
        let controller: Retained<Self> = unsafe { msg_send![super(controller), init] };
        controller
            .ivars()
            .owned
            .include("launcher.controller", &controller)?;
        Ok(controller)
    }

    /// Transfer the complete control graph once; replacement would leave native action links stale.
    fn install_result_views(&self, result_views: LauncherResultViews) {
        assert!(
            self.ivars().result_views.set(result_views).is_ok(),
            "native launcher result views must be installed exactly once"
        );
        // Construction installs all initial content; later stable selection changes update two row appearances only.
        self.ivars().results_projection_complete.set(true);
    }

    /// Retain the button whose target property does not itself own this controller.
    fn install_dismiss_button(&self, dismiss_button: Retained<NSButton>) {
        assert!(
            self.ivars().dismiss_button.set(dismiss_button).is_ok(),
            "native launcher dismiss button must be installed exactly once"
        );
    }

    /// Replace the bounded result snapshot before projecting it onto native controls.
    fn update_selection_for_query(&self, query: &str) {
        // Only the currently published foreground graph may refresh native controls or scan applications.
        if !is_current_controller(self) {
            // A callback that survived detachment must not recreate result ownership after close.
            return;
        }
        let results = resolve_launcher_results(&self.ivars().catalog, query);
        self.install_results(results);
    }

    /// Commit the bounded result model before installing content; synchronous reentry immediately revokes old projections.
    fn install_results(&self, results: NativeLauncherResults) {
        if *self.ivars().results.borrow() == results {
            // Identical results preserve selection without rebridging labels or requesting native layout.
            return;
        }
        // Stack snapshot and committed model each own their paths; end model borrows before AppKit calls.
        *self.ivars().results.borrow_mut() = results.clone();
        self.ivars().selected_index.set(0);
        self.ivars().results_projection_complete.set(false);
        let revision = self.begin_projection();
        let is_current = || self.projection_is_current(revision);
        let projection = LauncherProjection::new(&is_current);
        if let Some(result_views) = self.ivars().result_views.get() {
            // Before row construction, commit only the model; installed rows receive content/appearance under one revision.
            if result_views.install(&results, 0, &projection).is_some() {
                // Enable the two-row selection path only after an uninterrupted complete installation.
                self.ivars().results_projection_complete.set(true);
            }
        }
    }

    /// Stable arrow navigation moves a scalar index and updates old/new rows without copying results or labels.
    pub(super) fn move_selection(&self, direction: LauncherSelectionDirection) {
        if !is_current_controller(self) {
            // An old controller retained by AppKit cannot mutate a closed or replacement foreground graph.
            return;
        }
        // Borrow Rust state for its length only; selection changes do not inspect strings, paths or descriptors.
        let result_count = self.ivars().results.borrow().len;
        let previous_index = self.ivars().selected_index.get();
        let next_index = move_launcher_selection_index(previous_index, result_count, direction);
        if previous_index == next_index {
            // Empty and single-row lists keep their appearance without any native setter.
            return;
        }
        // Publish the new index first; reentrant events see current selection and revoke the old projection by revision.
        self.ivars().selected_index.set(next_index);
        self.project_selection_change(previous_index, next_index);
    }

    /// Update two rows for a stable graph; reentry during partial updates makes the new revision repair all rows.
    fn project_selection_change(&self, previous_index: usize, next_index: usize) {
        // Mark the transition before clearing old highlight; reentry may otherwise leave a third row with stale colors.
        let previous_projection_complete = self.ivars().results_projection_complete.replace(false);
        let revision = self.begin_projection();
        let is_current = || self.projection_is_current(revision);
        let projection = LauncherProjection::new(&is_current);
        if let Some(result_views) = self.ivars().result_views.get() {
            // Project only when fixed rows are installed; partial construction cannot call missing native rows.
            if previous_projection_complete {
                // Normal arrows/clicks read no result contents; remove old highlight and apply the new one only.
                if result_views
                    .update_selection(previous_index, next_index, &projection)
                    .is_some()
                {
                    // A complete two-row update restores stability; interruption cannot overwrite a deeper callback's completion.
                    self.ivars().results_projection_complete.set(true);
                }
            } else {
                // Reentry aborts the old content/color projection; the new revision must repair the entire row set.
                // Only this repair path copies the bounded snapshot, ending model borrows before every native setter.
                let results = self.ivars().results.borrow().clone();
                if result_views
                    .install(&results, next_index, &projection)
                    .is_some()
                {
                    // Do not reset the flag on failure; deeper reentry may already have completed the latest installation.
                    self.ivars().results_projection_complete.set(true);
                }
            }
        }
    }

    /// Reserve a session-local projection revision without caching, native retain or background work.
    fn begin_projection(&self) -> u64 {
        // Revisions distinguish synchronous UI operations still on the stack; wrapping extends no resource lifetime.
        let revision = self.ivars().projection_revision.get().wrapping_add(1);
        self.ivars().projection_revision.set(revision);
        revision
    }

    /// Check foreground identity and projection revision without native messages or accepting superseded work.
    fn projection_is_current(&self, revision: u64) -> bool {
        self.ivars().projection_revision.get() == revision && is_current_controller(self)
    }

    /// Use the same immutable result handoff for keyboard activation as for a row click.
    pub(super) fn enqueue_selected_action(&self) {
        // The shared row path copies the selected descriptor before accepting a deferred successor.
        self.enqueue_action_at(self.ivars().selected_index.get());
    }

    /// Accept one pending successor before asynchronously requesting Launcher close.
    fn enqueue_action_at(&self, index: usize) {
        if !is_current_controller(self) {
            // A delayed native action from the closing graph cannot choose a successor's result.
            return;
        }
        let Some(selection) = self.ivars().results.borrow().get(index).cloned() else {
            // A stale mouse tag or an empty placeholder cannot dispatch an action.
            return;
        };
        if self.ivars().pending_action.borrow().is_some() {
            // Repeated Return/click events coalesce around the first accepted successor.
            return;
        }
        // Accept the sole successor first; selection affects appearance without copying the full result model for clicks.
        *self.ivars().pending_action.borrow_mut() = Some(selection);
        let previous_index = self.ivars().selected_index.replace(index);
        // Stable same-row selection needs no setter; the shared path still repairs content installation interrupted by reentry.
        self.project_selection_change(previous_index, index);
        // Dispatch only after the enclosing action callback exits its native stack.
        self.enqueue_owned_close();
    }

    /// Queue a close request without dropping the controller inside an AppKit callback.
    fn enqueue_launcher_dismissal(&self) {
        // A mouse callback retains no competing close intent or hidden Launcher surface.
        self.enqueue_owned_close();
    }

    /// Bind queued close to this controller identity instead of whichever surface is active later.
    fn enqueue_owned_close(&self) {
        // MainThreadBound transports only a weak reference; it never keeps a closed graph alive.
        let weak = dispatch2::MainThreadBound::new(objc2::rc::Weak::new(self), self.mtm());
        DispatchQueue::main().exec_async(move || {
            // This queue runs on the main thread; no native handle is extracted on another thread.
            let mtm =
                MainThreadMarker::new().expect("Launcher close callback is on the main queue");
            let Some(controller) = weak.get(mtm).load() else {
                // A callback after object deallocation has no close authority.
                return;
            };
            if !is_current_controller(&controller) {
                // A still-retained old controller cannot close a later Launcher session.
                return;
            }
            request_active_launcher_dismissal();
        });
    }

    /// Disable all incoming action/delegate edges before releasing controller-owned controls.
    fn detach_controls(&self, input: &NSTextField) {
        // SAFETY: the live input's delegate/target currently name this retained controller;
        // setting nullable callback properties to nil removes those non-owning links on the main thread.
        unsafe {
            input.setDelegate(None);
            input.setTarget(None);
            input.setAction(None);
        }
        if let Some(result_views) = self.ivars().result_views.get() {
            // A fully constructed surface has rows; partial construction may not have installed them.
            result_views.detach_actions();
        }
        if let Some(dismiss_button) = self.ivars().dismiss_button.get() {
            // SAFETY: the retained main-thread button remains valid while its nullable
            // target/action properties are cleared before the controller can deallocate.
            unsafe {
                dismiss_button.setTarget(None);
                dismiss_button.setAction(None);
            }
        }
        // Release installed-application paths; the pending owned action alone survives close handoff.
        *self.ivars().results.borrow_mut() = NativeLauncherResults::empty();
    }

    /// Transfer the accepted successor once after native callbacks have been detached.
    pub(super) fn take_pending_action(&self) -> Option<NativeLauncherSelection> {
        // Taking transfers the owned descriptor once and ends the borrow before activation can reenter.
        self.ivars().pending_action.borrow_mut().take()
    }
}

/// A native event is authoritative only for the same currently active panel identity.
fn is_current_panel(panel: &NativeLauncherPanel) -> bool {
    super::ACTIVE_SURFACE.with_borrow(|owner| {
        // Compare live borrowed identities without invoking native effects while the cell is borrowed.
        owner
            .as_ref()
            .is_some_and(|surface| std::ptr::eq(&*surface.panel, panel))
    })
}

/// Reject stale action/delegate callbacks even when AppKit temporarily retains their old controller.
fn is_current_controller(controller: &LauncherController) -> bool {
    super::ACTIVE_SURFACE.with_borrow(|owner| {
        // The active unique owner alone authorizes result selection and deferred close.
        owner
            .as_ref()
            .is_some_and(|surface| std::ptr::eq(&*surface.controller, controller))
    })
}

/// During preflight the owner is Closing; permit its provider until accepted freeze separately revokes creation.
fn owns_field_editor_window(controller: &LauncherController, window: &NSWindow) -> bool {
    let matches = |surface: &NativeLauncherSurface| {
        // Compare existing owned identities only, with no AppKit messages inside the TLS borrow.
        let panel: &NSWindow = &surface.panel;
        std::ptr::eq(&*surface.controller, controller) && std::ptr::eq(panel, window)
    };
    super::ACTIVE_SURFACE.with_borrow(|owner| {
        // Active input obtains explicit undo ownership through this session's provider.
        owner.as_ref().is_some_and(matches)
    }) || super::CLOSING_SURFACE.with_borrow(|owner| {
        // Preserve this graph during resignation/validation; later provider.freeze prevents post-admission reconstruction.
        owner.as_ref().is_some_and(matches)
    })
}

/// Match an editing notification to the active controller and its exact owned search field.
fn is_current_query_field(controller: &LauncherController, field: &NSTextField) -> bool {
    super::ACTIVE_SURFACE.with_borrow(|owner| {
        // Compare only live object identities under the owner borrow; all native reads occur afterwards.
        owner.as_ref().is_some_and(|surface| {
            // Both identities are necessary because an unrelated field can still send a valid notification.
            std::ptr::eq(&*surface.controller, controller) && std::ptr::eq(&*surface.input, field)
        })
    })
}

/// Per-projection authority check, repeated after every setter to reject supersession by native reentry.
struct LauncherProjection<'a> {
    is_current: &'a dyn Fn() -> bool,
}

impl<'a> LauncherProjection<'a> {
    /// Borrow stack-local identity checks without copying models or extending controller/view lifetimes.
    fn new(is_current: &'a dyn Fn() -> bool) -> Self {
        Self { is_current }
    }

    /// Perform one authorized native mutation; callers must stop the remaining projection after revocation.
    fn apply(&self, mutation: impl FnOnce()) -> Option<()> {
        if !(self.is_current)() {
            // A previous native call closed the foreground or installed another revision; leave the old graph untouched.
            return None;
        }
        // No result borrow crosses the setter, so synchronous callbacks may publish a model or close safely.
        mutation();
        if !(self.is_current)() {
            // This setter reentered; the old stack cannot overwrite the callback's newly completed projection.
            return None;
        }
        Some(())
    }
}

/// A row owns a fixed control set with separate content-installation and selection-appearance updates.
struct LauncherResultRowViews {
    background: Retained<NSBox>,
    command_background: Retained<NSBox>,
    command_label: Retained<NSTextField>,
    title_label: Retained<NSTextField>,
    subtitle_label: Retained<NSTextField>,
    action_hint_label: Retained<NSTextField>,
    action_button: Retained<NSButton>,
}

impl LauncherResultRowViews {
    /// Install changed content without caching application paths or borrowed text elsewhere.
    fn install(
        &self,
        selection: Option<&NativeLauncherSelection>,
        is_selected: bool,
        shows_empty_state: bool,
        projection: &LauncherProjection<'_>,
    ) -> Option<()> {
        if let Some(selection) = selection {
            // Bridge text during content installation only; visible and AX button titles share one NSString.
            let command = NSString::from_str(selection.activation_code());
            let title = NSString::from_str(selection.title());
            let subtitle = NSString::from_str(selection.subtitle());
            self.set_hidden(false, projection)?;
            projection.apply(|| self.command_label.setStringValue(&command))?;
            projection.apply(|| self.title_label.setStringValue(&title))?;
            projection.apply(|| self.subtitle_label.setStringValue(&subtitle))?;
            projection.apply(|| self.action_button.setTitle(&title))?;
            self.update_selection(is_selected, projection)?;
        } else if shows_empty_state {
            // Empty results show one explanatory row, with no executable button or Return hint.
            self.set_hidden(false, projection)?;
            projection.apply(|| self.command_label.setStringValue(ns_string!("—")))?;
            projection.apply(|| {
                self.title_label
                    .setStringValue(ns_string!("没有找到匹配功能"))
            })?;
            projection.apply(|| {
                self.subtitle_label
                    .setStringValue(ns_string!("换个关键词或命令试试"))
            })?;
            projection.apply(|| self.action_hint_label.setStringValue(ns_string!("")))?;
            projection.apply(|| self.action_button.setHidden(true))?;

            // Preserve empty-state colors; temporary color objects never become a cross-session cache.
            let border = NSColor::colorWithSRGBRed_green_blue_alpha(0.13, 0.16, 0.22, 1.0);
            let fill = NSColor::colorWithSRGBRed_green_blue_alpha(0.073, 0.086, 0.12, 1.0);
            let badge = NSColor::colorWithSRGBRed_green_blue_alpha(0.12, 0.14, 0.2, 1.0);
            projection.apply(|| self.background.setBorderColor(&border))?;
            projection.apply(|| self.background.setFillColor(&fill))?;
            projection.apply(|| self.command_background.setFillColor(&badge))?;
        } else {
            // Hide all remaining fixed rows so stale buttons cannot activate nonexistent results.
            self.set_hidden(true, projection)?;
        }
        Some(())
    }

    /// Selection changes update the Return hint and three colors only, preserving labels, AX titles and visibility.
    fn update_selection(
        &self,
        is_selected: bool,
        projection: &LauncherProjection<'_>,
    ) -> Option<()> {
        let (hint, border, fill, badge) = if is_selected {
            // The keyboard-selected row receives the Return hint and stronger highlight using existing visual values.
            (
                ns_string!("↵  打开"),
                [0.24, 0.39, 0.7],
                [0.115, 0.18, 0.31],
                [0.2, 0.36, 0.76],
            )
        } else {
            // Unselected rows remain clickable without claiming the current Return target's hint.
            (
                ns_string!(""),
                [0.13, 0.16, 0.22],
                [0.073, 0.086, 0.12],
                [0.13, 0.21, 0.39],
            )
        };
        // Foundation may allocate temporary colors; this optimization reduces updated rows, not all allocations.
        let border =
            NSColor::colorWithSRGBRed_green_blue_alpha(border[0], border[1], border[2], 1.0);
        let fill = NSColor::colorWithSRGBRed_green_blue_alpha(fill[0], fill[1], fill[2], 1.0);
        let badge = NSColor::colorWithSRGBRed_green_blue_alpha(badge[0], badge[1], badge[2], 1.0);
        projection.apply(|| self.action_hint_label.setStringValue(hint))?;
        projection.apply(|| self.background.setBorderColor(&border))?;
        projection.apply(|| self.background.setFillColor(&fill))?;
        projection.apply(|| self.command_background.setFillColor(&badge))?;
        Some(())
    }

    /// Apply the same authority check to each visibility setter, stopping old-row writes after reentry.
    fn set_hidden(&self, hidden: bool, projection: &LauncherProjection<'_>) -> Option<()> {
        // Keep decoration and transparent hit buttons consistent, rechecking session identity after every native call.
        projection.apply(|| self.background.setHidden(hidden))?;
        projection.apply(|| self.command_background.setHidden(hidden))?;
        projection.apply(|| self.command_label.setHidden(hidden))?;
        projection.apply(|| self.title_label.setHidden(hidden))?;
        projection.apply(|| self.subtitle_label.setHidden(hidden))?;
        projection.apply(|| self.action_hint_label.setHidden(hidden))?;
        projection.apply(|| self.action_button.setHidden(hidden))?;
        Some(())
    }

    /// Remove the native callback edge before the controller's last strong owner can drop.
    fn detach_action(&self) {
        // SAFETY: this retained main-thread button is live; nil target/action values are
        // permitted by AppKit and prevent subsequent delivery to the closing controller.
        unsafe {
            self.action_button.setTarget(None);
            self.action_button.setAction(None);
        }
    }
}

/// Fixed visible rows owned by one controller, with no hidden result graph.
struct LauncherResultViews {
    rows: Vec<LauncherResultRowViews>,
}

impl LauncherResultViews {
    /// Reuse fixed rows for changed results; reentrant model replacement stops all remaining old-snapshot writes.
    fn install(
        &self,
        results: &NativeLauncherResults,
        selected_index: usize,
        projection: &LauncherProjection<'_>,
    ) -> Option<()> {
        for (index, row) in self.rows.iter().enumerate() {
            // At most six rows exist; nonempty results have one selection and empty state has one explanatory row.
            row.install(
                results.get(index),
                !results.is_empty() && index == selected_index,
                results.is_empty() && index == 0,
                projection,
            )?;
        }
        Some(())
    }

    /// Selection-only updates touch at most two rows without accessing result strings or bundle paths.
    fn update_selection(
        &self,
        previous_index: usize,
        selected_index: usize,
        projection: &LauncherProjection<'_>,
    ) -> Option<()> {
        if previous_index == selected_index {
            // Callers already filter identical rows; this guard also prevents redundant native mutation.
            return Some(());
        }
        if let Some(previous_row) = self.rows.get(previous_index) {
            // Remove the old target hint first; stale indices cannot access outside the fixed native rows.
            previous_row.update_selection(false, projection)?;
        }
        if let Some(selected_row) = self.rows.get(selected_index) {
            // Highlight the new target only if authority survives the old-row update; reentry can stop it early.
            selected_row.update_selection(true, projection)?;
        }
        Some(())
    }

    /// Stop every row's action delivery before any retained row/controller ownership is released.
    fn detach_actions(&self) {
        for row in &self.rows {
            // There are only the fixed visible rows, so cleanup is bounded and synchronous.
            row.detach_action();
        }
    }
}

/// Unique foreground owner of the panel, field editor input and weak native delegate target.
pub(super) struct NativeLauncherSurface {
    pub(super) panel: Retained<NativeLauncherPanel>,
    pub(super) input: Retained<NSTextField>,
    pub(super) controller: Retained<LauncherController>,
    pub(super) close_observation: Option<LauncherReleaseProbe>,
}

impl NativeLauncherSurface {
    /// Build a complete hidden graph; the composition publishes ownership before presentation.
    pub(super) fn new(mtm: MainThreadMarker) -> Result<Self, String> {
        let catalog = crate::composition::launcher_catalog();
        let initial_results = resolve_launcher_results(&catalog, "");
        if initial_results.is_empty() {
            // Reject incomplete composition before allocating a native panel.
            return Err("启动器目录没有可见入口".to_owned());
        }

        // The surface owns the panel; native close must not balance Rust's retain claim.
        let panel = build_panel(mtm);

        // Keep the weak panel delegate alive through the surface's explicit controller owner.
        let controller = LauncherController::new(catalog, initial_results.clone(), mtm)?;
        panel.setDelegate(Some(ProtocolObject::from_ref(&*controller)));
        let content_view = panel
            .contentView()
            .ok_or_else(|| "原生启动器窗口没有内容视图".to_owned())?;
        // Adopt default content as the business root; register that role without claiming internal framework decoration.
        controller
            .ivars()
            .owned
            .include("launcher.root-content", &content_view)?;

        // Build one owned search field and its non-interactive chrome before installing callbacks.
        let input = build_search_input(&content_view, &controller, mtm)?;

        let mut result_rows = Vec::with_capacity(MAX_VISIBLE_LAUNCHER_RESULTS);
        for index in 0..MAX_VISIBLE_LAUNCHER_RESULTS {
            // Allocate only the six visible rows; unused rows are hidden, never a retained overflow list.
            result_rows.push(build_result_row(index, &content_view, &controller, mtm)?);
        }

        // The initial graph is unpublished; callbacks have no active authority while all fixed rows are installed.
        let result_views = LauncherResultViews { rows: result_rows };
        let construction_is_current = || true;
        let construction_projection = LauncherProjection::new(&construction_is_current);
        let _ = result_views.install(&initial_results, 0, &construction_projection);

        // Footer actions share the controller's close queue; the label reflects actual registration.
        let dismiss_button = build_footer(&content_view, &controller, mtm)?;

        // Transfer callback-target/control ownership once and establish the intended initial responder.
        controller.install_result_views(result_views);
        controller.install_dismiss_button(dismiss_button);
        panel.setInitialFirstResponder(Some(&input));

        Ok(Self {
            panel,
            input,
            controller,
            close_observation: None,
        })
    }

    /// Retain only stack-scoped presentation handles so no TLS borrow crosses native callbacks.
    pub(super) fn presentation(&self) -> LauncherPresentation {
        // Clone known native retain claims only; callers end their surface borrow before using handles.
        LauncherPresentation {
            panel: self.panel.clone(),
            input: self.input.clone(),
            controller: self.controller.clone(),
        }
    }

    /// Borrow the preflight snapshot; every native identity was captured before callbacks/responder teardown.
    pub(super) fn release_probe(&self) -> Option<LauncherReleaseProbe> {
        self.close_observation.clone()
    }

    /// Read the actual close notification reservation without sending a native callback.
    pub(super) fn close_was_notified(&self) -> bool {
        // Only windowWillClose sets this once-only flag before queuing the finalizer.
        self.controller.ivars().close_finalizer_scheduled.get()
    }

    /// Stop composition and incoming control callbacks while the panel/delegate remain owned.
    pub(super) fn prepare_to_close(&self) -> Result<(), String> {
        // Hidden-construction smoke uses the same public cleanup path without claiming that absence proves deallocation.
        let observation = self.presentation().prepare_close_release_probe()?;
        if !observation.borrowed.is_detached() {
            // A standalone construction test must not ignore shared-editor detachment failure.
            return Err(observation.failure_description());
        }
        Ok(())
    }

    /// Disconnect the graph after will-close has returned and before consuming the project barrier.
    pub(super) fn release_native_ownership(&self) {
        // The native close callback has unwound, so breaking delegate/content edges cannot lose its signal.
        self.panel.setDelegate(None);
        self.panel.setInitialFirstResponder(None);
        self.panel.setContentView(None);
    }
}

/// Non-owning evidence of panel, input, controller and native content destruction.
#[derive(Clone)]
pub(super) struct LauncherReleaseProbe {
    pub(super) panel: objc2::rc::Weak<NativeLauncherPanel>,
    pub(super) input: objc2::rc::Weak<NSTextField>,
    pub(super) controller: objc2::rc::Weak<LauncherController>,
    pub(super) content: Option<objc2::rc::Weak<objc2_app_kit::NSView>>,
    pub(super) payload: super::native_payload::NativePayloadSnapshot,
    borrowed: BorrowedEditorSnapshot,
}

impl LauncherReleaseProbe {
    /// Report bounded static roles and public-detachment errors without input text or private framework contents.
    pub(super) fn failure_description(&self) -> String {
        format!(
            "Launcher owned roles {:?}; borrowed editor {:?}",
            self.payload.surviving_roles(),
            self.borrowed.failure()
        )
    }

    /// Only the base panel may be leased by AppKit during accepted application termination.
    pub(super) fn payload_is_released(&self) -> bool {
        // Parent nil alone cannot prove independently retained child/cell/TextKit payload nil.
        self.payload.is_released()
            && self.borrowed.is_detached()
            && self.input.load().is_none()
            && self.controller.load().is_none()
            && self
                .content
                .as_ref()
                .is_some_and(|content| content.load().is_none())
    }

    /// Release observations are independent from the application's close-notification barrier.
    pub(super) fn is_released(&self) -> bool {
        // Weak loads create only temporary claims; no observation itself keeps a successor-old graph alive.
        self.panel.load().is_none()
            && self.payload.is_released()
            && self.borrowed.is_detached()
            && self.input.load().is_none()
            && self.controller.load().is_none()
            && self
                .content
                .as_ref()
                .is_none_or(|content| content.load().is_none())
    }
}

/// Temporary presentation claims protect synchronous callbacks after the unique owner is published.
pub(super) struct LauncherPresentation {
    panel: Retained<NativeLauncherPanel>,
    input: Retained<NSTextField>,
    controller: Retained<LauncherController>,
}

/// Probes keep construction-time weak identities only, never extending foreground or shared-editor lifetimes.
#[cfg(feature = "native-acceptance")]
#[derive(Clone)]
pub(super) struct LauncherProbeIdentity {
    pub(super) panel: objc2::rc::Weak<NativeLauncherPanel>,
    pub(super) input: objc2::rc::Weak<NSTextField>,
}

/// Bounded first-responder roles for diagnostics, without addresses or user text.
#[cfg(feature = "native-acceptance")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum LauncherResponderRole {
    None,
    Panel,
    Input,
    Editor,
    Other,
}

/// Capture all public focus facts in one observation so boolean short-circuiting cannot hide a failure.
#[cfg(feature = "native-acceptance")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct LauncherReadiness {
    pub(super) owner_active: bool,
    pub(super) app_running: bool,
    pub(super) app_active: bool,
    pub(super) app_hidden: bool,
    pub(super) activation_policy: isize,
    pub(super) visible: bool,
    pub(super) key: bool,
    pub(super) can_become_key: bool,
    pub(super) app_key_matches: bool,
    pub(super) enabled: bool,
    pub(super) editable: bool,
    pub(super) selectable: bool,
    pub(super) first_responder: LauncherResponderRole,
    pub(super) editor_present: bool,
    pub(super) editor_is_text_view: bool,
    pub(super) editor_same_window: bool,
    pub(super) input_context_present: bool,
    pub(super) activated_context_matches: bool,
}

#[cfg(feature = "native-acceptance")]
impl LauncherReadiness {
    /// A nonactivating panel may own input; app_active/global context are diagnostic, not replacement endpoints.
    pub(super) fn is_ready(self) -> bool {
        self.owner_active
            && self.app_running
            && self.visible
            && self.key
            && self.enabled
            && self.editable
            && self.editor_present
            && self.editor_is_text_view
            && self.editor_same_window
            && self.first_responder == LauncherResponderRole::Editor
            && self.input_context_present
    }
}

#[cfg(all(test, feature = "native-acceptance"))]
mod readiness_tests {
    use super::{LauncherReadiness, LauncherResponderRole};

    /// Pure fact fixtures create no AppKit objects and cannot establish actual focus or keyboard behavior.
    fn ready_nonactivating_panel() -> LauncherReadiness {
        LauncherReadiness {
            owner_active: true,
            app_running: true,
            app_active: false,
            app_hidden: false,
            activation_policy: 1,
            visible: true,
            key: true,
            can_become_key: true,
            app_key_matches: false,
            enabled: true,
            editable: true,
            selectable: true,
            first_responder: LauncherResponderRole::Editor,
            editor_present: true,
            editor_is_text_view: true,
            editor_same_window: true,
            input_context_present: true,
            activated_context_matches: false,
        }
    }

    #[test]
    fn readiness_requires_every_input_identity_and_window_fact() {
        let ready = ready_nonactivating_panel();
        assert!(ready.is_ready());
        let mutations: [fn(&mut LauncherReadiness); 11] = [
            |facts| facts.owner_active = false,
            |facts| facts.app_running = false,
            |facts| facts.visible = false,
            |facts| facts.key = false,
            |facts| facts.enabled = false,
            |facts| facts.editable = false,
            |facts| facts.editor_present = false,
            |facts| facts.editor_is_text_view = false,
            |facts| facts.editor_same_window = false,
            |facts| facts.first_responder = LauncherResponderRole::Panel,
            |facts| facts.input_context_present = false,
        ];
        for mutate in mutations {
            // Reject every independently missing endpoint fact; visibility or makeFirstResponder success alone is insufficient.
            let mut facts = ready;
            mutate(&mut facts);
            assert!(!facts.is_ready(), "{facts:?}");
        }
    }

    #[test]
    fn diagnostic_app_activation_is_not_a_nonactivating_panel_requirement() {
        let facts = ready_nonactivating_panel();
        // Define the endpoint by actual key/editor/window/context identities, not another process-activation policy.
        assert!(!facts.app_active);
        assert!(!facts.activated_context_matches);
        assert!(facts.is_ready());
    }
}

impl LauncherPresentation {
    /// Protect one synchronous native capture outside any foreground owner borrow.
    pub(super) fn from_handles(
        panel: &Retained<NativeLauncherPanel>,
        input: &Retained<NSTextField>,
        controller: &Retained<LauncherController>,
    ) -> Self {
        Self {
            panel: panel.clone(),
            input: input.clone(),
            controller: controller.clone(),
        }
    }

    /// Copy weak identities before installing probe observers, without retaining an extra panel/input owner.
    #[cfg(feature = "native-acceptance")]
    pub(super) fn probe_identity(&self) -> LauncherProbeIdentity {
        LauncherProbeIdentity {
            panel: objc2::rc::Weak::from_retained(&self.panel),
            input: objc2::rc::Weak::from_retained(&self.input),
        }
    }

    /// Run AppKit getters outside global owner/model borrows and return a scalar snapshot.
    #[cfg(feature = "native-acceptance")]
    pub(super) fn input_readiness(&self, mtm: MainThreadMarker) -> LauncherReadiness {
        fn same(left: &AnyObject, right: &AnyObject) -> bool {
            // Compare identity rather than isEqual contents so equal text cannot impersonate the same editor.
            std::ptr::eq(left, right)
        }
        let app = NSApplication::sharedApplication(mtm);
        let responder = self.panel.firstResponder();
        let editor = self.input.currentEditor();
        let editor_present = editor.is_some();
        let editor = editor.and_then(|editor| editor.downcast::<NSTextView>().ok());
        let editor_context = editor.as_ref().and_then(|editor| editor.inputContext());
        let active_context = objc2_app_kit::NSTextInputContext::currentInputContext(mtm);
        let first_responder = match responder.as_deref() {
            None => {
                // A visible window is not input-ready before its responder relationship exists.
                LauncherResponderRole::None
            }
            Some(responder) if same(responder, &self.panel) => {
                // makeFirstResponder can succeed while the window itself becomes responder; distinguish that outcome.
                LauncherResponderRole::Panel
            }
            Some(responder) if same(responder, &self.input) => {
                // Do not report text readiness before the field acquires its actual editor.
                LauncherResponderRole::Input
            }
            Some(responder)
                if editor
                    .as_ref()
                    .is_some_and(|editor| same(responder, editor)) =>
            {
                // This field owns input only when currentEditor matches the actual first responder.
                LauncherResponderRole::Editor
            }
            Some(_) => {
                // Classify other responders by bounded roles without exposing internal objects or claiming their focus.
                LauncherResponderRole::Other
            }
        };
        LauncherReadiness {
            owner_active: is_current_panel(&self.panel)
                && is_current_query_field(&self.controller, &self.input),
            app_running: app.isRunning(),
            app_active: app.isActive(),
            app_hidden: app.isHidden(),
            activation_policy: app.activationPolicy().0,
            visible: self.panel.isVisible(),
            key: self.panel.isKeyWindow(),
            can_become_key: self.panel.canBecomeKeyWindow(),
            app_key_matches: app
                .keyWindow()
                .is_some_and(|window| same(&window, &self.panel)),
            enabled: self.input.isEnabled(),
            editable: self.input.isEditable(),
            selectable: self.input.isSelectable(),
            first_responder,
            editor_present,
            editor_is_text_view: editor.is_some(),
            editor_same_window: editor.as_ref().is_some_and(|editor| {
                // Matching window identity excludes editors still associated with an old field or another window.
                editor
                    .window()
                    .is_some_and(|window| same(&window, &self.panel))
            }),
            input_context_present: editor_context.is_some(),
            activated_context_matches: editor_context.as_ref().is_some_and(|context| {
                // The global active context is diagnostic only; this getter synthesizes no input event.
                active_context
                    .as_ref()
                    .is_some_and(|active| same(context, active))
            }),
        }
    }

    /// Keep weak native evidence without retaining any object in the graph being measured.
    pub(super) fn release_probe(&self) -> Result<LauncherReleaseProbe, String> {
        let mut payload = self.controller.ivars().owned.snapshot();
        let content = self
            .panel
            .contentView()
            .ok_or("Launcher content is missing before close capture")?;
        // Record public cache-detachment facts without adding field-editor internals to the project cohort.
        let borrowed = self.controller.ivars().borrowed.clone();
        borrowed.capture(&self.panel)?;
        payload.seal();
        Ok(LauncherReleaseProbe {
            panel: objc2::rc::Weak::from_retained(&self.panel),
            input: objc2::rc::Weak::from_retained(&self.input),
            controller: objc2::rc::Weak::from_retained(&self.controller),
            content: Some(objc2::rc::Weak::from_retained(&content)),
            payload,
            borrowed,
        })
    }

    /// Preserve pre/post-resignation payload identities before disabling any usable control callback.
    pub(super) fn prepare_close_release_probe(&self) -> Result<LauncherReleaseProbe, String> {
        // Hidden-construction probes use the same gate; the first check changes neither focus nor close authority.
        self.controller.prepare_history_for_close()?;
        let mut observation = self.release_probe()?;
        // Preflight never discards composition; normal resignation validates it while refused close preserves editing.
        if !self.panel.makeFirstResponder(None) {
            // All action/delegate callbacks remain attached, so this same owner remains usable after cancellation.
            return Err("启动器输入框拒绝在关闭前结束编辑".into());
        }
        self.panel.setInitialFirstResponder(None);
        // After normal resignation, observe any replacement default editor without clearing valid input.
        observation.borrowed.capture(&self.panel)?;
        // Resignation callbacks have returned; confirm no history owner remains lent before freezing resources.
        self.controller.prepare_history_for_close()?;
        // Every fallible capture/refusal is complete before these callbacks are disabled.
        self.controller.detach_controls(&self.input);
        // Host published the sole Closing owner; retain public-detachment failure without rollback or retry.
        observation.borrowed.finish_accepted(
            &self.panel,
            &[&self.input],
            &self.controller.ivars().undo,
            &self.controller.ivars().field_editor,
        );
        // Include legitimate lazy-undo queries from preflight in the final ledger; SessionUndo then forbids creation.
        observation.payload = self.controller.ivars().owned.snapshot();
        observation.payload.seal();
        Ok(observation)
    }

    /// Restore usable input after capture/resignation refusal, without reviving a reentrantly replaced graph.
    pub(super) fn restore_after_close_refusal(&self) -> Result<(), String> {
        if !is_current_panel(&self.panel) {
            // Another accepted owner/transition already replaced this one; stale cleanup cannot reactivate it.
            return Err("关闭被拒绝后，启动器会话已变化，无法恢复输入焦点".into());
        }
        // A result action whose close failed must be selectable again, never retained as an unexecuted hidden successor.
        self.controller.take_pending_action();
        self.panel.setInitialFirstResponder(Some(&self.input));
        if !is_current_panel(&self.panel) {
            // Initial-responder callbacks can accept a new close; never refocus that now-closing graph.
            return Err("恢复初始输入焦点时，启动器会话已变化".into());
        }
        if !self.panel.makeFirstResponder(Some(&self.input)) {
            // Preserve the owner and report actual native refusal; do not claim a focus-ready recovery.
            return Err("关闭被拒绝后，无法恢复启动器输入焦点".into());
        }
        Ok(())
    }

    /// Verify that key equivalents do not bypass the panel's Escape/IME routing.
    #[cfg(feature = "native-acceptance")]
    pub(super) fn has_exclusive_escape_routing(&self) -> bool {
        // A default-empty footer key equivalent leaves composition cancellation entirely in the field editor.
        self.controller
            .ivars()
            .dismiss_button
            .get()
            .is_some_and(|button| {
                // This getter runs outside TLS/model borrows and returns only a bounded scalar observation.
                button.keyEquivalent().length() == 0
            })
    }

    /// Present the already-published graph without borrowing the global active-owner cell.
    pub(super) fn show(self, mtm: MainThreadMarker) {
        if !is_current_panel(&self.panel) {
            // Presentation handles are temporary claims; a replaced/closing graph has no show authority.
            return;
        }
        // Reset the input/model before making the panel key or allowing text-system callbacks.
        self.input.setStringValue(ns_string!(""));
        self.controller.update_selection_for_query("");
        if !is_current_panel(&self.panel) {
            // A reentrant input callback may have accepted close while the model was being reset.
            return;
        }
        self.panel.center();
        NSApplication::sharedApplication(mtm).activate();
        if !is_current_panel(&self.panel) {
            // Native activation can deliver callbacks; do not bring a graph back after close acceptance.
            return;
        }
        self.panel.orderFrontRegardless();
        self.panel.makeKeyWindow();
        if !is_current_panel(&self.panel) {
            // Focus callbacks can also close the current owner before the field editor is installed.
            return;
        }
        if !self.panel.makeFirstResponder(Some(&self.input)) {
            // A failed focus claim is observable; a visible panel alone is not input-ready evidence.
            eprintln!("native launcher could not focus its input");
        }
        #[cfg(feature = "native-product-acceptance")]
        {
            // Presentation has returned only; a later real driver event must independently verify this input's readiness.
            super::acceptance_product::launcher_presented(self.probe_identity());
        }
        // These transient retain claims drop here; the active/closing surface remains the only owner.
    }
}

/// Configure the floating nonactivating panel while preserving explicit surface release ownership.
fn build_panel(mtm: MainThreadMarker) -> Retained<NativeLauncherPanel> {
    let panel = NativeLauncherPanel::new(mtm);
    // SAFETY: this +1 panel is retained by NativeLauncherSurface; disabling automatic
    // close release prevents AppKit from consuming the Rust owner's claim.
    unsafe { panel.setReleasedWhenClosed(false) };
    panel.setExcludedFromWindowsMenu(true);
    panel.setFloatingPanel(true);
    panel.setBecomesKeyOnlyIfNeeded(false);
    panel.setWorksWhenModal(true);
    panel.setHidesOnDeactivate(false);
    panel.setMovableByWindowBackground(true);
    panel.setHasShadow(true);
    panel.setOpaque(false);
    panel.setLevel(NSFloatingWindowLevel);
    panel.setCollectionBehavior(
        NSWindowCollectionBehavior::CanJoinAllSpaces
            | NSWindowCollectionBehavior::FullScreenAuxiliary,
    );
    panel.setBackgroundColor(Some(&NSColor::clearColor()));
    panel
}

/// Construct the search field and decorative backgrounds; only the controller can handle edits.
fn build_search_input(
    content_view: &objc2_app_kit::NSView,
    controller: &LauncherController,
    mtm: MainThreadMarker,
) -> Result<Retained<NSTextField>, String> {
    // The content view retains this decorative background; it receives no callbacks.
    let panel_background = NSBox::initWithFrame(
        NSBox::alloc(mtm),
        NSRect::new(
            NSPoint::new(0.0, 0.0),
            NSSize::new(LAUNCHER_WIDTH, LAUNCHER_HEIGHT),
        ),
    );
    panel_background.setBoxType(NSBoxType::Custom);
    panel_background.setTitlePosition(NSTitlePosition::NoTitle);
    panel_background.setBorderWidth(1.0);
    panel_background.setCornerRadius(18.0);
    panel_background.setBorderColor(&NSColor::colorWithSRGBRed_green_blue_alpha(
        0.18, 0.21, 0.29, 1.0,
    ));
    panel_background.setFillColor(&NSColor::colorWithSRGBRed_green_blue_alpha(
        0.055, 0.067, 0.094, 0.985,
    ));
    content_view.addSubview(&panel_background);

    // Separate the editable field from its decoration so the hit target remains the native field.
    let search_background = NSBox::initWithFrame(
        NSBox::alloc(mtm),
        NSRect::new(NSPoint::new(18.0, 417.0), NSSize::new(724.0, 60.0)),
    );
    search_background.setBoxType(NSBoxType::Custom);
    search_background.setTitlePosition(NSTitlePosition::NoTitle);
    search_background.setBorderWidth(1.0);
    search_background.setCornerRadius(12.0);
    search_background.setBorderColor(&NSColor::colorWithSRGBRed_green_blue_alpha(
        0.17, 0.2, 0.28, 1.0,
    ));
    search_background.setFillColor(&NSColor::colorWithSRGBRed_green_blue_alpha(
        0.105, 0.125, 0.17, 1.0,
    ));
    content_view.addSubview(&search_background);

    // Configure one plain native field whose field editor owns IME behavior while focused.
    let input = NSTextField::textFieldWithString(ns_string!(""), mtm);
    input.setFrame(NSRect::new(
        NSPoint::new(38.0, 428.0),
        NSSize::new(684.0, 38.0),
    ));
    input.setPlaceholderString(Some(&NSString::from_str("搜索应用或功能，或输入简写命令")));
    input.setFont(Some(&NSFont::systemFontOfSize(21.0)));
    input.setBordered(false);
    input.setBezeled(false);
    input.setDrawsBackground(false);
    input.setFocusRingType(NSFocusRingType::None);
    input.setTextColor(Some(&NSColor::colorWithSRGBRed_green_blue_alpha(
        0.96, 0.97, 0.99, 1.0,
    )));
    // SAFETY: the main-thread controller implements NSTextFieldDelegate with exact selectors;
    // the surface retains it until this weak delegate link is explicitly cleared before close.
    unsafe {
        input.setDelegate(Some(ProtocolObject::from_ref(controller)));
    }
    content_view.addSubview(&input);
    // Register both project backgrounds and configured input/cell without claiming NSBox or editor internals.
    let owned = &controller.ivars().owned;
    owned.include("launcher.panel-background", &panel_background)?;
    owned.include("launcher.search-background", &search_background)?;
    owned.include_control("launcher.search-input", &input)?;
    Ok(input)
}

/// Allocate one complete row whose transparent button is its sole action target.
fn build_result_row(
    index: usize,
    content_view: &objc2_app_kit::NSView,
    controller: &LauncherController,
    mtm: MainThreadMarker,
) -> Result<LauncherResultRowViews, String> {
    // Assign each fixed-capacity row a stable position without a scrolling or retained overflow graph.
    let row_y = 351.0 - (index as f64 * 58.0);
    let background = NSBox::initWithFrame(
        NSBox::alloc(mtm),
        NSRect::new(NSPoint::new(18.0, row_y), NSSize::new(724.0, 54.0)),
    );
    background.setBoxType(NSBoxType::Custom);
    background.setTitlePosition(NSTitlePosition::NoTitle);
    background.setBorderWidth(1.0);
    background.setCornerRadius(10.0);
    content_view.addSubview(&background);

    // The command badge is non-interactive; only the row button dispatches selection.
    let command_background = NSBox::initWithFrame(
        NSBox::alloc(mtm),
        NSRect::new(NSPoint::new(32.0, row_y + 11.0), NSSize::new(76.0, 32.0)),
    );
    command_background.setBoxType(NSBoxType::Custom);
    command_background.setTitlePosition(NSTitlePosition::NoTitle);
    command_background.setBorderWidth(0.0);
    command_background.setCornerRadius(8.0);
    content_view.addSubview(&command_background);

    // Display the stable descriptor command independently from the user-facing title.
    let command_label = NSTextField::labelWithString(ns_string!(""), mtm);
    command_label.setFrame(NSRect::new(
        NSPoint::new(32.0, row_y + 17.0),
        NSSize::new(76.0, 20.0),
    ));
    command_label.setAlignment(NSTextAlignment::Center);
    command_label.setFont(Some(&NSFont::boldSystemFontOfSize(13.0)));
    command_label.setTextColor(Some(&NSColor::colorWithSRGBRed_green_blue_alpha(
        0.96, 0.97, 1.0, 1.0,
    )));
    content_view.addSubview(&command_label);

    // Keep the primary action label readable above the explanatory subtitle.
    let title_label = NSTextField::labelWithString(ns_string!(""), mtm);
    title_label.setFrame(NSRect::new(
        NSPoint::new(124.0, row_y + 27.0),
        NSSize::new(470.0, 20.0),
    ));
    title_label.setFont(Some(&NSFont::boldSystemFontOfSize(15.0)));
    title_label.setTextColor(Some(&NSColor::colorWithSRGBRed_green_blue_alpha(
        0.95, 0.96, 0.99, 1.0,
    )));
    content_view.addSubview(&title_label);

    // Explain the selected capability without displaying private bundle paths.
    let subtitle_label = NSTextField::labelWithString(ns_string!(""), mtm);
    subtitle_label.setFrame(NSRect::new(
        NSPoint::new(124.0, row_y + 8.0),
        NSSize::new(500.0, 17.0),
    ));
    subtitle_label.setFont(Some(&NSFont::systemFontOfSize(12.0)));
    subtitle_label.setTextColor(Some(&NSColor::colorWithSRGBRed_green_blue_alpha(
        0.59, 0.64, 0.74, 1.0,
    )));
    content_view.addSubview(&subtitle_label);

    // Reserve a fixed hint column so selection changes do not shift row geometry.
    let action_hint_label = NSTextField::labelWithString(ns_string!(""), mtm);
    action_hint_label.setFrame(NSRect::new(
        NSPoint::new(642.0, row_y + 18.0),
        NSSize::new(76.0, 18.0),
    ));
    action_hint_label.setAlignment(NSTextAlignment::Right);
    action_hint_label.setFont(Some(&NSFont::systemFontOfSize(12.0)));
    action_hint_label.setTextColor(Some(&NSColor::colorWithSRGBRed_green_blue_alpha(
        0.51, 0.57, 0.69, 1.0,
    )));
    content_view.addSubview(&action_hint_label);

    // SAFETY: the main-thread controller is retained by the surface and implements
    // activateSelection: with the declared single-object action ABI; its target is cleared at close.
    let action_button = unsafe {
        NSButton::buttonWithTitle_target_action(
            ns_string!(""),
            Some(controller),
            Some(sel!(activateSelection:)),
            mtm,
        )
    };
    action_button.setFrame(NSRect::new(
        NSPoint::new(18.0, row_y),
        NSSize::new(724.0, 54.0),
    ));
    action_button.setTag(index as isize);
    action_button.setBordered(false);
    action_button.setTransparent(true);
    action_button.setFocusRingType(NSFocusRingType::None);
    content_view.addSubview(&action_button);

    // Each row has seven constructed roles; configured label/button cells also receive independent ledger entries.
    let owned = &controller.ivars().owned;
    owned.include("launcher.row.background", &background)?;
    owned.include("launcher.row.command-background", &command_background)?;
    owned.include_control("launcher.row.command", &command_label)?;
    owned.include_control("launcher.row.title", &title_label)?;
    owned.include_control("launcher.row.subtitle", &subtitle_label)?;
    owned.include_control("launcher.row.hint", &action_hint_label)?;
    owned.include_control("launcher.row.action", &action_button)?;
    Ok(LauncherResultRowViews {
        background,
        command_background,
        command_label,
        title_label,
        subtitle_label,
        action_hint_label,
        action_button,
    })
}

/// Build the current shortcut label and the controller-owned dismiss button.
fn build_footer(
    content_view: &objc2_app_kit::NSView,
    controller: &LauncherController,
    mtm: MainThreadMarker,
) -> Result<Retained<NSButton>, String> {
    // Resolve the current registered shortcut only during explicit foreground construction.
    let shortcut_footer = NSString::from_str(&shortcuts::footer());
    let command_explanation = NSTextField::labelWithString(&shortcut_footer, mtm);
    command_explanation.setFrame(NSRect::new(
        NSPoint::new(24.0, 17.0),
        NSSize::new(500.0, 16.0),
    ));
    command_explanation.setFont(Some(&NSFont::systemFontOfSize(11.0)));
    command_explanation.setTextColor(Some(&NSColor::colorWithSRGBRed_green_blue_alpha(
        0.42, 0.47, 0.57, 1.0,
    )));
    content_view.addSubview(&command_explanation);

    // SAFETY: this retained main-thread controller implements dismissLauncher: with the
    // declared action ABI and outlives the button target link, which is cleared before release.
    let dismiss_button = unsafe {
        NSButton::buttonWithTitle_target_action(
            ns_string!("收起（Esc）"),
            Some(controller),
            Some(sel!(dismissLauncher:)),
            mtm,
        )
    };
    // Configure the explicit Escape action consistently with the panel key routing.
    dismiss_button.setFrame(NSRect::new(
        NSPoint::new(644.0, 10.0),
        NSSize::new(92.0, 28.0),
    ));
    dismiss_button.setBordered(false);
    dismiss_button.setAlignment(NSTextAlignment::Right);
    dismiss_button.setFocusRingType(NSFocusRingType::None);
    dismiss_button.setFont(Some(&NSFont::systemFontOfSize(11.0)));
    dismiss_button.setContentTintColor(Some(&NSColor::colorWithSRGBRed_green_blue_alpha(
        0.48, 0.53, 0.63, 1.0,
    )));
    // Leave the button's key equivalent empty. The panel alone owns physical Escape outside marked
    // composition; an Escape key equivalent could otherwise bypass that guard through AppKit routing.
    content_view.addSubview(&dismiss_button);
    // Footer text/actions are configured project roles; content nil cannot independently prove their release.
    controller
        .ivars()
        .owned
        .include_control("launcher.footer.label", &command_explanation)?;
    controller
        .ivars()
        .owned
        .include_control("launcher.footer.dismiss", &dismiss_button)?;
    Ok(dismiss_button)
}

#[cfg(test)]
mod projection_tests {
    use std::cell::Cell;

    use super::LauncherProjection;

    #[test]
    fn stale_projection_never_enters_native_mutation() {
        // Simulate losing foreground identity before a native setter; the original value must remain unchanged.
        let value = Cell::new(7);
        let is_current = || false;
        let projection = LauncherProjection::new(&is_current);
        assert!(
            projection
                .apply(|| {
                    // Only an incorrectly authorized old-graph write could overwrite this value.
                    value.set(99);
                })
                .is_none()
        );
        assert_eq!(value.get(), 7);
    }

    #[test]
    fn reentrant_projection_owns_the_final_values() {
        // Two slots model related row appearance; an earlier setter may synchronously start a complete new projection.
        let revision = Cell::new(1);
        let controls = Cell::new([0, 0]);
        let old_is_current = || revision.get() == 1;
        let old_projection = LauncherProjection::new(&old_is_current);
        let interrupted = old_projection.apply(|| {
            // Begin the old setter, then let its callback advance revision and install new content/selection.
            controls.set([1, 0]);
            revision.set(2);
            let new_is_current = || revision.get() == 2;
            let new_projection = LauncherProjection::new(&new_is_current);
            assert!(
                new_projection
                    .apply(|| {
                        // The new projection synchronously owns all final values; the outer call cannot overwrite either slot.
                        controls.set([2, 2]);
                    })
                    .is_some()
            );
        });
        assert!(interrupted.is_none());
        assert!(
            old_projection
                .apply(|| {
                    // If the old sequence remained authorized, this trailing write would corrupt the completed new projection.
                    controls.set([1, 1]);
                })
                .is_none()
        );
        assert_eq!(controls.get(), [2, 2]);
    }

    #[test]
    fn native_close_revokes_the_remaining_projection() {
        // Simulate a setter callback removing this controller from Active; even the same revision loses authority.
        let active = Cell::new(true);
        let value = Cell::new(0);
        let is_current = || active.get();
        let projection = LauncherProjection::new(&is_current);
        assert!(
            projection
                .apply(|| {
                    // The current mutation already occurred; close blocks subsequent writes without claiming setter rollback.
                    value.set(1);
                    active.set(false);
                })
                .is_none()
        );
        assert!(
            projection
                .apply(|| {
                    // Trailing operations after close cannot revive old controls or refresh business appearance.
                    value.set(2);
                })
                .is_none()
        );
        assert_eq!(value.get(), 1);
    }
}
