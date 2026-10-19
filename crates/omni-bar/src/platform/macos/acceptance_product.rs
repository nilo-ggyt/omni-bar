//! Bounded synthetic workloads on the actual Host event loop, owning paths, scalars and weak native evidence only.

mod readiness;

use std::cell::{Cell, RefCell};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant};

use objc2::rc::{Retained, autoreleasepool};
use objc2::{ClassType, MainThreadMarker, MainThreadOnly, define_class, msg_send};
use objc2_app_kit::{NSApplication, NSEvent, NSEventModifierFlags, NSEventType};
use objc2_foundation::{NSNotification, NSPoint};
use omni_md_plugin::{NativeMarkdownHostedStep, NativeMarkdownHostedWorkload};
use omni_plugin_sdk::{
    PluginCloseIntent, PluginCloseRequest, PluginCloseScope, PluginCommandTarget,
};

use self::readiness::{LauncherReadinessWait, ReadinessSchedule};
use super::launcher_ui::{LauncherPresentation, LauncherProbeIdentity, LauncherReleaseProbe};
use crate::ForegroundSurface;
use crate::capabilities::shortcut_preferences::{ShortcutBinding, ShortcutSettingsStore};

const CYCLES: usize = 3;
const EVENT_SUBTYPE: i16 = 0x4f4d;
const STEP_FIRST: isize = 100;
const IDLE_SECONDS: u64 = 5;

/// Use existing plugin actions; adjacent checks are separate AppKit events, never queue pumping within one callback.
const STEPS: [NativeMarkdownHostedStep; 31] = [
    NativeMarkdownHostedStep::VerifyOpen,
    NativeMarkdownHostedStep::SelectSource,
    NativeMarkdownHostedStep::VerifySource,
    NativeMarkdownHostedStep::ShowFind,
    NativeMarkdownHostedStep::VerifyFindShown,
    NativeMarkdownHostedStep::EnterTableQuery,
    NativeMarkdownHostedStep::VerifyTableQuery,
    NativeMarkdownHostedStep::FindNext,
    NativeMarkdownHostedStep::VerifyTableFind,
    NativeMarkdownHostedStep::SelectPreview,
    NativeMarkdownHostedStep::VerifyPreview,
    NativeMarkdownHostedStep::SelectSource,
    NativeMarkdownHostedStep::VerifySource,
    NativeMarkdownHostedStep::EnterCodeQuery,
    NativeMarkdownHostedStep::VerifyCodeQuery,
    NativeMarkdownHostedStep::FindNext,
    NativeMarkdownHostedStep::VerifyCodeFind,
    NativeMarkdownHostedStep::SelectPreview,
    NativeMarkdownHostedStep::VerifyPreview,
    NativeMarkdownHostedStep::FindNext,
    NativeMarkdownHostedStep::VerifyCodeFindPreview,
    // Return to the event loop between edits/observations; only a verified restored save authorizes Back.
    NativeMarkdownHostedStep::BeginEditing,
    NativeMarkdownHostedStep::VerifyInserted,
    NativeMarkdownHostedStep::UndoEditing,
    NativeMarkdownHostedStep::VerifyUndo,
    NativeMarkdownHostedStep::RedoEditing,
    NativeMarkdownHostedStep::VerifyRedo,
    NativeMarkdownHostedStep::RestoreEditing,
    NativeMarkdownHostedStep::VerifyRestored,
    NativeMarkdownHostedStep::SaveRestored,
    NativeMarkdownHostedStep::VerifySaved,
];

/// Separate actual release receipts from next-event weak evidence; elapsed time changes no authority.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Phase {
    WaitingLaunch,
    LauncherReady,
    LauncherRelease { generation: u64, final_close: bool },
    ActivatePlugin,
    ObservePlugin,
    Step(usize),
    PluginRelease { generation: u64 },
    VerifyPluginRelease,
    Idle,
    Finished,
}

/// Scalar accepted-close evidence comes from a production-owner hook, never from the driver's expected phase.
#[derive(Clone, Copy)]
enum AcceptedClose {
    Launcher(u64),
    Plugin(u64, PluginCommandTarget, PluginCloseRequest),
}

/// Match request and actual acceptance separately; refusal, foreign or terminal-only evidence cannot authorize waiting.
fn matches_accepted_close(
    phase: Phase,
    expected_target: Option<PluginCommandTarget>,
    accepted: Option<AcceptedClose>,
) -> bool {
    match (phase, accepted) {
        (Phase::LauncherRelease { generation, .. }, Some(AcceptedClose::Launcher(actual))) => {
            generation == actual // Await only the generation established by this same actual will-close.
        }
        (
            Phase::PluginRelease { generation },
            Some(AcceptedClose::Plugin(actual, target, request)),
        ) => {
            // Require the same dynamic session, static target and foreground-return scope.
            generation == actual
                && expected_target == Some(target)
                && request.intent == PluginCloseIntent::ExitToLauncher
                && request.scope == PluginCloseScope::ForegroundTransition
        }
        _ => false, // Missing acceptance, mismatched stage type or prior completion preserves failure immediately.
    }
}

/// Bounded event values and driver epochs reject duplicates, reordering and late delivery from earlier cycles.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Stage {
    LauncherReady,
    ActivatePlugin,
    ObservePlugin,
    Step(usize),
    Return,
    PluginReleased,
    FinishIdle,
}

/// Pending ends an observation only; it grants no functional success, close or next-cycle authority.
#[derive(Clone, Copy, Eq, PartialEq)]
enum StageOutcome {
    Completed,
    Pending,
}

/// Return this request's scalar authority only; old cycles/drivers or duplicate endpoints cannot schedule events.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct StepTerminalSignal {
    epoch: u64,
    cycle: usize,
    index: usize,
}

impl Stage {
    /// Fixed scalar encoding accepts no arbitrary selector, plugin identity or file path.
    fn code(self) -> isize {
        match self {
            // Structural events use disjoint fixed ranges; step values come only from the bounded arrays below.
            Self::LauncherReady => 1,
            Self::ActivatePlugin => 2,
            Self::ObservePlugin => 3,
            Self::Step(index) => STEP_FIRST + index as isize,
            Self::Return => 200,
            Self::PluginReleased => 201,
            Self::FinishIdle => 202,
        }
    }

    /// Unknown event data has no successful default; callers must preserve ordering failure.
    fn decode(code: isize) -> Option<Self> {
        match code {
            1 => Some(Self::LauncherReady),
            2 => Some(Self::ActivatePlugin),
            3 => Some(Self::ObservePlugin),
            200 => Some(Self::Return),
            201 => Some(Self::PluginReleased),
            202 => Some(Self::FinishIdle),
            value if (STEP_FIRST..STEP_FIRST + STEPS.len() as isize).contains(&value) => {
                // Validate ranges before usize conversion so negative/overflowing values cannot become array indices.
                Some(Self::Step((value - STEP_FIRST) as usize))
            }
            _ => None, // External ApplicationDefined events have no authority to run probe actions.
        }
    }
}

