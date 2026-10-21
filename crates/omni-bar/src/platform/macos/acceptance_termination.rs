//! Opt-in production-loop termination evidence over a fresh, parent-owned synthetic fixture.
//!
//! The driver owns weak native identities and scalar observations only. Actual save, scope-specific
//! native payload release, SDK-session Drop and the foreground barrier retain their production implementation.

use std::cell::RefCell;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use objc2::rc::{Retained, Weak, autoreleasepool};
use objc2::runtime::{AnyObject, ProtocolObject};
use objc2::{MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{
    NSApplication, NSApplicationDidBecomeActiveNotification, NSApplicationTerminateReply,
    NSMenuItem, NSTextInputClient, NSWindow, NSWindowDidBecomeKeyNotification,
};
use objc2_foundation::{
    NSNotFound, NSNotification, NSNotificationCenter, NSObject, NSObjectProtocol, NSRange,
    NSString, ns_string,
};
use omni_md_plugin::native_surface::paged_text_view::{
    NativeMarkdownPagedTextView, NativeMarkdownPagedTextViewError,
};

use crate::capabilities::shortcut_preferences::{ShortcutBinding, ShortcutSettingsStore};

/// Six explicit synthetic foregrounds run in separate processes, each with a fresh NSApplication.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NativeTerminationProbeScenario {
    /// Terminate with the actual Launcher still owning its foreground graph.
    Launcher,
    /// Insert into the actual SDK Markdown responder and immediately terminate before autosave can run.
    MarkdownDirty,
    /// Activate one concrete built-in through the same Host catalog/foreground composition root.
    BuiltIn(crate::BuiltInSurface),
}

/// Exercise explicit menu and serial-main-queue termination through their actual production entries.
/// Neither route registers a quit shortcut or synthesizes a keyboard event.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NativeTerminationProbeOrigin {
    /// Invoke the installed NSMenu item through its public action entry, preserving its target and selector.
    MenuAction,
    /// A serial-main-dispatch callback uses the production common-mode termination-entry wrapper.
    Dispatch,
}

/// The exact ASCII prefix is shared with the parent verifier and never represents personal input.
pub const NATIVE_TERMINATION_PROBE_INSERTION: &str = "Synthetic immediate-quit draft A9\n";

/// This diagnostic owner never extends the life of the foreground graph it observes.
struct ProbeState {
    scenario: NativeTerminationProbeScenario,
    origin: NativeTerminationProbeOrigin,
    directory: PathBuf,
    started: bool,
    // The observer owns only its two notification registrations, never an observed native graph.
    readiness_observer: Option<Retained<TerminationReadinessObserver>>,
    readiness_check_pending: bool,
    readiness_changed_during_check: bool,
    termination_started: bool,
    window: Option<Weak<NSWindow>>,
    editor: Option<Weak<NativeMarkdownPagedTextView>>,
    // Observe only the invoked original item; retain neither its menu tree, target nor foreground graph.
    menu_item: Option<Weak<NSMenuItem>>,
    launcher: Option<super::launcher_ui::LauncherReleaseProbe>,
    terminal_receipt: bool,
    accounting: super::native_deallocation::NativeDeallocationAccounting,
    later_replies: u8,
    async_replies: u8,
    failure: Option<String>,
}

thread_local! {
    /// Normal product and manual acceptance entry points leave this optional driver absent.
    static PROBE: RefCell<Option<ProbeState>> = const { RefCell::new(None) };
}

define_class!(
    /// Observe original application/window transitions without requesting focus or another foreground.
    // SAFETY: This NSObject subclass has no ivars. Registration, delivery and removal use the
    // AppKit main thread, and the selector preserves the notification's actual Objective-C ABI.
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    struct TerminationReadinessObserver;

    // SAFETY: Preserve NSObject initialization, reference counting and identity semantics.
    unsafe impl NSObjectProtocol for TerminationReadinessObserver {}

    impl TerminationReadinessObserver {
        #[unsafe(method(terminationReadinessEvent:))]
        /// A matching real transition permits one observation; the event itself never proves readiness.
        fn readiness_event(&self, notification: &NSNotification) {
            if MainThreadMarker::new().is_some() && readiness_notification_matches(notification) {
                // Exact sender identity is checked outside observer-state borrows. No notification
                // object, native window, or activation action is retained by the queued successor.
                queue_readiness_check();
            }
        }
    }
);

/// Register before original show so late transitions are observed; one initial state read also
/// covers notifications that occurred before the original window identity could be captured.
fn install_readiness_notifications(mtm: MainThreadMarker) {
    // SAFETY: This zero-ivar NSObject uses its designated initializer on the actual AppKit main thread.
    let observer: Retained<TerminationReadinessObserver> = unsafe {
        msg_send![
            super(TerminationReadinessObserver::alloc(mtm).set_ivars(())),
            init
        ]
    };
    let center = NSNotificationCenter::defaultCenter();
    // SAFETY: AppKit exports immutable notification-name objects with process-long lifetimes.
    let names = unsafe {
        [
            NSApplicationDidBecomeActiveNotification,
            NSWindowDidBecomeKeyNotification,
        ]
    };
    for name in names {
        // SAFETY: These two process-lifetime names and the notification selector are exact. The
        // retained token below outlives registration. Broad delivery does not grant authority:
        // callbacks independently require this probe's original application/window sender.
        unsafe {
            center.addObserver_selector_name_object(
                &observer,
                sel!(terminationReadinessEvent:),
                Some(name),
                None,
            );
        }
    }
    PROBE.with_borrow_mut(|slot| {
        // Only the once-only launch callback installs these tokens. Move ownership without an
        // Objective-C retain or any native call while the scalar state is borrowed.
        slot.as_mut()
            .expect("the launch callback owns the configured termination probe")
            .readiness_observer = Some(observer);
    });
}

