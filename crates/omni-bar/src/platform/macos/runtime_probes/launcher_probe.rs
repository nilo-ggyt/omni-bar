//! Standalone event-driven probe; default product builds contain no observer or resident probe state.

mod observers;
mod sequence;

use std::cell::{Cell, RefCell};
use std::time::Instant;

use objc2::MainThreadMarker;
use objc2::rc::{Retained, autoreleasepool};
use objc2::runtime::AnyObject;
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSApplicationDidBecomeActiveNotification,
    NSEvent, NSEventModifierFlags, NSEventType, NSWindowDidBecomeKeyNotification,
};
use objc2_foundation::{NSNotification, NSPoint};

use self::observers::{FirstResponderObservation, Notifications, ReadinessObserver};
use self::sequence::{Phase, Sequence};
use super::super::launcher_ui::{
    LauncherPresentation, LauncherProbeIdentity, LauncherReadiness, LauncherReleaseProbe,
};

thread_local! {
    /// Monotonic epochs prevent queued scalar blocks from affecting a later probe invocation.
    static NEXT_EPOCH: Cell<u64> = const { Cell::new(0) };
    /// Own observers, scalars and weak evidence only, never the native graph under test.
    static DRIVER: RefCell<Option<Driver>> = const { RefCell::new(None) };
}

/// Statistics include observer/getter and event-queue overhead, excluding OS key delivery and presented frames.
#[derive(Debug)]
pub struct LauncherRuntimeProbeResult {
    /// Number of complete warm creation/readiness/close/release samples; report the cold sample separately.
    pub samples: usize,
    /// First complete readiness observation before this tool has warmed AppKit.
    pub first_ms: f64,
    /// Minimum completed warm-sample latency.
    pub min_ms: f64,
    /// Nearest-rank median of completed warm samples.
    pub p50_ms: f64,
    /// Nearest-rank 95th percentile of completed warm samples.
    pub p95_ms: f64,
    /// Nearest-rank 99th percentile of completed warm samples.
    pub p99_ms: f64,
    /// Actual maximum latency, retaining the observed tail.
    pub max_ms: f64,
}

/// Per-invocation metadata uses weak native identities; observers have no graph ownership back-edge.
struct Driver {
    sequence: Sequence,
    observer: Retained<ReadinessObserver>,
    started: Option<Instant>,
    identity: Option<LauncherProbeIdentity>,
    kvo: Option<FirstResponderObservation>,
    last_readiness: Option<LauncherReadiness>,
    release: Option<LauncherReleaseProbe>,
    first_ms: f64,
    observations: Vec<f64>,
    failure: Option<String>,
    cleanup_generation: Option<u64>,
}

/// Compare Objective-C identity through a normal superclass reference, never content equality.
fn same_object(left: &AnyObject, right: &AnyObject) -> bool {
    std::ptr::eq(left, right)
}

/// A common-mode block carries only an epoch and static function, leaving the main-dispatch stack free.
fn enqueue(epoch: u64, work: fn(u64)) -> Result<(), String> {
    let run_loop = objc2_core_foundation::CFRunLoop::main().ok_or("probe main run loop missing")?;
    let callback: block2::RcBlock<dyn Fn()> = block2::RcBlock::new(move || {
        // Drain a local pool per dispatch so observation retains cannot cross a stage or release receipt.
        autoreleasepool(|_| work(epoch));
    });
    // SAFETY: The framework owns the loop/mode constants; perform_block copies the heap block for main-thread execution.
    unsafe {
        run_loop.perform_block(
            objc2_core_foundation::kCFRunLoopCommonModes.map(|mode| mode.as_ref()),
            Some(&callback),
        );
    }
    run_loop.wake_up();
    Ok(())
}

/// Copy weak identities and end the TLS borrow before loading native objects for identity comparison.
fn matches_panel(epoch: u64, object: &AnyObject) -> bool {
    let identity = DRIVER.with_borrow(|slot| {
        slot.as_ref()
            .filter(|driver| driver.sequence.epoch == epoch)?
            .identity
            .clone()
    });
    identity.is_some_and(|identity| {
        // Weak loading adds a temporary claim only for this synchronous comparison.
        identity
            .panel
            .load()
            .is_some_and(|panel| same_object(&panel, object))
    })
}

