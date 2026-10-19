//! Launcher barrier publication follows actual native deallocation events, never elapsed queue turns.

use std::cell::{Cell, RefCell};

use dispatch2::DispatchQueue;
use objc2::MainThreadMarker;
use objc2::rc::autoreleasepool;
use omni_plugin_sdk::PluginCloseScope;

use super::launcher_results::NativeLauncherSelection;
use super::launcher_ui::LauncherReleaseProbe;
use super::native_deallocation::{
    NativeDeallocationCohort, TerminalReleaseAuthority, verify_terminal_window,
};
use crate::ForegroundSurface;
use crate::application::termination::TerminationTicket;

thread_local! {
    /// Each accepted close owns scalar scope until its exact event-based completion consumes the barrier.
    static SCOPE: Cell<PluginCloseScope> = const { Cell::new(PluginCloseScope::ForegroundTransition) };
    /// Accepted terminal scope belongs to exactly one delegate transaction, never an eventual retry.
    static TERMINAL_TICKET: Cell<Option<TerminationTicket>> = const { Cell::new(None) };
    /// Generation identity prevents an old queued native relay from finalizing a later Launcher instance.
    static GENERATION: Cell<u64> = const { Cell::new(0) };
    /// Only finite weak evidence and one owned catalog action survive synchronous unique-owner destruction.
    static FINALIZING: RefCell<Option<Completion>> = const { RefCell::new(None) };
    /// Inventory capture may invoke framework getters; reentrant Quit cannot interrupt this accepted local stack.
    static CAPTURING: Cell<bool> = const { Cell::new(false) };
    /// Native getter callbacks during strict verification cannot initiate an unsupported scope promotion.
    static VERIFYING: Cell<bool> = const { Cell::new(false) };
}

/// A close retains no native graph while the runtime delivers its real final deallocation event.
struct Completion {
    generation: u64,
    scope: PluginCloseScope,
    terminal_ticket: Option<TerminationTicket>,
    full: NativeDeallocationCohort,
    payload: NativeDeallocationCohort,
    terminal: TerminalReleaseAuthority,
    weak: LauncherReleaseProbe,
    close_notified: bool,
    pending_action: Option<NativeLauncherSelection>,
}

/// Mark bounded native capture as busy without carrying a native owner or state borrow through getters.
pub(super) fn capture_preflight<T>(work: impl FnOnce() -> T) -> T {
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            // Unwinding clears only scalar reentry authority, never completing a native graph or barrier.
            CAPTURING.set(false);
        }
    }
    CAPTURING.set(true);
    let _reset = Reset;
    work()
}

/// Prevent nested modal termination while inventory/verification has a native callback on its stack.
pub(super) fn is_call_in_flight() -> bool {
    CAPTURING.get() || VERIFYING.get()
}

/// Reserve one ordinary close before the surface is published as Closing and native callbacks begin.
pub(super) fn begin_close() {
    // Foreground state already forbids a competing owner; no old weak-only transaction may be overwritten.
    assert!(FINALIZING.with_borrow(Option::is_none));
    SCOPE.set(PluginCloseScope::ForegroundTransition);
    TERMINAL_TICKET.set(None);
}

/// Record the next actual-close epoch for the probe without modifying or consuming close authority.
#[cfg(feature = "native-acceptance")]
pub(super) fn probe_generation() -> u64 {
    GENERATION.get()
}

/// Failed cleanup waits only for foreground close after actual will-close, not for a preflight Destroying claim.
#[cfg(feature = "native-product-acceptance")]
pub(super) fn product_probe_accepted_close_generation() -> Option<u64> {
    let transition_matches = super::FOREGROUND_STATE.with_borrow(|state| {
        state.pending_transition()
            == Some(crate::SurfaceTransition::Destroying {
                surface: ForegroundSurface::Launcher,
                restore_launcher: false,
            })
    });
    if !transition_matches || SCOPE.get() != PluginCloseScope::ForegroundTransition {
        // Do not await foreground release after refused-close rollback, terminal promotion or an already consumed barrier.
        return None;
    }
    let finalizing = FINALIZING.with_borrow(|slot| {
        slot.as_ref()
            .filter(|completion| {
                // Only actual will-close and an established completion in the same scope identify a forthcoming receipt.
                completion.close_notified
                    && completion.scope == PluginCloseScope::ForegroundTransition
            })
            .map(|completion| completion.generation)
    });
    if finalizing.is_some() {
        return finalizing;
    }
    let notified = super::CLOSING_SURFACE.with_borrow(|slot| {
        // close_was_notified reads a controller Cell only, sending no native message and copying no graph.
        slot.as_ref()
            .is_some_and(|surface| surface.close_was_notified())
    });
    if notified {
        // Before finalize installs completion, the notified original graph still identifies the sole next generation.
        GENERATION.get().checked_add(1)
    } else {
        None // Refused preflight before close has no forthcoming will-close to await.
    }
}