/// Remove only this diagnostic observer's exact named registrations before its last retain ends.
fn detach_readiness_notifications() {
    let observer = PROBE.with_borrow_mut(|slot| {
        slot.as_mut()
            .and_then(|state| state.readiness_observer.take())
    });
    if let Some(observer) = observer {
        // End the state borrow before Foundation removal can release registration metadata.
        let center = NSNotificationCenter::defaultCenter();
        // SAFETY: These are the same immutable process-long AppKit names used at registration.
        let names = unsafe {
            [
                NSApplicationDidBecomeActiveNotification,
                NSWindowDidBecomeKeyNotification,
            ]
        };
        for name in names {
            // SAFETY: This observer registered only these names with object=None. Remove exactly
            // those tokens without unregistering another observer or touching other app resources.
            unsafe { center.removeObserver_name_object(&observer, Some(name), None) };
        }
    }
}

/// Ignore every unrelated sender and every event arriving after failure or the one Quit reservation.
fn readiness_notification_matches(notification: &NSNotification) -> bool {
    let window = PROBE.with_borrow(|slot| {
        let state = slot.as_ref()?;
        if state.failure.is_some() || state.termination_started {
            return None;
        }
        state.window.clone()
    });
    let Some(window) = window else {
        // An early notification cannot invent a window identity. Original show's later initial
        // observation will inspect actual state, so no early readiness event needs replaying.
        return false;
    };
    let Some(sender) = notification.object() else {
        return false;
    };
    let name = notification.name();
    // SAFETY: Compare only immutable AppKit notification names, without retaining a foreground graph.
    if unsafe { name.isEqualToString(NSWindowDidBecomeKeyNotification) } {
        // Upgrade only the originally captured window on this stack, never its controller or input.
        return window.load().is_some_and(|original| {
            let original: &AnyObject = &original;
            std::ptr::eq(original, &*sender)
        });
    }
    // SAFETY: The process-long AppKit name is valid for this synchronous main-thread comparison.
    if unsafe { name.isEqualToString(NSApplicationDidBecomeActiveNotification) } {
        let mtm =
            MainThreadMarker::new().expect("AppKit readiness notifications use the main thread");
        let application = NSApplication::sharedApplication(mtm);
        let original: &AnyObject = &application;
        return std::ptr::eq(original, &*sender);
    }
    false
}

/// Coalesce real transition signals, retaining no native object in the serial-main-queue callback.
fn queue_readiness_check() {
    let queued = PROBE.with_borrow_mut(|slot| {
        let Some(state) = slot.as_mut() else {
            return false;
        };
        if state.failure.is_some() || state.termination_started || state.window.is_none() {
            return false;
        }
        if state.readiness_check_pending {
            // A notification arriving during native getters must not be lost. Queued-before-check
            // signals are consumed by the initial observation instead of causing idle retries.
            state.readiness_changed_during_check = true;
            return false;
        }
        state.readiness_check_pending = true;
        true
    });
    if queued {
        dispatch2::DispatchQueue::main().exec_async(check_readiness);
    }
}

/// Inspect actual visibility/key facts once. A pending result waits only for another real transition.
fn check_readiness() {
    let window = PROBE.with_borrow_mut(|slot| {
        let state = slot.as_mut()?;
        if state.failure.is_some() || state.termination_started || !state.readiness_check_pending {
            return None;
        }
        // All notifications before this getter stack are represented by the state read about to
        // occur. Only a genuinely new notification during the getters can justify a successor.
        state.readiness_changed_during_check = false;
        state.window.clone()
    });
    let Some(window) = window else {
        return;
    };
    let mtm = MainThreadMarker::new().expect("the dispatch main queue is the AppKit thread");
    let ready = autoreleasepool(|_| {
        window
            .load()
            .map(|window| window.isVisible() && window.isKeyWindow())
            .ok_or("原退出验收窗口在就绪前已释放")
    });
    let ready = match ready {
        Ok(ready) => ready,
        Err(error) => {
            fail(error.into(), mtm);
            return;
        }
    };
    let (start, observed_again) = PROBE.with_borrow_mut(|slot| {
        let Some(state) = slot.as_mut() else {
            return (false, false);
        };
        if state.failure.is_some() || state.termination_started {
            return (false, false);
        }
        state.readiness_check_pending = false;
        if ready {
            // Visible+key are actual facts of the original window. Nonactivating panels do not
            // require NSApplication.isActive, and no focus/show action is replayed to obtain them.
            state.termination_started = true;
            state.readiness_changed_during_check = false;
            (true, false)
        } else {
            (false, state.readiness_changed_during_check)
        }
    });
    if start {
        // End exact notification authority and every temporary window claim before entering the
        // existing input/Quit path. The parent retains its unchanged 20-second failure deadline.
        detach_readiness_notifications();
        request_termination(mtm);
    } else if observed_again {
        // This successor consumes a real signal received during the getters, never a timer,
        // elapsed queue turn, or unconditional retry of a still-pending native state.
        queue_readiness_check();
    }
}