/// Accept notifications only from this app or the current panel/input; unrelated activity grants no sample authority.
fn notification_matches(epoch: u64, notification: &NSNotification) -> bool {
    let name = notification.name();
    // SAFETY: Exported AppKit names are process-valid immutable NSStrings, compared on the notification's main thread.
    if unsafe { name.isEqualToString(NSApplicationDidBecomeActiveNotification) } {
        // This AppKit event on the default center identifies the process app; queue_check revalidates its epoch.
        return true;
    }
    let Some(object) = notification.object() else {
        // Without an actual sender, an unscoped signal cannot identify this window or field.
        return false;
    };
    // SAFETY: Read public framework constants only, without accessing cross-thread native state.
    if unsafe { name.isEqualToString(NSWindowDidBecomeKeyNotification) } {
        return matches_panel(epoch, &object);
    }
    let input = DRIVER.with_borrow(|slot| {
        // End this handler borrow before weak loading or message dispatch.
        slot.as_ref()
            .filter(|driver| driver.sequence.epoch == epoch)?
            .identity
            .as_ref()
            .map(|identity| identity.input.clone())
    });
    input.is_some_and(|input| {
        // Only the current input field's actual begin-editing event can request a check.
        input
            .load()
            .is_some_and(|input| same_object(&input, &object))
    })
}

/// Coalesce real events once; if facts are not ready, wait for another real event without self-rescheduling.
fn queue_ready_check(epoch: u64) {
    let queue = DRIVER.with_borrow_mut(|slot| {
        // An empty slot or old epoch makes a stopped probe callback inert.
        slot.as_mut()
            .is_some_and(|driver| driver.sequence.queue_check(epoch))
    });
    if queue && let Err(error) = enqueue(epoch, observe_ready) {
        // Queue failure cannot synthesize readiness; retain the first cause and exit through normal close.
        fail(epoch, error);
    }
}

/// Copy presentation from the exact Active owner, ending the TLS borrow before every AppKit getter.
fn active_presentation() -> Option<LauncherPresentation> {
    super::super::ACTIVE_SURFACE
        .with_borrow(|owner| owner.as_ref().map(|owner| owner.presentation()))
}

/// Reserve timing and authority before real Host toggle; use no alternate construction path.
fn start_sample(epoch: u64) {
    let started = DRIVER.with_borrow_mut(|slot| {
        let driver = slot.as_mut()?;
        if !driver.sequence.begin_sample(epoch) {
            // A stale block or mismatched phase cannot open another window.
            return None;
        }
        driver.started = Some(Instant::now());
        driver.last_readiness = None;
        Some(())
    });
    if started.is_none() {
        // The probe stopped or this creation authority was already consumed.
        return;
    }
    super::super::toggle_foreground_surface();
    let Some(presentation) = active_presentation() else {
        fail(epoch, "Launcher probe did not acquire a surface".into());
        return;
    };
    let identity = presentation.probe_identity();
    drop(presentation);
    DRIVER.with_borrow_mut(|slot| {
        // Even if observer installation fails, cleanup targets only the acquired weak identity.
        if let Some(driver) = slot.as_mut() {
            driver.identity = Some(identity.clone());
        }
    });
    let observer = DRIVER.with_borrow(|slot| {
        slot.as_ref()
            .filter(|driver| driver.sequence.epoch == epoch)
            .map(|driver| driver.observer.clone())
    });
    let Some(observer) = observer else {
        // A revoked probe cannot install another observer.
        return;
    };
    let kvo = match FirstResponderObservation::install(&identity, &observer) {
        Ok(kvo) => kvo,
        Err(error) => {
            // Close has not begun; retain the first cause and normally close the acquired probe graph.
            fail(epoch, error);
            return;
        }
    };
    DRIVER.with_borrow_mut(|slot| {
        let driver = slot
            .as_mut()
            .expect("probe driver survives its own start block");
        driver.kvo = Some(kvo);
    });
    // Cover synchronously completed activation/focus; subsequent checks require real native events.
    queue_ready_check(epoch);
}

