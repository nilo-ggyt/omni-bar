//! One native foreground owner for calculator, units, clipboard history and shortcut preferences.

#[cfg(feature = "native-acceptance")]
pub(super) mod acceptance;
mod clipboard;
#[cfg(test)]
mod close_tests;
mod release;
mod views;

use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;
use std::time::{SystemTime, UNIX_EPOCH};

use dispatch2::{DispatchQueue, MainThreadBound};
use objc2::rc::{Retained, Weak, autoreleasepool};
use objc2::runtime::AnyObject;
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send};
use objc2_app_kit::{
    NSApplication, NSControlTextEditingDelegate, NSEvent, NSEventModifierFlags, NSEventType,
    NSPanel, NSTextFieldDelegate, NSTextView, NSWindow, NSWindowDelegate,
};
use objc2_foundation::{
    NSNotification, NSObject, NSObjectProtocol, NSPoint, NSPointInRect, NSString, NSUndoManager,
    ns_string,
};
use omni_plugin_sdk::PluginCloseScope;

use super::native_editor::{BorrowedEditorSnapshot, SessionFieldEditorProvider, SessionUndo};
use super::native_payload::NativeOwnedLedger;

use crate::application::termination::TerminationTicket;
use crate::capabilities::clipboard::{
    ClipboardEntry, ClipboardSelectionAction, ClipboardSession, clipboard_selection,
};
use crate::capabilities::shortcut_preferences::{ShortcutPickerModel, ShortcutPreferences};
use crate::capabilities::{calculator, numeric_input, unit_conversion};
use crate::{BuiltInSurface, SurfaceCloseIntent};

/// The composition root coordinates these callbacks with its one foreground state machine.
pub(super) trait BuiltInLifecycle {
    /// Commit the already-constructed owner before showing its native window.
    fn commit_creation(&self, kind: BuiltInSurface) -> Result<(), String>;
    /// Validate the unchanged intent and explicit scope before resources stop or scope promotes.
    fn begin_close(
        &self,
        kind: BuiltInSurface,
        intent: SurfaceCloseIntent,
        scope: PluginCloseScope,
    ) -> Result<(), String>;
    /// Consume the accepted scope's sole completion receipt only after project-resource and cache-boundary checks pass.
    fn finish_close(
        &self,
        kind: BuiltInSurface,
        intent: SurfaceCloseIntent,
        scope: PluginCloseScope,
    );
}

/// Foreground-only dependencies supplied by the host, with no plugin-specific routing.
#[derive(Clone)]
pub(super) struct BuiltInContext {
    pub(super) application_data: PathBuf,
    pub(super) lifecycle: Rc<dyn BuiltInLifecycle>,
    pub(super) shortcuts: Rc<dyn ShortcutPreferences>,
}

thread_local! {
    /// Active and closing owners are mutually exclusive; callbacks borrow neither across AppKit.
    static ACTIVE: RefCell<Option<Rc<NativeBuiltInSession>>> = const { RefCell::new(None) };
    static CLOSING: RefCell<Option<Rc<NativeBuiltInSession>>> = const { RefCell::new(None) };
    /// Native release waits retain only weak/scalar metadata, while still owning the foreground close slot.
    static RELEASING: RefCell<Option<Rc<release::PendingRelease>>> = const { RefCell::new(None) };
}

/// The session retains its delegate and lazy editor; native callbacks hold only weak history/ledger links.
struct ControllerIvars {
    finalizer_scheduled: Cell<bool>,
    close_notified: Cell<bool>,
    // Rc supports same-session Rust Weak checks without creating a native controller ownership back-edge.
    owned: Rc<NativeOwnedLedger>,
    undo: Rc<SessionUndo>,
    borrowed: BorrowedEditorSnapshot,
    field_editor: SessionFieldEditorProvider,
    ownership_failure: RefCell<Option<String>>,
}

impl Default for ControllerIvars {
    /// Initialize Rust owners only; actual input requests lazily create the AppKit editor and undo manager.
    fn default() -> Self {
        let owned = Rc::new(NativeOwnedLedger::default());
        let undo = Rc::new(SessionUndo::default());
        let borrowed = BorrowedEditorSnapshot::default();
        let field_editor = SessionFieldEditorProvider::new(&undo, &owned, &borrowed);
        Self {
            finalizer_scheduled: Cell::new(false),
            close_notified: Cell::new(false),
            owned,
            undo,
            borrowed,
            field_editor,
            ownership_failure: RefCell::new(None),
        }
    }
}