/// Write finite scalar evidence without allowing a broken parent pipe to unwind through Objective-C.
fn emit(message: &str) {
    // Missing output remains a parent-side verification failure; diagnostics never alter product close policy.
    let _ = writeln!(std::io::stdout().lock(), "termination_probe {message}");
}

/// Enter the production application with synthetic authorities installed before any shortcut/plugin acquisition.
///
/// `directory` must be a new, empty directory owned by the invoking test parent. The function is for
/// a dedicated subprocess's actual main thread, before any NSApplication event loop. Successful AppKit
/// termination exits that subprocess, so the parent retains fixture ownership and verifies saved bytes.
/// This exercises programmatic responder delivery and resource ordering, not physical keyboard input.
///
/// # Errors
///
/// Rejects another driver, non-main-thread calls, fixture acquisition failure or a stopped/failed loop.
pub fn run_native_termination_probe_child(
    directory: &Path,
    scenario: NativeTerminationProbeScenario,
    origin: NativeTerminationProbeOrigin,
) -> Result<(), String> {
    // A standalone child must establish real AppKit main-thread authority before publishing diagnostic state.
    let mtm = MainThreadMarker::new().ok_or("termination probe requires the AppKit main thread")?;
    if PROBE.with_borrow(Option::is_some) {
        // A second observer cannot replace the first operation's weak identities or reply accounting.
        return Err("a termination probe is already installed".into());
    }
    // Verify source-free public association accounting before any actual product graph can be acquired.
    let accounting = super::native_deallocation::verify_native_deallocation_accounting(mtm)?;
    super::native_payload::verify_native_payload_accounting(mtm)?;
    let _pasteboard = super::acceptance_fixture::install_fixture(directory)?;
    // Every foreground/origin pair uses Alt+Space in this child's isolated settings only.
    // Empty settings would try Cmd+Space; system refusal opens the real recovery Launcher and changes
    // foreground ownership. Registration and recovery still belong solely to production startup.
    ShortcutSettingsStore::new(directory.join("application-support"))
        .persist(ShortcutBinding::OptionSpace)
        .map_err(|_| {
            // Before publishing the observer or starting AppKit, release fixture authority and preserve the write failure.
            super::acceptance_fixture::clear_fixture();
            "无法为隔离退出验收持久化已选启动器快捷键".to_owned()
        })?;
    PROBE.with_borrow_mut(|slot| {
        // Publish only complete authorities; startup itself remains the production delegate's launch callback.
        *slot = Some(ProbeState {
            scenario,
            origin,
            directory: directory.to_owned(),
            started: false,
            readiness_observer: None,
            readiness_check_pending: false,
            readiness_changed_during_check: false,
            termination_started: false,
            window: None,
            editor: None,
            menu_item: None,
            launcher: None,
            terminal_receipt: false,
            accounting,
            later_replies: 0,
            async_replies: 0,
            failure: None,
        });
    });
    emit("event=configured input=programmatic_responder physical_acceptance=false");
    match origin {
        // Every child declares which actual production entry route the parent must verify.
        NativeTerminationProbeOrigin::MenuAction => emit("event=origin value=menu_action"),
        // The dispatch regression specifically requires its original serial block to return before delegate entry.
        NativeTerminationProbeOrigin::Dispatch => emit("event=origin value=dispatch"),
    }
    let result = super::run_application();
    // Initialization failure or an explicit diagnostic stop must not leave native registration
    // tokens alive merely because the original foreground never became ready.
    detach_readiness_notifications();
    // Initialization failure or explicit loop stop must release this fixture's named server-side authority explicitly.
    super::acceptance_fixture::clear_fixture();
    // A successful NSApplication.terminate never returns through the ordinary run-loop stop path.
    let state = PROBE.take().ok_or("termination probe state disappeared")?;
    result?;
    Err(state
        .failure
        .unwrap_or_else(|| "application run loop returned without terminating the child".into()))
}

/// Open one real foreground after the production delegate, menu and shortcut owner have been installed.
pub(super) fn application_did_finish_launching(mtm: MainThreadMarker) {
    let scenario = PROBE.with_borrow_mut(|slot| {
        let state = slot.as_mut()?;
        if state.started {
            // Duplicate launch notifications cannot open another foreground or schedule another termination.
            return None;
        }
        state.started = true;
        Some(state.scenario)
    });
    let Some(scenario) = scenario else {
        // This cfg-only callback is inert in ordinary product/manual-fixture runs and on duplicate delivery.
        return;
    };
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        // Drain construction temporaries before the later input/quit turn; the probe retains only weak identities.
        autoreleasepool(|_| {
            install_readiness_notifications(mtm);
            open_foreground(scenario, mtm)
        })
    }))
    .unwrap_or_else(|_| {
        // A diagnostic Rust panic must become failure before returning through the native delegate ABI.
        Err("termination probe foreground construction panicked".into())
    });
    if let Err(error) = result {
        // Preserve the first concrete failure; no alternate surface, retry or forced successful close follows.
        fail(error, mtm);
        return;
    }
    // Show returning is not readiness. Inspect actual current facts once after that stack/pool;
    // later pending observations are triggered only by matching real application/window events.
    queue_readiness_check();
}

