//! Opt-in production-loop termination evidence over a fresh, parent-owned synthetic fixture.
//!
//! The driver owns weak native identities and scalar observations only. Actual save, scope-specific
//! native payload release, SDK-session Drop and the foreground barrier retain their production implementation.

use std::cell::RefCell;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use objc2::rc::{Weak, autoreleasepool};
use objc2::{MainThreadMarker, msg_send};
use objc2_app_kit::{
    NSApplication, NSApplicationTerminateReply, NSEvent, NSEventModifierFlags, NSEventType,
    NSTextInputClient, NSTextView, NSWindow,
};
use objc2_foundation::{NSNotFound, NSPoint, NSRange, NSString, ns_string};

/// Two explicit synthetic scenarios run in separate processes, each with a fresh NSApplication.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NativeTerminationProbeScenario {
    /// Terminate with the actual Launcher still owning its foreground graph.
    Launcher,
    /// Insert into the actual SDK Markdown responder and immediately terminate before autosave can run.
    MarkdownDirty,
}

/// Separate ordinary app-local menu delivery from a programmatic quit requested on serial main dispatch.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NativeTerminationProbeOrigin {
    /// AppKit consumes a synthetic Command-Q event through its real application/menu event route.
    MenuEvent,
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
    window: Option<Weak<NSWindow>>,
    editor: Option<Weak<NSTextView>>,
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
    PROBE.with_borrow_mut(|slot| {
        // Publish only complete authorities; startup itself remains the production delegate's launch callback.
        *slot = Some(ProbeState {
            scenario,
            origin,
            directory: directory.to_owned(),
            started: false,
            window: None,
            editor: None,
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
        NativeTerminationProbeOrigin::MenuEvent => emit("event=origin value=menu_event"),
        // The dispatch regression specifically requires its original serial block to return before delegate entry.
        NativeTerminationProbeOrigin::Dispatch => emit("event=origin value=dispatch"),
    }
    let result = super::run_application();
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
        autoreleasepool(|_| open_foreground(scenario, mtm))
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
    dispatch2::DispatchQueue::main().exec_async(|| {
        // A separate actual main-queue turn lets launch/construction callbacks and their pools return first.
        let mtm = MainThreadMarker::new().expect("the dispatch main queue is the AppKit thread");
        request_termination(mtm);
    });
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
        Some(NativeTerminationProbeOrigin::MenuEvent) => {
            // An app-local event exercises standard Quit routing without a global event tap, TCC or physical-input claim.
            if let Err(error) = post_quit_menu_event(mtm) {
                // Missing event construction/window identity stays a failed observation; no direct-call fallback exists.
                fail(error, mtm);
                return;
            }
            emit("event=menu_event_posted");
        }
        None => {
            // The configured owner must survive through its one selected entry; a missing observer cannot pass.
            fail("the termination probe origin disappeared".into(), mtm);
        }
    }
}

/// Post one synthetic Command-Q into this application's event queue, with no system-wide event injection.
fn post_quit_menu_event(mtm: MainThreadMarker) -> Result<(), String> {
    let weak = PROBE.with_borrow(|slot| {
        // Copy a zeroing weak handle before its native upgrade, leaving no observer borrow across AppKit effects.
        slot.as_ref().and_then(|state| {
            // A missing observed window remains None and rejects event construction below.
            state.window.clone()
        })
    });
    let number = weak
        .and_then(|window| {
            // Upgrade only long enough to read the window number; no native graph is captured by the event.
            window.load()
        })
        .ok_or("the foreground window disappeared before menu event construction")?
        .windowNumber();
    // The window upgrade ends at the statement above; only a scalar native window number enters the event.
    let event = NSEvent::keyEventWithType_location_modifierFlags_timestamp_windowNumber_context_characters_charactersIgnoringModifiers_isARepeat_keyCode(
        NSEventType::KeyDown,
        NSPoint::new(0.0, 0.0),
        NSEventModifierFlags::Command,
        0.0,
        number,
        None,
        ns_string!("q"),
        ns_string!("q"),
        false,
        12,
    )
    .ok_or("AppKit could not construct the app-local synthetic Quit event")?;
    NSApplication::sharedApplication(mtm).postEvent_atStart(&event, false);
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
    let window = window
        .and_then(|window| {
            // A released source cannot be revived; an existing source is retained only inside the draining input pool.
            window.load()
        })
        .ok_or("the Markdown window disappeared before synthetic input")?;
    let editor = window
        .initialFirstResponder()
        .and_then(|responder| {
            // The actual constructor's initial source responder must be NSTextView, not a system field editor.
            responder.downcast::<NSTextView>().ok()
        })
        .ok_or("the Markdown initial responder is not a source text view")?;
    if editor.isFieldEditor() || !window.makeFirstResponder(Some(&editor)) {
        // A rejected responder transition cannot establish that inserted text reached the active document.
        return Err("the actual source text view could not become first responder".into());
    }
    let source = editor.string();
    let note = if source.hasPrefix(ns_string!("# Synthetic acceptance note")) {
        // Directory enumeration may choose either declared fixture note; identify it without a full body copy.
        "Acceptance.md"
    } else if source.hasPrefix(ns_string!("OmniFrameOldA9\n")) {
        // The one-MiB workload remains supported without assuming filesystem enumeration order.
        "SourcePerformance.md"
    } else {
        // An unexpected document cannot be mutated or treated as a successful synthetic-data observation.
        return Err("the active document is not one of the declared synthetic fixtures".into());
    };
    drop(source);
    editor.setSelectedRange(NSRange::new(0, 0));
    let insertion = NSString::from_str(NATIVE_TERMINATION_PROBE_INSERTION);
    // SAFETY: NSTextInputClient's public insertText:replacementRange: accepts NSString. The live
    // main-thread source responder is retained here; 0..0 is a valid scalar-aligned empty source range.
    // This opt-in synthetic driver impersonates input delivery and makes no physical-keyboard claim.
    unsafe { editor.insertText_replacementRange(&insertion, NSRange::new(0, 0)) };
    if !editor.string().hasPrefix(&insertion) {
        // Native callback delivery must actually alter the canonical-backed source before Quit can be exercised.
        return Err("native insertText did not change the source prefix".into());
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
    PROBE.with_borrow_mut(|slot| {
        // Keep only the actual editor's zeroing weak identity for the final native-release assertion.
        if let Some(state) = slot.as_mut() {
            // Verification may upgrade it once later, but the observer never retains the text view between callbacks.
            state.editor = Some(Weak::from_retained(&editor));
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
            .is_some_and(|state| state.origin == NativeTerminationProbeOrigin::MenuEvent)
    })
}

/// Verify that the actual menu action is driven by the parent observer's Command-Q event.
pub(super) fn record_menu_action() {
    if !has_menu_probe() {
        // Unconfigured manual menu actions are outside this synthetic observation; neither log nor query currentEvent.
        return;
    }
    let mtm = MainThreadMarker::new().expect("menu actions use the AppKit thread");
    let delivered = NSApplication::sharedApplication(mtm)
        .currentEvent()
        .is_some_and(|event| {
            // Require the original app-local event; a later direct quit-selector call cannot impersonate menu delivery.
            event.r#type() == NSEventType::KeyDown
                && event.keyCode() == 12
                && event
                    .modifierFlags()
                    .contains(NSEventModifierFlags::Command)
        });
    emit(&format!(
        "event=menu_action current_event_matches={delivered}"
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
    let origin = PROBE.with_borrow_mut(|slot| {
        let Some(state) = slot.as_mut() else {
            // Production/manual acceptance has no diagnostic observer or additional work.
            return None;
        };
        if reply == NSApplicationTerminateReply::TerminateLater {
            // Count real Later obligations so the parent rejects duplicate or missing async completion.
            state.later_replies = state.later_replies.saturating_add(1);
        }
        Some(state.origin)
    });
    if let Some(origin) = origin {
        if origin == NativeTerminationProbeOrigin::MenuEvent {
            // Observe the native event being dispatched when the standard menu actually enters its delegate.
            let mtm =
                MainThreadMarker::new().expect("the application delegate uses the AppKit thread");
            let delivered = NSApplication::sharedApplication(mtm)
                .currentEvent()
                .is_some_and(|event| {
                    // Only this exact app-local key-down qualifies; a guessed direct selector call cannot satisfy it.
                    event.r#type() == NSEventType::KeyDown
                        && event.keyCode() == 12
                        && event
                            .modifierFlags()
                            .contains(NSEventModifierFlags::Command)
                });
            emit(&format!(
                "event=menu_delivery current_event_matches={delivered}"
            ));
        }
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

/// Independently inspect the edited text identity; the complete SDK receipt still proves private capacity/TextKit cleanup.
fn observe_terminated_editor(editor: &Weak<NSTextView>) -> (bool, &'static str) {
    let Some(editor) = editor.load() else {
        // The original weak identity ended; a new or replacement control cannot substitute for this observation.
        return (true, "nil");
    };
    // Query existing text's public state only, avoiding potentially creating layoutManager/undoManager getters.
    // Inspect no plugin-private fields and waive no class; the complete terminal receipt separately proves its sealed type.
    let selection = editor.selectedRange();
    let selection_empty = selection.length == 0
        && (selection.location == 0 || selection.location == NSNotFound as usize);
    // SAFETY: A main-thread Weak upgrade protects the captured NSTextView; the three public getters take no arguments
    // and return borrowed object pointers compared with nil, without retaining/dereferencing or recreating ended parent owners.
    let links_detached = unsafe {
        let window: *mut objc2::runtime::AnyObject = msg_send![&*editor, window];
        let parent: *mut objc2::runtime::AnyObject = msg_send![&*editor, superview];
        let responder: *mut objc2::runtime::AnyObject = msg_send![&*editor, nextResponder];
        window.is_null() && parent.is_null() && responder.is_null()
    };
    let inert = !editor.isEditable()
        && !editor.isSelectable()
        && !editor.allowsUndo()
        && editor.delegate().is_none()
        && editor.string().length() == 0
        && !editor.hasMarkedText()
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
        }
    });
    NSApplication::sharedApplication(mtm).stop(None);
}