define_class!(
    /// A session-retained native delegate routes actions only to its exact active foreground graph.
    // SAFETY: this NSObject delegate has main-thread ivars and implements the declared AppKit
    // selectors with their exact types; all referenced controls are retained by the session.
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    #[ivars = ControllerIvars]
    struct BuiltInController;

    // SAFETY: this NSObject subclass follows the NSObject protocol without extra requirements.
    unsafe impl NSObjectProtocol for BuiltInController {}

    // SAFETY: AppKit invokes this main-thread delegate while the foreground session retains it.
    unsafe impl NSControlTextEditingDelegate for BuiltInController {
        #[unsafe(method(controlTextDidBeginEditing:))]
        /// Record the active field's actual session history before later close preflight can inspect it.
        fn text_began(&self, _notification: &NSNotification) {
            if let Some(session) = active_session_for_controller(self) {
                // Bind actual undo identity at editing begin; close observation must not create substitute history for a test.
                session.observe_input_history();
            }
        }

        #[unsafe(method(controlTextDidChange:))]
        /// Refresh only this active capability, preserving clipboard-query reset and numeric input semantics.
        fn text_changed(&self, _notification: &NSNotification) {
            // Resolve an owned session outside TLS access before changing native result controls.
            if let Some(session) = active_session_for_controller(self) {
                // Recheck history authority during actual edits; record failure without clearing current input.
                session.observe_input_history();
                if session.kind == BuiltInSurface::Clipboard {
                    // Every actual query edit selects the first result, including changes beyond its stored prefix.
                    session.refresh_clipboard(ClipboardSelectionAction::QueryChanged);
                } else {
                    // Numeric input edits retain their existing capability-specific computation path.
                    session.refresh();
                }
            }
        }
    }

    // SAFETY: the inherited text-editing delegate methods use the framework's declared types.
    unsafe impl NSTextFieldDelegate for BuiltInController {}

    // SAFETY: the weak panel delegate remains retained through the native close notification.
    unsafe impl NSWindowDelegate for BuiltInController {
        #[unsafe(method_id(windowWillReturnFieldEditor:toObject:))]
        /// Lend this session's existing or lazy editor only to its own active/preflight-closing window.
        fn field_editor(
            &self,
            window: &NSWindow,
            client: Option<&AnyObject>,
        ) -> Option<Retained<AnyObject>> {
            if owns_editing_window(self, window) {
                // A nil-client observation creates nothing; accepted close freezes further provider authority.
                self.ivars().field_editor.provide(client, self.mtm())
            } else {
                // Reject stale delegates and foreign windows; normal resignation still permits the same Closing owner.
                None
            }
        }

        #[unsafe(method_id(windowWillReturnUndoManager:))]
        /// Return the original session history while its window/controller pair still owns editing authority.
        fn undo_manager(&self, window: &NSWindow) -> Option<Retained<NSUndoManager>> {
            if owns_editing_window(self, window) {
                // Lend exclusive history while active or normally resigning; accepted close prevents further creation.
                self.ivars().undo.manager(&self.ivars().owned)
            } else {
                // Match both window and controller before lending history to avoid old-window or cross-session access.
                None
            }
        }

        #[unsafe(method(windowShouldClose:))]
        /// Convert native advisory close into the same fallible foreground transaction used by dismissal.
        fn should_close(&self, _window: &NSWindow) -> bool {
            // Native close must enter the same host state transition as keyboard/global dismissal.
            self.enqueue_close(SurfaceCloseIntent::DismissAndResume);
            false
        }

        #[unsafe(method(windowWillClose:))]
        /// Record actual native close and reserve one weak-owner finalizer after this callback stack returns.
        fn will_close(&self, _notification: &NSNotification) {
            // Native close notification is an independent receipt fact, rather than a deallocation claim.
            self.ivars().close_notified.set(true);
            if self.ivars().finalizer_scheduled.replace(true) {
                // Duplicate native notifications cannot release a successor's ownership.
                return;
            }
            // Leave the native callback stack with only this controller's weak identity; stale work
            // cannot finalize a later same-kind session or hold the observed graph alive while waiting.
            let weak = MainThreadBound::new(Weak::new(self), self.mtm());
            DispatchQueue::main().exec_async(move || {
                let mtm = MainThreadMarker::new().expect("built-in finalizers use the AppKit main queue");
                if let Some(controller) = weak.get(mtm).load() {
                    // The temporary controller claim ends with this callback, before its real release event.
                    finalize_close(&controller);
                }
            });
        }
    }

    impl BuiltInController {
        #[unsafe(method(returnToLauncher:))]
        /// Preserve explicit back navigation as a distinct successor intent from temporary dismissal.
        fn return_to_launcher(&self, _sender: &AnyObject) {
            // Explicit back preserves the separate restore-Launcher semantic.
            self.enqueue_close(SurfaceCloseIntent::ExitToLauncher);
        }

        #[unsafe(method(copyResult:))]
        /// Copy a freshly validated current numeric result through the real pasteboard adapter.
        fn copy_result(&self, _sender: &AnyObject) {
            if let Some(session) = active_session_for_controller(self) {
                // Compute from current input, then write without a state borrow crossing AppKit.
                session.copy_result();
            }
        }

        #[unsafe(method(unitsChanged:))]
        /// Invalidate submitted output and rebuild only destinations compatible with the chosen source dimension.
        fn units_changed(&self, _sender: &AnyObject) {
            if let Some(session) = active_session_for_controller(self) {
                // Rebuild only incompatible destination choices and clear the prior submitted result.
                session.refresh_units();
            }
        }

        #[unsafe(method(convertUnits:))]
        /// Share explicit conversion behavior between the native button and the numeric field's Enter action.
        fn convert_units(&self, _sender: &AnyObject) {
            if let Some(session) = active_session_for_controller(self) {
                // The Convert button and numeric field's Enter action share the same validated conversion.
                session.convert_units();
            }
        }

        #[unsafe(method(restoreClipboard:))]
        /// Resolve only an actual nonnegative row tag before lending one validated stored body to pasteboard write.
        fn restore_clipboard(&self, sender: &AnyObject) {
            let Some(button) = sender.downcast_ref::<objc2_app_kit::NSButton>() else {
                // Only tagged row controls belong to this action; reject unrelated native senders.
                return;
            };
            if let Ok(index) = usize::try_from(button.tag()) {
                // Copy the row identity before restoration can update previews or request close.
                if let Some(session) = active_session_for_controller(self) {
                    // Storage lends one validated owned payload for the explicit pasteboard write.
                    session.restore_clipboard(index);
                }
            }
        }

        #[unsafe(method(clearClipboard:))]
        /// Delegate irreversible clearing to the active session's original modal confirmation and storage boundary.
        fn clear_clipboard(&self, _sender: &AnyObject) {
            if let Some(session) = active_session_for_controller(self) {
                // The modal confirmation runs without a TLS/model borrow and blocks competing close.
                session.clear_clipboard();
            }
        }

        #[unsafe(method(refreshClipboard:))]
        /// Reload the current query without capturing a clipboard body or changing the selected row unnecessarily.
        fn refresh_clipboard(&self, _sender: &AnyObject) {
            if let Some(session) = active_session_for_controller(self) {
                // The explicit baseline Refresh action reruns only this live session's bounded history search.
                session.refresh_clipboard(ClipboardSelectionAction::Refresh);
            }
        }

        #[unsafe(method(changeShortcut:))]
        /// Apply only the binding selected from the displayed model through the sole host registration owner.
        fn change_shortcut(&self, _sender: &AnyObject) {
            if let Some(session) = active_session_for_controller(self) {
                // Carbon registration and persistence rollback remain the host's responsibility.
                session.update_shortcut();
            }
        }
    }
);

impl BuiltInController {
    /// Bind a native action to its own graph before deferring the close beyond AppKit's callback stack.
    fn enqueue_close(&self, intent: SurfaceCloseIntent) {
        if let Some(session) = active_session_for_controller(self) {
            // A stale delegate notification cannot borrow the identity of a newer foreground session.
            enqueue_close_for(&session.panel, intent);
        }
    }

    /// Initialize callback metadata before any control may target this delegate.
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        // SAFETY: set_ivars initializes the Rust payload before NSObject's designated init.
        unsafe {
            msg_send![
                super(Self::alloc(mtm).set_ivars(ControllerIvars::default())),
                init
            ]
        }
    }
}

define_class!(
    /// A main-thread capability panel owns actual key-event dismissal and clipboard navigation.
    // SAFETY: this main-thread panel overrides only public key/event behavior without extra ivars.
    #[unsafe(super = NSPanel)]
    #[thread_kind = MainThreadOnly]
    struct BuiltInPanel;

    impl BuiltInPanel {
        #[unsafe(method(canBecomeKeyWindow))]
        /// Allow the existing capability panel's native controls and input method to acquire keyboard focus.
        fn can_become_key(&self) -> bool {
            // Text fields and native input methods require a key window.
            true
        }

        #[unsafe(method(sendEvent:))]
        /// Consume only existing dismissal/clipboard gestures and forward all remaining events to the native panel.
        fn send_event(&self, event: &NSEvent) {
            if event.r#type() == NSEventType::MouseMoved {
                // Reuse the window's ordinary event stream; no tracking area or retained observer owns hover selection.
                if let Some(session) = active_session_for_panel(self)
                    && session.kind == BuiltInSurface::Clipboard
                {
                    // The copied window point is resolved only against the existing live bounded row controls.
                    session.select_clipboard_row_at(event.locationInWindow());
                }
            }
            if event.r#type() == NSEventType::KeyDown {
                // Handle dismissal at the window boundary, including focused field editors/buttons.
                // Input composition owns Escape and navigation until AppKit commits/cancels it.
                let marked_text = self.firstResponder()
                    .and_then(|responder| responder.downcast::<NSTextView>().ok())
                    .is_some_and(|view| {
                        // SAFETY: AppKit's main-thread field editor implements NSTextInputClient.
                        unsafe { msg_send![&*view, hasMarkedText] }
                    });
                let modifiers = event.modifierFlags().intersection(NSEventModifierFlags::Command
                    | NSEventModifierFlags::Control | NSEventModifierFlags::Option | NSEventModifierFlags::Shift);
                if super::dismissal_shortcut::is_dismissal_event(event, marked_text) {
                    // Only the declared Escape/Command-W gestures enter the same fallible dismissal preflight.
                    enqueue_close_for(self, SurfaceCloseIntent::DismissAndResume);
                    return;
                }
                if let Some(session) = active_session_for_panel(self) {
                    // Clipboard rows support keyboard selection without registering a global monitor.
                    if session.kind == BuiltInSurface::Clipboard && modifiers.is_empty() && !marked_text {
                        match event.keyCode() {
                            // Up/Down cycle only the bounded current result set.
                            126 => { session.select_clipboard_row(ClipboardSelectionAction::Previous); return; }
                            // Down advances through the same bounded list in the opposite direction.
                            125 => { session.select_clipboard_row(ClipboardSelectionAction::Next); return; }
                            // Home and End select the actual first/final history result rather than moving the search caret.
                            115 => { session.select_clipboard_row(ClipboardSelectionAction::First); return; }
                            // End also handles a one-row/empty history through the same bounded pure policy.
                            119 => { session.select_clipboard_row(ClipboardSelectionAction::Last); return; }
                            // Return copies the selected row through the same action as a click.
                            36 | 76 => { session.restore_clipboard(session.selected_row.get()); return; }
                            // Other input remains owned by AppKit's normal text/control routing.
                            _ => {}
                        }
                    }
                }
            }
            // SAFETY: forward every unconsumed event exactly once to the NSPanel superclass.
            unsafe { let _: () = msg_send![super(self), sendEvent: event]; }
        }
    }
);

