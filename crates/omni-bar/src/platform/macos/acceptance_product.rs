//! Exercise long documents, giant paragraphs, source modes and resource lifetimes through the real Host.
//! The final five-second empty-foreground observation retains only synthetic paths, scalars and weak
//! native evidence. Functional results, saved bytes, SDK receipts and actual release remain separate gates.

mod external_http;
mod readiness;

use std::cell::{Cell, RefCell};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant};

use objc2::rc::{Retained, autoreleasepool};
use objc2::runtime::{AnyObject, Sel};
use objc2::{ClassType, MainThreadMarker, MainThreadOnly, define_class, msg_send};
use objc2_app_kit::{NSApplication, NSEvent, NSEventModifierFlags, NSEventType};
use objc2_foundation::{NSNotification, NSPoint};
use omni_md_plugin::{
    NativeMarkdownHostedFlow, NativeMarkdownHostedImeProbe, NativeMarkdownHostedImeStep,
    NativeMarkdownHostedResourceCase, NativeMarkdownHostedResourceProbe, NativeMarkdownHostedStep,
    NativeMarkdownHostedTableCase, NativeMarkdownHostedTableProbe, NativeMarkdownHostedTableStep,
    NativeMarkdownHostedWorkload,
};
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

/// Each resource scenario acquires and closes a fresh real SDK session, without borrowing a prior document or receipt.
const RESOURCE_CASES: [NativeMarkdownHostedResourceCase; 9] = [
    NativeMarkdownHostedResourceCase::UnsubmittedSearch,
    NativeMarkdownHostedResourceCase::SubmittedSearch,
    NativeMarkdownHostedResourceCase::FindField,
    NativeMarkdownHostedResourceCase::ReplaceFields,
    NativeMarkdownHostedResourceCase::CellDraft,
    NativeMarkdownHostedResourceCase::PendingNoteClose,
    NativeMarkdownHostedResourceCase::BodyConflict,
    NativeMarkdownHostedResourceCase::CellConflict,
    NativeMarkdownHostedResourceCase::SequentialReplaceFields,
];

/// Repeat the long-document lifetime three times, then start fresh sessions for the giant paragraph and source flows.
/// The plugin owns the sole step table; Host neither copies it nor omits the giant paragraph's later editing stages.
const DOCUMENT_FLOWS: [NativeMarkdownHostedFlow; 8] = [
    NativeMarkdownHostedFlow::Full,
    NativeMarkdownHostedFlow::Full,
    NativeMarkdownHostedFlow::Full,
    NativeMarkdownHostedFlow::Full,
    NativeMarkdownHostedFlow::SourceSession,
    NativeMarkdownHostedFlow::SourceFolding,
    NativeMarkdownHostedFlow::SourcePendingMode,
    NativeMarkdownHostedFlow::SourceRelativeLink,
];

/// Reacquire the real plugin session per table case so history, formatting and conflicts cannot share stale authority.
const TABLE_CASES: [NativeMarkdownHostedTableCase; 8] = [
    NativeMarkdownHostedTableCase::Positive,
    NativeMarkdownHostedTableCase::Cancel,
    NativeMarkdownHostedTableCase::FormatUnchanged,
    NativeMarkdownHostedTableCase::CommitResigned,
    NativeMarkdownHostedTableCase::FormatSelection,
    NativeMarkdownHostedTableCase::ToolbarFormatSelection,
    NativeMarkdownHostedTableCase::ToolbarFieldHistory,
    NativeMarkdownHostedTableCase::FindReplacementComposition,
];

/// Standalone and same-process workloads share one driver; mode selects existing steps without changing product interaction.
#[derive(Clone, Copy, Eq, PartialEq)]
enum Workload {
    Full,
    SystemIme,
    /// Preserve all document, table, resource and Chinese-input flows without acquiring a new browser page.
    CoreUi,
    CompleteUi,
}

/// Accept only actions named by their owning plugin workload, never arbitrary externally supplied native operations.
#[derive(Clone, Copy)]
enum WorkloadStep {
    Document(NativeMarkdownHostedStep),
    Table(NativeMarkdownHostedTableStep),
    Ime(NativeMarkdownHostedImeStep),
}

impl WorkloadStep {
    /// Only source flows return synchronously from their final plugin step; Host closes other flows after verified success.
    fn closes_surface(self) -> bool {
        match self {
            // The document workload owns which of its original steps requests the existing return interaction.
            Self::Document(step) => step.closes_surface(),
            // Table and IME steps leave close initiation to the verified Host terminal boundary.
            Self::Table(_) => false,
            Self::Ime(_) => false,
        }
    }
}

/// Observe actual release receipts separately from later weak evidence; elapsed time grants no completion authority.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Phase {
    WaitingLaunch,
    LauncherReady,
    LauncherRelease {
        generation: u64,
        final_close: bool,
    },
    ActivatePlugin,
    ObservePlugin,
    Step(usize),
    ResourceConfigure,
    ResourceInput,
    ResourceClose,
    PluginRelease {
        generation: u64,
        request: PluginCloseRequest,
    },
    VerifyPluginRelease,
    Idle,
    Finished,
}

/// Accept close facts only from the real production owner, never from the driver's expected phase.
#[derive(Clone, Copy)]
enum AcceptedClose {
    Launcher(u64),
    Plugin(u64, PluginCommandTarget, PluginCloseRequest),
}

/// Match the requested close to actual admission; refused, foreign-session or terminal-only receipts cannot authorize waiting.
fn matches_accepted_close(
    phase: Phase,
    expected_target: Option<PluginCommandTarget>,
    accepted: Option<AcceptedClose>,
) -> bool {
    match (phase, accepted) {
        (Phase::LauncherRelease { generation, .. }, Some(AcceptedClose::Launcher(actual))) => {
            // Wait only for the generation already established by this actual launcher close.
            generation == actual
        }
        (
            Phase::PluginRelease {
                generation,
                request: expected,
            },
            Some(AcceptedClose::Plugin(actual, target, request)),
        ) => {
            // Dynamic generation, static target and foreground return scope must identify this exact admitted close.
            generation == actual && expected_target == Some(target) && request == expected
        }
        // Missing admission, a different phase or a stale receipt cannot rewrite current close authority.
        _ => false,
    }
}

/// Bounded event values and the driver epoch reject duplicate, reordered and prior-cycle delivery.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Stage {
    LauncherReady,
    ActivatePlugin,
    ObservePlugin,
    Step(usize),
    ResourceConfigure,
    ResourceInput,
    ResourceClose,
    Return,
    PluginReleased,
    FinishIdle,
}

/// Pending ends this observation only; it proves no success and grants neither close nor next-cycle authority.
#[derive(Clone, Copy, Eq, PartialEq)]
enum StageOutcome {
    Completed,
    Pending,
}

/// Preserve only this request's original scalar terminal authority, excluding old epochs and duplicate completion.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct StepTerminalSignal {
    epoch: u64,
    session_generation: u64,
    index: usize,
}

impl Stage {
    /// Encode fixed diagnostic stages, never arbitrary selectors, plugin identities or file paths.
    fn code(self) -> isize {
        match self {
            // Structural events have distinct fixed codes; editing events use only actual bounded workload indices.
            Self::LauncherReady => 1,
            Self::ActivatePlugin => 2,
            Self::ObservePlugin => 3,
            Self::Step(index) => STEP_FIRST + index as isize,
            Self::Return => 200,
            Self::PluginReleased => 201,
            Self::FinishIdle => 202,
            Self::ResourceConfigure => 300,
            Self::ResourceInput => 301,
            Self::ResourceClose => 302,
        }
    }

    /// Reject unknown codes without a successful default, preserving the caller's actual ordering failure.
    fn decode(code: isize) -> Option<Self> {
        match code {
            1 => Some(Self::LauncherReady),
            2 => Some(Self::ActivatePlugin),
            3 => Some(Self::ObservePlugin),
            200 => Some(Self::Return),
            201 => Some(Self::PluginReleased),
            202 => Some(Self::FinishIdle),
            300 => Some(Self::ResourceConfigure),
            301 => Some(Self::ResourceInput),
            302 => Some(Self::ResourceClose),
            value
                if value >= STEP_FIRST
                    && workload_step((value - STEP_FIRST) as usize).is_some() =>
            {
                // Check the signed range before conversion so negative or unknown indices cannot name a valid step.
                Some(Self::Step((value - STEP_FIRST) as usize))
            }
            // Other ApplicationDefined events have no authority to execute this diagnostic workload.
            _ => None,
        }
    }
}

/// Observation bridges retain native weak identities and file facts; Rc adds no document, window or SDK-session ownership.
struct Driver {
    epoch: u64,
    root: PathBuf,
    origin: Instant,
    phase: Phase,
    pending: Option<Stage>,
    step_terminal: Option<StepTerminalSignal>,
    completed_documents: usize,
    verified_editing_flows: usize,
    /// Granted only by the current workload's successful final step, never by cleanup or a later Host close receipt.
    workload_verified: bool,
    launcher_readiness: Option<Rc<LauncherReadinessWait>>,
    readiness_schedule: ReadinessSchedule,
    launcher_release: Option<Rc<LauncherReleaseProbe>>,
    target: Option<PluginCommandTarget>,
    /// Static targets repeat; capture this Host generation when activation returns so failed cleanup cannot take over a successor.
    session_generation: Option<u64>,
    hosted: Option<Rc<NativeMarkdownHostedWorkload>>,
    /// Observe only the original graph weakly; advance counts after the actual Host receipt and disk/resource checks.
    resource: Option<Rc<NativeMarkdownHostedResourceProbe>>,
    table: Option<Rc<NativeMarkdownHostedTableProbe>>,
    verified_table_cases: usize,
    verified_resource_cases: usize,
    first_failure: Option<String>,
    // Combined workloads retain one driver, PID and timing origin; append IME only after the complete first idle boundary.
    workload: Workload,
    full_idle_verified: bool,
    ime: Option<Rc<NativeMarkdownHostedImeProbe>>,
    ime_verified: bool,
    /// Only CompleteUi's eighth document acquires this listener/browser authority; Full and SystemIme never do.
    external_http: Option<external_http::ExternalHttp>,
    external_http_handoff_verified: bool,
}

impl Driver {
    /// Counts advance only after real native/SDK close verification; full-editing success also requires the original final save.
    fn full_workload_verified(&self) -> bool {
        self.completed_documents == DOCUMENT_FLOWS.len()
            && self.verified_editing_flows == CYCLES + 1
            && self.verified_table_cases == TABLE_CASES.len()
            && self.verified_resource_cases == RESOURCE_CASES.len()
    }

    /// The standalone IME entry stays independent; combined IME steps require the full flow and first five-second idle to pass.
    fn current_ime(&self) -> bool {
        match self.workload {
            // Full alone never acquires an IME sequence; the focused entry starts with that sequence immediately.
            Workload::Full => false,
            Workload::SystemIme => true,
            Workload::CoreUi | Workload::CompleteUi => {
                // Append IME once, only after the original full workload and its idle boundary actually succeeded.
                self.full_idle_verified && self.full_workload_verified() && !self.ime_verified
            }
        }
    }