/// Activate the production catalog/SDK route or production Launcher and publish a weak window identity.
fn open_foreground(
    scenario: NativeTerminationProbeScenario,
    mtm: MainThreadMarker,
) -> Result<(), String> {
    let window = match scenario {
        NativeTerminationProbeScenario::Launcher => {
            // This is the actual global-shortcut foreground constructor, with its ordinary state and controls.
            super::show_launcher_surface(mtm);
            let presentation = super::ACTIVE_SURFACE
                .with_borrow(|slot| {
                    // Clone stack-only presentation claims; native getters run only after this owner borrow ends.
                    slot.as_ref().map(|surface| surface.presentation())
                })
                .ok_or("production Launcher did not acquire a foreground window")?;
            let release = presentation.release_probe()?;
            let window = release
                .panel
                .load()
                .map(|panel| {
                    // The observation keeps only a zeroing weak base-window identity across the native event boundary.
                    let window: &NSWindow = &panel;
                    Weak::from(window)
                })
                .ok_or("production Launcher panel disappeared before weak capture")?;
            PROBE.with_borrow_mut(|slot| {
                if let Some(state) = slot.as_mut() {
                    // Controller/content/input weak evidence is independent from the permitted terminal window shell.
                    state.launcher = Some(release);
                }
            });
            window
        }
        NativeTerminationProbeScenario::BuiltIn(kind) => {
            // The real Host claims the foreground and supplies its production lifecycle and
            // NativeShortcutPreferences. No direct NativeBuiltInSession constructor is available here.
            super::builtin_host::activate(kind, mtm)?;
            super::builtins::acceptance::capture_window(kind)?
        }
        NativeTerminationProbeScenario::MarkdownDirty => {
            // This fixture deliberately exercises the assembled first plugin through its static descriptor and SDK.
            let registration = *crate::composition::plugins::PluginCatalog::new()
                .registrations()
                .first()
                .ok_or("the assembled plugin catalog is empty")?;
            let command = registration
                .descriptor()
                .commands
                .first()
                .ok_or("the assembled plugin descriptor has no command")?;
            super::plugin_host::activate(registration, command.id)
                .map_err(|error| error.to_string())?;
            // The fixed synthetic scenario observes the actual Markdown window; production dispatch has no ID branch.
            let application = NSApplication::sharedApplication(mtm);
            let windows = application.windows();
            if windows.count() > 64 {
                // A standalone synthetic child cannot accept an unexpectedly large unrelated native window inventory.
                return Err(
                    "the termination child exceeded its native window inventory budget".into(),
                );
            }
            let mut selected = None;
            for index in 0..windows.count() {
                // This immutable NSArray snapshot keeps every bounded index valid through title inspection.
                let window = windows.objectAtIndex(index);
                if window.title().isEqualToString(ns_string!("Omni MD")) {
                    // Reject unrelated input-method/system windows rather than assigning them to the plugin.
                    selected = Some(Weak::from_retained(&window));
                    break;
                }
            }
            selected.ok_or("the activated SDK plugin did not publish its Markdown window")?
        }
    };
    PROBE.with_borrow_mut(|slot| {
        // Only zeroing weak ownership survives construction and the native event/pool boundary.
        if let Some(state) = slot.as_mut() {
            // The current driver's scalar observation takes no ownership of the native foreground window.
            state.window = Some(window);
        }
    });
    emit("event=foreground_ready production_owner=true");
    Ok(())
}

/// Commit programmatic input, drain every temporary graph retain, then call the real AppKit termination selector.
fn request_termination(mtm: MainThreadMarker) {
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        // The pool must end before terminate can enter its modal Later wait and demand complete weak release.
        autoreleasepool(|_| prepare_immediate_quit())
    }))
    .unwrap_or_else(|_| {
        // Convert only this diagnostic callback's panic; a native failure is never retried as an empty surface.
        Err("termination probe input preparation panicked".into())
    });
    if let Err(error) = result {
        // A missing responder or unexpected persisted body remains failure with its original fixture intact.
        fail(error, mtm);
        return;
    }
    emit("event=terminate_requested temporary_graph_retains=none");
    let origin = PROBE.with_borrow(|slot| {
        // Copy the selected route before native event posting or termination scheduling can reenter the observer.
        slot.as_ref().map(|state| {
            // Route metadata cannot extend any foreground/native object's lifetime.
            state.origin
        })
    });
    match origin {
        Some(NativeTerminationProbeOrigin::Dispatch) => {
            // This callback really is executing on serial main GCD; the production wrapper must return before Later.
            if let Err(error) = super::termination::request_application_termination() {
                // A rejected scheduler is failure, never an alternative direct terminate: that could mask the regression.
                fail(error, mtm);
                return;
            }
            emit("event=dispatch_request_returned");
        }
        Some(NativeTerminationProbeOrigin::MenuAction) => {
            // Invoke the installed menu item without keyboard replay or a coordinator bypass. This GCD
            // call lacks a mouse event's AppKit pool boundary: performAction may create highlight and
            // accessibility temporaries that otherwise survive into Later. Drain only those local claims;
            // surviving business owners still fail release verification. Only the Rust Result leaves the pool.
            let result = autoreleasepool(|_| perform_quit_menu_action(mtm));
            if let Err(error) = result {
                fail(error, mtm);
            }
        }
        None => {
            // The configured owner must survive through its one selected entry; a missing observer cannot pass.
            fail("the termination probe origin disappeared".into(), mtm);
        }
    }
}