/// Sole strong owner of one built-in native graph and its foreground resources.
struct NativeBuiltInSession {
    kind: BuiltInSurface,
    context: BuiltInContext,
    panel: Retained<BuiltInPanel>,
    controller: Retained<BuiltInController>,
    views: views::BuiltInViews,
    clipboard: Option<ClipboardSession>,
    monitor: RefCell<Option<clipboard::ClipboardMonitor>>,
    last_change: Cell<isize>,
    entries: RefCell<Vec<ClipboardEntry>>,
    destination_units: RefCell<Vec<&'static str>>,
    // Preserve the displayed option model; a later binding change cannot reinterpret its native indices.
    shortcut_choices: RefCell<Option<Rc<ShortcutPickerModel>>>,
    // Callbacks during menu rebuild cannot consume partial rows or publish another nested model.
    shortcut_refreshing: Cell<bool>,
    selected_row: Cell<usize>,
    closing: Cell<Option<SurfaceCloseIntent>>,
    close_scope: Cell<PluginCloseScope>,
    close_ticket: Cell<Option<TerminationTicket>>,
    preparing_close: Cell<bool>,
    close_observation: RefCell<Option<release::CloseObservation>>,
    modal_open: Cell<bool>,
}

impl NativeBuiltInSession {
    /// Observe history only for current active input; late closing callbacks cannot recreate session authority.
    fn observe_input_history(&self) {
        if self.closing.get().is_some() {
            // Accepted close releases resources only; native callback reentry cannot create another undo owner.
            return;
        }
        let ivars = self.controller.ivars();
        if let Err(error) =
            ivars
                .borrowed
                .observe_editing(&self.views.input, &ivars.undo, &ivars.owned)
        {
            // Preserve helper failure facts; log static role/contract errors only, never input text.
            eprintln!("Built-in undo ownership observation failed: {error}");
        }
    }

    /// Acquire one complete graph while it is still invisible; failure drops every partial owner.
    fn new(
        kind: BuiltInSurface,
        context: BuiltInContext,
        mtm: MainThreadMarker,
    ) -> Result<Rc<Self>, String> {
        let initial_clipboard = if kind == BuiltInSurface::Clipboard {
            // Bind the actual pasteboard namespace and clear boundary before native controls
            // or persistence can observe a body; this does not read clipboard contents.
            Some(clipboard::source_generation()?)
        } else {
            // Other built-ins do not acquire or inspect a clipboard authority.
            None
        };
        // Native controls are bound to a retained delegate before the panel can become visible.
        let controller = BuiltInController::new(mtm);
        // Register the controller independently; parent-panel release does not establish its deallocation.
        controller
            .ivars()
            .owned
            .include("builtin.controller", &controller)?;
        let (panel, views) = views::build(kind, &controller, mtm, &controller.ivars().owned)?;
        let (clipboard, initial_change) = if let Some(initial) = initial_clipboard {
            // The fixed file is created only for an explicitly opened clipboard foreground surface.
            let suppression = clipboard::clear_suppression();
            let initial_change = if suppression.ignores(initial) {
                // An unchanged cleared body need not even be materialized when its surface is reopened.
                initial.change_count
            } else {
                // Unsuppressed activation preserves the baseline's immediate current-item snapshot.
                isize::MIN
            };
            let session = ClipboardSession::new(
                context.application_data.clone(),
                initial.source,
                suppression,
            );
            session.begin().map_err(|error| error.to_string())?;
            (Some(session), initial_change)
        } else {
            // Other capabilities have no clipboard file, snapshot or monitor ownership.
            (None, isize::MIN)
        };
        let session = Rc::new(Self {
            kind,
            context,
            panel,
            controller,
            views,
            clipboard,
            monitor: RefCell::new(None),
            last_change: Cell::new(initial_change),
            entries: RefCell::new(Vec::new()),
            destination_units: RefCell::new(Vec::new()),
            shortcut_choices: RefCell::new(None),
            shortcut_refreshing: Cell::new(false),
            selected_row: Cell::new(0),
            closing: Cell::new(None),
            close_scope: Cell::new(PluginCloseScope::ForegroundTransition),
            close_ticket: Cell::new(None),
            preparing_close: Cell::new(false),
            close_observation: RefCell::new(None),
            modal_open: Cell::new(false),
        });
        if kind == BuiltInSurface::Clipboard {
            // Perform the user-triggered initial capture before exposing the panel's current results.
            session.capture_clipboard();
            let weak = Rc::downgrade(&session);
            let monitor = clipboard::ClipboardMonitor::start(move || {
                // The callback holds only a weak owner and cannot keep a closed graph alive.
                if let Some(session) = weak.upgrade() {
                    // A queued event is permitted only while this exact session remains foreground.
                    if session.closing.get().is_none() {
                        // Close acceptance invalidates both capture and UI mutation before cancellation.
                        session.capture_clipboard();
                    }
                }
            })?;
            session.monitor.replace(Some(monitor));
        }
        // Populate only the selected capability; no state borrow survives the native update.
        session.refresh();
        if let Some(error) = session
            .controller
            .ivars()
            .ownership_failure
            .borrow()
            .as_ref()
        {
            // Failed initial menu registration leaves the partial window unshown and drops all local session owners.
            return Err(error.clone());
        }
        Ok(session)
    }

    /// Recompute bounded foreground presentation from current control values.
    fn refresh(&self) {
        if self.closing.get().is_some() {
            // Text/control callbacks delivered during native teardown cannot revive the graph.
            return;
        }
        match self.kind {
            // The calculator has no retained expression AST or history.
            BuiltInSurface::Calculator => self.refresh_calculation(),
            // Unit selection and conversion share the same pure affine domain logic.
            BuiltInSurface::UnitConversion => self.refresh_units(),
            // Only current bounded previews are retained while the history panel is open.
            BuiltInSurface::Clipboard => self.refresh_clipboard(ClipboardSelectionAction::Refresh),
            // Settings always display the registration actually owned by the host.
            BuiltInSurface::Settings => self.refresh_shortcut(),
        }
    }