    /// Combined acceptance first enters the original idle, reopens the real launcher, then idles finally after IME success.
    fn closes_final_launcher(&self) -> bool {
        match self.workload {
            Workload::SystemIme => self.ime_verified,
            Workload::Full | Workload::CoreUi | Workload::CompleteUi => {
                // Every native document/table/resource graph must close before either full-workload idle boundary.
                let cases_closed = self.completed_documents == DOCUMENT_FLOWS.len()
                    && self.verified_table_cases == TABLE_CASES.len()
                    && self.verified_resource_cases == RESOURCE_CASES.len();
                cases_closed
                    && (self.workload == Workload::Full
                        || !self.full_idle_verified
                        || self.ime_verified)
            }
        }
    }

    /// Admit normal close only after the plugin's final step succeeds; special flows already returned from that step.
    fn accept_verified_workload(
        &mut self,
        index: usize,
        closes_surface: bool,
    ) -> Result<(), String> {
        let steps = self.current_step_count().ok_or("原工作负载已结束")?;
        let phase_matches = if closes_surface {
            // A plugin-owned final return may already have established or completed the actual close transaction.
            matches!(
                self.phase,
                Phase::PluginRelease { .. } | Phase::VerifyPluginRelease
            )
        } else {
            // Other final steps still belong to their original active step until Host requests return.
            self.phase == Phase::Step(index)
        };
        if !phase_matches
            || index + 1 != steps
            || self
                .pending
                .is_some_and(|stage| stage != Stage::PluginReleased)
            || self.step_terminal.is_some()
            || self.first_failure.is_some()
            || self.workload_verified
        {
            // A unique final step must have no unresolved signal, prior error or conflicting queued successor.
            return Err("原文稿验证不属于本会话唯一成功终点".to_owned());
        }
        self.workload_verified = true;
        if self.current_flow() == Some(NativeMarkdownHostedFlow::Full) {
            // Full's final step verifies saving after original-text restoration; other flows own their distinct save assertions.
            self.verified_editing_flows += 1;
        }
        Ok(())
    }

    /// Choose the current flow solely from released-document progress, excluding externally selected workloads or indices.
    fn current_flow(&self) -> Option<NativeMarkdownHostedFlow> {
        if self.workload == Workload::SystemIme {
            // The focused input sequence owns no preceding document workload.
            return None;
        }
        DOCUMENT_FLOWS
            .get(self.completed_documents)
            .copied()
            .map(|flow| {
                if self.workload == Workload::CompleteUi
                    && flow == NativeMarkdownHostedFlow::SourceRelativeLink
                {
                    // Only the declared complete-UI variant substitutes the existing HTTP handoff workload.
                    NativeMarkdownHostedFlow::SourceRelativeLinkHttp
                } else {
                    // All other modes keep the plugin's original document step table unchanged.
                    flow
                }
            })
    }

    /// Begin table cases only after all document flows release; native callbacks cannot choose their own case index.
    fn current_table_case(&self) -> Option<NativeMarkdownHostedTableCase> {
        if self.workload == Workload::SystemIme {
            // The standalone IME workload has no table phase and must not reuse a preceding case's identity.
            return None;
        }
        (self.completed_documents == DOCUMENT_FLOWS.len())
            .then(|| TABLE_CASES.get(self.verified_table_cases).copied())
            .flatten()
    }

    /// Read lengths from the plugin's sole step table to validate indices/final actions without copying its functional contract.
    fn current_step_count(&self) -> Option<usize> {
        if self.current_ime() {
            // Input steps come from the original finite IME table, without a Host-owned copy of that workflow.
            return Some(NativeMarkdownHostedImeStep::ALL.len());
        }
        self.current_flow()
            .map(|flow| flow.steps().len())
            .or_else(|| self.current_table_case().map(|case| case.steps().len()))
    }

    /// Select a named action from the current plugin-owned table, never an arbitrary selector or rediscovered document.
    fn current_step(&self, index: usize) -> Option<WorkloadStep> {
        if self.current_ime() {
            // Checked indexing cannot turn a stale or external integer into an input action.
            return NativeMarkdownHostedImeStep::ALL
                .get(index)
                .copied()
                .map(WorkloadStep::Ime);
        }
        if let Some(flow) = self.current_flow() {
            // Active documents retain precedence until their actual release count permits tables to begin.
            return flow.steps().get(index).copied().map(WorkloadStep::Document);
        }
        self.current_table_case()?
            .steps()
            .get(index)
            .copied()
            .map(WorkloadStep::Table)
    }

    /// Reserve one original terminal observation without replacing an unresolved signal or queued verification.
    fn begin_step_terminal(&mut self, index: usize) -> Result<StepTerminalSignal, String> {
        if self.current_step(index).is_none()
            || self.phase != Phase::Step(index)
            || self.pending.is_some()
            || self.step_terminal.is_some()
            || self.first_failure.is_some()
        {
            // A valid step has one current phase and no earlier pending work or failure to overwrite.
            return Err("整产品原步骤终点观察重复、失序或已撤权".to_owned());
        }
        let signal = StepTerminalSignal {
            epoch: self.epoch,
            session_generation: self.session_generation.ok_or("原步骤没有SDK会话代次")?,
            index,
        };
        self.step_terminal = Some(signal);
        Ok(signal)
    }

    /// Consume one original terminal signal; its step must still independently prove the actual functional result.
    fn accept_step_terminal(&mut self, signal: StepTerminalSignal) -> bool {
        if self.step_terminal != Some(signal)
            || signal.epoch != self.epoch
            || Some(signal.session_generation) != self.session_generation
            || self.phase != Phase::Step(signal.index)
            || self.pending.is_some()
            || self.first_failure.is_some()
        {
            // Old or cleanup notifications cannot consume another current step's observation.
            return false;
        }
        self.step_terminal = None;
        true
    }

    /// Consume scalar event authority before any native getter or Rc/Weak destruction outside the state borrow.
    fn accept_event(
        &mut self,
        epoch: isize,
        stage: Option<Stage>,
    ) -> Option<Result<Stage, String>> {
        if epoch != self.epoch as isize || self.phase == Phase::Finished {
            // Old epochs and stopped drivers stay inert, preserving the current queued successor.
            return None;
        }
        Some(match stage {
            Some(stage) if self.pending == Some(stage) => {
                // The exact pending event consumes its reservation once before native work may reenter.
                self.pending = None;
                Ok(stage)
            }
            // A matching epoch still cannot accept an unrequested, duplicated or reordered stage.
            _ => Err("整产品阶段交付失序或重复".to_owned()),
        })
    }
}

/// Release this tool's named pasteboard and injected paths on every return after fixture installation, including failure.
struct FixtureScope;

impl Drop for FixtureScope {
    /// End only this main-thread fixture's authority without retaining a TLS borrow across native cleanup.
    fn drop(&mut self) {
        // The real main-thread entry creates this scope; shared cleanup takes the owner before reentrant release.
        super::acceptance_fixture::clear_fixture();
    }
}

thread_local! {
    /// Normal products omit these slots; stopped acceptance retains neither business resources nor sample history.
    static DRIVER: RefCell<Option<Driver>> = const { RefCell::new(None) };
    static NEXT_EPOCH: Cell<u64> = const { Cell::new(0) };
}

define_class!(
    /// Consume only this diagnostic's non-input events and forward all other input to the original NSApplication.
    // SAFETY: this NSApplication subclass adds no Rust ivars; the first main-thread sharedApplication
    // acquisition constructs the process's sole application object.
    #[unsafe(super = NSApplication)]
    #[thread_kind = MainThreadOnly]
    struct ProductAcceptanceApplication;

    impl ProductAcceptanceApplication {
        #[unsafe(method(sendEvent:))]
        /// Observe the original dispatch return without keeping driver or foreground borrows on the native stack.
        fn send_event(&self, event: &NSEvent) {
            if route_event(event) {
                // Only the diagnostic subtype is consumed; keyboard, mouse and menu input retains production routing.
                return;
            }
            // SAFETY: the original event lives through this synchronous main-thread stack and the superclass
            // selector preserves NSApplication's public ABI.
            unsafe { let _: () = msg_send![super(self), sendEvent: event]; }
            if event.r#type() == NSEventType::LeftMouseDown {
                // Copy the current weak table bridge only after original mouse tracking returns.
                // No DRIVER or ACTIVE borrow spans super, so native reentry cannot inherit a stale graph lease.
                let probe = DRIVER.with_borrow(|slot| {
                    slot.as_ref()?.table.as_ref().map(Rc::clone)
                });
                if let Some(probe) = probe
                    && let Err(error) = probe.did_dispatch_hosted_event(event)
                {
                    // Markdown matches the original Down event number/window and releases its bounded event lease.
                    // Mouse return alone cannot prove the later selector delivered or queued business work.
                    fail(error);
                }
            }
        }

        #[unsafe(method(sendAction:to:from:))]
        /// Observe the actual target/action return in the acceptance app without replacing the product selector or target.
        fn send_action(&self, action: Sel, target: Option<&AnyObject>, sender: Option<&AnyObject>) -> bool {
            // SAFETY: AppKit supplies the original selector/target/sender with the public NSApplication ABI.
            // Invoke super once and preserve its Boolean result, without DRIVER/ACTIVE borrows across dispatch.
            let sent: bool = unsafe { msg_send![super(self), sendAction: action, to: target, from: sender] };
            let probe = DRIVER.with_borrow(|slot| {
                slot.as_ref()?.table.as_ref().map(Rc::clone)
            });
            if let Some(probe) = probe
                && let Err(error) = probe.did_dispatch_hosted_action(action, target, sender, sent)
            {
                // The original selector's return proves delivery only; Markdown still awaits its Down and CPU endpoints.
                // Failure uses normal driver cleanup, never action replay, a longer deadline or treating sent as completion.
                fail(error);
            }
            sent
        }
    }
);

/// Timestamp step observations monotonically without claiming presented frames, physical-input latency or memory success.
fn emit(event: &str) {
    let metadata = DRIVER.with_borrow(|slot| {
        // Copy scalars and end business-state borrowing before stdout I/O can reenter another boundary.
        slot.as_ref().map(|driver| {
            (
                driver.completed_documents,
                driver.origin.elapsed().as_nanos(),
            )
        })
    });
    if let Some((cycles, elapsed)) = metadata {
        // Output failure leaves close behavior unchanged; the external parent must reject missing protocol evidence.
        let _ = writeln!(
            std::io::stdout().lock(),
            "product_acceptance event={event} documents={cycles} mono_ns={elapsed}"
        );
    }
}