/// Invoke only the installed quit item after verifying both its empty key equivalent and real action.
/// Constructing a second menu would bypass the product interaction this scenario must exercise.
fn perform_quit_menu_action(mtm: MainThreadMarker) -> Result<(), String> {
    let application = NSApplication::sharedApplication(mtm);
    let main = application.mainMenu().ok_or("原应用菜单尚未安装")?;
    if main.numberOfItems() == 0 {
        // An absent installed menu is failure; do not synthesize a substitute quit item.
        return Err("原应用菜单没有实际条目".into());
    }
    let menu = main
        .itemAtIndex(0)
        .and_then(|item| item.submenu())
        .ok_or("原应用退出子菜单缺失")?;
    let index = menu.indexOfItemWithTitle(ns_string!("退出 Omni Bar"));
    if index < 0 || index >= menu.numberOfItems() {
        // Reject a missing localized item before converting the signed AppKit index into an action request.
        return Err("原应用退出项不存在".into());
    }
    let item = menu.itemAtIndex(index).ok_or("原应用退出项已经移除")?;
    if item.keyEquivalent().length() != 0
        || item.action() != Some(sel!(requestOmniTermination:))
        || item.target().is_none()
    {
        // A nonempty chord or changed target/action would exercise a different interaction contract.
        return Err("原退出项仍绑定快捷键或不再拥有正式退出action".into());
    }
    let weak = Weak::from_retained(&item);
    PROBE.with_borrow_mut(|slot| {
        let state = slot.as_mut().ok_or("原菜单退出观察已结束")?;
        if state.origin != NativeTerminationProbeOrigin::MenuAction || state.menu_item.is_some() {
            // The one configured action cannot be replaced by another route or admitted twice.
            return Err("原菜单退出动作不能重复接纳".to_owned());
        }
        state.menu_item = Some(weak);
        Ok(())
    })?;
    emit("event=menu_action_requested key_equivalent_empty=true");
    // AppKit synchronously sends the original action, which queues termination only after returning.
    // The local menu/item claims end here and own no plugin, document or foreground window resources.
    menu.performActionForItemAtIndex(index);
    emit("event=menu_action_returned");
    Ok(())
}