/// Initiate or monotonically promote close only for an actually accepted application quit.
pub(super) fn request_termination_close() -> Result<(), String> {
    if !super::termination::application_termination_pending() {
        // Queued Quit reservation alone cannot consume an empty framework-window lease.
        return Err("启动器的最终关闭需要先接受应用退出请求".into());
    }
    if TERMINAL_TICKET.get().is_some()
        && TERMINAL_TICKET.get() != super::termination::application_termination_ticket()
    {
        // A cancelled preparation cannot transfer its empty-shell authority into another explicit Quit retry.
        return Err("启动器的最终关闭属于另一次应用退出事务".into());
    }
    if SCOPE.get() == PluginCloseScope::ApplicationTermination {
        // A repeated accepted terminal request owns the same one completion, including during its verification.
        return Ok(());
    }
    if VERIFYING.get() {
        // Stack-local strict verification cannot transfer its completion owner through a reentrant native getter.
        return Err("启动器关闭验证正在进行".into());
    }
    super::try_request_launcher_close()?;
    let closing = super::FOREGROUND_STATE.with_borrow(|state| {
        // Pure copied lifecycle state must still name the same Launcher destruction transaction.
        state.active_surface() == Some(ForegroundSurface::Launcher)
            && matches!(
                state.pending_transition(),
                Some(crate::SurfaceTransition::Destroying {
                    surface: ForegroundSurface::Launcher,
                    ..
                })
            )
    });
    if !closing {
        // Failed close admission preserves its usable owner; terminal scope must not manufacture readiness.
        return Err("启动器尚未接受本次应用退出关闭".into());
    }
    SCOPE.set(PluginCloseScope::ApplicationTermination);
    TERMINAL_TICKET.set(super::termination::application_termination_ticket());
    let ready = FINALIZING.with_borrow_mut(|slot| {
        // Before the deferred finalizer exists, the copied scope/ticket above will be adopted when it is constructed.
        let completion = slot.as_mut()?;
        completion.scope = PluginCloseScope::ApplicationTermination;
        completion.terminal_ticket = TERMINAL_TICKET.get();
        // A prior real payload event can be consumed after promotion, but never guessed from elapsed time.
        completion
            .terminal
            .reserve(completion.scope, completion.payload.has_completed())
            .then_some(completion.generation)
    });
    if let Some(generation) = ready {
        // Promotion consumes an already observed event. The scalar-only relay leaves reentrant preflight first.
        DispatchQueue::main()
            .exec_async(move || finish(generation, PluginCloseScope::ApplicationTermination));
    }
    Ok(())
}

/// Install both actual-release authorities before releasing any owned native object or local pool.
pub(super) fn finalize() {
    let completion = autoreleasepool(|_| {
        let surface = super::CLOSING_SURFACE.take()?;
        let mtm =
            MainThreadMarker::new().expect("Launcher close finalization uses the main thread");
        let generation = GENERATION
            .get()
            .checked_add(1)
            .expect("Launcher close generation exhausted");
        GENERATION.set(generation);
        let weak = surface
            .release_probe()
            .expect("accepted Launcher close captures its complete payload before teardown");
        let mut full = NativeDeallocationCohort::new(mtm);
        let mut payload = NativeDeallocationCohort::new(mtm);
        full.track(&*surface.panel, mtm);
        // Both cohorts observe explicit ledger roles only; borrowed field editors have separate public-detachment facts.
        weak.payload.track_remaining(&mut full, mtm);
        weak.payload.track_remaining(&mut payload, mtm);
        let pending_action = surface.controller.take_pending_action();
        let close_notified = surface.close_was_notified();
        surface.release_native_ownership();
        drop(surface);
        full.seal();
        payload.seal();
        Some(Completion {
            generation,
            scope: SCOPE.get(),
            terminal_ticket: TERMINAL_TICKET.get(),
            full,
            payload,
            terminal: TerminalReleaseAuthority::default(),
            weak,
            close_notified,
            pending_action,
        })
    });
    let Some(completion) = completion else {
        // A duplicate will-close notification has no owner or release authority left to consume.
        return;
    };
    let generation = completion.generation;
    completion
        .full
        .when_released(move || finish(generation, PluginCloseScope::ForegroundTransition));
    completion
        .payload
        .when_released(move || payload_released(generation));
    // Registration and local pool drain finish before either main-queue release relay can execute.
    FINALIZING.with_borrow_mut(|slot| *slot = Some(completion));
}

