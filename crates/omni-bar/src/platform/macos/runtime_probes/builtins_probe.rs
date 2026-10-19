//! Drive four built-ins through real NSApplication.run; elapsed time and queue turns never prove release.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use dispatch2::{
    _dispatch_source_type_timer, DispatchObject, DispatchQueue, DispatchRetained, DispatchSource,
    DispatchTime, MainThreadBound,
};
use objc2::rc::{Retained, Weak, autoreleasepool};
use objc2::runtime::ProtocolObject;
use objc2::{ClassType, DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSApplicationDelegate, NSEvent,
    NSEventModifierFlags, NSEventType,
};
use objc2_foundation::{NSNotification, NSObject, NSObjectProtocol, NSPoint};
use omni_plugin_sdk::PluginCloseScope;

use super::super::builtins::{
    BuiltInContext, BuiltInLifecycle, BuiltInProbeIdentity, BuiltInRuntimeProbeCase,
};
use crate::{BuiltInSurface, SurfaceCloseIntent};

const CREATE: i16 = 300;
const INPUT: i16 = 301;
const UNDO: i16 = 302;
const REDO: i16 = 303;
const SETTLED: i16 = 304;
const CLOSE: i16 = 305;
const LATE: i16 = 306;
const RECEIPT: i16 = 307;
const WAITING: i16 = 308;
const EVENT_TAG: isize = 0x4f4d_4249;

/// Bounded stage authority accepts Late and the exact resource Receipt in either order, once each.
struct Gate {
    case: usize,
    next: i16,
    receipt: bool,
    late: bool,
}

impl Gate {
    /// Initial launch may create Calculator only and assumes no completed resource facts.
    fn new() -> Self {
        Self {
            case: 0,
            next: CREATE,
            receipt: false,
            late: false,
        }
    }

    /// Consume stage authority before native calls so duplicate events cannot repeat input or close.
    fn admit(&mut self, stage: i16, case: usize) -> Result<(), String> {
        if case != self.case {
            // Neither a previous case nor a future successor may borrow the current owner's authority.
            return Err("stale or future built-in fixture event".into());
        }
        if self.next == WAITING {
            match stage {
                RECEIPT if !self.receipt => {
                    // Only the lifecycle callback can post this event; failed resource verification cannot retry.
                    self.receipt = true;
                    return Ok(());
                }
                LATE if !self.late => {
                    // The actual late callback has separate once-only authority and cannot impersonate resource completion.
                    self.late = true;
                    return Ok(());
                }
                _ => {
                    // Duplicate completion or an ordinary stage during close fails this acceptance run.
                    return Err("duplicate or out-of-order built-in completion event".into());
                }
            }
        }
        if stage != self.next {
            // Do not skip actual input/Undo/close boundaries or repeat an earlier stage to reproduce its result.
            return Err("out-of-order built-in fixture stage".into());
        }
        // Enter close-waiting before native effects so reentrant signals cannot acquire the next case's authority.
        self.next = if stage == CLOSE { WAITING } else { stage + 1 };
        Ok(())
    }

    /// Require both independent completion sources before releasing a case and permitting its successor.
    fn complete(&mut self) -> bool {
        if !(self.next == WAITING && self.receipt && self.late) {
            // A delivered late callback cannot authorize a successor without the exact resource completion.
            return false;
        }
        self.case += 1;
        self.next = CREATE;
        self.receipt = false;
        self.late = false;
        true
    }
}

struct ProbeIvars {
    context: BuiltInContext,
    gate: RefCell<Gate>,
    started: Cell<bool>,
    current: RefCell<Option<BuiltInRuntimeProbeCase>>,
    previous: RefCell<Option<BuiltInProbeIdentity>>,
    deadline: RefCell<Option<DispatchRetained<DispatchSource>>>,
    result: RefCell<Option<Result<(), String>>>,
}