/// Consume readiness only at the first complete endpoint observation, recording bounded facts for every check.
fn observe_ready(epoch: u64) {
    let check = DRIVER.with_borrow_mut(|slot| {
        let driver = slot.as_mut()?;
        driver
            .sequence
            .begin_check(epoch)
            .then(|| driver.identity.clone())
            .flatten()
    });
    let Some(identity) = check else {
        // Closed, duplicate or pre-acquisition callbacks have no observation authority.
        return;
    };
    let Some(presentation) = active_presentation() else {
        fail(
            epoch,
            "Launcher owner disappeared while awaiting readiness".into(),
        );
        return;
    };
    let current = presentation.probe_identity();
    let same_owner =
        identity
            .panel
            .load()
            .zip(current.panel.load())
            .is_some_and(|(expected, actual)| {
                // Load both weak identities synchronously; no address comparison survives deallocation and reuse.
                std::ptr::eq(&*expected, &*actual)
            });
    if !same_owner {
        // Reject an externally replaced owner; never close the replacement, which may contain user work.
        fail(epoch, "Launcher readiness owner identity changed".into());
        return;
    }
    let mtm = MainThreadMarker::new().expect("probe blocks execute on main AppKit loop");
    let facts = presentation.input_readiness(mtm);
    // Timestamp readiness before extra Escape/weak capture so teardown diagnostics do not inflate its endpoint.
    let observed_at = Instant::now();
    DRIVER.with_borrow_mut(|slot| {
        let driver = slot
            .as_mut()
            .expect("current probe exists during readiness callback");
        if driver.last_readiness.is_none() {
            // Preserve the first failed facts for environment/focus diagnosis without logging queries or internal objects.
            eprintln!(
                "launcher_probe_sample={} readiness={facts:?}",
                driver.sequence.sample
            );
        }
        driver.last_readiness = Some(facts);
    });
    if !facts.is_ready() {
        // A false observation proves neither lock nor defect; missing events eventually fail the single parent watchdog.
        return;
    }
    if !presentation.has_exclusive_escape_routing() {
        fail(
            epoch,
            "Launcher has an Escape key equivalent outside guarded panel routing".into(),
        );
        return;
    }
    let release = match presentation.release_probe() {
        Ok(release) => release,
        Err(error) => {
            // Failed weak-evidence capture cannot begin the close of a successful sample.
            fail(epoch, error);
            return;
        }
    };
    drop(presentation);
    let Some(host_generation) = super::super::launcher_close::probe_generation().checked_add(1)
    else {
        fail(epoch, "Launcher close generation exhausted".into());
        return;
    };
    let kvo = DRIVER.with_borrow_mut(|slot| {
        let driver = slot
            .as_mut()
            .expect("probe driver owns the measured sample");
        let duration = observed_at
            .duration_since(driver.started.expect("sample start precedes readiness"))
            .as_secs_f64()
            * 1000.0;
        if !driver.sequence.accept_ready(epoch, host_generation) {
            // Revoked reentrant authority cannot issue another close or record a duplicate sample.
            return None;
        }
        if driver.sequence.sample == 0 {
            // Report the first sample separately instead of silently discarding cold framework cost.
            driver.first_ms = duration;
        } else {
            // Every warm statistic still corresponds to a genuinely reconstructed graph.
            driver.observations.push(duration);
        }
        driver.release = Some(release);
        driver.kvo.take()
    });
    // End the temporary KVO-removal window claim before requesting close or checking native release.
    drop(kvo);
    if let Err(error) = super::super::try_request_launcher_close() {
        fail(epoch, error);
        return;
    }
    if let Err(error) = super::super::try_request_launcher_close() {
        // Repeated identical intent exercises production coalescing without changing the accepted close direction.
        fail(epoch, error);
    }
}

