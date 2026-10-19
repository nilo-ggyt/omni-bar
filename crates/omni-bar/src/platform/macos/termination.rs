//! Main-thread application termination through existing generic foreground save/release contracts.
//!
//! State borrows end before plugin/AppKit effects. Only an empty adapter set and consumed foreground
//! barrier authorize termination; a method's accepted-close result alone never does.

use std::cell::{Cell, RefCell};

use objc2::MainThreadMarker;
use objc2_app_kit::{NSApplication, NSApplicationTerminateReply};

use crate::ForegroundSurface;
use crate::application::termination::{
    ApplicationTermination, TerminationOwnership, TerminationReadiness, TerminationReply,
    TerminationRequest, TerminationTicket,
};

thread_local! {
    /// Process-owned scalar coordination never retains a foreground graph or schedules idle work.
    static TERMINATION: RefCell<ApplicationTermination> = const {
        RefCell::new(ApplicationTermination::new())
    };
    /// Menu/programmatic entry reserves scalar quit intent only; the actual delegate grants terminal scope.
    static QUEUED_REQUEST_PENDING: Cell<bool> = const { Cell::new(false) };
}

/// Gate shortcut, action and successor creation from the first protected preparation until cancellation/exit.
pub(super) fn suppress_successors() -> bool {
    QUEUED_REQUEST_PENDING.get()
        || TERMINATION.with_borrow(ApplicationTermination::suppress_successors)
}

/// Authorize terminal SDK receipts only after the actual delegate has accepted a quit transaction.
pub(super) fn application_termination_pending() -> bool {
    // Menu/programmatic reservations gate activation only and grant no delegate preparation/reply authority.
    // Allowed also has no remaining matching session generation.
    application_termination_ticket().is_some()
}

/// Copy the actual accepted quit identity without exposing a state borrow across native or SDK effects.
pub(super) fn application_termination_ticket() -> Option<TerminationTicket> {
    TERMINATION.with_borrow(ApplicationTermination::transaction_ticket)
}

/// Return from the menu action/main-dispatch stack before one common-mode block enters standard terminate:.
///
/// Menu events may retain temporary foreground objects; a live serial-dispatch stack can also block later release work.
/// Do not cancel/reissue quit; direct OS delegate requests retain the same coordinator for save, veto and release decisions.
pub(super) fn request_application_termination() -> Result<(), String> {
    // Reject a foreign caller before accessing another thread's scalar TLS or the AppKit application.
    let _mtm = MainThreadMarker::new()
        .ok_or("application termination scheduling requires the AppKit main thread")?;
    if suppress_successors() {
        // An already queued/preparing/waiting/allowed quit owns the request; repeated dispatch cannot add another wait.
        return Ok(());
    }
    let run_loop = objc2_core_foundation::CFRunLoop::main()
        .ok_or("the application main run loop is unavailable")?;
    // SAFETY: CoreFoundation exports this immutable process-lifetime CFString pseudo-mode constant.
    let mode = unsafe { objc2_core_foundation::kCFRunLoopCommonModes }
        .ok_or("the application common run-loop modes are unavailable")?;
    QUEUED_REQUEST_PENDING.set(true);
    let callback = block2::RcBlock::new(|| {
        if !QUEUED_REQUEST_PENDING.replace(false) {
            // A stale/cancelled copied block cannot initiate another application termination transaction.
            return;
        }
        if TERMINATION.with_borrow(ApplicationTermination::suppress_successors) {
            // A direct OS request may already own termination; preserve its sole reply obligation without another terminate: call.
            return;
        }
        let mtm =
            MainThreadMarker::new().expect("the main CFRunLoop executes on the AppKit thread");
        // No dispatch-main block or retained foreground/native handle remains on this entry's callback stack.
        NSApplication::sharedApplication(mtm).terminate(None);
    });
    // SAFETY: the main-thread owner passes the actual main run loop, a valid immutable common-mode
    // CFString and a live heap block. CFRunLoopPerformBlock copies it; it captures no borrowed data,
    // native handle or graph, and executes only on the main run loop after this entry returns.
    unsafe { run_loop.perform_block(Some(mode.as_ref()), Some(&callback)) };
    // Enqueueing does not wake a waiting run loop. This explicit wake adds no timer, idle work or retry.
    run_loop.wake_up();
    Ok(())
}