/// The bridge stores original native weak/file identities; Rc adds no text, window or SDK-session ownership.
struct Driver {
    epoch: u64,
    root: PathBuf,
    origin: Instant,
    phase: Phase,
    pending: Option<Stage>,
    step_terminal: Option<StepTerminalSignal>,
    completed_cycles: usize,
    verified_editing_cycles: usize,
    launcher_readiness: Option<Rc<LauncherReadinessWait>>,
    readiness_schedule: ReadinessSchedule,
    launcher_release: Option<Rc<LauncherReleaseProbe>>,
    target: Option<PluginCommandTarget>,
    hosted: Option<Rc<NativeMarkdownHostedWorkload>>,
    first_failure: Option<String>,
}

impl Driver {
    /// Record editing success only after this cycle's sole final VerifySaved, never from Back or cleanup receipts.
    fn accept_verified_editing(&mut self) -> Result<(), String> {
        if self.phase != Phase::Step(STEPS.len() - 1)
            || self.pending.is_some()
            || self.step_terminal.is_some()
            || self.first_failure.is_some()
            || self.completed_cycles >= CYCLES
            || self.verified_editing_cycles != self.completed_cycles
        {
            // Intermediate stages, duplicate verification and failed cleanup cannot increase the editing-success count.
            return Err("正式保存验证不属于本轮唯一编辑终点".to_owned());
        }
        self.verified_editing_cycles += 1;
        Ok(())
    }

    /// Register one original endpoint observation without replacing a pending signal or queued verification.
    fn begin_step_terminal(&mut self, index: usize) -> Result<StepTerminalSignal, String> {
        if index >= STEPS.len()
            || self.phase != Phase::Step(index)
            || self.pending.is_some()
            || self.step_terminal.is_some()
            || self.first_failure.is_some()
            || self.completed_cycles >= CYCLES
        {
            return Err("整产品原步骤终点观察重复、失序或已撤权".to_owned());
        }
        let signal = StepTerminalSignal {
            epoch: self.epoch,
            cycle: self.completed_cycles,
            index,
        };
        self.step_terminal = Some(signal);
        Ok(signal)
    }

    /// A signal authorizes one original verification event; the functional predicate still decides success/failure.
    fn accept_step_terminal(&mut self, signal: StepTerminalSignal) -> bool {
        if self.step_terminal != Some(signal)
            || signal.epoch != self.epoch
            || signal.cycle != self.completed_cycles
            || self.phase != Phase::Step(signal.index)
            || self.pending.is_some()
            || self.first_failure.is_some()
        {
            return false; // Stale/cleanup notifications remain inert without consuming another current observation.
        }
        self.step_terminal = None;
        true
    }

    /// Consume scalar event authority first; native getters and Rc/Weak drops occur after the state borrow ends.
    fn accept_event(
        &mut self,
        epoch: isize,
        stage: Option<Stage>,
    ) -> Option<Result<Stage, String>> {
        if epoch != self.epoch as isize || self.phase == Phase::Finished {
            // Stale epochs and stopped drivers remain inert without consuming the current successor event.
            return None;
        }
        Some(match stage {
            Some(stage) if self.pending == Some(stage) => {
                self.pending = None;
                Ok(stage)
            }
            _ => Err("整产品阶段交付失序或重复".to_owned()),
        })
    }
}

/// Every return after install_fixture must release the named pasteboard and injected paths, not only success.
struct FixtureScope;

impl Drop for FixtureScope {
    fn drop(&mut self) {
        // Only the real main-thread entry constructs this owner; shared cleanup removes TLS ownership before reentry.
        super::acceptance_fixture::clear_fixture();
    }
}

thread_local! {
    /// Default products omit these slots; stopped probes retain no business resources or sampling history.
    static DRIVER: RefCell<Option<Driver>> = const { RefCell::new(None) };
    static NEXT_EPOCH: Cell<u64> = const { Cell::new(0) };
}

define_class!(
    /// Route only this probe's noninput events; forward all other events to the actual NSApplication.
    // SAFETY: NSApplication has no extra Rust ivars; the main-thread first sharedApplication creates the sole app object.
    #[unsafe(super = NSApplication)]
    #[thread_kind = MainThreadOnly]
    struct ProductAcceptanceApplication;

    impl ProductAcceptanceApplication {
        #[unsafe(method(sendEvent:))]
        fn send_event(&self, event: &NSEvent) {
            if route_event(event) {
                // Consume matching diagnostic subtypes only, never keyboard, mouse or product-menu events.
                return;
            }
            // SAFETY: The event remains borrowed on main; the matching superclass selector preserves NSApplication's ABI.
            unsafe { let _: () = msg_send![super(self), sendEvent: event]; }
        }
    }
);

/// Monotonic time locates stage observations; it proves neither presentation, physical-input latency nor memory acceptance.
fn emit(event: &str) {
    let metadata = DRIVER.with_borrow(|slot| {
        // Copy scalars only; stdout writes hold no reentrant business-state borrow.
        slot.as_ref()
            .map(|driver| (driver.completed_cycles, driver.origin.elapsed().as_nanos()))
    });
    if let Some((cycles, elapsed)) = metadata {
        // Output failure cannot change product close behavior; missing protocol records must fail in the parent.
        let _ = writeln!(
            std::io::stdout().lock(),
            "product_acceptance event={event} cycles={cycles} mono_ns={elapsed}"
        );
    }
}

/// Schedule one explicit successor for the acquired epoch, without timer/retry-based functional or resource progress.
fn enqueue(stage: Stage) -> Result<(), String> {
    let epoch = DRIVER.with_borrow_mut(|slot| {
        let driver = slot.as_mut().ok_or("整产品 driver 已结束")?;
        if driver.phase == Phase::Finished || driver.pending.is_some() {
            // Preserve an existing event; duplicate hooks cannot replace its successor action.
            return Err("整产品阶段重复排队".to_owned());
        }
        driver.pending = Some(stage);
        Ok(driver.epoch)
    })?;
    let mtm = MainThreadMarker::new().ok_or("整产品事件需要主线程")?;
    let event = NSEvent::otherEventWithType_location_modifierFlags_timestamp_windowNumber_context_subtype_data1_data2(
        NSEventType::ApplicationDefined,
        NSPoint::new(0.0, 0.0),
        NSEventModifierFlags::empty(),
        0.0,
        0,
        None,
        EVENT_SUBTYPE,
        epoch as isize,
        stage.code(),
    ).ok_or("整产品阶段原生事件创建失败")?;
    NSApplication::sharedApplication(mtm).postEvent_atStart(&event, false);
    Ok(())
}

