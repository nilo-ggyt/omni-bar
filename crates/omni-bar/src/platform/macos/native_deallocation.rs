//! Observes actual native deallocation without retaining any tracked graph object.
//!
//! Public Objective-C associations retain timestamp/counter markers, never their source object.
//! A unique-identity cohort queues one main-thread callback after the last actual release stack.
//! The callback still has to perform the complete strict weak graph verification.

use std::cell::{Cell, RefCell};
use std::ffi::c_void;
use std::rc::Rc;

use dispatch2::{DispatchQueue, MainThreadBound};
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{MainThreadMarker, MainThreadOnly, define_class, msg_send};
use objc2_app_kit::NSWindow;
use objc2_foundation::{NSObject, NSObjectProtocol};
use omni_plugin_sdk::PluginCloseScope;
use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

unsafe extern "C" {
    /// Public objc/runtime.h ABI; the chosen policy retains value under the operation's unique key.
    fn objc_setAssociatedObject(
        object: *const AnyObject,
        key: *const c_void,
        value: *const AnyObject,
        policy: usize,
    );
}

/// Main-thread completion metadata contains no view, document, mapping or worker owner.
struct NativeDeallocationCallback {
    work: RefCell<Option<Box<dyn FnOnce()>>>,
    consumed: Cell<bool>,
}

/// Atomic counters can be notified by native destruction without touching main-thread Rust state.
struct NativeDeallocationSignals {
    remaining: AtomicUsize,
    relay_queued: AtomicBool,
    cancelled: AtomicBool,
    callback: MainThreadBound<std::rc::Weak<NativeDeallocationCallback>>,
    started: Instant,
    completed_after_ns: AtomicU64,
}

impl NativeDeallocationSignals {
    /// Release one builder/marker claim; only the final actual claim makes completion eligible.
    fn release_claim(self: &Arc<Self>) {
        // The builder contributes one claim and each unique associated marker contributes exactly
        // one more; the runtime removes each marker once, so the previous count is always positive.
        if self.remaining.fetch_sub(1, Ordering::AcqRel) == 1 {
            // Publish timing before making a relay eligible, including a concurrent handler installation.
            self.completed_after_ns.store(
                self.started
                    .elapsed()
                    .as_nanos()
                    .min(u128::from(u64::MAX - 1)) as u64,
                Ordering::Release,
            );
            // End the final native deallocation stack before any weak graph loads or host callback.
            self.queue_if_complete();
        }
    }

    /// Coalesce a completed cohort into one callback, including completion before handler installation.
    fn queue_if_complete(self: &Arc<Self>) {
        if self.cancelled.load(Ordering::Acquire)
            || self.remaining.load(Ordering::Acquire) != 0
            || self.completed_after_ns.load(Ordering::Acquire) == u64::MAX
            || self.relay_queued.swap(true, Ordering::AcqRel)
        {
            // Unreleased objects and an already queued relay cannot initiate another strict check.
            return;
        }
        let signals = Arc::clone(self);
        DispatchQueue::main().exec_async(move || {
            if signals.cancelled.load(Ordering::Acquire) {
                // Scope promotion revoked this observer before its queued relay could claim completion.
                return;
            }
            let mtm =
                MainThreadMarker::new().expect("native deallocation relays use the main queue");
            let Some(callback) = signals.callback.get(mtm).upgrade() else {
                // Dropped operation metadata has no authority to revive a completed/replaced session.
                return;
            };
            let work = callback.work.borrow_mut().take();
            if let Some(work) = work {
                // End state borrowing before the unique actual-completion handler can reenter AppKit or the Host.
                callback.consumed.set(true);
                work();
            } else {
                // Registration may follow synchronous deallocation. A later installation can enqueue
                // one relay, while no strict graph check has happened in this handler-less observation.
                signals.relay_queued.store(false, Ordering::Release);
            }
        });
    }
}

/// The associated value holds only a counter claim; it has no reverse reference to its source object.
struct NativeDeallocationMarkerIvars {
    signals: Arc<NativeDeallocationSignals>,
}

impl Drop for NativeDeallocationMarkerIvars {
    /// Release this observer's sole counter claim when the runtime destroys or explicitly removes its marker.
    fn drop(&mut self) {
        // The runtime removes this unique association at source deallocation or explicit observer cancellation.
        // Cancellation disables completion first; only an uncancelled final claim supplies actual release authority.
        self.signals.release_claim();
    }
}