/// Use only the actual native responder and fixed source prefix; no direct core document mutation is available here.
fn prepare_immediate_quit() -> Result<(), String> {
    PROBE.with_borrow(|slot| {
        // This existing later event observes the actual earlier main-queue marker relay exactly once;
        // no native handle, graph getter or completion callback runs under this scalar evidence borrow.
        slot.as_ref()
            .ok_or("the configured native accounting observer is missing")?
            .accounting
            .verify_callback_once()
    })?;
    let (scenario, directory, window) = PROBE
        .with_borrow(|slot| {
            // Clone paths/weak handles before any AppKit method or filesystem operation can reenter this observer.
            slot.as_ref().map(|state| {
                (
                    state.scenario,
                    state.directory.clone(),
                    state.window.clone(),
                )
            })
        })
        .ok_or("the configured termination probe is missing")?;
    if scenario == NativeTerminationProbeScenario::Launcher {
        // Launcher has no document draft; its still-live production owner is the close target.
        return Ok(());
    }
    if let NativeTerminationProbeScenario::BuiltIn(kind) = scenario {
        // UI observations run through the actual activation on a later native turn. Only original
        // weak identities survive; size/focus/editor temporaries drain before requesting Quit.
        let observed = super::builtins::acceptance::verify_ui(kind)?;
        let original = window
            .and_then(|window| window.load())
            .ok_or("内建原窗口在界面检查前已经消失")?;
        let actual = observed.load().ok_or("内建已验证窗口已经消失")?;
        if !std::ptr::eq(&*original, &*actual) {
            return Err("内建界面检查更换了原激活窗口".into());
        }
        let (kind, undo) = match kind {
            crate::BuiltInSurface::Calculator => ("calculator", "verified"),
            crate::BuiltInSurface::UnitConversion => ("units", "verified"),
            crate::BuiltInSurface::Clipboard => ("clipboard", "verified"),
            // Settings has no visible editable field. Keep its real picker focus rather than
            // creating a hidden editor/undo owner solely to manufacture a diagnostic success.
            crate::BuiltInSurface::Settings => ("settings", "not_applicable"),
        };
        emit(&format!(
            "event=builtin_ui kind={kind} undecorated=true resizable=true chinese_title=true resize_geometries=3 input_ready=true session_undo={undo}"
        ));
        return Ok(());
    }
    let window = window
        .and_then(|window| {
            // A released source cannot be revived; an existing source is retained only inside the draining input pool.
            window.load()
        })
        .ok_or("the Markdown window disappeared before synthetic input")?;
    let editor = window
        .initialFirstResponder()
        .and_then(|responder| {
            // Require the paged body installed by the real constructor, excluding field editors and obsolete TextKit paths.
            responder.downcast::<NativeMarkdownPagedTextView>().ok()
        })
        .ok_or("原Markdown初始responder不是正式分页正文")?;
    let before = editor.input_state().map_err(|cause| cause.to_string())?;
    if before.marked.is_some()
        || before.primary_range != NSRange::new(0, 0)
        || before.selection_range_count != 1
    {
        // The declared insertion starts at one empty selection outside composition; any other state is foreign input.
        return Err("原分页正文不是预声明的空选区输入前态".into());
    }
    if !window.makeFirstResponder(Some(&editor))
        || !window.firstResponder().is_some_and(|actual| {
            let expected: &objc2_app_kit::NSResponder = &editor;
            std::ptr::eq(&*actual, expected)
        })
        || editor.input_state().map_err(|cause| cause.to_string())? != before
    {
        // Native focus can reenter: a changed complete Input cannot authorize insertion into a later client.
        return Err("原分页正文焦点或完整输入已经变化".into());
    }
    let client = ProtocolObject::<dyn NSTextInputClient>::from_ref(&*editor);
    let requested = NSRange::new(0, "# Synthetic acceptance note".len());
    let mut actual = NSRange::new(NSNotFound as usize, 0);
    // SAFETY: the public protocol reads only a fixed ASCII prefix; actual is writable stack storage.
    // Neither declared fixture has a surrogate-pair boundary at this request.
    let prefix =
        unsafe { client.attributedSubstringForProposedRange_actualRange(requested, &mut actual) }
            .ok_or("原分页客户没有返回有限夹具前缀")?;
    if actual != requested || editor.input_state().map_err(|cause| cause.to_string())? != before {
        // A shorter range or changed input invalidates identity; do not expand the read or retry against new text.
        return Err("有限夹具前缀并非同一原Input的完整请求范围".into());
    }
    // Materialize only the bounded protocol response, never the paged body's complete document string.
    let prefix = prefix.string();
    let note = if prefix.isEqualToString(ns_string!("# Synthetic acceptance note")) {
        // Directory order is unspecified; accept only the fixed header of a declared synthetic fixture.
        "Acceptance.md"
    } else if prefix.hasPrefix(ns_string!("OmniFrameOldA9\n")) {
        // The second fixed header identifies the independently streamed one-MiB source workload.
        "SourcePerformance.md"
    } else {
        // Unknown content is never redirected into another file or treated as an editable user document.
        return Err("the active document is not one of the declared synthetic fixtures".into());
    };
    drop(prefix);
    let insertion = NSString::from_str(NATIVE_TERMINATION_PROBE_INSERTION);
    if editor.input_state().map_err(|cause| cause.to_string())? != before {
        // NSString construction may invoke native work; recheck the original authority immediately before mutation.
        return Err("原退出插字在Native载荷构造期间已失去输入权限".into());
    }
    // SAFETY: the same paged client accepts NSString and the explicit 0..0 range through its public
    // input protocol. This programmatic insertion proves neither a real IME nor physical keyboard input.
    unsafe { client.insertText_replacementRange(&insertion, NSRange::new(0, 0)) };
    let after = editor.input_state().map_err(|cause| cause.to_string())?;
    // This declared insertion is strictly ASCII, so its byte count is also its exact UTF-16 length.
    let inserted_units = NATIVE_TERMINATION_PROBE_INSERTION.len();
    // Character edits legitimately revoke old folding/layout tickets, even for an empty fold set.
    // Verify the committed user-visible input state instead of requiring internal layout tickets to stay unchanged.
    if after.generation
        != before
            .generation
            .checked_add(1)
            .ok_or("原正文generation耗尽")?
        || after.utf16_len
            != before
                .utf16_len
                .checked_add(inserted_units)
                .ok_or("原正文长度溢出")?
        || after.primary_range != NSRange::new(inserted_units, 0)
        || after.marked.is_some()
        || after.selection_range_count != 1
    {
        // The void callback's first failure is available only on this original client. Report fixed input
        // metadata without replaying insertion, waiting for a successor, reading the body or changing close policy.
        let failure = editor.take_failure();
        return Err(format!(
            "原分页插字未形成准确一次预声明后态 before={before:?} after={after:?} view_failure={failure:?}"
        ));
    }
    let requested = NSRange::new(0, inserted_units);
    let mut actual = NSRange::new(NSNotFound as usize, 0);
    // SAFETY: request only the fixed newly inserted ASCII prefix from the same client; no whole-body getter is used.
    let prefix =
        unsafe { client.attributedSubstringForProposedRange_actualRange(requested, &mut actual) }
            .ok_or("原分页插字没有返回有限实际前缀")?;
    if actual != requested
        || !prefix.string().isEqualToString(&insertion)
        || editor.input_state().map_err(|cause| cause.to_string())? != after
    {
        // Saving must follow exactly this committed prefix and input state; a stale response cannot prove the edit.
        return Err("原分页输入未改变精确源前缀或后态已撤销".into());
    }
    // Inspect only fixed-prefix scratch; even the one-MiB fixture needs no second document-sized body mirror.
    let mut disk = [0_u8; NATIVE_TERMINATION_PROBE_INSERTION.len()];
    std::fs::File::open(directory.join("workspace").join(note))
        .and_then(|mut file| {
            // This independent read descriptor ends before Quit and cannot keep a plugin snapshot/session alive.
            file.read_exact(&mut disk)
        })
        .map_err(|_| "the synthetic document could not be checked before immediate Quit")?;
    if disk == NATIVE_TERMINATION_PROBE_INSERTION.as_bytes() {
        // Already persisted input would fail to exercise save-before-close, so it cannot satisfy this scenario.
        return Err("synthetic draft was already saved before the immediate Quit request".into());
    }
    let weak = Weak::from_retained(&editor);
    PROBE.with_borrow_mut(|slot| {
        // Retain only the original zeroing weak identity; all native responses and strong views end with this input pool.
        if let Some(state) = slot.as_mut() {
            state.editor = Some(weak);
        }
    });
    emit(&format!(
        "event=dirty_input note={note} disk_still_old=true"
    ));
    Ok(())
}