/// Queue one successor for the existing epoch; timers and retries cannot infer functional or resource progress.
fn enqueue(stage: Stage) -> Result<(), String> {
    let epoch = DRIVER.with_borrow_mut(|slot| {
        let driver = slot.as_mut().ok_or("整产品 driver 已结束")?;
        if driver.phase == Phase::Finished || driver.pending.is_some() {
            // Preserve the queued stage instead of allowing duplicate callbacks to replace its successor.
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

/// Execute stages only in their real event callback's autorelease pool; subtype alone cannot authorize a foreign epoch.
fn route_event(event: &NSEvent) -> bool {
    if event.r#type() != NSEventType::ApplicationDefined || event.subtype().0 != EVENT_SUBTYPE {
        // Driver presence never authorizes swallowing unrelated product or user input.
        return false;
    }
    let epoch = event.data1();
    let event_code = event.data2();
    let stage = Stage::decode(event_code);
    let claim = DRIVER.with_borrow_mut(|slot| {
        // Consume already-copied event scalars under the state borrow without invoking native getters.
        slot.as_mut()?.accept_event(epoch, stage)
    });
    if let Some(claim) = claim {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            // Drain this event's temporary native claims before observing a later actual release receipt.
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
            // Convert Rust panic into failure at the ABI boundary instead of unwinding through Objective-C frames.
            fail(error);
        }
    }
    true
}

/// Lend presentation claims for this synchronous stack only, ending the Active-owner borrow before native getters.
fn active_launcher() -> Result<LauncherPresentation, String> {
    super::ACTIVE_SURFACE
        .with_borrow(|surface| surface.as_ref().map(|surface| surface.presentation()))
        .ok_or_else(|| "真实 Launcher owner 不存在".to_owned())
}

/// Require every native business owner and transition to end; hidden windows or one empty session slot do not prove release.
fn foreground_is_empty() -> bool {
    !super::ACTIVE_SURFACE.with_borrow(Option::is_some)
        && !super::CLOSING_SURFACE.with_borrow(Option::is_some)
        && !super::plugin_host::exists()
        && !super::builtins::is_open()
        && super::FOREGROUND_STATE.with_borrow(|state| {
            // Inspect active identity, transition and destruction barrier together; an unconsumed barrier is unfinished ownership.
            state.active_surface().is_none()
                && state.pending_transition().is_none()
                && state.destroyed_barrier().is_none()
        })
}

/// Observe the production launcher after launch/return without a substitute window or guessed input readiness.
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
        // Consume only a real notification received during this getter stack; Pending without a new signal cannot self-retry.
        let epoch = launcher_readiness_wait()
            .ok_or("Launcher 原始等待已撤销")?
            .epoch;
        queue_launcher_readiness_check(epoch);
    }
    observed
}

/// Observe only the original launcher graph; event scheduling preserves every production readiness condition.
fn observe_launcher_readiness() -> Result<StageOutcome, String> {
    let mtm = MainThreadMarker::new().ok_or("Launcher 验证需要主线程")?;
    let wait = launcher_readiness_wait().ok_or("Launcher 原始呈现弱身份缺失")?;
    if !wait.matches_active_owner() {
        // A lost graph or changed close generation revokes this observation; a successor cannot restart its predecessor's check.
        return Err("等待就绪期间原 Launcher 身份或关闭代际已变化".to_owned());
    }
    let launcher = active_launcher()?;
    let ready = launcher.input_readiness(mtm);
    // Return getter-generated temporary graph claims before a later close and independent release verification.
    drop(launcher);
    let state_matches = super::FOREGROUND_STATE.with_borrow(|state| {
        state.active_surface() == Some(ForegroundSurface::Launcher)
            && state.pending_transition().is_none()
            && state.destroyed_barrier().is_none()
    });
    if !state_matches || super::plugin_host::exists() || !wait.matches_active_owner() {
        // Visible input must belong to the original current owner with no transition; stale facts grant no activation authority.
        return Err("Launcher 可见状态与 Host 所有权不一致".to_owned());
    }
    if !ready.is_ready() {
        // show returning proves neither key status nor focus; keep weak identity and await a real later readiness notification.
        emit(&format!("launcher_readiness_pending facts={ready:?}"));
        return Ok(StageOutcome::Pending);
    }
    Ok(StageOutcome::Completed)
}