/// Remember the actual payload event even if an ordinary close still awaits its base panel.
fn payload_released(generation: u64) {
    let ready = FINALIZING.with_borrow_mut(|slot| {
        // A full-graph callback may already have consumed this operation; absence gives this late event no authority.
        let completion = slot.as_mut()?;
        if completion.generation != generation {
            // An expired native relay cannot annotate a newer transaction's scalar release evidence.
            return None;
        }
        completion
            .terminal
            .reserve(completion.scope, completion.payload.has_completed())
            .then_some(generation)
    });
    if let Some(generation) = ready {
        // This is the actual final deallocation relay; there is no sleep, polling or retry between proof and receipt.
        finish(generation, PluginCloseScope::ApplicationTermination);
    }
}

/// Validate one exact scope-specific native proof before publishing the reusable or terminal Host barrier.
fn finish(generation: u64, boundary: PluginCloseScope) {
    let Some(mut completion) = FINALIZING.take() else {
        // A late duplicate cannot revive an already consumed foreground generation.
        return;
    };
    if completion.generation != generation || completion.scope != boundary {
        // Promotion revokes old full-graph callback authority without redirecting the original catalog action.
        FINALIZING.with_borrow_mut(|slot| *slot = Some(completion));
        return;
    }
    VERIFYING.set(true);
    let verified = autoreleasepool(|_| {
        if !completion.payload.has_completed() || !completion.weak.payload_is_released() {
            // Any surviving project role or editor relationship keeps Closing; cohort counts alone are insufficient proof.
            return Err(completion.weak.failure_description());
        }
        match completion.scope {
            PluginCloseScope::ForegroundTransition => {
                if !completion.full.has_completed() || !completion.weak.is_released() {
                    // An ordinary successor is forbidden while even the base native panel remains alive.
                    return Err("启动器原生对象图尚未全部析构".to_owned());
                }
                Ok(false)
            }
            PluginCloseScope::ApplicationTermination => {
                if completion.terminal_ticket.is_none()
                    || completion.terminal_ticket
                        != super::termination::application_termination_ticket()
                {
                    // Terminal shell evidence cannot authorize a normal foreground transition after quit cancellation.
                    return Err(
                        "Launcher terminal completion lost its pending quit authority".to_owned(),
                    );
                }
                let owner_present = super::ACTIVE_SURFACE.with_borrow(Option::is_some)
                    || super::CLOSING_SURFACE.with_borrow(Option::is_some);
                let panel = completion.weak.panel.load();
                let lease = verify_terminal_window(
                    panel
                        .as_deref()
                        .map(|panel| -> &objc2_app_kit::NSWindow { panel }),
                    completion.close_notified,
                    owner_present,
                )?;
                // Revoke all callback captures before removing only our marker from the framework-held panel shell.
                completion.full.cancel_observation(panel.as_deref());
                Ok(lease.framework_panel_pending)
            }
        }
    });
    VERIFYING.set(false);
    let pending_framework_panel = match verified {
        Ok(pending) => pending,
        Err(error) => {
            // Preserve honest Closing state and evidence; no failed strict check is retried or treated as success.
            eprintln!("Launcher native release verification failed: {error}");
            FINALIZING.with_borrow_mut(|slot| *slot = Some(completion));
            return;
        }
    };
    let lifecycle = super::FOREGROUND_STATE.with_borrow(|state| {
        // Pure barrier publication occurs only after weak upgrades and their local autorelease pool ended.
        state.complete_destruction(ForegroundSurface::Launcher)?;
        state.consume_destroyed_barrier(ForegroundSurface::Launcher)
    });
    if let Err(error) = lifecycle {
        // No successor or termination reply can follow a mismatched logical barrier.
        eprintln!("Launcher destruction lifecycle failed: {error}");
        return;
    }
    if completion.scope == PluginCloseScope::ApplicationTermination {
        // A framework lease is reported explicitly; it is never called complete window deallocation.
        eprintln!(
            "Launcher terminal payload released framework_window_lease_pending={pending_framework_panel} release_us={}",
            completion.payload.elapsed().as_micros()
        );
    }
    #[cfg(feature = "native-acceptance")]
    if completion.scope == PluginCloseScope::ApplicationTermination {
        // The probe records only this already validated terminal native/Host barrier receipt.
        super::acceptance_termination::record_terminal_release();
    }
    let pending_action = completion.pending_action.take();
    drop(completion);
    #[cfg(feature = "native-acceptance")]
    if boundary == PluginCloseScope::ForegroundTransition {
        // Notify the probe only after release proof, barrier consumption and completion drop, without changing the close gate.
        super::runtime_probes::launcher_probe_foreground_released(generation);
        #[cfg(feature = "native-product-acceptance")]
        {
            // Share the real release receipt; product probes cannot consume the barrier early or advance from a terminal shell.
            super::acceptance_product::launcher_released(generation);
        }
    }
    if super::termination::foreground_did_release() {
        // Exact SDK/Host-free termination consumes the slot and suppresses the previously selected successor.
        return;
    }
    if let Some(selection) = pending_action {
        // Ordinary action dispatch happens only after every old native role has actually deallocated.
        super::activate_launcher_selection(selection);
    }
}