define_class!(
    // SAFETY: Fully initialize NSObject ivars before init; AppKit invokes all callbacks on the main thread.
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    #[ivars = ProbeIvars]
    struct BuiltinsProbeDelegate;

    // SAFETY: Preserve NSObject identity/reference semantics and introduce no cross-thread handles.
    unsafe impl NSObjectProtocol for BuiltinsProbeDelegate {}
    // SAFETY: The dedicated app retains this delegate until run returns and forwards ordinary events natively.
    unsafe impl NSApplicationDelegate for BuiltinsProbeDelegate {
        #[unsafe(method(applicationDidFinishLaunching:))]
        fn did_launch(&self, _notification: &NSNotification) {
            if !self.ivars().started.replace(true) {
                // Start the first case once from actual launch, never by polling after finishLaunching.
                post_event(CREATE, 0, self.mtm());
            } else {
                // Duplicate launch grants no second creation; subsequent progression requires actual stage/resource receipts.
            }
        }
    }
);

/// Receive completion from production BuiltInLifecycle, not weak polling or elapsed time.
struct EventLifecycle {
    inner: Rc<dyn BuiltInLifecycle>,
    driver: Weak<BuiltinsProbeDelegate>,
    case: usize,
    kind: BuiltInSurface,
}

impl BuiltInLifecycle for EventLifecycle {
    /// Commit creation through the existing domain state machine without replacing production rules.
    fn commit_creation(&self, kind: BuiltInSurface) -> Result<(), String> {
        self.inner.commit_creation(kind)
    }
    fn begin_close(
        &self,
        kind: BuiltInSurface,
        intent: SurfaceCloseIntent,
        scope: PluginCloseScope,
    ) -> Result<(), String> {
        // Preserve existing intent/scope validation; the event driver grants no additional close direction.
        self.inner.begin_close(kind, intent, scope)
    }
    fn finish_close(
        &self,
        kind: BuiltInSurface,
        intent: SurfaceCloseIntent,
        scope: PluginCloseScope,
    ) {
        if kind != self.kind
            || intent != SurfaceCloseIntent::DismissAndResume
            || scope != PluginCloseScope::ForegroundTransition
        {
            // Mismatched receipts fail explicitly without advancing the domain or authorizing successors.
            if let Some(driver) = self.driver.load() {
                // Notify only the same live probe; old weak references cannot revive a stopped driver.
                driver.finish(Err("built-in lifecycle receipt identity mismatch".into()));
            }
            return;
        }
        // Complete the domain and consume its barrier first; only success posts the once-only Receipt.
        self.inner.finish_close(kind, intent, scope);
        if let Some(driver) = self.driver.load() {
            // Completion captures only a case scalar, retaining no old business graph across events.
            post_event(RECEIPT, self.case, driver.mtm());
        }
    }
}