/// The probe subtype cannot authorize another epoch; execute stages in the actual event callback's local pool.
fn route_event(event: &NSEvent) -> bool {
    if event.r#type() != NSEventType::ApplicationDefined || event.subtype().0 != EVENT_SUBTYPE {
        // Unrelated events keep production routing; driver presence cannot swallow user input.
        return false;
    }
    let epoch = event.data1();
    let event_code = event.data2();
    let stage = Stage::decode(event_code);
    let claim = DRIVER.with_borrow_mut(|slot| {
        // data1 was copied from the event; consume scalars without native getters during the state borrow.
        slot.as_mut()?.accept_event(epoch, stage)
    });
    if let Some(claim) = claim {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            // End temporary native claims with this event, before the next native release receipt.
            autoreleasepool(|_| claim.and_then(operate))
        }))
        .unwrap_or_else(|_| Err("整产品原生事件发生 Rust panic".to_owned()));
        emit(&format!(
            "stage_pool_drained stage={} ok={} pending={}",
            event_code,
            result == Ok(StageOutcome::Completed),
            result == Ok(StageOutcome::Pending)
        ));
        if let Err(error) = result {
            // Convert panic to fixed failure at the ABI boundary; never unwind through Objective-C.
            fail(error);
        }
    }
    true
}

/// Copy a stack-local presentation from Active and end its owner borrow before all native getters.
fn active_launcher() -> Result<LauncherPresentation, String> {
    super::ACTIVE_SURFACE
        .with_borrow(|surface| surface.as_ref().map(|surface| surface.presentation()))
        .ok_or_else(|| "真实 Launcher owner 不存在".to_owned())
}

/// Require empty native business owners and pure state; hidden windows or one empty session slot are insufficient.
fn foreground_is_empty() -> bool {
    !super::ACTIVE_SURFACE.with_borrow(Option::is_some)
        && !super::CLOSING_SURFACE.with_borrow(Option::is_some)
        && !super::plugin_host::exists()
        && !super::builtins::is_open()
        && super::FOREGROUND_STATE.with_borrow(|state| {
            // Recheck all four state categories; an unconsumed barrier cannot represent an empty completed foreground.
            state.active_surface().is_none()
                && state.pending_transition().is_none()
                && state.destroyed_barrier().is_none()
        })
}

/// Observe the product Launcher at launch and Back, without alternate test windows or invented readiness.
fn verify_launcher_ready() -> Result<StageOutcome, String> {
    DRIVER.with_borrow_mut(|slot| {
        slot.as_mut()
            .ok_or("Launcher 观察 driver 缺失")?
            .readiness_schedule
            .begin_check()
    })?;
    let observed = observe_launcher_readiness();
    let deferred_signal = DRIVER.with_borrow_mut(|slot| {
        slot.as_mut().is_some_and(|driver| {
            driver
                .readiness_schedule
                .end_check(observed == Ok(StageOutcome::Pending))
        })
    });
    if deferred_signal {
        // Consume only actual notification reentry from that getter stack; otherwise Pending never self-reschedules.
        let epoch = launcher_readiness_wait()
            .ok_or("Launcher 原始等待已撤销")?
            .epoch;
        queue_launcher_readiness_check(epoch);
    }
    observed
}

/// Borrow the original graph for observation; outer scheduling preserves every production readiness predicate.
fn observe_launcher_readiness() -> Result<StageOutcome, String> {
    let mtm = MainThreadMarker::new().ok_or("Launcher 验证需要主线程")?;
    let wait = launcher_readiness_wait().ok_or("Launcher 原始呈现弱身份缺失")?;
    if !wait.matches_active_owner() {
        // Original-graph loss or changed close generation revokes authority; a new Active owner cannot restart this cycle.
        return Err("等待就绪期间原 Launcher 身份或关闭代际已变化".to_owned());
    }
    let launcher = active_launcher()?;
    let ready = launcher.input_readiness(mtm);
    drop(launcher); // End temporary getter graph claims before subsequent close and independent release checks.
    let state_matches = super::FOREGROUND_STATE.with_borrow(|state| {
        state.active_surface() == Some(ForegroundSurface::Launcher)
            && state.pending_transition().is_none()
            && state.destroyed_barrier().is_none()
    });
    if !state_matches || super::plugin_host::exists() || !wait.matches_active_owner() {
        // The visible input must belong to the current transition-free owner; stale observations cannot authorize activation.
        return Err("Launcher 可见状态与 Host 所有权不一致".to_owned());
    }
    if !ready.is_ready() {
        // show returning does not prove key/focus delivery; keep original weak identities until another actual notification.
        emit(&format!("launcher_readiness_pending facts={ready:?}"));
        return Ok(StageOutcome::Pending);
    }
    Ok(StageOutcome::Completed)
}

/// Capture original-graph weak evidence and the next close generation before normal Launcher close.
fn close_launcher(final_close: bool) -> Result<(), String> {
    let wait = launcher_readiness_wait().ok_or("Launcher 关闭缺少原始呈现权限")?;
    if !wait.matches_active_owner() {
        // Failed cleanup may close the original graph only, not another owner acquired while awaiting notifications.
        return Err("原 Launcher 关闭权限已失效".to_owned());
    }
    let release = active_launcher()?.release_probe()?;
    let generation = super::launcher_close::probe_generation()
        .checked_add(1)
        .ok_or("Launcher 关闭代际耗尽")?;
    if !wait.matches_active_owner() {
        // Snapshot getters may reenter; recheck original weak identities without replacing them with successor identities.
        return Err("捕获 Launcher 释放证据期间原图已变化".to_owned());
    }
    let old_wait = DRIVER.with_borrow_mut(|slot| {
        let driver = slot.as_mut().ok_or("Launcher 关闭 driver 缺失")?;
        driver.launcher_release = Some(Rc::new(release));
        driver.phase = Phase::LauncherRelease {
            generation,
            final_close,
        };
        Ok::<_, String>(driver.launcher_readiness.take())
    })?;
    drop(old_wait); // Drop original weak metadata outside TLS borrows, without retaining the wait across this close.
    super::try_request_launcher_close()
}

/// Verification borrows the observation bridge, which owns neither the native graph nor the SDK session.
fn hosted() -> Result<Rc<NativeMarkdownHostedWorkload>, String> {
    DRIVER
        .with_borrow(|slot| {
            slot.as_ref()
                .and_then(|driver| driver.hosted.as_ref().map(Rc::clone))
        })
        .ok_or_else(|| "整产品 Markdown 原始观察桥缺失".to_owned())
}