/// Receive the real generation from production success without creating a receipt or consuming the barrier twice.
pub(super) fn foreground_released(host_generation: u64) {
    let info = DRIVER.with_borrow(|slot| {
        let driver = slot.as_ref()?;
        if driver.failure.is_some() {
            // Failed cleanup waits only for the original close and cannot become a successful sample.
            return (driver.cleanup_generation == Some(host_generation)).then_some((
                driver.sequence.epoch,
                true,
                None,
            ));
        }
        (driver.sequence.phase == Phase::AwaitingRelease { host_generation })
            .then(|| (driver.sequence.epoch, false, driver.release.clone()))
    });
    let Some((epoch, failed, release)) = info else {
        // Another probe or stale-generation close notification cannot advance this invocation.
        return;
    };
    if failed {
        stop_loop();
        return;
    }
    let valid = autoreleasepool(|_| {
        // Verify independent weak evidence in addition to the production receipt; end its pool before the next sample.
        release.is_some_and(|release| release.is_released())
            && super::require_probe_environment().is_ok()
    });
    if !valid {
        fail(
            epoch,
            "Launcher release receipt disagrees with weak/domain evidence".into(),
        );
        // This receipt is once-only; retain failure and stop instead of waiting for a second receipt after consumption.
        stop_loop();
        return;
    }
    let next = DRIVER.with_borrow_mut(|slot| {
        let driver = slot.as_mut()?;
        if !driver.sequence.accept_release(host_generation) {
            // Duplicate receipts cannot advance the sample count.
            return None;
        }
        driver.identity = None;
        driver.release = None;
        if driver.sequence.phase == Phase::Finished {
            // Stop the event loop only after the final actual release.
            Some(false)
        } else {
            // Return from this close callback before a separate run-loop block starts the next creation.
            Some(true)
        }
    });
    match next {
        Some(true) => {
            if let Err(error) = enqueue(epoch, start_sample) {
                // Preserve failure to schedule the next sample; do not report partial statistics as success.
                fail(epoch, error);
            }
        }
        Some(false) => stop_loop(),
        None => {
            // A receipt already consumed by another callback is inert.
        }
    }
}

/// Record the first failure once; accepted close retains its sole finalizer without force-clearing owners.
fn fail(epoch: u64, error: String) {
    let cleanup = DRIVER.with_borrow_mut(|slot| {
        let driver = slot
            .as_mut()
            .filter(|driver| driver.sequence.epoch == epoch)?;
        if driver.failure.is_some() {
            // Repeated failure callbacks cannot replace evidence or issue another cleanup request.
            return None;
        }
        eprintln!(
            "launcher_probe_failure sample={} phase={:?} readiness={:?} reason={error}",
            driver.sequence.sample, driver.sequence.phase, driver.last_readiness
        );
        driver.cleanup_generation = match driver.sequence.phase {
            Phase::AwaitingRelease { host_generation } => Some(host_generation),
            _ => None,
        };
        driver.failure = Some(error);
        driver.sequence.fail();
        Some((
            driver.kvo.take(),
            driver.cleanup_generation.is_some(),
            driver.identity.clone(),
        ))
    });
    let Some((kvo, already_closing, identity)) = cleanup else {
        return;
    };
    // Remove KVO outside TLS borrows so cleanup messages can safely reenter the state owner.
    drop(kvo);
    if already_closing {
        // Let accepted close finish; missing events still fail through the parent watchdog.
        return;
    }
    let same_owner = identity
        .and_then(|identity| identity.panel.load())
        .is_some_and(|expected| {
            // Clean only the panel acquired by this probe, never a replacement session created externally.
            active_presentation().is_some_and(|presentation| {
                presentation
                    .probe_identity()
                    .panel
                    .load()
                    .is_some_and(|actual| std::ptr::eq(&*expected, &*actual))
            })
        });
    if same_owner {
        let generation = super::super::launcher_close::probe_generation().checked_add(1);
        DRIVER.with_borrow_mut(|slot| {
            // Record the exact failed-cleanup receipt; it authorizes stop only, not another sample.
            if let Some(driver) = slot.as_mut() {
                driver.cleanup_generation = generation;
            }
        });
        if generation.is_some() && super::super::try_request_launcher_close().is_ok() {
            // Normal close still requires actual release even when the probe has already failed.
            return;
        }
        eprintln!("launcher_probe_cleanup_incomplete=true");
    }
    // Missing probe ownership or refused close returns failure without clearing production Closing state.
    stop_loop();
}

/// A run-loop source may call stop; a noninput AppDefined event wakes nextEvent so run can return.
fn stop_loop() {
    let mtm = MainThreadMarker::new().expect("probe completion executes on AppKit main loop");
    let app = NSApplication::sharedApplication(mtm);
    app.stop(None);
    if let Some(event) = NSEvent::otherEventWithType_location_modifierFlags_timestamp_windowNumber_context_subtype_data1_data2(
        NSEventType::ApplicationDefined,
        NSPoint::new(0.0, 0.0),
        NSEventModifierFlags::empty(),
        0.0,
        0,
        None,
        0,
        0,
        0,
    ) {
        // Wake this standalone event loop only; do not synthesize keyboard, mouse or physical-input evidence.
        app.postEvent_atStart(&event, true);
    }
}