impl BuiltinsProbeDelegate {
    /// Give each authorized stage a local pool and convert Rust failure before returning through Cocoa callbacks.
    fn run_stage(&self, stage: i16, case: usize) {
        if self.ivars().result.borrow().is_some() {
            // After stop, queued events cannot create, edit or close a business surface.
            return;
        }
        let admitted = self.ivars().gate.borrow_mut().admit(stage, case);
        if let Err(error) = admitted {
            // Reject invalid events before native effects; the same stage cannot retry.
            self.finish(Err(error));
            return;
        }
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            autoreleasepool(|_| {
                if !NSApplication::sharedApplication(self.mtm()).isRunning() {
                    // Distinguish CLI run-loop pumping from a real NSApplication.run event loop.
                    return Err("built-in stage ran without NSApplication.run".into());
                }
                if stage == CREATE {
                    // Replace completion routing only; data, shortcuts and construction keep their existing probe paths.
                    let kinds = [
                        BuiltInSurface::Calculator,
                        BuiltInSurface::UnitConversion,
                        BuiltInSurface::Clipboard,
                        BuiltInSurface::Settings,
                    ];
                    let kind = *kinds.get(case).ok_or("unknown built-in fixture case")?;
                    let mut context = self.ivars().context.clone();
                    context.lifecycle = Rc::new(EventLifecycle {
                        inner: Rc::clone(&context.lifecycle),
                        driver: Weak::from(self),
                        case,
                        kind,
                    });
                    let previous = self.ivars().previous.borrow_mut().take();
                    let probe =
                        BuiltInRuntimeProbeCase::start(kind, context, previous, self.mtm())?;
                    *self.ivars().current.borrow_mut() = Some(probe);
                    return Ok(());
                }
                let mut probe = self
                    .ivars()
                    .current
                    .borrow_mut()
                    .take()
                    .ok_or("built-in phase owner missing")?;
                // End the driver RefMut before local field getters, Undo, close and weak loading.
                let result = match stage {
                    // Input uses the native field/delegate; temporary claims end with this event's pool.
                    INPUT => probe.input(),
                    // Separate history operations into actual events to exercise AppKit event grouping.
                    UNDO => probe.history(false),
                    // Restore input through the original manager; reinserting the fixture would not prove Redo.
                    REDO => probe.history(true),
                    // Exercise blank state after history verification so it cannot overwrite the input being tested.
                    SETTLED => probe.settled(),
                    // Close requests teardown only; returning cannot advance resource completion.
                    CLOSE => probe.request_close(),
                    // Deliver the queued business callback late without granting additional resource authority.
                    LATE => probe.late_callback(),
                    // The sole production barrier authorizes one strict weak-release/cancellation verification.
                    RECEIPT => probe.verify_receipt(),
                    // A stage outside the private event table cannot become default success.
                    _ => Err("unknown built-in application event".into()),
                };
                *self.ivars().current.borrow_mut() = Some(probe);
                result
            })
        }))
        .unwrap_or_else(|_| {
            // Do not unwind Rust through the Objective-C event-callback ABI.
            Err("built-in probe panicked in native phase".into())
        });
        if let Err(error) = outcome {
            // Preserve the first failing case/stage without adding wait turns until objects disappear.
            self.finish(Err(format!("case {case} stage {stage}: {error}")));
            return;
        }
        if matches!(stage, LATE | RECEIPT) {
            // Advance pure state only on the two completion sources, never by checking time or polling weak references.
            let complete = self.ivars().gate.borrow_mut().complete();
            if complete {
                let probe = self.ivars().current.borrow_mut().take();
                let Some(probe) = probe else {
                    // Completed authority still needs this case's receipt owner; missing ownership fails without panicking across Cocoa.
                    self.finish(Err("completed built-in case owner missing".into()));
                    return;
                };
                if let Err(error) = probe.verify_final() {
                    // Resource success cannot hide an undelivered late callback or revived business state.
                    self.finish(Err(error));
                    return;
                }
                *self.ivars().previous.borrow_mut() = Some(probe.identity());
                drop(probe);
                // Report the named case complete only after both the exact receipt and actual late callback pass.
                println!(
                    "native_builtin_case_completed case={case} exact_finish_close=true late_callback=true strict_release=true"
                );
                let next_case = self.ivars().gate.borrow().case;
                if next_case == 4 {
                    // Stop the actual application loop only after all four exact resource receipts complete.
                    self.finish(Ok(()));
                } else {
                    // Create successors only after consuming the previous case's completion authority.
                    post_event(CREATE, next_case, self.mtm());
                }
            }
        } else {
            // Post each next stage as a fresh AppKit event after returning from this event and its autorelease pool.
            let next = if stage == CLOSE { LATE } else { stage + 1 };
            DispatchQueue::main().exec_async(move || {
                post_event(
                    next,
                    case,
                    MainThreadMarker::new().expect("probe main queue"),
                );
            });
        }
    }

    /// Store one terminal result and stop the loop; deadlines and late events cannot replace it.
    fn finish(&self, result: Result<(), String>) {
        if self.ivars().result.borrow().is_some() {
            // Duplicate completion/deadline has no second stop or result-replacement authority.
            return;
        }
        self.cancel_deadline();
        *self.ivars().result.borrow_mut() = Some(result);
        NSApplication::sharedApplication(self.mtm()).stop(None);
        post_event(0, 0, self.mtm());
    }

    /// Cancel the probe deadline on normal return, failure or external stop, leaving no active source.
    fn cancel_deadline(&self) {
        let deadline = self.ivars().deadline.borrow_mut().take();
        if let Some(deadline) = deadline {
            // End the state borrow after take; cancel/drop handle probe tooling only, never the measured graph.
            deadline.cancel();
        }
    }

    /// One nonrepeating deadline bounds all four cases; its timer cannot establish stage or resource completion.
    fn arm_deadline(&self) -> Result<(), String> {
        let when = DispatchTime::try_from(Duration::from_secs(10))
            .map_err(|_| "probe deadline unavailable")?;
        // SAFETY: The dispatch timer uses zero handle/mask and its queue upgrades the delegate weak reference only on main.
        let source = unsafe {
            DispatchSource::new(
                std::ptr::addr_of!(_dispatch_source_type_timer).cast_mut(),
                0,
                0,
                Some(DispatchQueue::main()),
            )
        };
        let weak = MainThreadBound::new(Weak::from(self), self.mtm());
        let block: block2::RcBlock<dyn Fn()> = block2::RcBlock::new(move || {
            autoreleasepool(|_| {
                let mtm = MainThreadMarker::new().expect("failure deadline main queue");
                if let Some(driver) = weak.get(mtm).load() {
                    // Report failure once, without weak-graph retry, survivor exemptions or time-based success.
                    let probe = driver.ivars().current.borrow_mut().take();
                    let details = probe
                        .as_ref()
                        .map(BuiltInRuntimeProbeCase::failure_diagnostic);
                    let gate = driver.ivars().gate.borrow();
                    let error = format!(
                        "built-in 10 s fail-only deadline: case={} phase={} receipt={} late={} details={details:?}",
                        gate.case, gate.next, gate.receipt, gate.late
                    );
                    drop(gate);
                    *driver.ivars().current.borrow_mut() = probe;
                    driver.finish(Err(error));
                }
            })
        });
        // SAFETY: The source copies the heap block, which captures only a main-thread weak reference and no measured graph.
        unsafe { source.set_event_handler_with_block(block2::RcBlock::as_ptr(&block)) };
        source.set_timer(when, u64::MAX, 0);
        source.activate();
        *self.ivars().deadline.borrow_mut() = Some(source);
        Ok(())
    }
}