/// Classify pure foreground claims together with actual adapter ownership, never inferring release from visibility.
fn readiness() -> TerminationReadiness {
    let (active, transition, barrier) = super::FOREGROUND_STATE.with_borrow(|state| {
        // Copy finite state before consulting other owners; no state borrow crosses native close or plugin code.
        (
            state.active_surface(),
            state.pending_transition(),
            state.destroyed_barrier(),
        )
    });
    // Inventory only copied claims and presence bits; none of these getters sends a reentrant native message.
    TerminationOwnership {
        active_surface: active.is_some(),
        transition,
        destroyed_barrier: barrier.is_some(),
        adapter_owned: super::ACTIVE_SURFACE.with_borrow(Option::is_some)
            || super::CLOSING_SURFACE.with_borrow(Option::is_some)
            || super::plugin_host::exists()
            || super::builtins::is_open(),
        plugin_call_in_flight: super::plugin_host::is_call_in_flight()
            || super::launcher_close::is_call_in_flight(),
    }
    .readiness()
}

/// Expose only the copied production release classification to the opt-in synthetic parent verifier.
#[cfg(feature = "native-acceptance")]
pub(super) fn probe_foreground_released() -> bool {
    // No owner/native handle escapes this observation, and no close or coordinator transition is performed.
    readiness() == TerminationReadiness::Released
}

/// Run the exact existing save/composition/conflict preflight for whichever generic owner is active.
fn close_active_foreground() -> Result<(), String> {
    let active = super::FOREGROUND_STATE.with_borrow(|state| state.active_surface());
    match active {
        Some(ForegroundSurface::Launcher) => {
            // Launcher owns no document; its ordinary close still releases its controls/barrier before exit.
            super::launcher_close::request_termination_close()
        }
        Some(ForegroundSurface::Plugin(_)) => {
            // The SDK session commits visible plugin input, rejects save conflicts and retains its owner on failure.
            super::plugin_host::request_application_termination().map_err(|error| error.to_string())
        }
        Some(ForegroundSurface::BuiltIn(_)) => {
            // Built-in cleanup uses its own modal refusal and foreground resource cancellation contract.
            super::builtins::request_termination_close()
        }
        None => {
            // An ownership change before effects is revalidated afterward; never guess a concrete session.
            Err("Foreground ownership changed before application termination".into())
        }
    }
}

/// Answer every ordinary Quit, Command-Q and benchmark termination through one native delegate callback.
pub(super) fn application_should_terminate() -> NSApplicationTerminateReply {
    if MainThreadMarker::new().is_none() {
        // A foreign-thread selector cannot run AppKit/plugin preflight or infer a different TLS instance is empty.
        return NSApplicationTerminateReply::TerminateCancel;
    }
    let before = readiness();
    let request = TERMINATION.with_borrow_mut(|state| {
        // Reserve preparation and suppress competing activation before native work can reenter this callback.
        state.request(before)
    });
    let response = match request {
        TerminationRequest::Prepare(ticket) => {
            // Already accepted destruction keeps its exact intent; no opposing SDK close is requested.
            let result = objc2::rc::autoreleasepool(|_| {
                // Every adapter explicitly promotes an accepted close while preserving its original
                // intent. An adapter verifying on a reentrant stack may refuse safely instead of entering Later.
                close_active_foreground()
            });
            // The preflight pool drains while the coordinator is still Preparing. Reentrant release here may
            // authorize a synchronous Now result, but cannot send an async reply before this delegate returns Later.
            if let Err(error) = &result {
                // Plugin/native preflight already preserves the usable owner and displays its bounded status.
                eprintln!("application termination was cancelled: {error}");
            }
            let after = readiness();
            TERMINATION.with_borrow_mut(|state| {
                // A synchronous finalizer may have signalled release; return Now instead of an early async reply.
                state.prepared(ticket, result.is_ok(), after)
            })
        }
        other => {
            // Empty, busy, repeated or reentrant requests require no native effects or new close invocation.
            other
        }
    };
    match response {
        TerminationRequest::Now => {
            // Every foreground adapter is empty, or this exact close completed synchronously before returning.
            NSApplicationTerminateReply::TerminateNow
        }
        TerminationRequest::Later => {
            // AppKit now waits for the one original native/session/barrier completion signal.
            NSApplicationTerminateReply::TerminateLater
        }
        TerminationRequest::Cancel | TerminationRequest::Prepare(_) => {
            // Refusal and impossible unmatched preparation both preserve the process instead of risking unsaved input.
            NSApplicationTerminateReply::TerminateCancel
        }
    }
}