/// Queue verification after the original request's actual endpoint; even synchronous steps use a separate event.
fn schedule_hosted_step(index: usize) -> Result<(), String> {
    let step = *STEPS.get(index).ok_or("终点观察步骤越界")?;
    let signal = DRIVER.with_borrow_mut(|slot| {
        slot.as_mut()
            .ok_or("原步骤终点 driver 缺失")?
            .begin_step_terminal(index)
    })?;
    let terminal = hosted()?.observe_step_terminal(step, move || hosted_step_terminal(signal))?;
    if terminal {
        // Pre-resolved and synchronously published endpoints consume the same authority without a second notification.
        hosted_step_terminal(signal);
    } else {
        emit(&format!(
            "step_terminal_pending stage={} receipt=original",
            Stage::Step(index).code()
        ));
    }
    Ok(())
}

/// MD ends its observation-slot borrow first; receive scalars only, without reading or retaining native/text objects.
fn hosted_step_terminal(signal: StepTerminalSignal) {
    let accepted = DRIVER.with_borrow_mut(|slot| {
        slot.as_mut()
            .is_some_and(|driver| driver.accept_step_terminal(signal))
    });
    if accepted && let Err(error) = enqueue(Stage::Step(signal.index)) {
        fail(error); // Failed event creation cannot leave a resolved wait whose event can never arrive.
    }
}

/// Bind the current session generation before requesting plugin Back, never substituting whole-app Quit.
fn request_return() -> Result<(), String> {
    let editing_verified = DRIVER.with_borrow(|slot| {
        slot.as_ref().is_some_and(|driver| {
            driver.verified_editing_cycles == driver.completed_cycles + 1
                && driver.first_failure.is_none()
        })
    });
    if !editing_verified {
        // Normal Back requires this cycle's restored saved bytes; failed cleanup uses the bridge without reporting success.
        return Err("本轮编辑和正式保存尚未完成验证，不能记录成功返回".to_owned());
    }
    let generation =
        super::plugin_host::product_probe_generation().ok_or("真实插件 session 代际缺失")?;
    DRIVER.with_borrow_mut(|slot| {
        let driver = slot.as_mut().ok_or("插件返回 driver 缺失")?;
        driver.phase = Phase::PluginRelease { generation };
        Ok::<_, String>(())
    })?;
    emit("plugin_return_requested");
    hosted()?.request_return()
}

/// Each event performs one action; an independent actual-native receipt hook advances release stages.
fn operate(stage: Stage) -> Result<StageOutcome, String> {
    let phase = DRIVER
        .with_borrow(|slot| slot.as_ref().map(|driver| driver.phase))
        .ok_or("driver 缺失")?;
    emit(&format!("stage_begin stage={}", stage.code()));
    match (stage, phase) {
        (Stage::LauncherReady, Phase::LauncherReady) => {
            if verify_launcher_ready()? == StageOutcome::Pending {
                // Do not close or count success; only a later actual notification for the original identity may queue Stage1.
                return Ok(StageOutcome::Pending);
            }
            emit("launcher_ready visible=true input_ready=true sdk_session_owned=false");
            let final_close = DRIVER.with_borrow(|slot| {
                slot.as_ref().is_some_and(|driver| {
                    driver.completed_cycles == CYCLES || driver.first_failure.is_some()
                })
            });
            // Require actual Launcher readiness after each Back before choosing another cycle or final foreground close.
            close_launcher(final_close)?;
        }
        (Stage::ActivatePlugin, Phase::ActivatePlugin) => {
            if !foreground_is_empty() {
                return Err("原 Launcher 未完全释放，禁止插件激活".to_owned());
            }
            let registration = *crate::composition::plugins::PluginCatalog::new()
                .registrations()
                .first()
                .ok_or("真实插件目录为空")?;
            let command = registration
                .descriptor()
                .commands
                .first()
                .ok_or("真实插件命令描述为空")?;
            // Static descriptors determine activation; neither product nor probe branches on plugin ID or page type.
            super::plugin_host::activate(registration, command.id)
                .map_err(|error| error.to_string())?;
            let target = super::FOREGROUND_STATE
                .with_borrow(|state| {
                    if let Some(ForegroundSurface::Plugin(target)) = state.active_surface() {
                        Some(target)
                    } else {
                        None // Completed activation or the wrong foreground cannot represent a mounted Markdown session.
                    }
                })
                .ok_or("目录激活没有建立真实插件前台")?;
            DRIVER.with_borrow_mut(|slot| {
                if let Some(driver) = slot.as_mut() {
                    driver.target = Some(target);
                    driver.phase = Phase::ObservePlugin;
                }
            });
            emit("cycle_started activation=static_catalog session=production");
            enqueue(Stage::ObservePlugin)?;
        }
        (Stage::ObservePlugin, Phase::ObservePlugin) => {
            let path = DRIVER
                .with_borrow(|slot| {
                    slot.as_ref()
                        .map(|driver| driver.root.join("workspace/Acceptance.md"))
                })
                .ok_or("合成文档路径权限缺失")?;
            let bridge = NativeMarkdownHostedWorkload::observe_active_document(&path)?;
            DRIVER.with_borrow_mut(|slot| {
                if let Some(driver) = slot.as_mut() {
                    driver.hosted = Some(Rc::new(bridge));
                    driver.phase = Phase::Step(0);
                }
            });
            // Create the observation bridge and activate in separate actual events, without extra plugins or fake sessions.
            schedule_hosted_step(0)?;
        }
        (Stage::Step(index), Phase::Step(expected)) if index == expected => {
            let step = *STEPS.get(index).ok_or("工作负载步骤越界")?;
            hosted()?.step(step)?;
            if index + 1 == STEPS.len() {
                DRIVER.with_borrow_mut(|slot| {
                    slot.as_mut()
                        .ok_or("编辑验证 driver 缺失")?
                        .accept_verified_editing()
                })?;
                emit(
                    "editing_cycle_verified editing_verified=true restored_bytes=true formal_save_verified=true",
                );
                // Permit actual Back only after long-document, Unicode edit, Undo/Redo and real-save verification complete.
                enqueue(Stage::Return)?;
            } else {
                DRIVER.with_borrow_mut(|slot| {
                    if let Some(driver) = slot.as_mut() {
                        driver.phase = Phase::Step(index + 1);
                    }
                });
                schedule_hosted_step(index + 1)?;
            }
        }
        (Stage::Return, Phase::Step(index)) if index + 1 == STEPS.len() => request_return()?,
        (Stage::PluginReleased, Phase::VerifyPluginRelease) => {
            hosted()?.verify_native_closed()?;
            if super::plugin_host::exists()
                || super::plugin_host::product_probe_generation().is_some()
            {
                return Err("Native 弱证据完成但 Host 仍持 SDK session".to_owned());
            }
            let old_bridge = DRIVER.with_borrow_mut(|slot| {
                let driver = slot.as_mut()?;
                driver.completed_cycles += 1;
                driver.target = None;
                driver.phase = Phase::LauncherReady;
                driver.hosted.take()
            });
            drop(old_bridge); // End the original weak bridge before the next sample/cycle without retaining three cycles of history.
            let successful = DRIVER.with_borrow(|slot| {
                slot.as_ref().is_some_and(|driver| {
                    driver.first_failure.is_none()
                        && driver.verified_editing_cycles == driver.completed_cycles
                })
            });
            emit(&format!(
                "cycle_completed native_closed=true sdk_session_dropped=true host_barrier_consumed=true workload_passed={successful} editing_verified={successful}"
            ));
            enqueue(Stage::LauncherReady)?;
        }
        (Stage::FinishIdle, Phase::Idle) => {
            if !foreground_is_empty() {
                return Err("最终驻留窗口内出现新的业务 owner".to_owned());
            }
            DRIVER.with_borrow_mut(|slot| {
                if let Some(driver) = slot.as_mut() {
                    driver.phase = Phase::Finished;
                }
            });
            emit("resident_idle_completed seconds=5 foreground_empty=true");
            stop_loop();
        }
        _ => return Err("整产品事件与当前阶段不一致".to_owned()),
    }
    emit(&format!(
        "stage_end stage={} pool_scope=event",
        stage.code()
    ));
    Ok(StageOutcome::Completed)
}