define_class!(
    // SAFETY: NSObject has no extra subclass requirements. The marker's installed ivar owns only
    // Send/Sync atomic signal metadata, and its main-thread initialization retains no source object.
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    #[ivars = NativeDeallocationMarkerIvars]
    struct NativeDeallocationMarker;
    // SAFETY: the initialized NSObject subclass preserves native identity and main-thread construction.
    unsafe impl NSObjectProtocol for NativeDeallocationMarker {}
);

/// Owns operation metadata while markers observe each unique native identity's actual lifetime.
pub(super) struct NativeDeallocationCohort {
    callback: Rc<NativeDeallocationCallback>,
    signals: Arc<NativeDeallocationSignals>,
    identities: HashSet<usize>,
    sealed: bool,
}

impl NativeDeallocationCohort {
    /// Begin registration with a builder claim so early native destruction cannot complete a partial set.
    pub(super) fn new(mtm: MainThreadMarker) -> Self {
        let callback = Rc::new(NativeDeallocationCallback {
            work: RefCell::new(None),
            consumed: Cell::new(false),
        });
        let signals = Arc::new(NativeDeallocationSignals {
            remaining: AtomicUsize::new(1),
            relay_queued: AtomicBool::new(false),
            cancelled: AtomicBool::new(false),
            callback: MainThreadBound::new(Rc::downgrade(&callback), mtm),
            started: Instant::now(),
            completed_after_ns: AtomicU64::new(u64::MAX),
        });
        Self {
            callback,
            signals,
            identities: HashSet::new(),
            sealed: false,
        }
    }

    /// Attach one public association per identity, without retaining the observed native object.
    pub(super) fn track<T: objc2::Message>(&mut self, object: &T, mtm: MainThreadMarker) {
        assert!(!self.sealed, "native cohort registration already ended");
        if !self.identities.insert(std::ptr::from_ref(object).addr()) {
            // Different graph roles can name the same native object; one actual deallocation is one claim.
            return;
        }
        self.signals.remaining.fetch_add(1, Ordering::Relaxed);
        let marker =
            NativeDeallocationMarker::alloc(mtm).set_ivars(NativeDeallocationMarkerIvars {
                signals: Arc::clone(&self.signals),
            });
        // SAFETY: NSObject initializes the fully installed atomic metadata without extra preconditions.
        let marker: Retained<NativeDeallocationMarker> = unsafe { msg_send![super(marker), init] };
        // SAFETY: this is the public runtime ABI from objc/runtime.h. The stable Arc allocation is
        // a unique key for this cohort and every marker keeps it alive. Policy 1 is the documented
        // RETAIN_NONATOMIC association: object retains marker, while marker never retains object.
        unsafe {
            objc_setAssociatedObject(
                std::ptr::from_ref(object).cast(),
                Arc::as_ptr(&self.signals).cast(),
                std::ptr::from_ref(&*marker).cast(),
                1,
            );
        }
    }

    /// Finish the complete identity set before transferring the operation's weak-only evidence.
    pub(super) fn seal(&mut self) {
        assert!(!self.sealed, "native cohort registration ended twice");
        self.sealed = true;
        // The address set is construction scratch only; no native graph/address catalog remains while waiting.
        self.identities = HashSet::new();
        self.signals.release_claim();
    }

    /// Install one operation-specific handler; only actual completion makes it eligible to execute.
    pub(super) fn when_released(&self, work: impl FnOnce() + 'static) {
        assert!(
            !self.signals.cancelled.load(Ordering::Acquire),
            "a cancelled observation cannot install completion work"
        );
        assert!(
            !self.callback.consumed.get(),
            "native completion already consumed"
        );
        assert!(
            self.callback.work.borrow().is_none(),
            "native completion already installed"
        );
        *self.callback.work.borrow_mut() = Some(Box::new(work));
        self.signals.queue_if_complete();
    }