/// Called after adapter destruction, SDK-session Drop and foreground-barrier acknowledgement.
pub(super) fn foreground_did_release() -> bool {
    if readiness() == TerminationReadiness::Released {
        // Actual empty owners are required even when a callback claims its individual close has finished.
        let reply = TERMINATION.with_borrow_mut(|state| {
            // Consume reply authority before any potentially reentrant AppKit call.
            state.foreground_released()
        });
        if reply == TerminationReply::Allow {
            // The delegate already returned Later. This is the single native reply for the accepted transaction.
            let mtm = MainThreadMarker::new()
                .expect("foreground release callbacks use AppKit's main thread");
            #[cfg(feature = "native-acceptance")]
            {
                // Observe only this existing post-Drop/barrier authorization; the probe cannot authorize a reply.
                super::acceptance_termination::record_async_reply();
            }
            NSApplication::sharedApplication(mtm).replyToApplicationShouldTerminate(true);
        }
    }
    // Preparing/Waiting/Allowed all suppress any successor action selected by an earlier close intent.
    suppress_successors()
}

/// Run pure fake-plugin callbacks under a real Preparing coordinator phase, never an AppKit wait.
#[cfg(test)]
pub(super) fn with_preparing_termination_for_test<T>(work: impl FnOnce() -> T) -> T {
    struct Restore(ApplicationTermination);
    impl Drop for Restore {
        fn drop(&mut self) {
            // Restore scalar TLS even if an assertion unwinds; no native operation occurs in this guard.
            TERMINATION.with_borrow_mut(|state| *state = std::mem::take(&mut self.0));
        }
    }
    let previous = TERMINATION.with_borrow_mut(std::mem::take);
    let _restore = Restore(previous);
    TERMINATION.with_borrow_mut(|state| {
        // Preparing records synchronous completion without issuing a reply on this worker thread.
        assert!(matches!(
            state.request(TerminationReadiness::Active),
            TerminationRequest::Prepare(_)
        ));
    });
    work()
}

/// Model an explicit refusal and new quit within one scalar-only fake-plugin scenario.
#[cfg(test)]
pub(super) fn reject_and_retry_termination_for_test() {
    TERMINATION.with_borrow_mut(|state| {
        let previous = state
            .transaction_ticket()
            .expect("the test owns a Preparing transaction");
        // Cancellation restores activation before a distinct explicit retry reserves a fresh identity.
        assert_eq!(
            state.prepared(previous, false, TerminationReadiness::Closing),
            TerminationRequest::Cancel
        );
        assert!(matches!(
            state.request(TerminationReadiness::Closing),
            TerminationRequest::Prepare(_)
        ));
        assert_ne!(state.transaction_ticket(), Some(previous));
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A Rust worker may not infer another TLS instance is empty and authorize application termination.
    #[test]
    fn off_main_delegate_request_refuses_before_native_or_foreground_access() {
        // The unit harness runs on a worker; the native delegate must refuse before touching AppKit/TLS owners.
        assert!(MainThreadMarker::new().is_none());
        assert_eq!(
            application_should_terminate(),
            NSApplicationTerminateReply::TerminateCancel
        );
        assert!(!suppress_successors());
    }

    /// A worker cannot queue a main-loop quit using another thread's empty local ownership inventory.
    #[test]
    fn off_main_programmatic_request_refuses_without_pending_work() {
        // A failed thread-authority check must leave both activation gating and queued-entry ownership untouched.
        assert!(MainThreadMarker::new().is_none());
        assert!(request_application_termination().is_err());
        assert!(!QUEUED_REQUEST_PENDING.get());
        assert!(!suppress_successors());
    }

    /// Activation reservation before delegate entry is not terminal cleanup authority.
    #[test]
    fn queued_request_does_not_authorize_terminal_receipts() {
        // Only the reservation flag is set; the actual coordinator remains Idle throughout this check.
        QUEUED_REQUEST_PENDING.set(true);
        assert!(suppress_successors());
        assert!(!application_termination_pending());
        QUEUED_REQUEST_PENDING.set(false);
        with_preparing_termination_for_test(|| {
            // An accepted delegate preparation has distinct authority even without a queued entry.
            assert!(application_termination_pending());
        });
        assert!(!application_termination_pending());
    }
}