/// Record actions only for the configured menu scenario; normal product/programmatic routes add no native query.
fn has_menu_probe() -> bool {
    PROBE.with_borrow(|slot| {
        // Scalar origin retains no menu, sender or foreground graph and holds no borrow across native calls.
        slot.as_ref()
            .is_some_and(|state| state.origin == NativeTerminationProbeOrigin::MenuAction)
    })
}

/// Require the action sender to be the exact installed menu item; no NSEvent or keyboard origin is fabricated.
pub(super) fn record_menu_action(sender: Option<&AnyObject>) {
    if !has_menu_probe() {
        // Unconfigured manual menu actions are outside this synthetic observation; neither log nor query currentEvent.
        return;
    }
    let item = PROBE.with_borrow(|slot| slot.as_ref().and_then(|state| state.menu_item.clone()));
    let delivered = item.and_then(|item| item.load()).is_some_and(|item| {
        let expected: &AnyObject = &item;
        sender.is_some_and(|sender| std::ptr::eq(expected, sender))
    });
    emit(&format!(
        "event=menu_action original_item_matches={delivered}"
    ));
}

/// Entry scheduling returned; the parent requires this observation before the standard delegate termination reply.
pub(super) fn record_menu_request_returned() {
    if has_menu_probe() {
        // Copy one fixed scalar without closing, checking resources or starting another termination.
        emit("event=menu_request_returned");
    }
}

/// Record the actual production delegate's selected response without invoking or changing its close decision.
pub(super) fn record_delegate_reply(reply: NSApplicationTerminateReply) {
    let installed = PROBE.with_borrow_mut(|slot| {
        let Some(state) = slot.as_mut() else {
            // Production/manual acceptance has no diagnostic observer or additional work.
            return false;
        };
        if reply == NSApplicationTerminateReply::TerminateLater {
            // Count real Later obligations so the parent rejects duplicate or missing async completion.
            state.later_replies = state.later_replies.saturating_add(1);
        }
        true
    });
    if installed {
        // Both actual entry routes now share one quit transaction; a later currentEvent cannot invent its origin.
        // Unknown native replies cannot be disguised as a successful Later transaction.
        let value = if reply == NSApplicationTerminateReply::TerminateLater {
            // The actual modal wait must be followed by the existing strict release-authorized async reply.
            "later"
        } else if reply == NSApplicationTerminateReply::TerminateNow {
            // A synchronous outcome is exposed verbatim, even though these active-foreground scenarios require Later.
            "now"
        } else {
            // Refusal is preserved verbatim; the parent must not interpret a cancelled close as successful exit.
            "cancel"
        };
        emit(&format!("event=delegate_reply value={value}"));
    }
}

/// Observe the exact post-native/SDK-Drop/barrier point immediately before the one public AppKit async reply.
pub(super) fn record_async_reply() {
    let installed = PROBE.with_borrow_mut(|slot| {
        let Some(state) = slot.as_mut() else {
            // An absent opt-in driver cannot retain state or emit output in ordinary product runs.
            return false;
        };
        state.async_replies = state.async_replies.saturating_add(1);
        true
    });
    if installed {
        // The production release hook follows exact terminal payload verification, SDK session Drop and Host barrier consumption.
        emit(&format!(
            "event=async_reply foreground_released={} sdk_session_owned={}",
            super::termination::probe_foreground_released(),
            super::plugin_host::exists()
        ));
    }
}

/// Record only the exact validated terminal release receipt after native payload and Host owners end.
pub(super) fn record_terminal_release() {
    PROBE.with_borrow_mut(|slot| {
        if let Some(state) = slot.as_mut() {
            // Ordinary close or a queued quit cannot reach this explicit post-verification receipt hook.
            state.terminal_receipt = true;
        }
    });
}