    /// Revoke callback authority before removing only this cohort's association from a terminal shell.
    ///
    /// This operation is observation cancellation, never an actual-deallocation signal. Terminal
    /// callers must already have proved the complete payload graph released and the panel detached.
    pub(super) fn cancel_observation<T: objc2::Message>(&mut self, panel: Option<&T>) {
        // A queued relay checks cancellation before touching its weak callback. Clearing the work
        // also removes every project callback capture before the framework-held shell is accepted.
        self.signals.cancelled.store(true, Ordering::Release);
        self.callback.work.borrow_mut().take();
        self.callback.consumed.set(true);
        if let Some(panel) = panel {
            // SAFETY: the caller transiently owns this live main-thread panel. The stable Arc key
            // identifies only this observer's association; nil removes that marker, not another
            // owner's association or a panel retain. Cancellation preceded marker destruction.
            unsafe {
                objc_setAssociatedObject(
                    std::ptr::from_ref(panel).cast(),
                    Arc::as_ptr(&self.signals).cast(),
                    std::ptr::null(),
                    1,
                );
            }
        }
    }

    /// Report measured registration-to-actual-completion latency without introducing a timer or retry.
    pub(super) fn elapsed(&self) -> Duration {
        let completed = self.signals.completed_after_ns.load(Ordering::Acquire);
        if completed == u64::MAX {
            // An unfinished cohort may be reported diagnostically, but cannot authorize weak verification yet.
            self.signals.started.elapsed()
        } else {
            // Timing stops at the final actual release signal rather than including later event dispatch latency.
            Duration::from_nanos(completed)
        }
    }

    /// Distinguish actual completed ownership from a guessed queue boundary before any native weak loads.
    pub(super) fn has_completed(&self) -> bool {
        self.sealed
            && !self.signals.cancelled.load(Ordering::Acquire)
            && self.signals.remaining.load(Ordering::Acquire) == 0
            && self.signals.completed_after_ns.load(Ordering::Acquire) != u64::MAX
    }
}

impl Drop for NativeDeallocationCohort {
    /// Balance an unfinished builder claim without claiming that surviving observed objects deallocated.
    fn drop(&mut self) {
        if !self.sealed {
            // Construction failure releases its builder claim; surviving markers still retain no native source owner.
            self.signals.release_claim();
        }
    }
}

/// Remembers only an actual payload event and its once-only terminal verification reservation.
#[derive(Default)]
pub(super) struct TerminalReleaseAuthority {
    resource_event_observed: bool,
    terminal_check_reserved: bool,
}

impl TerminalReleaseAuthority {
    /// Admit either event-before-promotion or promotion-before-event without guessing or retrying release.
    pub(super) fn reserve(
        &mut self,
        scope: PluginCloseScope,
        actual_resources_released: bool,
    ) -> bool {
        self.resource_event_observed |= actual_resources_released;
        if scope != PluginCloseScope::ApplicationTermination
            || !self.resource_event_observed
            || self.terminal_check_reserved
        {
            // Ordinary scope stores only readiness; an incomplete or already consumed terminal event has no new authority.
            return false;
        }
        self.terminal_check_reserved = true;
        true
    }
}

/// Copied observations leave every native upgrade and project-state borrow behind before receipt use.
#[derive(Clone, Copy, Debug)]
pub(super) struct TerminalWindowFacts {
    pub close_notified: bool,
    pub project_owner_present: bool,
    pub panel_present: bool,
    pub visible: bool,
    pub key: bool,
    pub content_attached: bool,
    pub delegate_attached: bool,
    pub initial_responder_attached: bool,
    pub payload_responder_attached: bool,
    pub sheet_attached: bool,
}

/// A terminal-only receipt states whether AppKit still leases an empty panel, without owning it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct FrameworkWindowLeaseReceipt {
    pub framework_panel_pending: bool,
}