/// Capture original weak graph evidence and close generation before normal launcher close, never from a successor afterward.
fn close_launcher(final_close: bool) -> Result<(), String> {
    let wait = launcher_readiness_wait().ok_or("Launcher 关闭缺少原始呈现权限")?;
    if !wait.matches_active_owner() {
        // Failure cleanup may close only this original graph; an owner acquired while waiting is outside its authority.
        return Err("原 Launcher 关闭权限已失效".to_owned());
    }
    let release = active_launcher()?.release_probe()?;
    let generation = super::launcher_close::probe_generation()
        .checked_add(1)
        .ok_or("Launcher 关闭代际耗尽")?;
    if !wait.matches_active_owner() {
        // Snapshot getters may reenter; recheck original weak identity rather than substituting a successor's close proof.
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
    // Drop original weak wait metadata outside TLS borrowing so it cannot survive into this close transaction.
    drop(old_wait);
    super::try_request_launcher_close()
}

/// Borrow the original verification bridge without adding native-graph or SDK-session ownership.
fn hosted() -> Result<Rc<NativeMarkdownHostedWorkload>, String> {
    DRIVER
        .with_borrow(|slot| {
            slot.as_ref()
                .and_then(|driver| driver.hosted.as_ref().map(Rc::clone))
        })
        .ok_or_else(|| "整产品 Markdown 原始观察桥缺失".to_owned())
}

/// Queue verification only after the original request reaches its endpoint, including a later event for synchronous completion.
fn schedule_hosted_step(index: usize) -> Result<(), String> {
    let step = workload_step(index).ok_or("终点观察步骤越界")?;
    let signal = DRIVER.with_borrow_mut(|slot| {
        slot.as_mut()
            .ok_or("原步骤终点 driver 缺失")?
            .begin_step_terminal(index)
    })?;
    // Preserve the workload's original three-second failure deadline and signal scalars, without retaining live graphs.
    let duration = match step {
        WorkloadStep::Ime(step) => step.deadline(),
        _ => Duration::from_secs(3),
    };
    let deadline =
        dispatch2::DispatchTime::try_from(duration).map_err(|_| "原步骤失败期限无效".to_owned())?;
    dispatch2::DispatchQueue::main()
        .after(deadline, move || {
            let current = DRIVER.with_borrow(|slot| {
                slot.as_ref().is_some_and(|driver| {
                    driver.step_terminal == Some(signal)
                        && driver.session_generation == Some(signal.session_generation)
                        && driver.phase == Phase::Step(signal.index)
                        && driver.first_failure.is_none()
                })
            });
            if !current {
                // Completion, cancellation, close or replacement revokes this old timeout's access to a successor graph.
                return;
            }
            if matches!(
                step,
                WorkloadStep::Document(NativeMarkdownHostedStep::VerifyRelativeLinkHttpUnchanged)
            ) {
                // HTTP awaits the exclusive server's real GET and completed response. The old source body
                // already released and cannot supply diagnostics for this independent network endpoint.
                DRIVER.with_borrow(|slot| {
                    if let Some(server) = slot
                        .as_ref()
                        .and_then(|driver| driver.external_http.as_ref())
                    {
                        server.diagnose_deadline();
                    }
                });
            } else if let WorkloadStep::Document(step) = step
                && let Ok(bridge) = hosted()
            {
                // Read bounded original-ticket diagnostics without advancing execution or granting another Ready ticket.
                bridge.diagnose_deadline(step);
            }
            fail("原工作负载未在所属步骤期限内交付真实终点".to_owned());
        })
        .map_err(|_| "原步骤失败通知无法调度".to_owned())?;
    let terminal = match step {
        WorkloadStep::Document(NativeMarkdownHostedStep::VerifyRelativeLinkHttpUnchanged) => {
            // The sole server retains an early real network endpoint until this original driver signal registers.
            DRIVER.with_borrow(|slot| {
                slot.as_ref()
                    .and_then(|driver| driver.external_http.as_ref())
                    .ok_or("HTTP终点缺少原服务器")?
                    .observe(signal)
            })?
        }
        WorkloadStep::Document(step) => {
            // Borrow the original document observer and deliver only this request's scalar terminal signal.
            hosted()?.observe_step_terminal(step, move || hosted_step_terminal(signal))?
        }
        WorkloadStep::Table(step) => {
            // Table endpoints remain owned by their actual native table workload.
            table()?.observe_step_terminal(step, move || hosted_step_terminal(signal))?
        }
        WorkloadStep::Ime(step) => {
            // IME completion comes from the actual existing input workload without switching input sources.
            ime()?.observe_step_terminal(step, move || hosted_step_terminal(signal))?
        }
    };
    if terminal {
        // Already-completed or synchronous endpoints consume the same authority without a second synthetic notification.
        hosted_step_terminal(signal);
    } else {
        // Pending is observation only; the original endpoint, never this log, schedules later verification.
        emit(&format!(
            "step_terminal_pending stage={} receipt=original",
            Stage::Step(index).code()
        ));
    }
    Ok(())
}

/// Markdown ends its observation borrow before delivery; Host accepts scalars without retaining native objects or body text.
fn hosted_step_terminal(signal: StepTerminalSignal) {
    let accepted = DRIVER.with_borrow_mut(|slot| {
        slot.as_mut()
            .is_some_and(|driver| driver.accept_step_terminal(signal))
    });
    if accepted && let Err(error) = enqueue(Stage::Step(signal.index)) {
        // Event creation failure stays explicit rather than leaving completed work waiting forever for missing verification.
        fail(error);
    }
}

/// Select the current document's named flow without granting terminal notifications a choice of next session.
fn hosted_flow() -> Option<NativeMarkdownHostedFlow> {
    DRIVER.with_borrow(|slot| slot.as_ref()?.current_flow())
}

/// Resolve actions only from the original current workload; foreign indices or error codes grant no successor input authority.
fn workload_step(index: usize) -> Option<WorkloadStep> {
    DRIVER.with_borrow(|slot| slot.as_ref()?.current_step(index))
}

/// Choose the next table case from completed releases so old callbacks cannot revive a previous scenario.
fn table_case() -> Option<NativeMarkdownHostedTableCase> {
    DRIVER.with_borrow(|slot| slot.as_ref()?.current_table_case())
}

/// Borrow only this real table session's weak observation bridge without retaining SDK/native business ownership.
fn table() -> Result<Rc<NativeMarkdownHostedTableProbe>, String> {
    DRIVER
        .with_borrow(|slot| slot.as_ref()?.table.as_ref().map(Rc::clone))
        .ok_or_else(|| "原表格场景观察桥缺失".to_owned())
}

/// Borrow the current production IME bridge without owning the input method, document or candidate window in Host.
fn ime() -> Result<Rc<NativeMarkdownHostedImeProbe>, String> {
    DRIVER
        .with_borrow(|slot| slot.as_ref()?.ime.as_ref().map(Rc::clone))
        .ok_or_else(|| "原中文输入法观察桥缺失".to_owned())
}

/// Observe only the step category granted by the original driver; full mode cannot start IME before its first idle completes.
fn ime_workload() -> bool {
    DRIVER.with_borrow(|slot| slot.as_ref().is_some_and(Driver::current_ime))
}

/// Choose resource cases only after all long-document/source flows finish; absence of a current case proves no global success.
fn resource_case() -> Option<NativeMarkdownHostedResourceCase> {
    DRIVER.with_borrow(|slot| {
        let driver = slot.as_ref()?;
        if driver.workload == Workload::SystemIme {
            // The focused input workload deliberately owns no document/table/resource sequence.
            return None;
        }
        (driver.completed_documents == DOCUMENT_FLOWS.len()
            && driver.verified_table_cases == TABLE_CASES.len())
        .then(|| RESOURCE_CASES.get(driver.verified_resource_cases).copied())
        .flatten()
    })
}

/// Lend the original resource bridge without storing a session, native control or document in Host verification state.
fn resource() -> Result<Rc<NativeMarkdownHostedResourceProbe>, String> {
    DRIVER
        .with_borrow(|slot| slot.as_ref()?.resource.as_ref().map(Rc::clone))
        .ok_or_else(|| "原资源场景观察桥缺失".to_owned())
}

/// Reset only this tool's finite fixture after the preceding real foreground releases, never while documents remain mapped.
fn prepare_resource_fixture() -> Result<(), String> {
    if !foreground_is_empty() {
        // Fixture replacement cannot overlap an existing session's mappings, snapshots or input authority.
        return Err("资源样本准备前仍有真实前台 owner".to_owned());
    }
    let workspace = DRIVER.with_borrow(|slot| {
        let driver = slot.as_ref().ok_or("资源样本 driver 缺失")?;
        Ok::<_, String>(driver.root.join("workspace"))
    })?;
    reset_owned_workspace(&workspace)?;
    std::fs::write(
        workspace.join("A.md"),
        omni_md_plugin::native_markdown_runtime_probe_fixture(),
    )
    .map_err(|_| "无法创建原资源场景样本".to_owned())?;
    std::fs::write(
        workspace.join("B.md"),
        b"# Second\n\nnew independent note\n",
    )
    .map_err(|_| "无法创建原笔记交接样本".to_owned())?;
    Ok(())
}

/// Recreate only this entry's exclusive workspace while real foreground ownership is empty, including prior attachment fixtures.
fn reset_owned_workspace(workspace: &Path) -> Result<(), String> {
    if !foreground_is_empty() {
        // A merely hidden or closing graph still owns data; reject mutation instead of guessing release from visibility.
        return Err("原前台尚未释放，不能重置合成笔记库".to_owned());
    }
    std::fs::remove_dir_all(workspace).map_err(|_| "无法归还原合成笔记库".to_owned())?;
    std::fs::create_dir(workspace).map_err(|_| "无法创建后继合成笔记库".to_owned())
}

/// Stream fixtures to disk in chunks no larger than eight KiB; Host never constructs a complete giant-paragraph String.
fn prepare_document_fixture() -> Result<(), String> {
    let (workspace, giant, http) = DRIVER.with_borrow(|slot| {
        let driver = slot.as_ref().ok_or("原长文driver缺失")?;
        Ok::<_, String>((
            driver.root.join("workspace"),
            driver.completed_documents == CYCLES,
            driver.current_flow() == Some(NativeMarkdownHostedFlow::SourceRelativeLinkHttp),
        ))
    })?;
    reset_owned_workspace(&workspace)?;
    let path = workspace.join("Acceptance.md");
    if giant {
        // The plugin streams the declared giant paragraph directly into this freshly owned fixture file.
        omni_md_plugin::write_native_markdown_giant_paragraph_fixture(&path)
    } else {
        // Ordinary long documents use the same bounded streaming fixture writer as their production probe.
        omni_md_plugin::write_native_markdown_memory_fixture(&path)
    }
    .map_err(|_| "流式合成文稿生成失败".to_owned())?;
    if http {
        // Acquire external handoff authority only for its declared flow, after exclusive fixture reset.
        let mut server = Some(external_http::ExternalHttp::new()?);
        DRIVER.with_borrow_mut(|slot| {
            let driver = slot.as_mut().ok_or("HTTP夹具原driver已结束")?;
            if driver.external_http.is_some() || driver.external_http_handoff_verified {
                // Neither an existing listener nor completed handoff can be replaced by another fixture operation.
                return Err("HTTP夹具不得重复创建或接管已完成交接".to_owned());
            }
            driver.external_http = server.take();
            Ok(())
        })?;
        // An untransferred server stays in the local Option and cancels/joins only after TLS borrowing ends.
    }
    Ok(())
}

/// The plugin case owns its fixture text; Host writes required files only into its released exclusive directory.
fn prepare_table_fixture(case: NativeMarkdownHostedTableCase) -> Result<(), String> {
    let workspace = DRIVER
        .with_borrow(|slot| slot.as_ref().map(|driver| driver.root.join("workspace")))
        .ok_or("原表格样本driver已结束")?;
    reset_owned_workspace(&workspace)?;
    std::fs::write(
        workspace.join("Acceptance.md"),
        case.fixture_source().as_bytes(),
    )
    .map_err(|_| "无法创建原表格合成样本".to_owned())
}

/// Bind the actual session generation and exact close intent before action, limiting synchronous completion to this observation.
fn begin_plugin_close_observation(intent: PluginCloseIntent) -> Result<(), String> {
    let generation =
        super::plugin_host::product_probe_generation().ok_or("真实插件 session 代际缺失")?;
    let request = PluginCloseRequest {
        intent,
        scope: PluginCloseScope::ForegroundTransition,
    };
    DRIVER.with_borrow_mut(|slot| {
        let driver = slot.as_mut().ok_or("插件关闭 driver 缺失")?;
        if driver.session_generation != Some(generation) {
            // Reused static identity does not permit cleanup to close a later activation of the same plugin.
            return Err("原插件激活代次已经改变，不能关闭后继会话".to_owned());
        }
        driver.phase = Phase::PluginRelease {
            generation,
            request,
        };
        Ok(())
    })
}

/// Show or continue observing the original launcher after actual Host release, sharing presentation-notification deduplication.
fn resume_launcher_after_release() -> Result<(), String> {
    let epoch = DRIVER
        .with_borrow(|slot| {
            slot.as_ref()
                .filter(|driver| driver.phase == Phase::LauncherReady)
                .map(|driver| driver.epoch)
        })
        .ok_or("释放后的 Launcher 观察权限已结束")?;
    if super::ACTIVE_SURFACE.with_borrow(Option::is_none) {
        // Reconstruct only when no launcher adapter exists; callbacks observe the published production owner.
        let mtm = MainThreadMarker::new().ok_or("资源完成需要原主线程")?;
        super::show_launcher_surface(mtm);
    }
    queue_launcher_readiness_check(epoch);
    Ok(())
}

/// Bind this real session generation before invoking its existing plugin-return action, without substituting process quit.
fn request_return() -> Result<(), String> {
    let editing_verified = DRIVER.with_borrow(|slot| {
        slot.as_ref()
            .is_some_and(|driver| driver.workload_verified && driver.first_failure.is_none())
    });
    if !editing_verified {
        // Only restored original text and verified real save authorize successful return; cleanup keeps its separate failing result.
        return Err("本轮编辑和正式保存尚未完成验证，不能记录成功返回".to_owned());
    }
    begin_plugin_close_observation(PluginCloseIntent::ExitToLauncher)?;
    emit("plugin_return_requested");
    if table_case().is_some() {
        // Table return follows the original table surface action and its existing cell/history preflight.
        table()?.request_return(false)
    } else if ime_workload() {
        // IME return uses the real input workload's own save/composition contract.
        ime()?.request_return()
    } else {
        // Document flows share the original document return path and cannot borrow a table or input receipt.
        hosted()?.request_return()
    }
}

/// Execute one action per original event; only actual production release receipts advance teardown after action returns.
fn operate(stage: Stage) -> Result<StageOutcome, String> {
    let phase = DRIVER
        .with_borrow(|slot| slot.as_ref().map(|driver| driver.phase))
        .ok_or("driver 缺失")?;
    if matches!(
        phase,
        Phase::ObservePlugin
            | Phase::Step(_)
            | Phase::ResourceConfigure
            | Phase::ResourceInput
            | Phase::ResourceClose
    ) {
        let expected = DRIVER
            .with_borrow(|slot| {
                let driver = slot.as_ref()?;
                Some((driver.session_generation?, driver.target?))
            })
            .ok_or("原插件激活权限已经结束")?;
        let current = super::FOREGROUND_STATE.with_borrow(|state| {
            state.active_surface() == Some(ForegroundSurface::Plugin(expected.1))
                && state.pending_transition().is_none()
        });
        if super::plugin_host::product_probe_generation() != Some(expected.0) || !current {
            // Close/reopen can occur while this event queues; equal static commands or paths cannot authorize successor input.
            return Err("原插件会话在验收事件交付前已被关闭或替换".to_owned());
        }
    }
    emit(&format!("stage_begin stage={}", stage.code()));
    match (stage, phase) {
        (Stage::LauncherReady, Phase::LauncherReady) => {
            if verify_launcher_ready()? == StageOutcome::Pending {
                // Keep the original graph while pending; only its new real notification can schedule another check or grant success.
                return Ok(StageOutcome::Pending);
            }
            emit("launcher_ready visible=true input_ready=true sdk_session_owned=false");
            let final_close = DRIVER.with_borrow(|slot| {
                slot.as_ref().is_some_and(|driver| {
                    driver.closes_final_launcher() || driver.first_failure.is_some()
                })
            });
            // Reverify the real launcher after every return before starting another workload or ending the final foreground.
            close_launcher(final_close)?;
        }
        (Stage::ActivatePlugin, Phase::ActivatePlugin) => {
            if !foreground_is_empty() {
                // Launcher close must satisfy actual resource release before any plugin can acquire a new graph.
                return Err("原 Launcher 未完全释放，禁止插件激活".to_owned());
            }
            if ime_workload() {
                // Recreate only the exclusive synthetic input fixture after the previous foreground has fully released.
                let workspace = DRIVER
                    .with_borrow(|slot| slot.as_ref().map(|driver| driver.root.join("workspace")))
                    .ok_or("原中文验收目录缺失")?;
                reset_owned_workspace(&workspace)?;
                omni_md_plugin::write_native_markdown_ime_fixture(
                    &workspace.join("Acceptance.md"),
                )?;
            } else if resource_case().is_some() {
                // Resource scenarios own their declared A/B documents and conflict/cell fixtures.
                prepare_resource_fixture()?;
            } else if let Some(case) = table_case() {
                // Each table case receives its plugin-owned exact fixture text in a fresh workspace.
                prepare_table_fixture(case)?;
            } else if hosted_flow().is_some() {
                // Long-document/source flows keep their bounded streaming initializer and optional handoff authority.
                prepare_document_fixture()?;
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
            // Activate the real static catalog command without a plugin-ID-specific or page-specific alternate route.
            super::plugin_host::activate(registration, command.id)
                .map_err(|error| error.to_string())?;
            let target = super::FOREGROUND_STATE
                .with_borrow(|state| {
                    if let Some(ForegroundSurface::Plugin(target)) = state.active_surface() {
                        // Copy the target actually published by production activation, independently of the requested descriptor.
                        Some(target)
                    } else {
                        // A completed inline action or another foreground kind cannot prove Markdown mounted.
                        None
                    }
                })
                .ok_or("目录激活没有建立真实插件前台")?;
            let generation = super::plugin_host::product_probe_generation()
                .ok_or("目录激活没有建立原 SDK 会话代次")?;
            DRIVER.with_borrow_mut(|slot| {
                if let Some(driver) = slot.as_mut() {
                    driver.target = Some(target);
                    driver.session_generation = Some(generation);
                    driver.phase = Phase::ObservePlugin;
                }
            });
            emit("cycle_started activation=static_catalog session=production");
            enqueue(Stage::ObservePlugin)?;
        }
        (Stage::ObservePlugin, Phase::ObservePlugin) => {
            if ime_workload() {
                // Observe the actual current input client only after production activation returns in an earlier event.
                let path = DRIVER
                    .with_borrow(|slot| {
                        slot.as_ref()
                            .map(|driver| driver.root.join("workspace/Acceptance.md"))
                    })
                    .ok_or("原中文合成路径缺失")?;
                let probe = NativeMarkdownHostedImeProbe::observe_active_document(&path)?;
                DRIVER.with_borrow_mut(|slot| {
                    if let Some(driver) = slot.as_mut() {
                        driver.ime = Some(Rc::new(probe));
                        driver.phase = Phase::Step(0);
                    }
                });
                schedule_hosted_step(0)?;
                return Ok(StageOutcome::Completed);
            }
            if let Some(case) = resource_case() {
                // Bind this declared resource case to the actual A.md document, never a reconstructed session.
                let path = DRIVER
                    .with_borrow(|slot| {
                        slot.as_ref()
                            .map(|driver| driver.root.join("workspace/A.md"))
                    })
                    .ok_or("原资源样本路径权限缺失")?;
                let probe =
                    NativeMarkdownHostedResourceProbe::observe_active_document(&path, case)?;
                DRIVER.with_borrow_mut(|slot| {
                    if let Some(driver) = slot.as_mut() {
                        driver.resource = Some(Rc::new(probe));
                        driver.phase = Phase::ResourceConfigure;
                    }
                });
                enqueue(Stage::ResourceConfigure)?;
                return Ok(StageOutcome::Completed);
            }
            if let Some(case) = table_case() {
                // The current native table owns its history and cells; retain only its original weak probe bridge.
                let path = DRIVER
                    .with_borrow(|slot| {
                        slot.as_ref()
                            .map(|driver| driver.root.join("workspace/Acceptance.md"))
                    })
                    .ok_or("原表格样本权限已结束")?;
                let probe = NativeMarkdownHostedTableProbe::observe_active_document(&path, case)?;
                DRIVER.with_borrow_mut(|slot| {
                    if let Some(driver) = slot.as_mut() {
                        driver.table = Some(Rc::new(probe));
                        driver.phase = Phase::Step(0);
                    }
                });
                schedule_hosted_step(0)?;
                return Ok(StageOutcome::Completed);
            }
            let path = DRIVER
                .with_borrow(|slot| {
                    slot.as_ref()
                        .map(|driver| driver.root.join("workspace/Acceptance.md"))
                })
                .ok_or("合成文档路径权限缺失")?;
            let flow = hosted_flow().ok_or("原长文流程缺失")?;
            let bridge = if flow == NativeMarkdownHostedFlow::SourceRelativeLinkHttp {
                // Supply only this flow's exclusive nonce URL to the existing production link workload.
                let url = DRIVER
                    .with_borrow(|slot| {
                        slot.as_ref()
                            .and_then(|driver| driver.external_http.as_ref())
                            .map(|server| server.url().to_owned())
                    })
                    .ok_or("HTTP流程没有原监听器URL")?;
                NativeMarkdownHostedWorkload::observe_active_document_with_http_link(&path, &url)?
            } else {
                // Other flows need no network resource and use their original document observation entry.
                NativeMarkdownHostedWorkload::observe_active_document(&path, flow)?
            };
            DRIVER.with_borrow_mut(|slot| {
                if let Some(driver) = slot.as_mut() {
                    driver.hosted = Some(Rc::new(bridge));
                    driver.phase = Phase::Step(0);
                }
            });
            // Activation and observation occur in separate real events, borrowing the published session without a replacement graph.
            schedule_hosted_step(0)?;
        }
        (Stage::Step(index), Phase::Step(expected)) if index == expected => {
            let step = workload_step(index).ok_or("工作负载步骤越界")?;
            let steps = DRIVER
                .with_borrow(|slot| slot.as_ref()?.current_step_count())
                .ok_or("原工作负载已结束")?;
            let closes_surface = step.closes_surface();
            if matches!(
                step,
                WorkloadStep::Document(NativeMarkdownHostedStep::RelativeLinkHttpMouseUp)
            ) {
                // Consume the sole network-action reservation before the original body delivers Command-click; never retry navigation.
                DRIVER.with_borrow(|slot| {
                    slot.as_ref()
                        .and_then(|driver| driver.external_http.as_ref())
                        .ok_or("HTTP点击缺少原服务器")?
                        .arm()
                })?;
            }
            if closes_surface {
                // Special flows return from their original final-step stack; bind actual Host close authority before invoking it.
                begin_plugin_close_observation(PluginCloseIntent::ExitToLauncher)?;
            }
            match step {
                // Each bridge invokes only its owning plugin workload, preserving the original action and input routes.
                WorkloadStep::Document(step) => hosted()?.step(step)?,
                WorkloadStep::Table(step) => table()?.step(step)?,
                WorkloadStep::Ime(step) => ime()?.step(step)?,
            }
            if matches!(
                step,
                WorkloadStep::Document(NativeMarkdownHostedStep::VerifyRelativeLinkHttpUnchanged)
            ) {
                // Markdown checks original input/source/disk/history first; Host separately checks the real HTTP terminal state.
                let mut server = DRIVER
                    .with_borrow_mut(|slot| {
                        slot.as_mut().and_then(|driver| driver.external_http.take())
                    })
                    .ok_or("HTTP核验缺少原服务器owner")?;
                let verified = server.verify();
                let released = server.shutdown();
                match (verified, released) {
                    (Err(primary), Err(cleanup)) => {
                        // Preserve both the functional cause and independent listener/worker cleanup failure.
                        return Err(format!("{primary}；HTTP释放失败：{cleanup}"));
                    }
                    // Cleanup success cannot excuse a failed handoff, nor can successful HTTP excuse leaked ownership.
                    (Err(primary), Ok(())) => return Err(primary),
                    (Ok(()), Err(cleanup)) => return Err(cleanup),
                    // Publish success only after both independent boundaries actually completed.
                    (Ok(()), Ok(())) => {}
                }
                DRIVER.with_borrow_mut(|slot| {
                    slot.as_mut()
                        .ok_or("HTTP完成后原driver缺失")?
                        .external_http_handoff_verified = true;
                    Ok::<_, String>(())
                })?;
                emit(
                    "external_http_handoff_verified actual_nonce_get=true response_status=204 response_write_complete=true listener_closed=true worker_joined=true https_verified=false browser_tab_cleanup=unverified browser_history_cleanup=unverified",
                );
            }
            if index + 1 == steps {
                DRIVER.with_borrow_mut(|slot| {
                    slot.as_mut()
                        .ok_or("原文稿验证driver缺失")?
                        .accept_verified_workload(index, closes_surface)
                })?;
                emit("document_flow_verified full_steps=true original_data_checks=true");
                if !closes_surface {
                    // Full returns after actual restoration/save; lifecycle receipts cannot stand in for editing success.
                    enqueue(Stage::Return)?;
                }
            } else {
                // Advance only after this exact step's original action succeeds, then observe the next real endpoint.
                DRIVER.with_borrow_mut(|slot| {
                    if let Some(driver) = slot.as_mut() {
                        driver.phase = Phase::Step(index + 1);
                    }
                });
                schedule_hosted_step(index + 1)?;
            }
        }
        (Stage::Return, Phase::Step(index))
            if DRIVER.with_borrow(|slot| slot.as_ref().and_then(Driver::current_step_count))
                == Some(index + 1) =>
        {
            // The final verified step is the only event allowed to request the existing return interaction.
            request_return()?
        }
        (Stage::ResourceConfigure, Phase::ResourceConfigure) => {
            // configure waits for actual CPU/active-cell state only after returning the surface borrow, without a test Host port.
            resource()?.configure()?;
            DRIVER.with_borrow_mut(|slot| {
                if let Some(driver) = slot.as_mut() {
                    driver.phase = Phase::ResourceInput;
                }
            });
            enqueue(Stage::ResourceInput)?;
        }
        (Stage::ResourceInput, Phase::ResourceInput) => {
            let case = resource_case().ok_or("当前资源场景已失权")?;
            if case == NativeMarkdownHostedResourceCase::PendingNoteClose {
                // This scenario closes within the real sidebar handoff transaction; bind exact receipt identity before that input.
                begin_plugin_close_observation(PluginCloseIntent::DismissAndResume)?;
            }
            let probe = resource()?;
            probe.input()?;
            if probe.input_started_close() {
                if case != NativeMarkdownHostedResourceCase::PendingNoteClose {
                    // Only the declared pending-note case may synchronously close as an input side effect.
                    return Err("资源输入没有本次精确关闭观察权限".to_owned());
                }
            } else if probe.more_input_events() {
                // The original resource workload owns its finite continuation count and exact next input action.
                enqueue(Stage::ResourceInput)?;
            } else {
                // No pending input remains, so close only in a later event after native input callbacks return.
                DRIVER.with_borrow_mut(|slot| {
                    if let Some(driver) = slot.as_mut() {
                        driver.phase = Phase::ResourceClose;
                    }
                });
                enqueue(Stage::ResourceClose)?;
            }
        }
        (Stage::ResourceClose, Phase::ResourceClose) => {
            let case = resource_case().ok_or("资源关闭场景已失权")?;
            let intent = if case == NativeMarkdownHostedResourceCase::UnsubmittedSearch {
                // This original case explicitly returns to Launcher while discarding no committed document input.
                PluginCloseIntent::ExitToLauncher
            } else {
                // Other resource cases retain their original dismissal/resume identity and close preflight.
                PluginCloseIntent::DismissAndResume
            };
            begin_plugin_close_observation(intent)?;
            resource()?.request_close()?;
        }
        (Stage::PluginReleased, Phase::VerifyPluginRelease) => {
            let cleanup_without_bridge = DRIVER.with_borrow(|slot| {
                slot.as_ref().is_some_and(|driver| {
                    driver.first_failure.is_some()
                        && driver.hosted.is_none()
                        && driver.resource.is_none()
                        && driver.table.is_none()
                        && driver.ime.is_none()
                })
            });
            if cleanup_without_bridge {
                if super::plugin_host::exists()
                    || super::plugin_host::product_probe_generation().is_some()
                {
                    // A real Host receipt still requires both session ownership and generation authority to be absent.
                    stop_loop();
                    return Err("无观察桥清理没有归还原 SDK 会话".to_owned());
                }
                DRIVER.with_borrow_mut(|slot| {
                    if let Some(driver) = slot.as_mut() {
                        driver.target = None;
                        driver.session_generation = None;
                        driver.phase = Phase::LauncherReady;
                    }
                });
                // Preserve the first-frame/identity failure; acknowledge Host release without inventing Markdown weak evidence or success.
                emit("cleanup_without_observation_bridge host_released=true acceptance=false");
                resume_launcher_after_release()?;
                return Ok(StageOutcome::Completed);
            }
            if ime_workload() {
                // Chinese-input release remains independently verified on its original native client and SDK owner.
                let cleanup_only = DRIVER.with_borrow(|slot| {
                    slot.as_ref()
                        .is_some_and(|driver| driver.first_failure.is_some())
                });
                let verified = ime()?.verify_native_closed();
                if !cleanup_only {
                    // Ordinary success must pass the native close proof; cleanup cannot manufacture a passed IME flow.
                    verified?;
                }
                if super::plugin_host::exists()
                    || super::plugin_host::product_probe_generation().is_some()
                {
                    // Native close alone cannot release a still-owned SDK session or authorize another launcher.
                    return Err("中文原 Native 已释放但 SDK 会话仍被持有".into());
                }
                let old = DRIVER.with_borrow_mut(|slot| {
                    let driver = slot.as_mut()?;
                    driver.ime_verified = !cleanup_only && driver.workload_verified;
                    // Revoke the completed session's step/target authority before returning its weak bridge outside this borrow.
                    driver.workload_verified = false;
                    driver.target = None;
                    driver.session_generation = None;
                    driver.phase = Phase::LauncherReady;
                    driver.ime.take()
                });
                drop(old);
                emit("system_ime_closed sdk_session_dropped=true host_barrier_consumed=true");
                resume_launcher_after_release()?;
                return Ok(StageOutcome::Completed);
            }
            if resource_case().is_some() {
                // Resource release and success accounting stay distinct, including cleanup after the first functional error.
                let cleanup_only = DRIVER.with_borrow(|slot| {
                    slot.as_ref()
                        .is_some_and(|driver| driver.first_failure.is_some())
                });
                let verified = resource()?.verify_closed();
                if !cleanup_only {
                    // Only a normal successful scenario may let the original native/resource proof authorize progression.
                    verified?;
                } else if verified.is_err() {
                    // Preserve the first functional cause and actual Host receipt even if this additional cleanup proof also fails.
                    emit("resource_cleanup_verified=false acceptance=false");
                }
                if super::plugin_host::exists()
                    || super::plugin_host::product_probe_generation().is_some()
                {
                    // Released native views cannot waive a remaining real SDK session or generation owner.
                    return Err("资源原 Native 已释放但真实 SDK Session 仍被持有".to_owned());
                }
                let old = DRIVER.with_borrow_mut(|slot| {
                    let driver = slot.as_mut()?;
                    if !cleanup_only {
                        // Cleanup after failure never contributes another passed business case, even when native resources released.
                        driver.verified_resource_cases += 1;
                    }
                    driver.target = None;
                    driver.session_generation = None;
                    driver.phase = Phase::LauncherReady;
                    driver.resource.take()
                });
                // Release weak bridge metadata outside TLS borrowing rather than retaining all nine prior cases as business state.
                drop(old);
                emit(&format!(
                    "resource_case_closed sdk_session_dropped=true host_barrier_consumed=true workload_passed={}",
                    !cleanup_only,
                ));
                resume_launcher_after_release()?;
                return Ok(StageOutcome::Completed);
            }
            if table_case().is_some() {
                // The original table client must close before its step success can count as a completed table case.
                let cleanup_only = DRIVER.with_borrow(|slot| {
                    slot.as_ref()
                        .is_some_and(|driver| driver.first_failure.is_some())
                });
                let verified = table()?.verify_native_closed();
                if !cleanup_only {
                    // Normal completion requires the native close proof; failed-flow cleanup never grants case success.
                    verified?;
                } else if verified.is_err() {
                    // Report additional cleanup failure while retaining the already-recorded first functional cause.
                    emit("table_cleanup_verified=false acceptance=false");
                }
                if super::plugin_host::exists()
                    || super::plugin_host::product_probe_generation().is_some()
                {
                    // The Host session/generation must end independently of the table's native weak evidence.
                    return Err("表格Native已释放但Host仍持原SDK会话".to_owned());
                }
                let old = DRIVER.with_borrow_mut(|slot| {
                    let driver = slot.as_mut()?;
                    if !cleanup_only && driver.workload_verified {
                        // Only the original successful final step plus actual close increases the completed case count.
                        driver.verified_table_cases += 1;
                    }
                    driver.workload_verified = false;
                    driver.target = None;
                    driver.session_generation = None;
                    driver.phase = Phase::LauncherReady;
                    driver.table.take()
                });
                drop(old);
                emit(&format!(
                    "table_case_closed sdk_session_dropped=true host_barrier_consumed=true workload_passed={}",
                    !cleanup_only
                ));
                resume_launcher_after_release()?;
                return Ok(StageOutcome::Completed);
            }
            let cleanup_only = DRIVER.with_borrow(|slot| {
                slot.as_ref()
                    .is_some_and(|driver| driver.first_failure.is_some())
            });
            let verified = hosted()?.verify_native_closed();
            if !cleanup_only {
                // Successful document accounting requires independent release verification on the original bridge.
                verified?;
            } else if verified.is_err() {
                // Preserve the first error while still releasing the observer and returned launcher after the actual Host receipt.
                emit("document_cleanup_verified=false acceptance=false");
            }
            if super::plugin_host::exists()
                || super::plugin_host::product_probe_generation().is_some()
            {
                // An empty native graph is insufficient while Host still owns the real SDK session/generation.
                return Err("Native弱证据完成但Host仍持SDK会话".to_owned());
            }
            let old_bridge = DRIVER.with_borrow_mut(|slot| {
                let driver = slot.as_mut()?;
                if !cleanup_only && driver.workload_verified {
                    // Cleanup and lifecycle receipts cannot increase document success without its original verified final step.
                    driver.completed_documents += 1;
                }
                driver.workload_verified = false;
                driver.target = None;
                driver.session_generation = None;
                driver.phase = Phase::LauncherReady;
                driver.hosted.take()
            });
            // Keep scalar results only; no earlier document, native graph or action observation survives into the next round.
            drop(old_bridge);
            emit(&format!(
                "document_flow_closed native_closed=true sdk_session_dropped=true host_barrier_consumed=true workload_passed={}",
                !cleanup_only,
            ));
            resume_launcher_after_release()?;
        }
        (Stage::FinishIdle, Phase::Idle) => {
            if !foreground_is_empty() {
                // New ownership invalidates this idle interval rather than being hidden by a successful timer callback.
                return Err("最终驻留窗口内出现新的业务 owner".to_owned());
            }
            let append_ime = DRIVER.with_borrow_mut(|slot| {
                let driver = slot.as_mut().ok_or("原驻留 driver 缺失")?;
                if matches!(driver.workload, Workload::CoreUi | Workload::CompleteUi)
                    && !driver.full_idle_verified
                {
                    // The original five-second endpoint arrived; counts come from actual SDK/native close receipts.
                    // Continue the same process without resetting timing, peak observation, fixture or global shortcut lifetime.
                    if !driver.full_workload_verified() || driver.first_failure.is_some() {
                        // Missing full-flow evidence or any previous failure forbids appending an apparently successful IME run.
                        return Err("完整流程或其原驻留尚未通过，不能追加中文验收".to_owned());
                    }
                    driver.full_idle_verified = true;
                    driver.phase = Phase::LauncherReady;
                    Ok(true)
                } else {
                    // A standalone workload or the combined workload's second idle ends this one driver's lifetime.
                    driver.phase = Phase::Finished;
                    Ok(false)
                }
            })?;
            emit("resident_idle_completed seconds=5 foreground_empty=true");
            if append_ime {
                emit(
                    "full_ui_flows_verified repeat_cycles=3 document_flows=8 table_cases=8 resource_cases=9 idle_seconds=5 same_process=true",
                );
                // Original foreground and launcher weak evidence ended. Show a new production launcher and
                // reactivate through its catalog, without direct Markdown construction, input switching or a nested loop.
                resume_launcher_after_release()?;
            } else {
                // Stop only after this workload's final idle; the outer owner still verifies empty resources and reaps the process.
                stop_loop();
            }
        }
        // An unrequested stage/phase pair cannot advance a different workflow or be counted as completion.
        _ => return Err("整产品事件与当前阶段不一致".to_owned()),
    }
    emit(&format!(
        "stage_end stage={} pool_scope=event",
        stage.code()
    ));
    Ok(StageOutcome::Completed)
}

/// Start only the installed diagnostic driver from the real launch callback, without another product initialization path.
pub(super) fn application_did_finish_launching(mtm: MainThreadMarker) {
    let start = DRIVER.with_borrow_mut(|slot| {
        let Some(driver) = slot.as_mut() else {
            // Ordinary product launch has no diagnostic driver or synthetic foreground work.
            return false;
        };
        if driver.phase != Phase::WaitingLaunch {
            // Duplicate launch notifications cannot reopen fixtures or construct another driver.
            return false;
        }
        driver.phase = Phase::LauncherReady;
        true
    });
    if start {
        if !super::shortcuts::product_probe_registration_active() {
            // Preserve real shortcut conflict/settings failure without a fallback key hiding product registration or resident cost.
            fail("真实产品启动未取得唯一已配置热键".to_owned());
            return;
        }
        emit(
            "initialized product_scope=host_launcher_catalog_sdk_markdown physical_input=false present=false editing_verified=false memory_budget_pass=unmeasured",
        );
        super::show_launcher_surface(mtm);
    }
}

/// Capture original graph identity at presentation without inferring readiness or close while show still owns temporary claims.
pub(super) fn launcher_presented(identity: LauncherProbeIdentity) {
    let claim = DRIVER.with_borrow(|slot| {
        let driver = slot.as_ref()?;
        if !matches!(
            driver.phase,
            Phase::LauncherReady | Phase::VerifyPluginRelease
        ) {
            // Other plugin phases and stopped drivers cannot acquire a new diagnostic readiness wait.
            return None;
        }
        Some((driver.epoch, driver.launcher_readiness.is_some()))
    });
    let Some((epoch, already_captured)) = claim else {
        // A presentation outside the admitted phase cannot replace the original observation identity.
        return;
    };
    if !already_captured {
        // Bind identity once when original show returns; later notifications, repeated presentation and getters cannot replace it.
        let wait = Rc::new(LauncherReadinessWait::new(
            epoch,
            super::launcher_close::probe_generation(),
            identity,
        ));
        if !wait.matches_active_owner() {
            // Identity lost during weak capture is failure, never a reason to adopt the latest active panel.
            fail("Launcher 呈现结束时原弱身份已经失效".to_owned());
            return;
        }
        DRIVER.with_borrow_mut(|slot| {
            if let Some(driver) = slot.as_mut()
                && driver.epoch == epoch
                && driver.launcher_readiness.is_none()
            {
                // Native weak loads have returned; transfer weak metadata only, with no reentrant native calls under the borrow.
                driver.launcher_readiness = Some(wait);
            }
        });
    }
    queue_launcher_readiness_check(epoch);
}

/// Copy only the current wait's Rc, whose weak metadata adds no native, document or SDK-session ownership.
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

/// Coalesce real signals into one queued check; Pending without a new notification never retries through this entry.
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
        // Successor-event creation failure stays explicit instead of leaving reserved observation authority undeliverable.
        fail(error);
    }
}