    /// Reevaluate current input for Copy; never copy an error, blank-state hint or stale result.
    fn calculation_text(&self) -> Result<String, String> {
        autoreleasepool(|pool| {
            // Reject oversized pasted UTF-16 before Foundation creates a temporary UTF-8 representation.
            let input = self.views.input.stringValue();
            if input.length() > 256 {
                // Preserve the input and report its length error without parsing an oversized body.
                return Err("表达式过长".to_owned());
            }
            // SAFETY: This is the innermost pool; input and its UTF-8 view remain alive until pure parsing returns.
            // Parsing calls no native setter or foreign callback; returned display text/errors own their storage.
            let expression = unsafe { input.to_str(pool) };
            calculator::evaluate(expression)
                .map(|result| {
                    // Copy accepts only the domain's valid numeric representation, dropping other calculation metadata.
                    result.display_value
                })
                .map_err(|error| {
                    // Return expected input errors directly instead of inferring a value from the old output label.
                    error.to_string()
                })
        })
    }

    /// Share one bounded UTF-8 view for whitespace/parsing; native setters run after the borrow and pool end.
    fn refresh_calculation(&self) {
        let presentation = autoreleasepool(|pool| {
            // Read input once per event; reject excessive UTF-16 length before creating another text representation.
            let input = self.views.input.stringValue();
            if input.length() > 256 {
                // Share Copy's native length bound; errors update output only and never truncate editable input.
                return Err("表达式过长".to_owned());
            }
            // SAFETY: This innermost pool outlives pure whitespace/parsing work, which calls no AppKit and retains no borrow.
            // Input and any Foundation temporary UTF-8 buffer remain alive through this closure.
            let expression = unsafe { input.to_str(pool) };
            if numeric_input::is_blank(expression) {
                // Preserve the neutral ECMAScript-whitespace state without treating its hint as a copyable value.
                return Ok(None);
            }
            // Parse nonempty expressions on the same borrow, avoiding another getter, conversion or Rust input copy.
            calculator::evaluate(expression)
                .map(|result| {
                    // Some distinguishes a valid value from neutral empty state and moves owned display text outside the pool.
                    Some(result.display_value)
                })
                .map_err(|error| {
                    // Syntax, complexity and numeric errors remain bounded inline messages while input stays unchanged.
                    error.to_string()
                })
        });
        if self.closing.get().is_some() {
            // If a getter or pool drain triggered close, computed text cannot be written back into the closing graph.
            return;
        }
        match presentation {
            // Reuse a static NSString hint instead of rebridging the same constant each time input becomes blank.
            Ok(None) => self
                .views
                .output
                .setStringValue(ns_string!("输入表达式，例如 (12 + 8) * 3")),
            // Output owns its text; no input slice, model borrow or local pool crosses the native setter.
            Ok(Some(value)) => self.message(&value),
            // Length and domain errors share the existing message path without overwriting or truncating input.
            Err(error) => self.message(&error),
        }
    }

    /// Resolve selected compatible units and invoke the pure conversion contract.
    fn conversion_result(&self) -> Result<unit_conversion::UnitConversionResult, String> {
        // Resolve picker indices into immutable domain identities before accepting the numeric value.
        let definitions = unit_conversion::definitions();
        let from_index = usize::try_from(self.views.from.indexOfSelectedItem())
            .map_err(|_| "请选择源单位".to_owned())?;
        let from = definitions
            .get(from_index)
            .ok_or_else(|| "请选择源单位".to_owned())?;
        let to_index = usize::try_from(self.views.to.indexOfSelectedItem())
            .map_err(|_| "请选择目标单位".to_owned())?;
        let to = self
            .destination_units
            .borrow()
            .get(to_index)
            .copied()
            .ok_or_else(|| "请选择目标单位".to_owned())?;
        let input = self.views.input.stringValue();
        if input.length() > numeric_input::MAX_INPUT_UTF16 {
            // Numeric input has no meaningful unbounded representation in the finite conversion model.
            return Err("待换算数值过长".to_owned());
        }
        let value = numeric_input::parse_finite_number(&input.to_string())
            .map_err(|error| error.to_string())?;
        // Keep baseline numeric-field grammar separate from unchanged affine/domain validation.
        unit_conversion::convert(value, from.id, to).map_err(|error| error.to_string())
    }

    /// Keep the destination picker constrained to the selected source dimension.
    fn refresh_units(&self) {
        if self.closing.get().is_some() {
            // A late picker/edit callback cannot mutate presentation after this owner accepted close.
            return;
        }
        // Source selection determines the only destination dimension valid for this foreground input.
        let definitions = unit_conversion::definitions();
        let Ok(index) = usize::try_from(self.views.from.indexOfSelectedItem()) else {
            // No source selection means there is no meaningful destination or conversion.
            return;
        };
        let Some(from) = definitions.get(index) else {
            // A stale native index is rejected without indexing the static catalog.
            return;
        };
        let compatible: Vec<_> = definitions
            .iter()
            .filter(|unit| {
                // Filtering the static catalog makes cross-dimension native selections impossible.
                unit.dimension == from.dimension
            })
            .copied()
            .collect();
        let identifiers: Vec<_> = compatible
            .iter()
            .map(|unit| {
                // Retain only static identities, never a second set of mutable conversion descriptors.
                unit.id
            })
            .collect();
        if *self.destination_units.borrow() != identifiers {
            // Drop the model borrow before rebuilding native menu items, which may send callbacks.
            let initial_catalog = self.destination_units.borrow().is_empty();
            self.destination_units.replace(identifiers);
            self.views.to.removeAllItems();
            for unit in &compatible {
                // At most fifteen static definitions can contribute a destination item.
                self.views.to.addItemWithTitle(&NSString::from_str(&format!(
                    "{} ({})",
                    unit.label, unit.symbol
                )));
            }
            let initial = if initial_catalog {
                // Initial meter-to-kilometer selection matches baseline startup; later dimension changes choose first.
                compatible
                    .iter()
                    .position(|unit| {
                        // The initial source is meter, so kilometer is its useful supported default.
                        unit.id == "kilometer"
                    })
                    .unwrap_or(0)
            } else {
                // Crossing dimensions invalidates the previous destination and selects the first compatible unit.
                0
            };
            self.views.to.selectItemAtIndex(initial as isize);
            if !self.register_current_menus() {
                // Preserve an incomplete-ledger failure instead of treating the new menu as verified resources.
                return;
            }
        }
        // Editing any input/picker invalidates a submitted result; only Convert/Enter computes the next result.
        self.message("—");
    }

    /// Submit one finite current conversion from either the explicit button or the input's Enter action.
    fn convert_units(&self) {
        if self.kind != BuiltInSurface::UnitConversion || self.closing.get().is_some() {
            // Reject wrong-surface or late callbacks without touching another capability or a closing graph.
            return;
        }
        let presentation = self
            .conversion_result()
            .and_then(|result| {
                // Visible output identifies the target unit; numeric-only copy uses the untouched result field.
                result
                    .display_with_unit()
                    .map_err(|error| error.to_string())
            })
            .unwrap_or_else(|error| {
                // Expected input/domain failures remain inline while the current controls stay editable.
                error
            });
        self.message(&presentation);
    }