impl TerminalWindowFacts {
    /// Reject every project/resource edge before acknowledging a possible framework-only shell.
    pub(super) fn verify(self) -> Result<FrameworkWindowLeaseReceipt, &'static str> {
        if !self.close_notified || self.project_owner_present {
            // Visibility alone cannot prove the real close callback occurred or the unique owner ended.
            return Err("The Host terminal close callback or project-owner release is incomplete");
        }
        if self.visible
            || self.key
            || self.content_attached
            || self.delegate_attached
            || self.initial_responder_attached
            || self.payload_responder_attached
            || self.sheet_attached
        {
            // Any native payload, routing edge or visible/key state exceeds the sole permitted empty-shell lease.
            return Err("The Host terminal panel is not closed and detached");
        }
        Ok(FrameworkWindowLeaseReceipt {
            framework_panel_pending: self.panel_present,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A terminal receipt permits only a completely detached shell or its already deallocated identity.
    #[test]
    fn every_live_edge_and_missing_close_or_owner_proof_rejects() {
        let empty = TerminalWindowFacts {
            close_notified: true,
            project_owner_present: false,
            panel_present: true,
            visible: false,
            key: false,
            content_attached: false,
            delegate_attached: false,
            initial_responder_attached: false,
            payload_responder_attached: false,
            sheet_attached: false,
        };
        assert_eq!(
            empty.verify(),
            Ok(FrameworkWindowLeaseReceipt {
                framework_panel_pending: true
            })
        );
        assert_eq!(
            TerminalWindowFacts {
                panel_present: false,
                ..empty
            }
            .verify(),
            Ok(FrameworkWindowLeaseReceipt {
                framework_panel_pending: false
            })
        );
        let failures = [
            TerminalWindowFacts {
                close_notified: false,
                ..empty
            },
            TerminalWindowFacts {
                project_owner_present: true,
                ..empty
            },
            TerminalWindowFacts {
                visible: true,
                ..empty
            },
            TerminalWindowFacts { key: true, ..empty },
            TerminalWindowFacts {
                content_attached: true,
                ..empty
            },
            TerminalWindowFacts {
                delegate_attached: true,
                ..empty
            },
            TerminalWindowFacts {
                initial_responder_attached: true,
                ..empty
            },
            TerminalWindowFacts {
                payload_responder_attached: true,
                ..empty
            },
            TerminalWindowFacts {
                sheet_attached: true,
                ..empty
            },
        ];
        for facts in failures {
            // Each independent edge vetoes the receipt; no aggregate visibility shortcut hides another owner.
            assert!(facts.verify().is_err());
        }
    }
}

/// Inspect only public shell state; the caller must separately prove actual payload nil and end its local pool.
pub(super) fn verify_terminal_window(
    panel: Option<&NSWindow>,
    close_notified: bool,
    project_owner_present: bool,
) -> Result<FrameworkWindowLeaseReceipt, String> {
    let mut facts = TerminalWindowFacts {
        close_notified,
        project_owner_present,
        panel_present: panel.is_some(),
        visible: false,
        key: false,
        content_attached: false,
        delegate_attached: false,
        initial_responder_attached: false,
        payload_responder_attached: false,
        sheet_attached: false,
    };
    if let Some(panel) = panel {
        // Native getters run outside ownership borrows. NSWindow may be its own inert responder;
        // any other responder or attached native role is still a forbidden project payload edge.
        facts.visible = panel.isVisible();
        facts.key = panel.isKeyWindow();
        facts.content_attached = panel.contentView().is_some();
        facts.delegate_attached = panel.delegate().is_some();
        facts.initial_responder_attached = panel.initialFirstResponder().is_some();
        facts.payload_responder_attached = panel.firstResponder().is_some_and(|responder| {
            // Compare native identity only; this temporary responder claim never leaves the caller's local pool.
            !std::ptr::eq(
                std::ptr::from_ref(&*responder).cast::<()>(),
                std::ptr::from_ref(panel).cast::<()>(),
            )
        });
        facts.sheet_attached = panel.attachedSheet().is_some();
    }
    facts.verify().map_err(str::to_owned)
}

/// Verify public marker identity accounting in the standalone child's actual main thread before foreground creation.
#[cfg(feature = "native-acceptance")]
pub(super) fn verify_native_deallocation_accounting(
    mtm: MainThreadMarker,
) -> Result<NativeDeallocationAccounting, String> {
    let mut empty = NativeDeallocationCohort::new(mtm);
    empty.seal();
    if !empty.has_completed() {
        // No tracked identity means the builder's final real claim must already be released.
        return Err("empty native deallocation cohort did not complete".into());
    }
    let object = NSObject::new();
    let mut first = NativeDeallocationCohort::new(mtm);
    let mut second = NativeDeallocationCohort::new(mtm);
    first.track(&*object, mtm);
    first.track(&*object, mtm);
    second.track(&*object, mtm);
    first.seal();
    second.seal();
    if first.signals.remaining.load(Ordering::Acquire) != 1
        || first.has_completed()
        || second.has_completed()
    {
        // Aliased roles count once; independent operation keys must never overwrite a still-live association.
        return Err("native deallocation duplicate/overlap accounting failed".into());
    }
    drop(object);
    if !first.has_completed() || !second.has_completed() {
        // Marker destruction occurs from the actual runtime release without retaining its source or pumping a loop.
        return Err("native deallocation marker retained its source".into());
    }
    let object = NSObject::new();
    let mut cancelled = NativeDeallocationCohort::new(mtm);
    let mut surviving = NativeDeallocationCohort::new(mtm);
    cancelled.track(&*object, mtm);
    surviving.track(&*object, mtm);
    cancelled.seal();
    surviving.seal();
    cancelled.cancel_observation(Some(&*object));
    if surviving.has_completed() || surviving.signals.remaining.load(Ordering::Acquire) != 1 {
        // Removing exactly one cohort's unique key cannot remove or complete another observer on the same source.
        return Err("cancelling one native observer removed another cohort's association".into());
    }
    if cancelled.has_completed() {
        // Explicit observer removal must never be reclassified as actual native source deallocation.
        return Err("cancelled native observation claimed actual deallocation".into());
    }
    drop(object);
    if !surviving.has_completed() || cancelled.has_completed() {
        // Actual destruction completes only the surviving observer; cancellation remains permanently ineligible.
        return Err("native deallocation cancellation isolation failed".into());
    }
    // An installed handler is transferred once and consumed before invocation; duplicate queue requests
    // retain the same relay reservation rather than creating a second callback authority.
    let callback = Rc::new(Cell::new(0));
    let counted = Rc::clone(&callback);
    surviving.when_released(move || counted.set(counted.get() + 1));
    surviving.signals.queue_if_complete();
    surviving.signals.queue_if_complete();
    if !surviving.signals.relay_queued.load(Ordering::Acquire) {
        // Even completed markers need their real queued relay; elapsed time grants no callback authority.
        return Err("completed native deallocation did not reserve its unique relay".into());
    }
    // The existing driver's later native event checks the real callback count. No extra loop,
    // polling, timer or native source owner is retained to make this observation happen.
    Ok(NativeDeallocationAccounting {
        cohort: surviving,
        callbacks: callback,
    })
}

/// Weak-only opt-in accounting evidence lives until the existing driver checks its one actual native relay.
#[cfg(feature = "native-acceptance")]
pub(super) struct NativeDeallocationAccounting {
    cohort: NativeDeallocationCohort,
    callbacks: Rc<Cell<usize>>,
}

#[cfg(feature = "native-acceptance")]
impl NativeDeallocationAccounting {
    /// Observe one existing queue event, rejecting either an absent callback or duplicate completion authority.
    pub(super) fn verify_callback_once(&self) -> Result<(), String> {
        if !self.cohort.has_completed()
            || !self.cohort.callback.consumed.get()
            || self.callbacks.get() != 1
        {
            // Elapsed time and callback scheduling alone cannot replace this actual one-time invocation evidence.
            return Err(
                "native deallocation actual relay did not invoke its completion exactly once"
                    .into(),
            );
        }
        Ok(())
    }
}

#[cfg(test)]
mod release_authority_tests {
    use super::*;

    /// Event-before-promotion and promotion-before-event each authorize exactly one terminal proof.
    #[test]
    fn terminal_release_reservation_requires_actual_event_and_consumes_it_once() {
        let mut event_first = TerminalReleaseAuthority::default();
        assert!(!event_first.reserve(PluginCloseScope::ForegroundTransition, true));
        assert!(event_first.reserve(PluginCloseScope::ApplicationTermination, false));
        assert!(!event_first.reserve(PluginCloseScope::ApplicationTermination, true));
        let mut promotion_first = TerminalReleaseAuthority::default();
        assert!(!promotion_first.reserve(PluginCloseScope::ApplicationTermination, false));
        assert!(promotion_first.reserve(PluginCloseScope::ApplicationTermination, true));
        assert!(!promotion_first.reserve(PluginCloseScope::ApplicationTermination, true));
    }
}