/// Activation notification authorizes one observation; the nonactivating panel still must satisfy actual readiness facts.
pub(super) fn application_did_become_active(notification: &NSNotification, mtm: MainThreadMarker) {
    let Some(wait) = launcher_readiness_wait() else {
        // An early notification cannot create readiness authority before original show identity is captured.
        return;
    };
    let Some(object) = notification.object() else {
        // A sender-less notification cannot establish that this application actually activated.
        return;
    };
    let app = NSApplication::sharedApplication(mtm);
    let expected: &objc2::runtime::AnyObject = &app;
    if std::ptr::eq(expected, &*object) {
        // Native getter/notification claims stay on this stack; queue only one bounded scalar successor event.
        queue_launcher_readiness_check(wait.epoch);
    }
}

/// Require a key notification from this round's original panel, excluding every other window's transition.
pub(super) fn launcher_did_become_key(notification: &NSNotification) {
    if let Some(wait) = launcher_readiness_wait()
        && wait.matches_panel_notification(notification)
    {
        // Only the matched original weak panel identity may signal another readiness observation.
        queue_launcher_readiness_check(wait.epoch);
    }
}

/// Handle editing-begin after an earlier key notification by observing only the original input's actual field editor.
pub(super) fn launcher_did_begin_editing(notification: &NSNotification) {
    if let Some(wait) = launcher_readiness_wait()
        && wait.matches_input_notification(notification)
    {
        queue_launcher_readiness_check(wait.epoch);
    }
}