    /// Write a freshly computed valid result instead of accidentally copying an error label.
    fn copy_result(&self) {
        let result = match self.kind {
            // Calculator copy re-evaluates the current expression rather than copying a stale label.
            BuiltInSurface::Calculator => self.calculation_text(),
            // Unit copy resolves the current picker identities and numeric input through the domain.
            BuiltInSurface::UnitConversion => self.conversion_result().map(|result| {
                // Clipboard output remains the canonical numeric representation, without the visible target suffix.
                result.display_value
            }),
            // Other panels expose dedicated actions and cannot copy arbitrary status text.
            _ => return,
        };
        match result.and_then(|value| clipboard::write_text(&value)) {
            // A successful write leaves the current foreground panel available for further input.
            Ok(()) => self.message("已复制结果"),
            // Validation or pasteboard failure leaves input and current session intact.
            Err(error) => self.message(&error),
        }
    }

    /// Capture one stable changed generation and update views only after a real stored change.
    fn capture_clipboard(&self) {
        if self.closing.get().is_some() {
            // A callback that already loaded the session still must not read data after close acceptance.
            return;
        }
        let Some(history) = &self.clipboard else {
            // Non-clipboard panels never read the system pasteboard for monitoring purposes.
            return;
        };
        let capture = match clipboard::capture_changed(history, &self.last_change) {
            // Only a stable, source-validated optional capture may reach fixed-file storage.
            Ok(capture) => capture,
            // Authority errors preserve history and expose a bounded foreground message without a body read.
            Err(error) => {
                self.message(&error);
                return;
            }
        };
        if let Some((generation, content)) = capture {
            // Timestamp is metadata only; the payload is released before this callback returns.
            let timestamp = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |value| value.as_millis().min(i64::MAX as u128) as i64);
            match history.record(generation, &content, timestamp) {
                // A valid foreground change updates bounded previews exactly once.
                Ok(true) => self.refresh_clipboard(ClipboardSelectionAction::Refresh),
                // Closed or rejected captures perform no view update.
                Ok(false) => {}
                // Surface a bounded storage error without logging the clipboard body or path.
                Err(error) => self.message(&error.to_string()),
            }
        }
    }

    /// Replace current previews after storage releases its transient buffers, lock and descriptor.
    fn refresh_clipboard(&self, selection: ClipboardSelectionAction) {
        let Some(history) = &self.clipboard else {
            // Numeric/settings sessions have no clipboard preview ownership.
            return;
        };
        // Read a finite native prefix without disturbing marked text or copying an unbounded search input.
        let query = views::clipboard_search_query(&self.views.input);
        match history.search(&query) {
            // Replace scalar identity/preview state before updating any native row controls.
            Ok(entries) => {
                self.selected_row.set(clipboard_selection(
                    self.selected_row.get(),
                    entries.len(),
                    selection,
                ));
                self.entries.replace(entries);
                self.render_rows();
            }
            // Failed queries cannot leave old results actionable; preserve disk/input while invalidating their previews.
            Err(error) => {
                self.entries.borrow_mut().clear();
                self.selected_row.set(0);
                self.render_rows();
                self.message(&error.to_string());
            }
        }
    }

    /// Snapshot preview strings before native setters so no state borrow can be reentered.
    fn render_rows(&self) {
        let labels: Vec<_> = self
            .entries
            .borrow()
            .iter()
            .map(views::clipboard_row_label)
            .collect();
        for (index, row) in self.views.rows.iter().enumerate() {
            // The fixed twenty controls never require retaining complete historic clipboard bodies.
            if let Some(label) = labels.get(index) {
                // A selected marker gives keyboard navigation a visible, accessible title change.
                let prefix = if index == self.selected_row.get() {
                    // The current keyboard target is explicit before Return can restore it.
                    "▶ "
                } else {
                    // Unselected rows retain only their ordinary bounded preview.
                    ""
                };
                row.setTitle(&NSString::from_str(&format!("{prefix}{label}")));
                row.setHidden(false);
            } else {
                // Hidden unused rows release stale preview strings on every refresh/clear.
                row.setTitle(&NSString::from_str(""));
                row.setHidden(true);
            }
        }
        self.message(&format!(
            "{} 条记录 · 仅在此面板打开时记录 · ↑↓ 选择，回车复制",
            labels.len()
        ));
    }

    /// Apply keyboard navigation to the actual finite result list, including an empty-history no-op.
    fn select_clipboard_row(&self, action: ClipboardSelectionAction) {
        let count = self.entries.borrow().len();
        if count == 0 {
            // Empty result sets have no valid restoration target.
            return;
        }
        self.selected_row
            .set(clipboard_selection(self.selected_row.get(), count, action));
        self.render_rows();
    }

    /// Select the hovered visible row without allocating a tracking resource or restoring text on movement.
    fn select_clipboard_row_at(&self, window_point: NSPoint) {
        let count = self.entries.borrow().len();
        for (index, row) in self.views.rows.iter().take(count).enumerate() {
            // At most twenty existing rows are inspected; a clipped/hidden row cannot become a hover target.
            if row.isHidden() {
                // Unused controls carry no current entry identity, even if their old frame remains allocated.
                continue;
            }
            let point = row.convertPoint_fromView(window_point, None);
            if NSPointInRect(point, row.visibleRect()) {
                // Only a changed target redraws the bounded preview titles; repeated movement has no storage effect.
                if self.selected_row.replace(index) != index {
                    self.render_rows();
                }
                return;
            }
        }
    }

    /// Restore through the bounded borrowed-body boundary, then return to Launcher on success.
    fn restore_clipboard(&self, index: usize) {
        let id = self.entries.borrow().get(index).map(|entry| entry.id);
        let (Some(id), Some(history)) = (id, self.clipboard.as_ref()) else {
            // Stale/empty row actions cannot read an arbitrary storage slot.
            return;
        };
        match history.with_entry(id, clipboard::write_text) {
            // Storage's owned payload, operation lock and descriptor end before the surface transition.
            Ok(Ok(())) => enqueue_close_for(&self.panel, SurfaceCloseIntent::ExitToLauncher),
            // Clipboard write failure preserves the panel so the user can retry.
            Ok(Err(error)) => self.message(&error),
            // Missing/corrupt history is reported without exposing body data.
            Err(error) => self.message(&error.to_string()),
        }
    }

    /// Confirm destructive clearing without holding an interior borrow through the modal loop.
    fn clear_clipboard(&self) {
        if self.modal_open.replace(true) {
            // A second clear request cannot create a second modal owner.
            return;
        }
        let confirmed = views::confirm_clear(self.panel.mtm(), &self.controller.ivars().owned);
        self.modal_open.set(false);
        let confirmed = match confirmed {
            // Confirmation and cancellation use the same fully registered modal; only confirmation mutates storage.
            Ok(confirmed) => confirmed,
            // Registration failure neither shows an incomplete alert nor clears history; preserve its release-proof failure.
            Err(error) => {
                self.controller
                    .ivars()
                    .ownership_failure
                    .replace(Some(error.clone()));
                self.message(&error);
                return;
            }
        };
        if confirmed {
            // Read the generation after confirmation so every older capture remains invalidated.
            if let Some(history) = &self.clipboard {
                let result = clipboard::source_generation().and_then(|generation| {
                    // Validate the exact source before disk mutation; only successful clear creates a receipt.
                    history.clear(generation).map_err(|error| error.to_string())
                });
                match result {
                    // Publish process-only suppression after disk clear, before refreshing or closing the session.
                    Ok(receipt) => {
                        // Mark the successful clear's count seen before any later timer can request that body.
                        self.last_change.set(receipt.generation().change_count);
                        clipboard::remember_clear(receipt);
                        self.refresh_clipboard(ClipboardSelectionAction::Refresh);
                    }
                    // Failed clearing leaves the operation visible and never closes the panel.
                    Err(error) => self.message(&error),
                }
            }
        }
    }

    /// Rebuild the actual active binding or unregistered placeholder; reentry cannot consume incomplete indices.
    fn refresh_shortcut(&self) {
        if self.closing.get().is_some() {
            return; // Accepted close revokes menu creation; an error refresh cannot restore models or controls.
        }
        if self.shortcut_refreshing.replace(true) {
            // The outer rebuild retains publication authority; nested calls neither reset it nor create another menu/message.
            return;
        }
        // Remove/drop the old model outside its borrow; selection actions during rebuild see None and reject.
        let previous = self.shortcut_choices.take();
        drop(previous);
        (|| {
            let current = match self.context.shortcuts.current() {
                Ok(current) => current,
                Err(error) => {
                    // A temporarily unreadable owner does not mean no active binding; publish no invented default.
                    self.message(&error.to_string());
                    return;
                }
            };
            let model = Rc::new(ShortcutPickerModel::new(current));
            self.views.shortcuts.removeAllItems();
            if self.closing.get().is_some() {
                return; // If removal reentry accepted close, do not repopulate the torn-down menu.
            }
            for binding in model.rows() {
                let title = match binding {
                    // Build every row from the same bounded pending model; localized labels do not alter actual bindings.
                    Some(binding) => binding.display_label(),
                    // The placeholder has no registrable key; a separate row avoids falsely presenting the default as active.
                    None => "尚未注册，请选择".to_owned(),
                };
                self.views
                    .shortcuts
                    .addItemWithTitle(&NSString::from_str(&title));
                if self.closing.get().is_some() {
                    return; // Recheck after each native construction; post-close temporary models are dropped on this stack only.
                }
            }
            self.views
                .shortcuts
                .selectItemAtIndex(model.selected() as isize);
            if self.closing.get().is_some() {
                return; // Native selection can reenter close; afterward neither register roles nor publish indices.
            }
            if !self.register_current_menus() || self.closing.get().is_some() {
                // Registration failure or reentrant close prevents index-model publication and further changes.
                return;
            }
            self.shortcut_choices.replace(Some(model));
            let status = match current {
                Some(view) => format!("当前快捷键：{}", view.binding.display_label()),
                None => "快捷键未注册，请在设置中选择".to_owned(),
            };
            self.message(&status);
        })();
        // Return rebuild authority on success and failure, without queues, waits or automatic alternative bindings.
        self.shortcut_refreshing.set(false);
    }

    /// Register actual rebuilt items and preserve the first failure across later refreshes.
    fn register_current_menus(&self) -> bool {
        match self.views.register_menus(&self.controller.ivars().owned) {
            // All current bounded menu roles are registered, allowing visible-state updates to continue.
            Ok(()) => true,
            // Retain release-proof failure for close admission; it differs from a recoverable input error.
            Err(error) => {
                let mut failure = self.controller.ivars().ownership_failure.borrow_mut();
                if failure.is_none() {
                    // Preserve the earliest missing-evidence cause instead of replacing it with later errors.
                    *failure = Some(error.clone());
                }
                drop(failure);
                self.message(&error);
                false
            }
        }
    }

    /// Consume explicit selection from the displayed model; the normal transaction also recovers unregistered state.
    fn update_shortcut(&self) {
        if self.closing.get().is_some() || self.shortcut_refreshing.get() {
            // Close or menu rebuild revokes selection authority; transient indices cannot identify a binding.
            return;
        }
        let Some(model) = self.shortcut_choices.borrow().clone() else {
            // Pin the displayed Rust model identity, retaining bounded Copy bindings and no additional native resources.
            return;
        };
        let Ok(index) = usize::try_from(self.views.shortcuts.indexOfSelectedItem()) else {
            // No selected row means no guessed default and no settings read or rewrite.
            return;
        };
        if self.closing.get().is_some()
            || self.shortcut_refreshing.get()
            || !self
                .shortcut_choices
                .borrow()
                .as_ref()
                .is_some_and(|current| Rc::ptr_eq(current, &model))
        {
            // Index getters may reenter refresh/close; an old index cannot select a binding in a replacement model.
            return;
        }
        let Some(binding) = model.binding_at(index) else {
            // Placeholders, out-of-range indices and incomplete models grant no registration authority.
            return;
        };
        // Copy the selected binding and end model borrows before Carbon, filesystem transactions or their reentry.
        match self.context.shortcuts.update(&binding.canonical()) {
            Ok(_) => self.refresh_shortcut(),
            Err(error) => {
                // Before-commit failure may leave no binding; after-commit errors may be applied, so refresh actual state first.
                self.refresh_shortcut();
                self.message(&error.to_string());
            }
        }
    }

    /// Update only the bounded native status label, never logs or traces containing user text.
    fn message(&self, message: &str) {
        // The caller retains the complete graph and has ended all mutable state borrows.
        self.views
            .output
            .setStringValue(&NSString::from_str(message));
    }

    /// Make queued callbacks inert before cancelling the monitor and dismantling controls.
    fn stop_resources(&self) {
        if let Some(history) = &self.clipboard {
            // Deactivate history first so an already-enqueued timer cannot repopulate it.
            history.close();
        }
        // Move the timer owner out before Drop cancels it; retain neither body previews nor callbacks.
        let monitor = self.monitor.take();
        drop(monitor);
        self.entries.borrow_mut().clear();
        // The settings model belongs to this foreground session; close releases its capacity and invalid native-index map.
        let choices = self.shortcut_choices.take();
        drop(choices);
    }
}