/// Actual launch starts the sole installed probe driver without adding another product startup path.
pub(super) fn application_did_finish_launching(mtm: MainThreadMarker) {
    let start = DRIVER.with_borrow_mut(|slot| {
        let Some(driver) = slot.as_mut() else {
            return false;
        };
        if driver.phase != Phase::WaitingLaunch {
            return false; // Duplicate notification cannot start another sample.
        }
        driver.phase = Phase::LauncherReady;
        true
    });
    if start {
        if !super::shortcuts::product_probe_registration_active() {
            // Registration conflict/settings failure is explicit; do not choose another hotkey to bypass product resident cost.
            fail("真实产品启动未取得唯一已配置热键".to_owned());
            return;
        }
        emit(
            "initialized product_scope=host_launcher_catalog_sdk_markdown physical_input=false present=false editing_verified=false memory_budget_pass=unmeasured",
        );
        super::show_launcher_surface(mtm);
    }
}

/// Presentation hooks infer no readiness and never close while show's temporary native claims remain live.
pub(super) fn launcher_presented(identity: LauncherProbeIdentity) {
    let claim = DRIVER.with_borrow(|slot| {
        let driver = slot.as_ref()?;
        if !matches!(
            driver.phase,
            Phase::LauncherReady | Phase::VerifyPluginRelease
        ) {
            // Other plugin stages or show after stop cannot create an acceptance wait.
            return None;
        }
        Some((driver.epoch, driver.launcher_readiness.is_some()))
    });
    let Some((epoch, already_captured)) = claim else {
        return;
    };
    if !already_captured {
        // Bind once at actual show return; later notifications, repeated show and getters cannot replace the anchor.
        let wait = Rc::new(LauncherReadinessWait::new(
            epoch,
            super::launcher_close::probe_generation(),
            identity,
        ));
        if !wait.matches_active_owner() {
            fail("Launcher 呈现结束时原弱身份已经失效".to_owned());
            return;
        }
        DRIVER.with_borrow_mut(|slot| {
            if let Some(driver) = slot.as_mut()
                && driver.epoch == epoch
                && driver.launcher_readiness.is_none()
            {
                // The native weak-load call has returned; pass weak metadata only, with no native calls here.
                driver.launcher_readiness = Some(wait);
            }
        });
    }
    queue_launcher_readiness_check(epoch);
}

/// Copy the current original wait's Rc only; it retains no additional native, text or SDK-session owner.
fn launcher_readiness_wait() -> Option<Rc<LauncherReadinessWait>> {
    DRIVER.with_borrow(|slot| {
        let driver = slot.as_ref()?;
        driver
            .launcher_readiness
            .as_ref()
            .filter(|wait| wait.epoch == driver.epoch)
            .map(Rc::clone)
    })
}

/// One queued Stage1 coalesces actual notifications; Pending never calls this function to retry itself.
fn queue_launcher_readiness_check(epoch: u64) {
    let queue = DRIVER.with_borrow_mut(|slot| {
        slot.as_mut().is_some_and(|driver| {
            driver.epoch == epoch
                && driver.phase == Phase::LauncherReady
                && driver.launcher_readiness.is_some()
                && driver.pending.is_none()
                && driver.readiness_schedule.signal()
        })
    });
    if queue && let Err(error) = enqueue(Stage::LauncherReady) {
        // Event creation failure must fail explicitly instead of leaving pending authority that cannot be delivered.
        fail(error);
    }
}

/// Actual app activation justifies observation only; nonactivating-panel readiness keeps the original predicate.
pub(super) fn application_did_become_active(notification: &NSNotification, mtm: MainThreadMarker) {
    let Some(wait) = launcher_readiness_wait() else {
        return; // Early notifications without the original show anchor cannot create observation authority.
    };
    let Some(object) = notification.object() else {
        return; // A notification name without a sender cannot establish this app's activation.
    };
    let app = NSApplication::sharedApplication(mtm);
    let expected: &objc2::runtime::AnyObject = &app;
    if std::ptr::eq(expected, &*object) {
        // Getter/notification retains stay within this callback; queue one bounded scalar successor only.
        queue_launcher_readiness_check(wait.epoch);
    }
}

/// Actual delegate key notifications must identify this cycle's original panel; other windows remain inert.
pub(super) fn launcher_did_become_key(notification: &NSNotification) {
    if let Some(wait) = launcher_readiness_wait()
        && wait.matches_panel_notification(notification)
    {
        queue_launcher_readiness_check(wait.epoch);
    }
}

/// Actual editing-begin covers the valid ordering where key notification precedes field-editor installation.
pub(super) fn launcher_did_begin_editing(notification: &NSNotification) {
    if let Some(wait) = launcher_readiness_wait()
        && wait.matches_input_notification(notification)
    {
        queue_launcher_readiness_check(wait.epoch);
    }
}