/// Accept the exact original launcher barrier generation, then independently verify weak graph release instead of trusting the receipt alone.
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
            // Another diagnostic close or prior round's late receipt cannot authorize this plugin activation.
            _ => None,
        }
    });
    let Some((release, final_close, failed, epoch)) = observation else {
        // Unmatched callbacks have no observation or successor authority to consume.
        return;
    };
    let released = autoreleasepool(|_| release.is_some_and(|release| release.is_released()));
    if !released || !foreground_is_empty() {
        fail("Launcher 原图弱证据与真实 Host 释放收据不一致".to_owned());
        // The sole original receipt already arrived; mismatched evidence fails now instead of waiting for an impossible second receipt.
        stop_loop();
        return;
    }
    let old_release = DRIVER.with_borrow_mut(|slot| {
        if let Some(driver) = slot.as_mut() {
            driver.phase = if final_close {
                // The final original close starts idle only after actual launcher graph verification.
                Phase::Idle
            } else {
                // Ordinary cycles may activate the next plugin only after this release boundary.
                Phase::ActivatePlugin
            };
            driver.launcher_release.take()
        } else {
            // An ended driver owns no original weak metadata to transfer.
            None
        }
    });
    // End even the last Objective-C weak reference outside DRIVER borrowing, avoiding destructor reentry into borrowed state.
    drop(old_release);
    emit("launcher_released native_weak_empty=true host_barrier_consumed=true");
    if failed {
        // Successful cleanup after failure remains a failed workload and needs no extra successful-idle observation interval.
        DRIVER.with_borrow_mut(|slot| {
            if let Some(driver) = slot.as_mut() {
                driver.phase = Phase::Finished;
            }
        });
        stop_loop();
    } else if final_close {
        // This one-shot timer measures already-empty resident time; it does not poll native release.
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
                            // A stale driver timer stays inert; a current driver's failed event creation must remain failure.
                            fail(error);
                        }
                    })
                    .map_err(|_| "驻留测量结束事件无法调度".to_owned())
            });
        // The one-shot duration extends observation after resources released, without polling or retrying release conditions.
        if let Err(error) = scheduled {
            fail(error);
        }
    } else if let Err(error) = enqueue(Stage::ActivatePlugin) {
        // Failure to queue the next admitted cycle cannot be replaced by immediate activation on this native callback stack.
        fail(error);
    }
}