impl Drop for NativeBuiltInSession {
    /// Stop foreground monitoring and sever callback/content links on every ordinary or partial-construction drop.
    fn drop(&mut self) {
        // Fallback cleanup remains bounded and never presents UI or recursively requests close.
        self.stop_resources();
        self.views.detach();
        self.panel.setDelegate(None);
        self.panel.setInitialFirstResponder(None);
        self.panel.setContentView(None);
    }
}

/// Obtain an owned callback reference while ending the TLS borrow before any native call.
fn active_session() -> Option<Rc<NativeBuiltInSession>> {
    // The caller is a main-thread AppKit callback or a validated public entry point.
    ACTIVE.with_borrow(Clone::clone)
}

/// Accept delegate actions only while this exact controller belongs to the active native graph.
fn active_session_for_controller(
    controller: &BuiltInController,
) -> Option<Rc<NativeBuiltInSession>> {
    // Filtering an owned Rc ends TLS access before callbacks perform native or filesystem work.
    active_session().filter(|session| {
        // Native callbacks from a detached prior delegate cannot mutate the newer active session.
        std::ptr::eq(controller, &*session.controller)
    })
}

/// Match window/controller after ending TLS borrows; preflight resignation permits the same Closing owner.
fn owns_editing_window(controller: &BuiltInController, window: &NSWindow) -> bool {
    let active = ACTIVE.with_borrow(Clone::clone);
    let closing = CLOSING.with_borrow(Clone::clone);
    [active, closing].into_iter().flatten().any(|session| {
        // Both identities must match the current owner; window type or a nonempty global slot cannot authorize a session.
        std::ptr::eq(controller, &*session.controller)
            && std::ptr::eq(window, &*session.panel as &NSWindow)
    })
}