/// Independently observe that the original paged client closed. The separate full SDK receipt still
/// proves document, source, font and worker release; an inert view alone cannot establish those facts.
fn observe_terminated_editor(editor: &Weak<NativeMarkdownPagedTextView>) -> (bool, &'static str) {
    let Some(editor) = editor.load() else {
        // The original weak identity ended; a new or replacement control cannot substitute for this observation.
        return (true, "nil");
    };
    let client = ProtocolObject::<dyn NSTextInputClient>::from_ref(&*editor);
    let selection = client.selectedRange();
    let selection_empty = selection.length == 0
        && (selection.location == 0 || selection.location == NSNotFound as usize);
    // SAFETY: upgrade the original weak identity only in this pool. These three public view/responder
    // getters compare nil links and cannot reconstruct a document or input owner.
    let links_detached = unsafe {
        let window: *mut objc2::runtime::AnyObject = msg_send![&*editor, window];
        let parent: *mut objc2::runtime::AnyObject = msg_send![&*editor, superview];
        let responder: *mut objc2::runtime::AnyObject = msg_send![&*editor, nextResponder];
        window.is_null() && parent.is_null() && responder.is_null()
    };
    // Revoked means lost foreground authority, whereas Closed plus empty CPU-owner slots proves closure.
    // Closed inputContext returns None without creating a context; do not inspect obsolete TextKit text/layout/history.
    let inert = matches!(
        editor.input_state(),
        Err(NativeMarkdownPagedTextViewError::Closed)
    ) && !editor.session_is_current()
        && editor.pixel_observer().is_none()
        && editor.cpu_layer_weaks().is_none()
        && editor.cpu_paints().is_none()
        && editor.inputContext().is_none()
        && !client.hasMarkedText()
        && selection_empty
        && links_detached;
    if inert {
        // Report a framework-retained empty object separately from deallocation; it cannot pass without the SDK receipt.
        (true, "inert")
    } else {
        // Residual text, edit/history authority, delegates or hierarchy still fail even when the process is about to exit.
        (false, "attached")
    }
}

/// At actual exit, independently inspect payload identities and distinguish window leases from text-shell observations.
pub(super) fn application_will_terminate() {
    // Normal probe Quit already removed these registrations at readiness. An independent OS
    // termination still releases the exact observer tokens without inventing missing probe steps.
    detach_readiness_notifications();
    let snapshot = PROBE.with_borrow(|slot| {
        // End observer borrowing before weak upgrades or native getters can execute callbacks.
        slot.as_ref().map(|state| {
            (
                state.window.clone(),
                state.editor.clone(),
                state.launcher.clone(),
                state.terminal_receipt,
                state.scenario,
                state.later_replies,
                state.async_replies,
            )
        })
    });
    let Some((window, editor, launcher, terminal_receipt, scenario, later, replies)) = snapshot
    else {
        // Ordinary product termination has no diagnostic state or synthetic output obligation.
        return;
    };
    let (shell_valid, lease, payload_clear, editor_payload) = autoreleasepool(|_| {
        let Some(window) = window else {
            // Missing captured evidence cannot masquerade as a fully deallocated or detached original window.
            return (false, "unobserved", false, "unobserved");
        };
        let panel = window.load();
        let shell = super::native_deallocation::verify_terminal_window(
            panel.as_deref(),
            terminal_receipt,
            !super::termination::probe_foreground_released(),
        );
        let lease = if panel.is_some() {
            // This surviving weak identity is explicitly an AppKit framework lease, never claimed deallocated.
            "pending"
        } else {
            // Actual nil is reported separately from the valid terminal shell case.
            "none"
        };
        let (payload_clear, editor_payload) = match scenario {
            NativeTerminationProbeScenario::Launcher => {
                // All required Launcher payload identities must be captured and actually nil at WillTerminate.
                (
                    editor.is_none()
                        && launcher.is_some_and(|launcher| launcher.payload_is_released()),
                    "not_applicable",
                )
            }
            NativeTerminationProbeScenario::BuiltIn(_) => {
                // Built-in completion comes from its actual role cohort, strict field-editor
                // boundary and consumed Host barrier. Do not substitute a fabricated probe graph.
                (
                    editor.is_none()
                        && launcher.is_none()
                        && terminal_receipt
                        && !super::builtins::is_open(),
                    "not_applicable",
                )
            }
            NativeTerminationProbeScenario::MarkdownDirty => {
                let (editor_clear, observation) = match editor {
                    // Observe the identity captured during actual input and recheck each public noncreating fact if it survives.
                    Some(editor) => observe_terminated_editor(&editor),
                    // Missing capture evidence never means deallocation; it always fails.
                    None => (false, "unobserved"),
                };
                // Require both independent observation and the full plugin terminal receipt; SDK session/barrier follow production paths.
                (
                    launcher.is_none() && terminal_receipt && editor_clear,
                    observation,
                )
            }
        };
        (shell.is_ok(), lease, payload_clear, editor_payload)
    });
    emit(&format!(
        "event=will_terminate terminal_receipt={terminal_receipt} payload_clear={payload_clear} editor_payload={editor_payload} shell_valid={shell_valid} framework_window_lease={lease} foreground_released={} later_replies={later} async_replies={replies}",
        super::termination::probe_foreground_released()
    ));
}

/// Stop an unsuccessful child without forcing a saved/closed result; the parent preserves its concrete evidence.
fn fail(error: String, mtm: MainThreadMarker) {
    emit(&format!("event=failure reason={error}"));
    PROBE.with_borrow_mut(|slot| {
        if let Some(state) = slot.as_mut() {
            // The first failure is retained; later callbacks cannot replace it with a success or guessed cleanup.
            state.failure.get_or_insert(error);
            state.readiness_check_pending = false;
            state.readiness_changed_during_check = false;
        }
    });
    // Revoke notification authority before stopping the loop. A queued scalar check observes the
    // original failure and cannot restart readiness, focus, or a termination action afterward.
    detach_readiness_notifications();
    NSApplication::sharedApplication(mtm).stop(None);
}