/// Accept the exact foreground Launcher barrier generation, then independently verify original-graph weak evidence.
pub(super) fn launcher_released(generation: u64) {
    let observation = DRIVER.with_borrow(|slot| {
        let driver = slot.as_ref()?;
        match driver.phase {
            Phase::LauncherRelease {
                generation: expected,
                final_close,
            } if generation == expected => Some((
                driver.launcher_release.as_ref().map(Rc::clone),
                final_close,
                driver.first_failure.is_some(),
                driver.epoch,
            )),
            _ => None, // Another probe's close or a previous cycle's late receipt cannot activate the plugin.
        }
    });
    let Some((release, final_close, failed, epoch)) = observation else {
        return;
    };
    let released = autoreleasepool(|_| release.is_some_and(|release| release.is_released()));
    if !released || !foreground_is_empty() {
        fail("Launcher 原图弱证据与真实 Host 释放收据不一致".to_owned());
        // The sole actual receipt arrived; failure cannot continue waiting for a nonexistent second receipt.
        stop_loop();
        return;
    }
    let old_release = DRIVER.with_borrow_mut(|slot| {
        if let Some(driver) = slot.as_mut() {
            driver.phase = if final_close {
                Phase::Idle
            } else {
                Phase::ActivatePlugin
            };
            driver.launcher_release.take()
        } else {
            None // A missing driver retains no original-graph weak metadata.
        }
    });
    drop(old_release); // Even the final objc Weak drop must occur outside the DRIVER borrow.
    emit("launcher_released native_weak_empty=true host_barrier_consumed=true");
    if failed {
        // Normal cleanup after failure cannot become success and need not extend the memory-sampling window.
        DRIVER.with_borrow_mut(|slot| {
            if let Some(driver) = slot.as_mut() {
                driver.phase = Phase::Finished;
            }
        });
        stop_loop();
    } else if final_close {
        emit("resident_idle_begin seconds=5 foreground_empty=true");
        let deadline = dispatch2::DispatchTime::try_from(Duration::from_secs(IDLE_SECONDS));
        let scheduled = deadline
            .map_err(|_| "驻留测量时长无效".to_owned())
            .and_then(|deadline| {
                dispatch2::DispatchQueue::main()
                    .after(deadline, move || {
                        let current = DRIVER.with_borrow(|slot| {
                            slot.as_ref().is_some_and(|driver| {
                                driver.epoch == epoch && driver.phase == Phase::Idle
                            })
                        });
                        if current && let Err(error) = enqueue(Stage::FinishIdle) {
                            fail(error);
                        }
                    })
                    .map_err(|_| "驻留测量结束事件无法调度".to_owned())
            });
        // This one-shot duration extends already-released resident sampling only, without checking or retrying release conditions.
        if let Err(error) = scheduled {
            fail(error);
        }
    } else if let Err(error) = enqueue(Stage::ActivatePlugin) {
        fail(error);
    }
}

/// Record foreground Back after matching finish_close, SDK session Drop and Host-barrier consumption.
pub(super) fn plugin_released(
    generation: u64,
    target: PluginCommandTarget,
    request: PluginCloseRequest,
) {
    let matched = DRIVER.with_borrow_mut(|slot| {
        let driver = slot.as_mut()?;
        if driver.phase != (Phase::PluginRelease { generation }) || driver.target != Some(target) {
            return None; // Stale, foreign or unrelated sessions cannot advance this cycle.
        }
        if request.intent != PluginCloseIntent::ExitToLauncher
            || request.scope != PluginCloseScope::ForegroundTransition
        {
            return Some(Err("整产品返回收到错误关闭意图或终止范围".to_owned()));
        }
        driver.phase = Phase::VerifyPluginRelease;
        Some(Ok(()))
    });
    if let Some(result) = matched {
        emit("plugin_release_receipt scope=foreground intent=return_to_launcher");
        if let Err(error) = result.and_then(|_| enqueue(Stage::PluginReleased)) {
            fail(error);
        }
    }
}

/// Preserve first failure and use existing Back/Launcher close cleanup, never app Quit as a substitute for plugin exit.
fn fail(error: String) {
    let first = DRIVER.with_borrow_mut(|slot| {
        let driver = slot.as_mut()?;
        if driver.phase == Phase::Finished || driver.first_failure.is_some() {
            return None; // Reentrant failure cleanup cannot request another close or overwrite the first cause.
        }
        driver.first_failure = Some(error);
        driver.pending = None;
        driver.step_terminal = None; // Revoke old verification queueing before cleanup can cancel the original request.
        Some((
            driver.phase,
            driver.hosted.as_ref().map(Rc::clone),
            driver.target,
        ))
    });
    let Some((phase, bridge, expected_target)) = first else {
        return;
    };
    emit("failed acceptance=false");
    if matches!(
        phase,
        Phase::LauncherRelease { .. } | Phase::PluginRelease { .. }
    ) {
        let accepted = match phase {
            Phase::LauncherRelease { .. } => {
                super::launcher_close::product_probe_accepted_close_generation()
                    .map(AcceptedClose::Launcher)
            }
            Phase::PluginRelease { .. } => super::plugin_host::product_probe_accepted_close().map(
                |(generation, target, request)| AcceptedClose::Plugin(generation, target, request),
            ),
            _ => None, // The type was constrained above; this branch grants no other close authority.
        };
        if !matches_accepted_close(phase, expected_target, accepted) {
            // Refused close/save preflight has no future receipt; stop with failure without repeated close or fake release.
            stop_loop();
        }
        // Await a receipt only for an actual matching accepted close; the parent watchdog still fails missing signals.
        return;
    }
    if let Some(bridge) = bridge {
        let generation = super::plugin_host::product_probe_generation();
        if let Some(generation) = generation {
            DRIVER.with_borrow_mut(|slot| {
                if let Some(driver) = slot.as_mut() {
                    driver.phase = Phase::PluginRelease { generation };
                }
            });
            if bridge.request_return().is_ok() {
                return; // After the actual release hook, verify native weak evidence before closing the returned Launcher.
            }
        }
    }
    if super::ACTIVE_SURFACE.with_borrow(Option::is_some) && close_launcher(true).is_ok() {
        return; // Cleanup also requires an actual foreground-close receipt; close returning is not release.
    }
    // Missing original bridge or refused close stops with failure, never clearing production owners to fake release.
    stop_loop();
}

/// Post a wake-only event after stop so nextEvent can return, without input or menu Quit.
fn stop_loop() {
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    let app = NSApplication::sharedApplication(mtm);
    app.stop(None);
    if let Some(event) = NSEvent::otherEventWithType_location_modifierFlags_timestamp_windowNumber_context_subtype_data1_data2(
        NSEventType::ApplicationDefined, NSPoint::new(0.0, 0.0), NSEventModifierFlags::empty(), 0.0, 0, None,
        0, 0, 0,
    ) {
        app.postEvent_atStart(&event, true);
    }
}