/// Accept window events only while this exact panel belongs to the active native graph.
fn active_session_for_panel(panel: &BuiltInPanel) -> Option<Rc<NativeBuiltInSession>> {
    // An old AppKit event may outlive its visible window, so global active ownership alone is insufficient.
    active_session().filter(|session| {
        // Compare live panel identity before granting access to the current capability's state.
        std::ptr::eq(panel, &*session.panel)
    })
}

/// Create one fully configured native built-in after the host claimed its creation transition.
pub(super) fn show(
    kind: BuiltInSurface,
    context: BuiltInContext,
    mtm: MainThreadMarker,
) -> Result<(), String> {
    if is_open() {
        // Never replace an active/closing graph or co-own a second built-in window.
        return Err("内建功能仍在显示或关闭中".to_owned());
    }
    // A failed construction/commit drops this still-invisible graph and cancels any partial monitor.
    let session = NativeBuiltInSession::new(kind, context, mtm)?;
    session.context.lifecycle.commit_creation(kind)?;
    ACTIVE.with_borrow_mut(|active| {
        // Transfer a strong foreground owner before AppKit can deliver focus/control callbacks.
        *active = Some(Rc::clone(&session));
    });
    // Ownership and callbacks are installed before activation can deliver key/focus events.
    session.panel.center();
    NSApplication::sharedApplication(mtm).activate();
    session.panel.orderFrontRegardless();
    session.panel.makeKeyWindow();
    if kind == BuiltInSurface::Settings {
        // Settings begins keyboard interaction at its visible picker rather than the hidden input.
        session
            .panel
            .makeFirstResponder(Some(&session.views.shortcuts));
    } else {
        // Numeric/search features become input-ready through their visible field.
        session.panel.makeFirstResponder(Some(&session.views.input));
    }
    Ok(())
}

/// Report both active and closing ownership, guarded by the actual main-thread capability.
pub(super) fn is_open() -> bool {
    // A foreign thread cannot inspect a different TLS instance and infer native availability.
    MainThreadMarker::new().is_some()
        && (ACTIVE.with_borrow(Option::is_some)
            || CLOSING.with_borrow(Option::is_some)
            || RELEASING.with_borrow(Option::is_some))
}

/// Accept a host/native close request, stopping foreground resources before requesting native close.
pub(super) fn request_close(intent: SurfaceCloseIntent) -> Result<(), String> {
    // Ordinary callers always select the complete native graph boundary explicitly.
    request_close_scoped(intent, PluginCloseScope::ForegroundTransition)
}

/// Preserve an existing close intent while actual pending application quit authorizes terminal scope.
pub(super) fn request_termination_close() -> Result<(), String> {
    let _mtm =
        MainThreadMarker::new().ok_or("Built-in termination close requires the main thread")?;
    let released = RELEASING.with_borrow(Clone::clone);
    let intent = if let Some(released) = released {
        // A finalized native owner still retains its original intent in weak-only operation metadata.
        released.intent()
    } else if let Some(session) = CLOSING.with_borrow(Clone::clone) {
        // Promotion cannot replace an already accepted back/dismiss direction with a fresh default.
        session
            .closing
            .get()
            .ok_or("Built-in closing metadata is incomplete")?
    } else {
        // A fresh quit has no ordinary successor to restore, but still uses the normal close preflight.
        SurfaceCloseIntent::DismissAndResume
    };
    request_close_scoped(intent, PluginCloseScope::ApplicationTermination)
}

/// Check explicit request direction and one-way scope changes before either participant mutates close state.
pub(super) fn validate_close_scope(
    accepted: Option<(SurfaceCloseIntent, PluginCloseScope)>,
    intent: SurfaceCloseIntent,
    scope: PluginCloseScope,
    termination_pending: bool,
) -> Result<(), String> {
    if scope == PluginCloseScope::ApplicationTermination && !termination_pending {
        // Activation suppression or an OS-owned panel alone cannot grant actual quit authority.
        return Err("Built-in terminal close requires pending application termination".to_owned());
    }
    if let Some((accepted_intent, accepted_scope)) = accepted {
        // Existing admission constrains both request fields, including a later legitimate application quit.
        if intent != accepted_intent {
            // Opposing directions preserve the original accepted transition and every resource owner.
            return Err("Built-in close cannot replace the accepted intent".to_owned());
        }
        if accepted_scope == PluginCloseScope::ApplicationTermination
            && scope == PluginCloseScope::ForegroundTransition
        {
            // A terminal receipt never becomes permission to construct an ordinary successor.
            return Err("Built-in terminal close scope cannot downgrade".to_owned());
        }
    }
    Ok(())
}

/// Bind terminal authority to the actual coordinator ticket, rejecting reuse during a later cancelled-quit retry.
pub(super) fn validate_close_ticket(
    accepted: Option<TerminationTicket>,
    scope: PluginCloseScope,
    current: Option<TerminationTicket>,
) -> Result<Option<TerminationTicket>, String> {
    if scope == PluginCloseScope::ForegroundTransition {
        // Ordinary request validation rejects downgrade separately and carries no process-termination authority.
        return Ok(None);
    }
    let ticket = current.ok_or("Built-in terminal close has no application termination ticket")?;
    if accepted.is_some_and(|accepted| accepted != ticket) {
        // A different actual quit transaction cannot consume a terminal receipt accepted by an earlier one.
        return Err(
            "Built-in terminal close belongs to a different application termination".to_owned(),
        );
    }
    Ok(Some(ticket))
}

/// Share normal close preflight and actual event observations, with no native polling or fallback scope.
fn request_close_scoped(intent: SurfaceCloseIntent, scope: PluginCloseScope) -> Result<(), String> {
    let mtm = MainThreadMarker::new().ok_or_else(|| "内建功能必须在主线程关闭".to_owned())?;
    objc2::rc::autoreleasepool(|_| {
        // Drain responder/capture temporaries locally before a TerminateLater caller can keep its outer pool alive.
        request_close_in_pool(intent, scope, mtm)
    })
}