thread_local! {
    static DRIVER: RefCell<Option<Weak<BuiltinsProbeDelegate>>> = const { RefCell::new(None) };
}

define_class!(
    // SAFETY: Preserve NSApplication initialization/main-thread requirements and forward system events unchanged.
    #[unsafe(super = NSApplication)]
    #[thread_kind = MainThreadOnly]
    struct BuiltinsProbeApplication;
    impl BuiltinsProbeApplication {
        #[unsafe(method(sendEvent:))]
        fn send_event(&self, event: &NSEvent) {
            if event.r#type() == NSEventType::ApplicationDefined && event.data1() == EVENT_TAG && (CREATE..=RECEIPT).contains(&event.subtype().0) {
                let weak = DRIVER.with_borrow(Clone::clone);
                if let Some(driver) = weak.and_then(|weak| weak.load()) {
                    // End the router borrow first; fixed event data contains stage/case scalars and no business/native owner.
                    match usize::try_from(event.data2()) {
                        Ok(case) => {
                            // Gate still validates positive case values; a future case cannot become a valid current stage.
                            driver.run_stage(event.subtype().0, case);
                        }
                        Err(_) => {
                            // A negative case with the private tag is a corrupt probe event, not an event to skip before reporting success.
                            driver.finish(Err("negative built-in fixture case".into()));
                        }
                    }
                } else {
                    // Old events lose authority after stop; losing the driver stops run and leaves missing-terminal-state failure.
                    // Without a writable result owner, do not synthesize success; diagnostics use fixed error codes only.
                    eprintln!("native_builtin_event_rejected=no_driver");
                    NSApplication::sharedApplication(self.mtm()).stop(None);
                }
                return;
            }
            // SAFETY: The main-thread event remains valid during this synchronous call and is forwarded once using sendEvent:'s ABI.
            unsafe { let _: () = msg_send![super(self), sendEvent: event]; }
        }
    }
);

/// Post stage/case scalars only, without synthesizing physical input or capturing the measured graph.
fn post_event(stage: i16, case: usize, mtm: MainThreadMarker) {
    let event = NSEvent::otherEventWithType_location_modifierFlags_timestamp_windowNumber_context_subtype_data1_data2(NSEventType::ApplicationDefined, NSPoint::new(0.0,0.0), NSEventModifierFlags::empty(), 0.0, 0, None, stage, EVENT_TAG, case as isize).expect("finite builtin probe event");
    NSApplication::sharedApplication(mtm).postEvent_atStart(&event, false);
}