/// Record this foreground receipt only after matching finish_close, SDK-session Drop and Host-barrier consumption.
pub(super) fn plugin_released(
    generation: u64,
    target: PluginCommandTarget,
    request: PluginCloseRequest,
) {
    let matched = DRIVER.with_borrow_mut(|slot| {
        let driver = slot.as_mut()?;
        let Phase::PluginRelease {
            generation: expected_generation,
            request: expected,
        } = driver.phase
        else {
            // A receipt outside this driver's original release phase grants no next-step authority.
            return None;
        };
        if generation != expected_generation || driver.target != Some(target) {
            // Old generations, other targets and unrelated sessions cannot advance this round.
            return None;
        }
        if request != expected {
            // Keep the exact intent and scope; a terminal receipt cannot stand in for ordinary plugin return.
            return Some(Err("整产品返回收到错误关闭意图或终止范围".to_owned()));
        }
        driver.phase = Phase::VerifyPluginRelease;
        Some(Ok(()))
    });
    if let Some(result) = matched {
        emit(&format!(
            "plugin_release_receipt scope=foreground intent={:?}",
            request.intent
        ));
        if let Err(error) = result.and_then(|_| enqueue(Stage::PluginReleased)) {
            fail(error);
        }
    }
}

/// Preserve the first failure and clean through original plugin return/launcher close, without substituting process exit for release.
fn fail(error: String) {
    let first = DRIVER.with_borrow_mut(|slot| {
        let driver = slot.as_mut()?;
        if driver.phase == Phase::Finished || driver.first_failure.is_some() {
            // Cleanup reentry cannot issue another close or replace the already-recorded first cause.
            return None;
        }
        driver.first_failure = Some(error);
        driver.pending = None;
        // Cleanup may cancel original work; revoke its queued verification before cancellation callbacks can advance success.
        driver.step_terminal = None;
        Some((
            driver.phase,
            driver.hosted.as_ref().map(Rc::clone),
            driver.target,
            driver.resource.as_ref().map(Rc::clone),
            driver.table.as_ref().map(Rc::clone),
            driver.ime.as_ref().map(Rc::clone),
        ))
    });
    let Some((phase, bridge, expected_target, resource_bridge, table_bridge, ime_bridge)) = first
    else {
        // An absent or already-failed driver adds no second teardown transaction.
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
            // The enclosing branch admits close phases only; other phases acquire no extra close authority.
            _ => None,
        };
        if !matches_accepted_close(phase, expected_target, accepted) {
            // Refused close/save preflight produces no later release receipt; preserve failure without replay or a false release claim.
            stop_loop();
        }
        // Wait only for an actually accepted, identity-matched original close; the parent deadline still rejects missing delivery.
        return;
    }
    if let Some(probe) = resource_bridge {
        // Resource cleanup uses its original probe and declared close direction, never another live session's controls.
        let intent = if resource_case() == Some(NativeMarkdownHostedResourceCase::UnsubmittedSearch)
        {
            // The unsubmitted-search scenario preserves its original return-to-launcher cleanup interaction.
            PluginCloseIntent::ExitToLauncher
        } else {
            // Other resource cases preserve dismissal/resume while the original failing result remains recorded.
            PluginCloseIntent::DismissAndResume
        };
        if begin_plugin_close_observation(intent).is_ok() && probe.request_close().is_ok() {
            // Await only this Host-admitted close; low memory or successful cleanup cannot make the workload pass.
            return;
        }
    } else if let Some(probe) = table_bridge {
        if begin_plugin_close_observation(PluginCloseIntent::ExitToLauncher).is_ok()
            && probe.request_return(true).is_ok()
        {
            // Clean only this original table session without increasing any successful-case count.
            return;
        }
    } else if let Some(probe) = ime_bridge {
        // Use the original IME session's normal return without changing the active input source.
        if begin_plugin_close_observation(PluginCloseIntent::ExitToLauncher).is_ok()
            && probe.request_return().is_ok()
        {
            return;
        }
    } else if let Some(bridge) = bridge {
        let requested = begin_plugin_close_observation(PluginCloseIntent::ExitToLauncher)
            .and_then(|_| bridge.request_return());
        if requested.is_ok() {
            // The original release callback must still verify native weak evidence before closing the returned launcher.
            return;
        }
    } else if expected_target.is_some() {
        // First-frame/path verification may fail before bridge construction. Close through production Host
        // only while the original activation generation remains current. With no Markdown weak proof,
        // the successor is failure cleanup only; process exit or a new observer cannot invent acceptance.
        if begin_plugin_close_observation(PluginCloseIntent::DismissAndResume).is_err() {
            // Lost original session authority forbids closing a successor, so stop with the first failure intact.
            stop_loop();
            return;
        }
        let result = super::plugin_host::request_close(PluginCloseIntent::DismissAndResume);
        let accepted = super::plugin_host::product_probe_accepted_close().map(
            |(generation, target, request)| AcceptedClose::Plugin(generation, target, request),
        );
        let closing_phase = DRIVER.with_borrow(|slot| slot.as_ref().map(|driver| driver.phase));
        if result.is_ok()
            || closing_phase
                .is_some_and(|phase| matches_accepted_close(phase, expected_target, accepted))
        {
            // Both synchronous and asynchronous admission await the original real lifecycle without retrying failed saves.
            return;
        }
    }
    if super::ACTIVE_SURFACE.with_borrow(Option::is_some) && close_launcher(true).is_ok() {
        // Cleanup still waits for the actual foreground receipt; close returning alone proves no resource release.
        return;
    }
    // A missing bridge or refused close stops with failure; clearing production owner slots would fabricate release.
    stop_loop();
}

/// Wake nextEvent after stop with a non-input event, without synthesizing input or changing close semantics through menu Quit.
fn stop_loop() {
    let Some(mtm) = MainThreadMarker::new() else {
        // A worker cannot access AppKit or pretend its own TLS describes the main thread's event loop.
        return;
    };
    let app = NSApplication::sharedApplication(mtm);
    app.stop(None);
    if let Some(event) = NSEvent::otherEventWithType_location_modifierFlags_timestamp_windowNumber_context_subtype_data1_data2(
        NSEventType::ApplicationDefined, NSPoint::new(0.0, 0.0), NSEventModifierFlags::empty(), 0.0, 0, None,
        0, 0, 0,
    ) {
        // This unowned wake event carries no diagnostic stage, input action or foreground mutation authority.
        app.postEvent_atStart(&event, true);
    }
}