/// Run three actual product cycles in a parent-owned new directory, then sample five seconds of empty-foreground residency.
/// Supply the verified existing shortcut explicitly; write synthetic settings only, without alternate keys or user edits.
///
/// Editing success requires each cycle's complete Unicode replacement, Undo/Redo, restoration and actual save;
/// this entry proves no physical input, presentation or whole-product memory budget. The parent checks attribution/sampling/exit.
/// Create/delete only this entry's synthetic files within the supplied directory.
///
/// # Errors
///
/// Preserve explicit failure for non-main calls, existing foreground, nonempty directories and native/step/save/release errors.
pub fn run_native_product_acceptance(root: &Path, shortcut: &str) -> Result<(), String> {
    let configured_shortcut = ShortcutBinding::parse(shortcut)
        .map_err(|_| "整产品验收必须使用明确有效的既有启动器快捷键".to_owned())?;
    MainThreadMarker::new().ok_or("整产品验收需要真实 AppKit 主线程")?;
    if DRIVER.with_borrow(Option::is_some) || !foreground_is_empty() {
        return Err("整产品验收需要全新空前台进程".to_owned());
    }
    if !root.is_absolute()
        || !root.is_dir()
        || root
            .read_dir()
            .map_err(|_| "合成根目录不可读")?
            .next()
            .is_some()
    {
        // Initialize only an explicitly new empty directory; never delete or rewrite caller-owned existing contents.
        return Err("整产品验收目录必须是绝对路径的新空目录".to_owned());
    }
    let _pasteboard = super::acceptance_fixture::install_fixture(root)?;
    let fixture = FixtureScope;
    // The isolated fixture has no settings and the default may conflict; copy the caller-verified binding and register it once.
    ShortcutSettingsStore::new(root.join("application-support"))
        .persist(configured_shortcut)
        .map_err(|_| "无法为合成验收持久化明确的既有快捷键".to_owned())?;
    // Remove only the two fixture-created inputs; one long note makes selection independent of directory enumeration order.
    for name in ["Acceptance.md", "SourcePerformance.md"] {
        std::fs::remove_file(root.join("workspace").join(name))
            .map_err(|_| "移除本工具短样本失败")?;
    }
    let note = root.join("workspace/Acceptance.md");
    omni_md_plugin::write_native_markdown_memory_fixture(&note)
        .map_err(|_| "流式长文夹具生成失败")?;
    let epoch = NEXT_EPOCH
        .get()
        .checked_add(1)
        .filter(|value| *value <= isize::MAX as u64)
        .ok_or("整产品验收代际耗尽")?;
    NEXT_EPOCH.set(epoch);
    // SAFETY: Receive the public singleton message as NSApplication; an existing different app still satisfies its return ABI and
    // +1 retain contract. The separate dynamic cast verifies the router without assuming the superclass is our subclass.
    let app: Retained<NSApplication> =
        unsafe { msg_send![ProductAcceptanceApplication::class(), sharedApplication] };
    let app = app
        .downcast::<ProductAcceptanceApplication>()
        .map_err(|_| "整产品验收需要首次创建自己的应用事件路由".to_owned())?;
    if app.isRunning() {
        return Err("整产品验收不能嵌套已有应用事件循环".to_owned());
    }
    DRIVER.with_borrow_mut(|slot| {
        *slot = Some(Driver {
            epoch,
            root: root.to_owned(),
            origin: Instant::now(),
            phase: Phase::WaitingLaunch,
            pending: None,
            step_terminal: None,
            completed_cycles: 0,
            verified_editing_cycles: 0,
            launcher_readiness: None,
            readiness_schedule: ReadinessSchedule::default(),
            launcher_release: None,
            target: None,
            hosted: None,
            first_failure: None,
        });
    });
    emit(
        "configured cycles_planned=3 steps_per_cycle=31 input=programmatic_native_actions editing_verified=false memory_budget_pass=unmeasured",
    );
    let result = super::run_application();
    // run_application unregisters hotkeys on stop; independently check actual empty ownership rather than infer release from source.
    let released = foreground_is_empty() && super::shortcuts::product_probe_registration_released();
    drop(fixture);
    let state = DRIVER.take().ok_or("整产品 driver 提前丢失")?;
    result?;
    if let Some(error) = state.first_failure {
        return Err(error); // Successful cleanup cannot erase a rendering/Find-step failure.
    }
    if state.phase != Phase::Finished
        || state.completed_cycles != CYCLES
        || state.verified_editing_cycles != CYCLES
        || !released
        || state.hosted.is_some()
        || state.step_terminal.is_some()
    {
        return Err("整产品循环缺失终点、真实前台释放或热键注销证据".to_owned());
    }
    println!(
        "product_acceptance event=finished cycles=3 foreground_empty=true sdk_session_owned=false global_key_owner_released=true fixture_cleared=true editing_verified=true memory_budget_pass=unmeasured"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A native/file/text-free driver tests asynchronous authority without constructing an alternate product graph.
    fn fresh_driver() -> Driver {
        Driver {
            epoch: 9,
            root: PathBuf::new(),
            origin: Instant::now(),
            phase: Phase::Step(0),
            pending: Some(Stage::Step(0)),
            step_terminal: None,
            completed_cycles: 0,
            verified_editing_cycles: 0,
            launcher_readiness: None,
            readiness_schedule: ReadinessSchedule::default(),
            launcher_release: None,
            target: None,
            hosted: None,
            first_failure: None,
        }
    }

    #[test]
    fn event_codes_reject_unowned_steps_and_preserve_every_declared_stage() {
        // Reject negative, gap and overflowing values before they can become indices or Back events.
        for value in [
            -1,
            0,
            4,
            STEP_FIRST - 1,
            STEP_FIRST + STEPS.len() as isize,
            203,
            isize::MAX,
        ] {
            assert_eq!(Stage::decode(value), None);
        }
        for index in 0..STEPS.len() {
            let step = Stage::Step(index);
            assert_eq!(Stage::decode(step.code()), Some(step));
        }
        for stage in [
            Stage::LauncherReady,
            Stage::ActivatePlugin,
            Stage::ObservePlugin,
            Stage::Return,
            Stage::PluginReleased,
            Stage::FinishIdle,
        ] {
            assert_eq!(Stage::decode(stage.code()), Some(stage));
        }
    }

    #[test]
    fn original_event_authority_survives_stale_delivery_and_rejects_duplicate_consumption() {
        let mut driver = fresh_driver();
        // A previous cycle's same-stage event cannot consume current authority; reject reordering without replacing successors.
        assert_eq!(driver.accept_event(8, Some(Stage::Step(0))), None);
        assert_eq!(driver.pending, Some(Stage::Step(0)));
        assert!(
            driver
                .accept_event(9, Some(Stage::Return))
                .unwrap()
                .is_err()
        );
        assert_eq!(driver.pending, Some(Stage::Step(0)));
        assert_eq!(
            driver.accept_event(9, Some(Stage::Step(0))),
            Some(Ok(Stage::Step(0)))
        );
        assert!(driver.pending.is_none());
        assert!(
            driver
                .accept_event(9, Some(Stage::Step(0)))
                .unwrap()
                .is_err()
        );
        // After actual stop, even a forged matching pending event has no execution authority.
        driver.phase = Phase::Finished;
        driver.pending = Some(Stage::Step(1));
        assert_eq!(driver.accept_event(9, Some(Stage::Step(1))), None);
        assert_eq!(driver.pending, Some(Stage::Step(1)));
    }

    #[test]
    fn pending_request_needs_one_matching_terminal_and_never_creates_its_own_event() {
        let mut driver = fresh_driver();
        driver.pending = None;
        driver.phase = Phase::Step(2);
        let signal = driver.begin_step_terminal(2).unwrap();
        // Registration is not completion and queues no Stage102 or success; missing endpoints fail through the external watchdog.
        assert_eq!(driver.step_terminal, Some(signal));
        assert!(driver.pending.is_none());
        assert_eq!(driver.verified_editing_cycles, 0);
        assert!(driver.begin_step_terminal(2).is_err());
        for stale in [
            StepTerminalSignal { epoch: 8, ..signal },
            StepTerminalSignal { cycle: 1, ..signal },
            StepTerminalSignal { index: 4, ..signal },
        ] {
            assert!(!driver.accept_step_terminal(stale));
            assert_eq!(driver.step_terminal, Some(signal));
        }
        assert!(driver.accept_step_terminal(signal));
        assert!(driver.step_terminal.is_none());
        assert!(!driver.accept_step_terminal(signal)); // Duplicate actual notification cannot queue another verification.
        assert_eq!(driver.verified_editing_cycles, 0); // The endpoint grants verification authority only, not functional success.
    }

    #[test]
    fn next_cycle_and_failure_cleanup_cannot_inherit_old_terminal_authority() {
        let mut driver = fresh_driver();
        driver.pending = None;
        driver.phase = Phase::Step(8);
        let old = driver.begin_step_terminal(8).unwrap();
        assert!(driver.accept_step_terminal(old));
        driver.completed_cycles = 1;
        let current = driver.begin_step_terminal(8).unwrap();
        assert!(!driver.accept_step_terminal(old));
        assert_eq!(driver.step_terminal, Some(current));
        driver.first_failure = Some("原生认证失败".to_owned());
        assert!(!driver.accept_step_terminal(current));
        assert!(driver.pending.is_none());
        // fail() revokes the scalar slot first; later valid cancellation cannot revive functional verification.
        driver.step_terminal = None;
        driver.phase = Phase::PluginRelease { generation: 2 };
        assert!(!driver.accept_step_terminal(current));
        assert!(driver.begin_step_terminal(8).is_err());
    }

    #[test]
    fn editing_success_requires_each_unique_saved_endpoint_and_cannot_be_signed_by_cleanup() {
        let mut driver = fresh_driver();
        driver.pending = None;
        driver.phase = Phase::Step(20);
        // Read-only long-text completion has not edited/saved; truncating 31 steps to 21 cannot retain editing success.
        assert!(driver.accept_verified_editing().is_err());
        assert_eq!(driver.verified_editing_cycles, 0);
        driver.phase = Phase::Step(STEPS.len() - 1);
        driver.pending = Some(Stage::Step(STEPS.len() - 1));
        assert!(driver.accept_verified_editing().is_err());
        // Only the first consumed final VerifySaved event may certify this cycle's editing result.
        driver.pending = None;
        assert!(driver.accept_verified_editing().is_ok());
        assert_eq!(driver.verified_editing_cycles, 1);
        assert!(driver.accept_verified_editing().is_err());
        // Back cleanup after failure cannot add another editing success even in the same phase.
        driver.completed_cycles = 1;
        driver.first_failure = Some("synthetic_rejection".to_owned());
        assert!(driver.accept_verified_editing().is_err());
        assert_eq!(driver.verified_editing_cycles, 1);
        driver.first_failure = None;
        assert!(driver.accept_verified_editing().is_ok());
        // Grant no verification beyond three full cycles; duplicate receipts or extra cycles cannot enlarge the declared sample set.
        driver.completed_cycles = CYCLES;
        driver.verified_editing_cycles = CYCLES;
        assert!(driver.accept_verified_editing().is_err());
        assert_eq!(driver.verified_editing_cycles, CYCLES);
    }

    #[test]
    fn rejected_launcher_preflight_cannot_wait_for_a_receipt_that_was_never_accepted() {
        let phase = Phase::LauncherRelease {
            generation: 17,
            final_close: true,
        };
        // Driver wait state does not prove native acceptance; absent actual will-close requires immediate failure.
        assert!(!matches_accepted_close(phase, None, None));
        assert!(!matches_accepted_close(
            phase,
            None,
            Some(AcceptedClose::Launcher(16))
        ));
        assert!(!matches_accepted_close(
            phase,
            None,
            Some(AcceptedClose::Launcher(18))
        ));
        assert!(matches_accepted_close(
            phase,
            None,
            Some(AcceptedClose::Launcher(17))
        ));
        // A receipt cannot reauthorize waiting after stop or before close was requested.
        assert!(!matches_accepted_close(
            Phase::Finished,
            None,
            Some(AcceptedClose::Launcher(17))
        ));
        assert!(!matches_accepted_close(
            Phase::LauncherReady,
            None,
            Some(AcceptedClose::Launcher(17))
        ));
    }

    #[test]
    fn plugin_cleanup_wait_requires_exact_accepted_session_target_intent_and_scope() {
        let registration = *crate::composition::plugins::PluginCatalog::new()
            .registrations()
            .first()
            .expect("静态目录至少有当前组装插件");
        let target = PluginCommandTarget {
            registration,
            command_id: registration
                .descriptor()
                .commands
                .first()
                .expect("插件描述符有命令")
                .id,
        };
        let request = PluginCloseRequest {
            intent: PluginCloseIntent::ExitToLauncher,
            scope: PluginCloseScope::ForegroundTransition,
        };
        let phase = Phase::PluginRelease { generation: 29 };
        let accepted = AcceptedClose::Plugin(29, target, request);
        // The expected phase may have changed even though save preflight accepted no Host close.
        assert!(!matches_accepted_close(phase, Some(target), None));
        assert!(matches_accepted_close(phase, Some(target), Some(accepted)));
        assert!(!matches_accepted_close(phase, None, Some(accepted)));
        assert!(!matches_accepted_close(
            Phase::PluginRelease { generation: 30 },
            Some(target),
            Some(accepted)
        ));
        let other_target = PluginCommandTarget {
            command_id: "synthetic_other_command",
            ..target
        };
        assert!(!matches_accepted_close(
            phase,
            Some(other_target),
            Some(accepted)
        ));
        // Terminal-only shells and dismiss intent cannot authorize this probe's normal Back wait or successor success.
        for request in [
            PluginCloseRequest {
                intent: PluginCloseIntent::DismissAndResume,
                ..request
            },
            PluginCloseRequest {
                scope: PluginCloseScope::ApplicationTermination,
                ..request
            },
        ] {
            assert!(!matches_accepted_close(
                phase,
                Some(target),
                Some(AcceptedClose::Plugin(29, target, request))
            ));
        }
        assert!(!matches_accepted_close(
            phase,
            Some(target),
            Some(AcceptedClose::Launcher(29))
        ));
    }
}