/// Receive sharedApplication as its superclass, then verify the actual dedicated router class.
pub(super) fn run(context: BuiltInContext, mtm: MainThreadMarker) -> Result<(), String> {
    // SAFETY: sharedApplication returns a valid NSApplication; receive it as superclass before checking its dynamic subclass.
    let application: Retained<NSApplication> =
        unsafe { msg_send![BuiltinsProbeApplication::class(), sharedApplication] };
    let application = application.downcast::<BuiltinsProbeApplication>().map_err(|_| "built-in probe requires its own first NSApplication; existing application class is incompatible")?;
    if application.isRunning() {
        // Never replace a running driver/delegate or nest NSApplication.run.
        return Err("built-in probe cannot enter a nested application loop".into());
    }
    if !application.setActivationPolicy(NSApplicationActivationPolicy::Accessory) {
        // Failure to establish the dedicated application mode prevents any input-readiness claim.
        return Err("built-in probe activation policy was rejected".into());
    }
    let delegate = BuiltinsProbeDelegate::alloc(mtm).set_ivars(ProbeIvars {
        context,
        gate: RefCell::new(Gate::new()),
        started: Cell::new(false),
        current: RefCell::new(None),
        previous: RefCell::new(None),
        deadline: RefCell::new(None),
        result: RefCell::new(None),
    });
    // SAFETY: Initialize Rust ivars before NSObject init and use this object only on the main thread.
    let delegate: Retained<BuiltinsProbeDelegate> = unsafe { msg_send![super(delegate), init] };
    DRIVER.with_borrow_mut(|slot| *slot = Some(Weak::from_retained(&delegate)));
    application.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
    if let Err(error) = delegate.arm_deadline() {
        // Failure before run still revokes all router/delegate authority.
        application.setDelegate(None);
        DRIVER.with_borrow_mut(|slot| {
            slot.take();
        });
        return Err(error);
    }
    application.run();
    delegate.cancel_deadline();
    application.setDelegate(None);
    DRIVER.with_borrow_mut(|slot| {
        slot.take();
    });
    let result = delegate.ivars().result.borrow_mut().take();
    result.ok_or_else(|| {
        "built-in application stopped without a terminal receipt result".to_owned()
    })?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn successor_requires_exact_receipt_and_late_event_in_either_order() {
        for order in [[RECEIPT, LATE], [LATE, RECEIPT]] {
            // Both actual arrival orders must grant the same authority, independent of native queue ordering.
            let mut gate = Gate::new();
            for case in 0..4 {
                // Each named feature must traverse creation, input, Undo/Redo, blank state and close.
                for stage in CREATE..=CLOSE {
                    // Each stage consumes one grant; none of its actual event boundaries may be skipped.
                    gate.admit(stage, case).unwrap();
                }
                // A close request or one completion source alone cannot create the next native graph.
                assert!(!gate.complete());
                gate.admit(order[0], case).unwrap();
                assert!(!gate.complete());
                assert!(gate.admit(CREATE, case + 1).is_err());
                gate.admit(order[1], case).unwrap();
                // Advance only after both sources arrive; this pure test does not replace actual weak-resource verification.
                assert!(gate.complete());
            }
            assert_eq!(gate.case, 4);
        }
    }

    #[test]
    fn duplicate_future_and_premature_receipts_never_advance() {
        // Before start, no input or receipt authority exists; forged events cannot bypass native creation.
        let mut gate = Gate::new();
        assert!(gate.admit(RECEIPT, 0).is_err());
        assert!(gate.admit(INPUT, 0).is_err());
        for stage in CREATE..=CLOSE {
            // Establish normal closing first, then check duplicate and cross-case Late/Receipt isolation.
            gate.admit(stage, 0).unwrap();
        }
        // Late alone is not resource completion; duplicate Late and a future case's Receipt leave authority unchanged.
        gate.admit(LATE, 0).unwrap();
        assert!(gate.admit(LATE, 0).is_err());
        assert!(gate.admit(RECEIPT, 1).is_err());
        assert!(!gate.complete());
    }
}