/// Run repeated full long-document flows, a giant paragraph, source modes and independent resource cases
/// in a new empty parent-owned directory, then observe five seconds of empty-foreground residency.
/// The caller supplies its verified existing summon binding; only synthetic settings are written.
///
/// Editing success requires exact Unicode replacement, undo/redo, original-text restoration and real save.
/// Resource cases separately verify release. This entry proves neither physical input, presentation nor
/// the product memory budget; the parent independently verifies process attribution, sampling and exit.
/// All fixture writes/deletions remain inside this new exclusive directory, without touching user documents.
///
/// # Errors
///
/// Reject foreign threads, existing foreground owners, nonempty directories and native/step/save/release failures.
/// Cleanup preserves the first concrete failure rather than turning resource release into functional success.
pub fn run_native_product_acceptance(root: &Path, shortcut: &str) -> Result<(), String> {
    run_product(root, shortcut, Workload::Full)
}

/// Exercise real Chinese candidates through the existing composition root, without a nested loop or reusing personal input clients.
pub fn run_native_product_ime_acceptance(root: &Path, shortcut: &str) -> Result<(), String> {
    run_product(root, shortcut, Workload::SystemIme)
}

/// Run original editing and Chinese-candidate flows in one driver, fixture and process without a new browser page.
/// External links have a separate real delivery boundary; this entry cannot replace CompleteUi's HTTP gate.
///
/// # Errors
/// Preserve the original first failure from functionality, save, system input, resource release or either idle endpoint.
pub fn run_native_product_core_ui_acceptance(root: &Path, shortcut: &str) -> Result<(), String> {
    run_product(root, shortcut, Workload::CoreUi)
}

/// Run every original flow and five-second idle, then eight real Chinese-input steps and final idle in the same PID.
/// Both phases share the original application composition and exclusive fixture; independent low peaks cannot
/// be combined into a passed whole-product memory result.
///
/// # Errors
/// The same driver preserves the first functional, save, system-input, resource or idle-endpoint failure.
pub fn run_native_product_complete_ui_acceptance(
    root: &Path,
    shortcut: &str,
) -> Result<(), String> {
    run_product(root, shortcut, Workload::CompleteUi)
}

/// Share one composition and cleanup path across standalone/combined workloads without starting another application loop.
fn run_product(root: &Path, shortcut: &str, workload: Workload) -> Result<(), String> {
    let configured_shortcut = ShortcutBinding::parse(shortcut)
        .map_err(|_| "整产品验收必须使用明确有效的既有启动器快捷键".to_owned())?;
    MainThreadMarker::new().ok_or("整产品验收需要真实 AppKit 主线程")?;
    if DRIVER.with_borrow(Option::is_some) || !foreground_is_empty() {
        // Another driver or live foreground graph cannot lend its state, documents or close authority to this run.
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
        // Initialize only the declared new empty root; existing contents cannot be removed or rewritten to satisfy this precondition.
        return Err("整产品验收目录必须是绝对路径的新空目录".to_owned());
    }
    let _pasteboard = super::acceptance_fixture::install_fixture(root)?;
    let fixture = FixtureScope;
    // Synthetic settings have no user choice and the default may conflict. Persist the caller's verified
    // existing binding here, letting original production startup perform its sole registration.
    ShortcutSettingsStore::new(root.join("application-support"))
        .persist(configured_shortcut)
        .map_err(|_| "无法为合成验收持久化明确的既有快捷键".to_owned())?;
    // Remove only the two files this fixture just created; one later long note avoids filesystem-order-dependent selection.
    for name in ["Acceptance.md", "SourcePerformance.md"] {
        std::fs::remove_file(root.join("workspace").join(name))
            .map_err(|_| "移除本工具短样本失败")?;
    }
    let epoch = NEXT_EPOCH
        .get()
        .checked_add(1)
        .filter(|value| *value <= isize::MAX as u64)
        .ok_or("整产品验收代际耗尽")?;
    NEXT_EPOCH.set(epoch);
    // SAFETY: receive sharedApplication using NSApplication's public return ABI and +1 retained binding contract.
    // A later dynamic cast independently checks the routing subclass instead of assuming an existing base object has that type.
    let app: Retained<NSApplication> =
        unsafe { msg_send![ProductAcceptanceApplication::class(), sharedApplication] };
    let app = app
        .downcast::<ProductAcceptanceApplication>()
        .map_err(|_| "整产品验收需要首次创建自己的应用事件路由".to_owned())?;
    if app.isRunning() {
        // A live application loop cannot be nested or inherited as if this run owned its full initialization and lifetime.
        return Err("整产品验收不能嵌套已有应用事件循环".to_owned());
    }
    DRIVER.with_borrow_mut(|slot| {
        // Publish the complete synthetic metadata once after all fallible acquisition and before production startup callbacks.
        *slot = Some(Driver {
            epoch,
            root: root.to_owned(),
            origin: Instant::now(),
            phase: Phase::WaitingLaunch,
            pending: None,
            step_terminal: None,
            completed_documents: 0,
            verified_editing_flows: 0,
            workload_verified: false,
            launcher_readiness: None,
            readiness_schedule: ReadinessSchedule::default(),
            launcher_release: None,
            target: None,
            session_generation: None,
            hosted: None,
            resource: None,
            table: None,
            verified_table_cases: 0,
            verified_resource_cases: 0,
            first_failure: None,
            workload,
            full_idle_verified: false,
            ime: None,
            ime_verified: false,
            external_http: None,
            external_http_handoff_verified: false,
        });
    });
    match workload {
        // Each receipt declares its own limits; none claims physical keyboard input or a measured memory budget.
        Workload::SystemIme => emit(
            "configured workload=system_ime actual_input_source=true input_source_selected=false physical_keyboard=false memory_budget_pass=unmeasured",
        ),
        Workload::Full => emit(
            "configured repeat_cycles_planned=3 document_flows_planned=8 full_steps_per_flow=45 table_cases_planned=8 resource_cases_planned=9 input=programmatic_native_actions editing_verified=false memory_budget_pass=unmeasured",
        ),
        Workload::CoreUi => emit(
            "configured workload=core_ui external_http_handoff_planned=false repeat_cycles_planned=3 document_flows_planned=8 full_steps_per_flow=45 table_cases_planned=8 resource_cases_planned=9 system_ime_steps_planned=8 full_idle_seconds=5 final_idle_seconds=5 same_process=true actual_input_source=true input_source_selected=false physical_keyboard=false editing_verified=false memory_budget_pass=unmeasured",
        ),
        Workload::CompleteUi => emit(
            "configured workload=complete_ui external_http_handoff_planned=true repeat_cycles_planned=3 document_flows_planned=8 full_steps_per_flow=45 table_cases_planned=8 resource_cases_planned=9 system_ime_steps_planned=8 full_idle_seconds=5 final_idle_seconds=5 same_process=true actual_input_source=true input_source_selected=false physical_keyboard=false editing_verified=false memory_budget_pass=unmeasured",
        ),
    }
    let result = super::run_application();
    // Production run_application unregisters on stop, but independently inspect empty foreground and registration
    // authority instead of inferring release from having executed that code path.
    let released = foreground_is_empty() && super::shortcuts::product_probe_registration_released();
    drop(fixture);
    let mut state = DRIVER.take().ok_or("整产品 driver 提前丢失")?;
    let http_cleanup = state
        .external_http
        .take()
        .map_or(Ok(()), |mut server| server.shutdown());
    // Every ordinary error still ends the sole network owner; resource cleanup never erases a failed functional outcome.
    let failure = result.err().or_else(|| state.first_failure.take());
    match (failure, http_cleanup) {
        // Keep functional and cleanup causes independent, including after a partially acquired HTTP listener.
        (Some(primary), Err(cleanup)) => return Err(format!("{primary}；HTTP释放失败：{cleanup}")),
        (Some(primary), Ok(())) => return Err(primary),
        (None, Err(cleanup)) => return Err(cleanup),
        (None, Ok(())) => {}
    }
    let workload_passed = match workload {
        Workload::SystemIme => state.ime_verified,
        Workload::Full => state.full_workload_verified(),
        Workload::CoreUi => {
            // Require every original editing/input endpoint and both idles; without actual HTTP this proves no external handoff.
            state.full_workload_verified() && state.full_idle_verified && state.ime_verified
        }
        Workload::CompleteUi => {
            // Complete UI additionally needs the actual network terminal receipt, not just correct document/source state.
            state.full_workload_verified()
                && state.full_idle_verified
                && state.ime_verified
                && state.external_http_handoff_verified
        }
    };
    if state.phase != Phase::Finished
        || !workload_passed
        || !released
        || state.hosted.is_some()
        || state.resource.is_some()
        || state.table.is_some()
        || state.ime.is_some()
        || state.step_terminal.is_some()
        || state.external_http.is_some()
    {
        // Functional results, final stage and all session/observer/registration owners must jointly finish before a receipt is printed.
        return Err("整产品循环缺失终点、真实前台释放或热键注销证据".to_owned());
    }
    match workload {
        // Final receipts report only this original workload's verified scope; the parent owns RSS and process-exit checks.
        Workload::SystemIme => println!(
            "product_acceptance event=system_ime_finished actual_han=true visible_candidate_near_caret=true undo_redo_restore=true saved=true viewport_lazy=true foreground_empty=true sdk_session_owned=false global_key_owner_released=true fixture_cleared=true physical_keyboard=false memory_budget_pass=unmeasured"
        ),
        Workload::Full => println!(
            "product_acceptance event=finished repeat_cycles=3 document_flows=8 full_editing_flows=4 table_cases=8 resource_cases=9 foreground_empty=true sdk_session_owned=false global_key_owner_released=true fixture_cleared=true editing_verified=true memory_budget_pass=unmeasured"
        ),
        Workload::CoreUi => println!(
            "product_acceptance event=finished workload=core_ui external_http_handoff_verified=not_run browser_tab_cleanup=unverified browser_history_cleanup=unverified repeat_cycles=3 document_flows=8 full_editing_flows=4 table_cases=8 resource_cases=9 system_ime_steps=8 full_idle_seconds=5 final_idle_seconds=5 same_process=true actual_han=true visible_candidate_near_caret=true undo_redo_restore=true saved=true viewport_lazy=true foreground_empty=true sdk_session_owned=false global_key_owner_released=true fixture_cleared=true physical_keyboard=false editing_verified=true memory_budget_pass=unmeasured"
        ),
        Workload::CompleteUi => println!(
            "product_acceptance event=finished workload=complete_ui external_http_handoff_verified=true https_verified=false browser_tab_cleanup=unverified browser_history_cleanup=unverified repeat_cycles=3 document_flows=8 full_editing_flows=4 table_cases=8 resource_cases=9 system_ime_steps=8 full_idle_seconds=5 final_idle_seconds=5 same_process=true actual_han=true visible_candidate_near_caret=true undo_redo_restore=true saved=true viewport_lazy=true foreground_empty=true sdk_session_owned=false global_key_owner_released=true fixture_cleared=true physical_keyboard=false editing_verified=true memory_budget_pass=unmeasured"
        ),
    }
    Ok(())
}