/// End all local state borrows before AppKit; publish exact closing ownership before native close callbacks.
fn request_close_in_pool(
    intent: SurfaceCloseIntent,
    scope: PluginCloseScope,
    mtm: MainThreadMarker,
) -> Result<(), String> {
    if let Some(operation) = RELEASING.with_borrow(Clone::clone) {
        // Native payload destruction may precede actual quit; metadata supports promotion without a second close.
        return operation.request(intent, scope);
    }
    if let Some(session) = CLOSING.with_borrow(Clone::clone) {
        // Preserve accepted intent while the existing native callback remains the sole graph finalizer.
        validate_close_scope(
            session
                .closing
                .get()
                .map(|intent| (intent, session.close_scope.get())),
            intent,
            scope,
            super::termination::application_termination_pending(),
        )?;
        let ticket = validate_close_ticket(
            session.close_ticket.get(),
            scope,
            super::termination::application_termination_ticket(),
        )?;
        session
            .context
            .lifecycle
            .begin_close(session.kind, intent, scope)?;
        session.close_scope.set(scope);
        session.close_ticket.set(ticket);
        return Ok(());
    }
    let session = active_session().ok_or_else(|| "内建功能已经关闭".to_owned())?;
    if session.modal_open.get() {
        // Keep the graph alive while a destructive-action confirmation owns a nested modal loop.
        return Err("请先完成当前确认操作".to_owned());
    }
    if session.preparing_close.replace(true) {
        // Responder preflight can reenter native actions; it cannot start a second preflight or release owner.
        return Err("Built-in close preparation is already active".to_owned());
    }
    /// Restore only the scalar guard; fallback cleanup never performs native or fallible work.
    struct Preparing<'a>(&'a Cell<bool>);
    impl Drop for Preparing<'_> {
        fn drop(&mut self) {
            // Both refusal and accepted close leave the original preparation frame without a stale busy flag.
            self.0.set(false);
        }
    }
    let _preparing = Preparing(&session.preparing_close);
    // Reject close during history lending/construction before resignation, save or admission, preserving input.
    session.controller.ivars().undo.prepare_close()?;
    // A locally lent field editor is independently busy even when its undo manager is idle.
    session.controller.ivars().field_editor.prepare_close()?;
    validate_close_scope(
        None,
        intent,
        scope,
        super::termination::application_termination_pending(),
    )?;
    let ticket = validate_close_ticket(
        None,
        scope,
        super::termination::application_termination_ticket(),
    )?;
    let mut observation = release::CloseObservation::capture(&session)?;
    // Normal resignation completes control/IME validation first; refused close never discards composition or clears text.
    if !session.panel.makeFirstResponder(None) {
        // A native field editor that refuses resignation cannot become a detached terminal shell.
        return Err("Built-in native responder refused close preparation".to_owned());
    }
    // Resignation may replace the shared editor; record both as borrowed caches without claiming project ownership.
    if let Err(error) = observation
        .capture_session_payload(&session)
        .and_then(|()| session.controller.ivars().undo.prepare_close())
        .and_then(|()| session.controller.ivars().field_editor.prepare_close())
    {
        // Before close acceptance, restore this same active owner's input focus and preserve normal keyboard access.
        restore_active_input(&session);
        return Err(error);
    }
    // The host must accept the transition before active state or resource ownership changes.
    if let Err(error) = session
        .context
        .lifecycle
        .begin_close(session.kind, intent, scope)
    {
        // Refused admission preserves the session; focus restoration cannot affect a replacement owner.
        restore_active_input(&session);
        return Err(error);
    }
    session.closing.set(Some(intent));
    session.close_scope.set(scope);
    session.close_ticket.set(ticket);
    ACTIVE.take();
    CLOSING.with_borrow_mut(|closing| {
        // The closing slot keeps the graph alive until windowWillClose schedules its deferred finalizer.
        *closing = Some(Rc::clone(&session));
    });
    // Foreground history admission and timer callbacks stop after the exact closing owner is published.
    session.stop_resources();
    // After admission revokes active authority, clear fields/editor/history; failure keeps the owner Closing.
    observation.finish_accepted(&session);
    observation.arm(mtm);
    session.close_observation.replace(Some(observation));
    // Detach every control sender before native close while retaining the window delegate for willClose.
    session.views.detach();
    session.panel.close();
    Ok(())
}

/// Failed preflight restores only the same active session's focus, never showing or reviving another owner.
fn restore_active_input(session: &Rc<NativeBuiltInSession>) {
    let still_active = ACTIVE.with_borrow(|active| {
        // Compare Rc identity inside TLS only; native calls occur after its borrow ends.
        active
            .as_ref()
            .is_some_and(|active| Rc::ptr_eq(active, session))
    });
    if !still_active || session.closing.get().is_some() {
        // If reentry replaced the owner, an old preflight failure cannot modify the replacement responder.
        return;
    }
    let restored = if session.kind == BuiltInSurface::Settings {
        // Settings uses the visible picker for keyboard input; its hidden text field cannot take focus.
        session
            .panel
            .makeFirstResponder(Some(&session.views.shortcuts))
    } else {
        // Other capabilities preserve the current numeric/search field and its contents.
        session.panel.makeFirstResponder(Some(&session.views.input))
    };
    if !restored {
        // Failed focus restoration preserves owner/data and reports failure without claiming input readiness.
        eprintln!("Built-in failed-preflight input focus could not be restored");
    }
}

/// Defer a close for one weak foreground owner without permitting it to close a successor surface.
fn enqueue_close_for(panel: &BuiltInPanel, intent: SurfaceCloseIntent) {
    let Some(session) = active_session_for_panel(panel) else {
        // Old/inactive native callbacks have no authority to enqueue work for the current surface.
        return;
    };
    // MainThreadBound permits dispatch transport of a non-Send weak owner while enforcing access/drop
    // on its original AppKit thread. The task retains no session or native graph while waiting.
    let weak = MainThreadBound::new(Rc::downgrade(&session), panel.mtm());
    DispatchQueue::main().exec_async(move || {
        // This queue is process-main; the safe marker proves access to the thread-bound weak owner.
        let mtm = MainThreadMarker::new()
            .expect("built-in close callbacks run on the AppKit main thread");
        request_close_for_weak_owner(weak.get(mtm), intent);
    });
}

/// Resolve one original weak callback owner and reject delivery to a different foreground graph.
fn request_close_for_weak_owner(
    owner: &std::rc::Weak<NativeBuiltInSession>,
    intent: SurfaceCloseIntent,
) {
    let Some(session) = owner.upgrade() else {
        // Full old-session destruction makes delayed work inert without acquiring a new owner.
        return;
    };
    let still_active = ACTIVE.with_borrow(|active| {
        // Pointer identity belongs to the still-live Rc allocation and cannot alias a successor.
        active
            .as_ref()
            .is_some_and(|current| Rc::ptr_eq(current, &session))
    });
    if !still_active {
        // A closing or replaced owner cannot redirect its queued close intent onto a successor.
        return;
    }
    // Expected duplicate/busy requests preserve existing ownership and need no sensitive logging.
    let _ = request_close(intent);
}

/// Release native ownership in a local pool, then wait for actual full/payload deallocation events.
fn finalize_close(controller: &BuiltInController) {
    let Some(session) = CLOSING.with_borrow(Clone::clone) else {
        // Duplicate or delayed callbacks cannot infer authority from a newer active surface.
        return;
    };
    if !std::ptr::eq(controller, &*session.controller) {
        // An old controller cannot finalize a later same-kind closing owner.
        return;
    }
    let kind = session.kind;
    let lifecycle = Rc::clone(&session.context.lifecycle);
    let Some(intent) = session.closing.get() else {
        // Only an accepted close enters CLOSING; reject malformed metadata without a false barrier.
        return;
    };
    let Some(observation) = session.close_observation.take() else {
        // Actual signal cohorts must already exist; absence cannot become a weak-only guessed queue check.
        eprintln!("Built-in close release observations are unavailable");
        return;
    };
    let operation = release::PendingRelease::new(
        kind,
        intent,
        session.close_scope.get(),
        session.close_ticket.get(),
        lifecycle,
        observation,
        controller.ivars().close_notified.get(),
    );
    RELEASING.with_borrow_mut(|slot| {
        // Weak-only operation metadata occupies the closing slot before the last strong graph owners are dropped.
        *slot = Some(Rc::clone(&operation));
    });
    operation.observe();
    let owner = CLOSING.take();
    objc2::rc::autoreleasepool(|_| {
        // Native detach/drop creates only local autoreleases; no owner or view escapes into a termination wait pool.
        drop(owner);
        drop(session);
    });
}