/// Standalone entry point exported only by `native-acceptance` builds.
///
/// Call on the main thread with an empty foreground and nonrunning NSApplication; use one parent watchdog
/// for missing events. No timer, sleep or polling in this function can convert absent events into success.
pub fn run_native_launcher_runtime_probe(
    samples: usize,
    mtm: MainThreadMarker,
) -> Result<LauncherRuntimeProbeResult, String> {
    if !(1..=10_000).contains(&samples) {
        // Bound both statistics memory and native sample count; zero samples have no reportable percentiles.
        return Err("Launcher probe requires between 1 and 10000 samples".into());
    }
    super::require_probe_environment()?;
    let app = NSApplication::sharedApplication(mtm);
    if app.isRunning() || DRIVER.with_borrow(Option::is_some) {
        // Never nest the production event loop or replace a running probe owner.
        return Err("Launcher probe requires its own non-running AppKit event loop".into());
    }
    let epoch = NEXT_EPOCH
        .get()
        .checked_add(1)
        .ok_or("Launcher probe epoch exhausted")?;
    NEXT_EPOCH.set(epoch);
    let old_policy = app.activationPolicy();
    if old_policy != NSApplicationActivationPolicy::Accessory
        && !app.setActivationPolicy(NSApplicationActivationPolicy::Accessory)
        && app.activationPolicy() != NSApplicationActivationPolicy::Accessory
    {
        // Failure to establish the product activation policy occurs before acquiring a measured surface.
        return Err("Launcher probe cannot establish Accessory activation policy".into());
    }
    let observer = ReadinessObserver::new(epoch, mtm);
    let notifications = Notifications::install(&observer);
    let delegate = super::super::application_delegate::ResidentDelegateScope::install(&app, mtm);
    DRIVER.with_borrow_mut(|slot| {
        // Only this entry owns TLS state; observers call back with epochs and create no retain cycle.
        *slot = Some(Driver {
            sequence: Sequence::new(epoch, samples),
            observer,
            started: None,
            identity: None,
            kvo: None,
            last_readiness: None,
            release: None,
            first_ms: 0.0,
            observations: Vec::with_capacity(samples),
            failure: None,
            cleanup_generation: None,
        });
    });
    let scheduled = enqueue(epoch, start_sample);
    if scheduled.is_ok() {
        // run performs launch and dispatches actual WindowServer events; finishLaunching alone cannot replace it.
        app.run();
    }
    let mut driver = DRIVER.take().expect("probe scope owns one driver");
    // End TLS borrows before native unregistration/policy restoration; the epoch revokes all old blocks.
    drop(driver.kvo.take());
    drop(notifications);
    drop(delegate);
    if app.activationPolicy() != old_policy
        && !app.setActivationPolicy(old_policy)
        && app.activationPolicy() != old_policy
    {
        // Activation-policy restoration failure also prevents a successful probe result.
        driver
            .failure
            .get_or_insert("Launcher probe could not restore activation policy".into());
    }
    scheduled?;
    if let Some(failure) = driver.failure {
        return Err(failure);
    }
    if driver.sequence.phase != Phase::Finished || driver.observations.len() != samples {
        // External stop or missing receipts preserves final facts instead of publishing partial-sample percentiles.
        return Err(format!(
            "Launcher probe stopped before completion: sample={} phase={:?} readiness={:?}",
            driver.sequence.sample, driver.sequence.phase, driver.last_readiness
        ));
    }
    driver.observations.sort_by(f64::total_cmp);
    let rank = |percent: usize| {
        // The entry requires samples>0, keeping nearest-rank indices within completed warm observations.
        driver.observations[(samples * percent).div_ceil(100).saturating_sub(1)]
    };
    Ok(LauncherRuntimeProbeResult {
        samples,
        first_ms: driver.first_ms,
        min_ms: driver.observations[0],
        p50_ms: rank(50),
        p95_ms: rank(95),
        p99_ms: rank(99),
        max_ms: driver.observations[samples - 1],
    })
}
