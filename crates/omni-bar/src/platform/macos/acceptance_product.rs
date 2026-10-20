//! 真实Host完整执行长文、巨段落、源模式与资源场景，最终观察5秒空前台驻留。
//! 本入口只持合成路径、标量和原生弱证据；功能、保存、SDK回执与真实释放分别决定结果。

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
    NativeMarkdownHostedFlow, NativeMarkdownHostedResourceCase, NativeMarkdownHostedResourceProbe,
    NativeMarkdownHostedStep, NativeMarkdownHostedTableCase, NativeMarkdownHostedTableProbe,
    NativeMarkdownHostedTableStep, NativeMarkdownHostedWorkload,
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

/// 原九个资源场景分别经过真实 SDK 激活和关闭，不复用上一场景的文稿或宿主回执。
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

/// 前三轮反复核相同长文的真实资源生命周期；其后巨段落和四种源模式各从全新会话开始。
/// 步骤由插件原工作负载持有，Host不维护第二份动作表，也不跳过巨段落的后半程。
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

/// 每个表格用例重新安装真实插件会话，单格历史、格式和冲突结果不能互相借票。
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

/// Host只接各插件工作负载拥有的具名动作；不会把外部整数解释成任意产品操作。
#[derive(Clone, Copy)]
enum WorkloadStep {
    Document(NativeMarkdownHostedStep),
    Table(NativeMarkdownHostedTableStep),
}

impl WorkloadStep {
    /// 仅四种源模式的原末动作会同步发返回，其余用例由Host在成功终点之后发起。
    fn closes_surface(self) -> bool {
        match self {
            Self::Document(step) => step.closes_surface(),
            Self::Table(_) => false,
        }
    }
}

/// 真实释放回执和后继事件中的弱证据分别观察；经过多久都不能补授完成权限。
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

/// 已接纳关闭只取生产owner回调的真实标量，不能从driver预期阶段推断接纳。
#[derive(Clone, Copy)]
enum AcceptedClose {
    Launcher(u64),
    Plugin(u64, PluginCommandTarget, PluginCloseRequest),
}

/// 分别核对关闭请求与真实接纳；拒绝、外来会话或仅终止应用的回执都不能授权继续等待。
fn matches_accepted_close(
    phase: Phase,
    expected_target: Option<PluginCommandTarget>,
    accepted: Option<AcceptedClose>,
) -> bool {
    match (phase, accepted) {
        (Phase::LauncherRelease { generation, .. }, Some(AcceptedClose::Launcher(actual))) => {
            generation == actual // 只等待本次真实关闭动作已经建立的原代次。
        }
        (
            Phase::PluginRelease {
                generation,
                request: expected,
            },
            Some(AcceptedClose::Plugin(actual, target, request)),
        ) => {
            // 动态会话代次、静态目标和前台返回scope必须属于同一次真实关闭。
            generation == actual && expected_target == Some(target) && request == expected
        }
        _ => false, // 接纳缺失、阶段类型不符或旧完成回执均保持失败，不改写当前关闭权限。
    }
}

/// 有限事件值与driver代次共同拒绝重复、失序和先前轮次的迟到交付。
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

/// Pending只结束本次观察，不代表功能成功，也不授予关闭或下一轮权限。
#[derive(Clone, Copy, Eq, PartialEq)]
enum StageOutcome {
    Completed,
    Pending,
}

/// 只保存本请求原终点的标量权限；旧轮次、旧driver或重复终点不能安排新事件。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct StepTerminalSignal {
    epoch: u64,
    session_generation: u64,
    index: usize,
}

impl Stage {
    /// 固定标量编码只表达本入口阶段，不接任意selector、插件身份或文件路径。
    fn code(self) -> isize {
        match self {
            // 结构事件使用互不重叠的固定编码；编辑步骤只取上方有限数组中的实际索引。
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

    /// 未知事件编码没有成功默认值；调用方必须保留阶段失序的真实失败。
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
                // 先核原有符号范围再转usize，负值或越界值不能变成有效步骤索引。
                Some(Self::Step((value - STEP_FIRST) as usize))
            }
            _ => None, // 其它ApplicationDefined事件没有执行本验收动作的权限。
        }
    }
}

/// 观察桥只保原生弱身份与文件事实；Rc不增加正文、窗口或SDK会话所有权。
struct Driver {
    epoch: u64,
    root: PathBuf,
    origin: Instant,
    phase: Phase,
    pending: Option<Stage>,
    step_terminal: Option<StepTerminalSignal>,
    completed_documents: usize,
    verified_editing_flows: usize,
    /// 只由当前原步骤表的最后成功动作授予，清理与Host关闭回执均不能补签。
    workload_verified: bool,
    launcher_readiness: Option<Rc<LauncherReadinessWait>>,
    readiness_schedule: ReadinessSchedule,
    launcher_release: Option<Rc<LauncherReleaseProbe>>,
    target: Option<PluginCommandTarget>,
    /// 静态 target 可重复；激活返回时就固定这一次宿主代次，失败清理也不能接管新会话。
    session_generation: Option<u64>,
    hosted: Option<Rc<NativeMarkdownHostedWorkload>>,
    /// 仅保当前原图的弱观察桥；计数只在真实 Host 收讫和磁盘/资源门通过后推进。
    resource: Option<Rc<NativeMarkdownHostedResourceProbe>>,
    table: Option<Rc<NativeMarkdownHostedTableProbe>>,
    verified_table_cases: usize,
    verified_resource_cases: usize,
    first_failure: Option<String>,
}

impl Driver {
    /// 只有插件原步骤表的最后一步通过才允许正常关闭；特殊流程的末步本身已发正式返回。
    fn accept_verified_workload(
        &mut self,
        index: usize,
        closes_surface: bool,
    ) -> Result<(), String> {
        let steps = self.current_step_count().ok_or("原工作负载已结束")?;
        let phase_matches = if closes_surface {
            matches!(
                self.phase,
                Phase::PluginRelease { .. } | Phase::VerifyPluginRelease
            )
        } else {
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
            return Err("原文稿验证不属于本会话唯一成功终点".to_owned());
        }
        self.workload_verified = true;
        if self.current_flow() == Some(NativeMarkdownHostedFlow::Full) {
            // Full的最后一步是恢复原文后的正式保存验证；其余场景各有自己的原保存断言。
            self.verified_editing_flows += 1;
        }
        Ok(())
    }

    /// 当前流程只由已完成且释放的文稿数量选择，外来事件不能自行指定工作负载或步骤。
    fn current_flow(&self) -> Option<NativeMarkdownHostedFlow> {
        DOCUMENT_FLOWS.get(self.completed_documents).copied()
    }

    /// 表格只能在全部文稿流程释放后开始，当前case索引不能由Native回调修改。
    fn current_table_case(&self) -> Option<NativeMarkdownHostedTableCase> {
        (self.completed_documents == DOCUMENT_FLOWS.len())
            .then(|| TABLE_CASES.get(self.verified_table_cases).copied())
            .flatten()
    }

    /// 步骤长度来自插件的单一原表；Host只据此核队列索引与末动作，不复制功能契约。
    fn current_step_count(&self) -> Option<usize> {
        self.current_flow()
            .map(|flow| flow.steps().len())
            .or_else(|| self.current_table_case().map(|case| case.steps().len()))
    }

    /// 仅从插件拥有的当前具名步骤表取动作，不转换任意selector或重新发现文稿。
    fn current_step(&self, index: usize) -> Option<WorkloadStep> {
        if let Some(flow) = self.current_flow() {
            return flow.steps().get(index).copied().map(WorkloadStep::Document);
        }
        self.current_table_case()?
            .steps()
            .get(index)
            .copied()
            .map(WorkloadStep::Table)
    }

    /// 登记一个原终点观察；已有待交付信号或排队核验不能被另一观察替换。
    fn begin_step_terminal(&mut self, index: usize) -> Result<StepTerminalSignal, String> {
        if self.current_step(index).is_none()
            || self.phase != Phase::Step(index)
            || self.pending.is_some()
            || self.step_terminal.is_some()
            || self.first_failure.is_some()
        {
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

    /// 原终点信号只授权一次核验事件；实际功能结果仍须由该步骤自己的条件证明。
    fn accept_step_terminal(&mut self, signal: StepTerminalSignal) -> bool {
        if self.step_terminal != Some(signal)
            || signal.epoch != self.epoch
            || Some(signal.session_generation) != self.session_generation
            || self.phase != Phase::Step(signal.index)
            || self.pending.is_some()
            || self.first_failure.is_some()
        {
            return false; // 旧通知或清理通知保持无副作用，不能消费当前另一份观察。
        }
        self.step_terminal = None;
        true
    }

    /// 先消费原标量事件权限；状态借用结束后才调用Native getter或丢弃Rc/Weak。
    fn accept_event(
        &mut self,
        epoch: isize,
        stage: Option<Stage>,
    ) -> Option<Result<Stage, String>> {
        if epoch != self.epoch as isize || self.phase == Phase::Finished {
            // 旧代次或已停止driver保持无副作用，不占用当前后继事件。
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

/// install_fixture之后的每条返回路径都归还本工具命名剪贴板和注入路径，失败也不能遗留。
struct FixtureScope;

impl Drop for FixtureScope {
    fn drop(&mut self) {
        // 只有真实主线程入口构造此owner；共享清理先取走TLS所有权，再执行可能重入的释放。
        super::acceptance_fixture::clear_fixture();
    }
}

thread_local! {
    /// 默认产品不编译这些槽；验收停止后不保业务资源或采样历史。
    static DRIVER: RefCell<Option<Driver>> = const { RefCell::new(None) };
    static NEXT_EPOCH: Cell<u64> = const { Cell::new(0) };
}

define_class!(
    /// 只接本验收的非输入事件；其余事件完整交还真实NSApplication处理。
    // SAFETY: 此NSApplication子类没有额外Rust ivars；主线程首次sharedApplication创建唯一应用对象。
    #[unsafe(super = NSApplication)]
    #[thread_kind = MainThreadOnly]
    struct ProductAcceptanceApplication;

    impl ProductAcceptanceApplication {
        #[unsafe(method(sendEvent:))]
        fn send_event(&self, event: &NSEvent) {
            if route_event(event) {
                // 只消费匹配的诊断subtype；键盘、鼠标和正式菜单事件仍走原产品路由。
                return;
            }
            // SAFETY: 原event在主线程同步栈内存活；匹配的super selector保持NSApplication公开ABI。
            unsafe { let _: () = msg_send![super(self), sendEvent: event]; }
            if event.r#type() == NSEventType::LeftMouseDown {
                // 原按钮的Down/tracking已真正返回，才从当前driver复制表格验收桥。
                // 不跨super保DRIVER或ACTIVE租约，Native重入不能继承此前借用的原图。
                let probe = DRIVER.with_borrow(|slot| {
                    slot.as_ref()?.table.as_ref().map(Rc::clone)
                });
                if let Some(probe) = probe
                    && let Err(error) = probe.did_dispatch_hosted_event(event)
                {
                    // MD只匹配事前保存的原Down唯一编号及窗口等派发值，再归还有限事件租约。
                    // 这里只记鼠标返回；实际selector可更晚交付，不据此猜业务已经入队。
                    fail(error);
                }
            }
        }

        #[unsafe(method(sendAction:to:from:))]
        /// 真实系统 target/action 返回边界；只在验收 App 中被动报告，不改产品 selector 或目标。
        fn send_action(&self, action: Sel, target: Option<&AnyObject>, sender: Option<&AnyObject>) -> bool {
            // SAFETY: 原selector/target/sender均来自本次AppKit同步调用，匹配公开NSApplication ABI；
            // super恰执行一次并保留原bool结果，不持DRIVER/ACTIVE借用跨越真实业务派发。
            let sent: bool = unsafe { msg_send![super(self), sendAction: action, to: target, from: sender] };
            let probe = DRIVER.with_borrow(|slot| {
                slot.as_ref()?.table.as_ref().map(Rc::clone)
            });
            if let Some(probe) = probe
                && let Err(error) = probe.did_dispatch_hosted_action(action, target, sender, sent)
            {
                // 原按钮的具体selector已返回才有业务排队证据；MD仍须等原Down和真实CPU终点。
                // 失败沿原driver收尾，不能重发action、延长期限或把系统sent当业务完成。
                fail(error);
            }
            sent
        }
    }
);

/// 单调时钟只定位步骤观察；它不能证明画面呈现、物理输入延迟或内存验收通过。
fn emit(event: &str) {
    let metadata = DRIVER.with_borrow(|slot| {
        // 只复制标量后结束业务状态借用，stdout输出不跨可能重入的租约。
        slot.as_ref().map(|driver| {
            (
                driver.completed_documents,
                driver.origin.elapsed().as_nanos(),
            )
        })
    });
    if let Some((cycles, elapsed)) = metadata {
        // 输出失败不改变产品关闭行为；父采样方必须把协议记录缺失作为失败。
        let _ = writeln!(
            std::io::stdout().lock(),
            "product_acceptance event={event} documents={cycles} mono_ns={elapsed}"
        );
    }
}

/// 只为原已取得代次安排一个后继事件，不用timer或重试推断功能与资源进度。
fn enqueue(stage: Stage) -> Result<(), String> {
    let epoch = DRIVER.with_borrow_mut(|slot| {
        let driver = slot.as_mut().ok_or("整产品 driver 已结束")?;
        if driver.phase == Phase::Finished || driver.pending.is_some() {
            // 保留已排队事件；重复回调不能覆盖它的后继动作。
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

/// 诊断subtype不能授权其它代次；每阶段只在真实事件回调自己的autorelease池中执行。
fn route_event(event: &NSEvent) -> bool {
    if event.r#type() != NSEventType::ApplicationDefined || event.subtype().0 != EVENT_SUBTYPE {
        // 无关事件保持生产路由，driver存在不能成为吞掉用户输入的依据。
        return false;
    }
    let epoch = event.data1();
    let event_code = event.data2();
    let stage = Stage::decode(event_code);
    let claim = DRIVER.with_borrow_mut(|slot| {
        // data1已从原event复制；状态借用内只消费这些标量，不调用Native getter。
        slot.as_mut()?.accept_event(epoch, stage)
    });
    if let Some(claim) = claim {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            // 本事件结束时归还全部临时Native claim，再观察后继真实释放回执。
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
            // 在ABI边界把Rust panic固定为失败，不能展开穿过Objective-C调用栈。
            fail(error);
        }
    }
    true
}

/// 从Active借出仅本同步栈使用的呈现句柄，结束owner借用后才调用Native getter。
fn active_launcher() -> Result<LauncherPresentation, String> {
    super::ACTIVE_SURFACE
        .with_borrow(|surface| surface.as_ref().map(|surface| surface.presentation()))
        .ok_or_else(|| "真实 Launcher owner 不存在".to_owned())
}

/// 必须同时没有Native业务owner和未完成状态；窗口隐藏或单个session槽为空均不足以证明释放。
fn foreground_is_empty() -> bool {
    !super::ACTIVE_SURFACE.with_borrow(Option::is_some)
        && !super::CLOSING_SURFACE.with_borrow(Option::is_some)
        && !super::plugin_host::exists()
        && !super::builtins::is_open()
        && super::FOREGROUND_STATE.with_borrow(|state| {
            // 同时核原前台、转移和销毁屏障；未消费barrier不能当成已完成的空前台。
            state.active_surface().is_none()
                && state.pending_transition().is_none()
                && state.destroyed_barrier().is_none()
        })
}

/// 在启动与返回后观察正式Launcher，不创建替代窗口或推测输入已就绪。
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
        // 只消费本次getter栈内真实重入的通知；没有新通知的Pending不能自行排队重试。
        let epoch = launcher_readiness_wait()
            .ok_or("Launcher 原始等待已撤销")?
            .epoch;
        queue_launcher_readiness_check(epoch);
    }
    observed
}

/// 只观察原Launcher图；外层事件调度仍保留生产端每个真实就绪条件。
fn observe_launcher_readiness() -> Result<StageOutcome, String> {
    let mtm = MainThreadMarker::new().ok_or("Launcher 验证需要主线程")?;
    let wait = launcher_readiness_wait().ok_or("Launcher 原始呈现弱身份缺失")?;
    if !wait.matches_active_owner() {
        // 原图消失或关闭代次改变即撤权；新的Active owner不能替原图重启这轮验收。
        return Err("等待就绪期间原 Launcher 身份或关闭代际已变化".to_owned());
    }
    let launcher = active_launcher()?;
    let ready = launcher.input_readiness(mtm);
    drop(launcher); // 后续关闭与独立释放核验前先归还getter产生的临时图句柄。
    let state_matches = super::FOREGROUND_STATE.with_borrow(|state| {
        state.active_surface() == Some(ForegroundSurface::Launcher)
            && state.pending_transition().is_none()
            && state.destroyed_barrier().is_none()
    });
    if !state_matches || super::plugin_host::exists() || !wait.matches_active_owner() {
        // 可见输入必须属于当前无在途转移的原owner；旧观察不能授予插件激活权限。
        return Err("Launcher 可见状态与 Host 所有权不一致".to_owned());
    }
    if !ready.is_ready() {
        // show返回不证明key或焦点已交付；保原弱身份，等待后继真实通知再观察。
        emit(&format!("launcher_readiness_pending facts={ready:?}"));
        return Ok(StageOutcome::Pending);
    }
    Ok(StageOutcome::Completed)
}

/// 正常关闭Launcher前先固定原图弱证据和本次关闭代次，不能在关闭后补采后继身份。
fn close_launcher(final_close: bool) -> Result<(), String> {
    let wait = launcher_readiness_wait().ok_or("Launcher 关闭缺少原始呈现权限")?;
    if !wait.matches_active_owner() {
        // 失败清理也只准关闭原图；等待期间出现的另一个owner不属于这份关闭权限。
        return Err("原 Launcher 关闭权限已失效".to_owned());
    }
    let release = active_launcher()?.release_probe()?;
    let generation = super::launcher_close::probe_generation()
        .checked_add(1)
        .ok_or("Launcher 关闭代际耗尽")?;
    if !wait.matches_active_owner() {
        // 快照getter可能重入；再次核原弱身份，不能改用后继对象补签本次关闭。
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
    drop(old_wait); // TLS借用外归还原弱元数据，不把旧等待跨过此次关闭。
    super::try_request_launcher_close()
}

/// 核验只借原观察桥；桥不拥有Native图或SDK会话。
fn hosted() -> Result<Rc<NativeMarkdownHostedWorkload>, String> {
    DRIVER
        .with_borrow(|slot| {
            slot.as_ref()
                .and_then(|driver| driver.hosted.as_ref().map(Rc::clone))
        })
        .ok_or_else(|| "整产品 Markdown 原始观察桥缺失".to_owned())
}

/// 原请求实际到达终点后才排核验；同步完成的步骤也使用独立后继事件。
fn schedule_hosted_step(index: usize) -> Result<(), String> {
    let step = workload_step(index).ok_or("终点观察步骤越界")?;
    let signal = DRIVER.with_borrow_mut(|slot| {
        slot.as_mut()
            .ok_or("原步骤终点 driver 缺失")?
            .begin_step_terminal(index)
    })?;
    // 继承原工作负载的三秒失败期限；只持同一原信号的标量，不保活动作、文稿或Native图。
    let deadline = dispatch2::DispatchTime::try_from(Duration::from_secs(3))
        .map_err(|_| "原步骤失败期限无效".to_owned())?;
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
                return; // 已有真实终点、取消、关闭或新会话时，旧失败通知没有读取后继图的权限。
            }
            if let WorkloadStep::Document(step) = step
                && let Ok(bridge) = hosted()
            {
                bridge.diagnose_deadline(step); // 只读原票有限诊断，不推进执行器或签另一份Ready。
            }
            fail("原工作负载三秒内没有交付真实终点".to_owned());
        })
        .map_err(|_| "原步骤失败通知无法调度".to_owned())?;
    let terminal = match step {
        WorkloadStep::Document(step) => {
            hosted()?.observe_step_terminal(step, move || hosted_step_terminal(signal))?
        }
        WorkloadStep::Table(step) => {
            table()?.observe_step_terminal(step, move || hosted_step_terminal(signal))?
        }
    };
    if terminal {
        // 已先完成或同步发布的终点消费同一原权限，不另等或伪造第二次通知。
        hosted_step_terminal(signal);
    } else {
        emit(&format!(
            "step_terminal_pending stage={} receipt=original",
            Stage::Step(index).code()
        ));
    }
    Ok(())
}

/// MD先结束观察槽借用再交付回调；宿主只接标量，不读或保留Native对象与正文。
fn hosted_step_terminal(signal: StepTerminalSignal) {
    let accepted = DRIVER.with_borrow_mut(|slot| {
        slot.as_mut()
            .is_some_and(|driver| driver.accept_step_terminal(signal))
    });
    if accepted && let Err(error) = enqueue(Stage::Step(signal.index)) {
        fail(error); // 事件创建失败必须固定失败，不能留下已经完成却永远没有核验事件的等待。
    }
}

/// 只取当前实际文稿的具名流程，终点通知不会获得选择下一会话的权限。
fn hosted_flow() -> Option<NativeMarkdownHostedFlow> {
    DRIVER.with_borrow(|slot| slot.as_ref()?.current_flow())
}

/// 当前动作仅从这次原工作负载选择，错误码或外来索引不能授予另一会话的输入权限。
fn workload_step(index: usize) -> Option<WorkloadStep> {
    DRIVER.with_borrow(|slot| slot.as_ref()?.current_step(index))
}

/// 后继表格用例由已完成释放数量选择，旧回调不能复活上一场景。
fn table_case() -> Option<NativeMarkdownHostedTableCase> {
    DRIVER.with_borrow(|slot| slot.as_ref()?.current_table_case())
}

/// 只借同一真实表格会话的弱观察桥，不保留SDK/Native业务所有权。
fn table() -> Result<Rc<NativeMarkdownHostedTableProbe>, String> {
    DRIVER
        .with_borrow(|slot| slot.as_ref()?.table.as_ref().map(Rc::clone))
        .ok_or_else(|| "原表格场景观察桥缺失".to_owned())
}

/// 所有长文与源模式流程完成后才选择资源场景，缺少当前场景不能冒充全部通过。
fn resource_case() -> Option<NativeMarkdownHostedResourceCase> {
    DRIVER.with_borrow(|slot| {
        let driver = slot.as_ref()?;
        (driver.completed_documents == DOCUMENT_FLOWS.len()
            && driver.verified_table_cases == TABLE_CASES.len())
        .then(|| RESOURCE_CASES.get(driver.verified_resource_cases).copied())
        .flatten()
    })
}

/// 外部调用只借原观察桥；不把 Session、Native 控件或文稿放入宿主验证状态。
fn resource() -> Result<Rc<NativeMarkdownHostedResourceProbe>, String> {
    DRIVER
        .with_borrow(|slot| slot.as_ref()?.resource.as_ref().map(Rc::clone))
        .ok_or_else(|| "原资源场景观察桥缺失".to_owned())
}

/// 上一真实前台已释放后才重置本工具自己的有限样本；不覆盖仍映射中的文稿。
fn prepare_resource_fixture() -> Result<(), String> {
    if !foreground_is_empty() {
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

/// 只在真实前台为空时回收本入口独占workspace，附件等样本不会泄漏到下一会话。
fn reset_owned_workspace(workspace: &Path) -> Result<(), String> {
    if !foreground_is_empty() {
        return Err("原前台尚未释放，不能重置合成笔记库".to_owned());
    }
    std::fs::remove_dir_all(workspace).map_err(|_| "无法归还原合成笔记库".to_owned())?;
    std::fs::create_dir(workspace).map_err(|_| "无法创建后继合成笔记库".to_owned())
}

/// 夹具仍按8KiB以内块写磁盘；巨段落从不在Host构造完整String。
fn prepare_document_fixture() -> Result<(), String> {
    let (workspace, giant) = DRIVER.with_borrow(|slot| {
        let driver = slot.as_ref().ok_or("原长文driver缺失")?;
        Ok::<_, String>((
            driver.root.join("workspace"),
            driver.completed_documents == CYCLES,
        ))
    })?;
    reset_owned_workspace(&workspace)?;
    let path = workspace.join("Acceptance.md");
    if giant {
        omni_md_plugin::write_native_markdown_giant_paragraph_fixture(&path)
    } else {
        omni_md_plugin::write_native_markdown_memory_fixture(&path)
    }
    .map_err(|_| "流式合成文稿生成失败".to_owned())
}

/// 样本文字由插件原用例唯一拥有；Host只在已释放的独占目录创建所需文件。
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

/// 动作前绑定真实 Session 代次与精确关闭方向；同步完成回调也只能消费这一次观察。
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
            return Err("原插件激活代次已经改变，不能关闭后继会话".to_owned());
        }
        driver.phase = Phase::PluginRelease {
            generation,
            request,
        };
        Ok(())
    })
}

/// 实际 Host 释放之后显示或继续观察原 Launcher；同步呈现通知与此出口共用原去重调度。
fn resume_launcher_after_release() -> Result<(), String> {
    let epoch = DRIVER
        .with_borrow(|slot| {
            slot.as_ref()
                .filter(|driver| driver.phase == Phase::LauncherReady)
                .map(|driver| driver.epoch)
        })
        .ok_or("释放后的 Launcher 观察权限已结束")?;
    if super::ACTIVE_SURFACE.with_borrow(Option::is_none) {
        let mtm = MainThreadMarker::new().ok_or("资源完成需要原主线程")?;
        super::show_launcher_surface(mtm);
    }
    queue_launcher_readiness_check(epoch);
    Ok(())
}

/// 正常返回前先绑定本次真实会话代次；调用插件返回动作，不用退出整个应用代替关闭。
fn request_return() -> Result<(), String> {
    let editing_verified = DRIVER.with_borrow(|slot| {
        slot.as_ref()
            .is_some_and(|driver| driver.workload_verified && driver.first_failure.is_none())
    });
    if !editing_verified {
        // 只有本轮原文恢复和真实保存已核完才允许正常返回；失败清理另走原桥，始终保留失败结果。
        return Err("本轮编辑和正式保存尚未完成验证，不能记录成功返回".to_owned());
    }
    begin_plugin_close_observation(PluginCloseIntent::ExitToLauncher)?;
    emit("plugin_return_requested");
    if table_case().is_some() {
        table()?.request_return(false)
    } else {
        hosted()?.request_return()
    }
}

/// 一个原事件只执行一个动作；释放阶段由生产回调交付的实际回执推进，不在动作返回时推定完成。
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
            // 原事件排队期间用户可关闭/重新打开插件；相同静态命令或路径不能接管新实例的输入。
            return Err("原插件会话在验收事件交付前已被关闭或替换".to_owned());
        }
    }
    emit(&format!("stage_begin stage={}", stage.code()));
    match (stage, phase) {
        (Stage::LauncherReady, Phase::LauncherReady) => {
            if verify_launcher_ready()? == StageOutcome::Pending {
                // 尚未就绪就保留原图等待；只有属于原身份的新通知能排后继观察，不能先关闭或计成功。
                return Ok(StageOutcome::Pending);
            }
            emit("launcher_ready visible=true input_ready=true sdk_session_owned=false");
            let final_close = DRIVER.with_borrow(|slot| {
                slot.as_ref().is_some_and(|driver| {
                    (driver.completed_documents == DOCUMENT_FLOWS.len()
                        && driver.verified_table_cases == TABLE_CASES.len()
                        && driver.verified_resource_cases == RESOURCE_CASES.len())
                        || driver.first_failure.is_some()
                })
            });
            // 每次返回都重新核验真实Launcher就绪，再决定开始下一轮或结束最终前台。
            close_launcher(final_close)?;
        }
        (Stage::ActivatePlugin, Phase::ActivatePlugin) => {
            if !foreground_is_empty() {
                return Err("原 Launcher 未完全释放，禁止插件激活".to_owned());
            }
            if resource_case().is_some() {
                prepare_resource_fixture()?;
            } else if let Some(case) = table_case() {
                prepare_table_fixture(case)?;
            } else if hosted_flow().is_some() {
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
            // 从正式静态目录取得命令并激活；验收不靠插件ID或页面类型另造专用激活分支。
            super::plugin_host::activate(registration, command.id)
                .map_err(|error| error.to_string())?;
            let target = super::FOREGROUND_STATE
                .with_borrow(|state| {
                    if let Some(ForegroundSurface::Plugin(target)) = state.active_surface() {
                        Some(target)
                    } else {
                        None // 激活已结束或前台类型错误都不代表Markdown已经挂载。
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
            if let Some(case) = resource_case() {
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
            let bridge = NativeMarkdownHostedWorkload::observe_active_document(
                &path,
                hosted_flow().ok_or("原长文流程缺失")?,
            )?;
            DRIVER.with_borrow_mut(|slot| {
                if let Some(driver) = slot.as_mut() {
                    driver.hosted = Some(Rc::new(bridge));
                    driver.phase = Phase::Step(0);
                }
            });
            // 激活与创建观察桥分属两个真实事件，借当前正式会话观察，不额外构造插件或替代会话。
            schedule_hosted_step(0)?;
        }
        (Stage::Step(index), Phase::Step(expected)) if index == expected => {
            let step = workload_step(index).ok_or("工作负载步骤越界")?;
            let steps = DRIVER
                .with_borrow(|slot| slot.as_ref()?.current_step_count())
                .ok_or("原工作负载已结束")?;
            let closes_surface = step.closes_surface();
            if closes_surface {
                // 四种特殊流程的最后一步在原同步栈发返回；必须在调用前固定真实Host关闭权限。
                begin_plugin_close_observation(PluginCloseIntent::ExitToLauncher)?;
            }
            match step {
                WorkloadStep::Document(step) => hosted()?.step(step)?,
                WorkloadStep::Table(step) => table()?.step(step)?,
            }
            if index + 1 == steps {
                DRIVER.with_borrow_mut(|slot| {
                    slot.as_mut()
                        .ok_or("原文稿验证driver缺失")?
                        .accept_verified_workload(index, closes_surface)
                })?;
                emit("document_flow_verified full_steps=true original_data_checks=true");
                if !closes_surface {
                    // Full在真实恢复保存后另发返回，不能把生命周期收据当编辑成功证据。
                    enqueue(Stage::Return)?;
                }
            } else {
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
            request_return()?
        }
        (Stage::ResourceConfigure, Phase::ResourceConfigure) => {
            // configure 只在退还原 Surface 租约后等待真实 CPU/活动格；没有测试版宿主端口。
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
                // 该原场景在真实侧栏交接动作的同一事务内关闭，必须先绑定精确收据身份。
                begin_plugin_close_observation(PluginCloseIntent::DismissAndResume)?;
            }
            let probe = resource()?;
            probe.input()?;
            if probe.input_started_close() {
                if case != NativeMarkdownHostedResourceCase::PendingNoteClose {
                    return Err("资源输入没有本次精确关闭观察权限".to_owned());
                }
            } else if probe.more_input_events() {
                enqueue(Stage::ResourceInput)?;
            } else {
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
                PluginCloseIntent::ExitToLauncher
            } else {
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
                })
            });
            if cleanup_without_bridge {
                if super::plugin_host::exists()
                    || super::plugin_host::product_probe_generation().is_some()
                {
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
                // 原首帧/身份失败仍是失败；只确认真实 Host 回执，绝不补造 MD 弱证据或场景成功。
                emit("cleanup_without_observation_bridge host_released=true acceptance=false");
                resume_launcher_after_release()?;
                return Ok(StageOutcome::Completed);
            }
            if resource_case().is_some() {
                let cleanup_only = DRIVER.with_borrow(|slot| {
                    slot.as_ref()
                        .is_some_and(|driver| driver.first_failure.is_some())
                });
                let verified = resource()?.verify_closed();
                if !cleanup_only {
                    verified?;
                } else if verified.is_err() {
                    // 原业务首错仍保留；真实 Host 已收讫释放，这里不能因第二次失败遗失最终清理方向。
                    emit("resource_cleanup_verified=false acceptance=false");
                }
                if super::plugin_host::exists()
                    || super::plugin_host::product_probe_generation().is_some()
                {
                    return Err("资源原 Native 已释放但真实 SDK Session 仍被持有".to_owned());
                }
                let old = DRIVER.with_borrow_mut(|slot| {
                    let driver = slot.as_mut()?;
                    if !cleanup_only {
                        driver.verified_resource_cases += 1;
                    } // 错误后的清理不增加业务成功数量，即使 Native 图已经实际释放。
                    driver.target = None;
                    driver.session_generation = None;
                    driver.phase = Phase::LauncherReady;
                    driver.resource.take()
                });
                drop(old); // 弱观察桥同样在 TLS 借用外归还，不把九轮证据常驻为业务状态。
                emit(&format!(
                    "resource_case_closed sdk_session_dropped=true host_barrier_consumed=true workload_passed={}",
                    !cleanup_only,
                ));
                resume_launcher_after_release()?;
                return Ok(StageOutcome::Completed);
            }
            if table_case().is_some() {
                let cleanup_only = DRIVER.with_borrow(|slot| {
                    slot.as_ref()
                        .is_some_and(|driver| driver.first_failure.is_some())
                });
                let verified = table()?.verify_native_closed();
                if !cleanup_only {
                    verified?;
                } else if verified.is_err() {
                    emit("table_cleanup_verified=false acceptance=false");
                }
                if super::plugin_host::exists()
                    || super::plugin_host::product_probe_generation().is_some()
                {
                    return Err("表格Native已释放但Host仍持原SDK会话".to_owned());
                }
                let old = DRIVER.with_borrow_mut(|slot| {
                    let driver = slot.as_mut()?;
                    if !cleanup_only && driver.workload_verified {
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
                verified?;
            } else if verified.is_err() {
                // 首错保持失败，真实Host已释放后仍须归还观察桥及返回的Launcher。
                emit("document_cleanup_verified=false acceptance=false");
            }
            if super::plugin_host::exists()
                || super::plugin_host::product_probe_generation().is_some()
            {
                return Err("Native弱证据完成但Host仍持SDK会话".to_owned());
            }
            let old_bridge = DRIVER.with_borrow_mut(|slot| {
                let driver = slot.as_mut()?;
                if !cleanup_only && driver.workload_verified {
                    driver.completed_documents += 1;
                }
                driver.workload_verified = false;
                driver.target = None;
                driver.session_generation = None;
                driver.phase = Phase::LauncherReady;
                driver.hosted.take()
            });
            drop(old_bridge); // 只保标量结果，下一轮不持先前文稿、Native图或动作观察。
            emit(&format!(
                "document_flow_closed native_closed=true sdk_session_dropped=true host_barrier_consumed=true workload_passed={}",
                !cleanup_only,
            ));
            resume_launcher_after_release()?;
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

/// 正式应用启动回调只启动已安装的唯一验收driver，不另外建立产品启动路径。
pub(super) fn application_did_finish_launching(mtm: MainThreadMarker) {
    let start = DRIVER.with_borrow_mut(|slot| {
        let Some(driver) = slot.as_mut() else {
            return false;
        };
        if driver.phase != Phase::WaitingLaunch {
            return false; // 重复启动通知不重开样本或另建driver。
        }
        driver.phase = Phase::LauncherReady;
        true
    });
    if start {
        if !super::shortcuts::product_probe_registration_active() {
            // 热键冲突或配置失败必须保留为失败，不能换备用键掩盖真实产品注册和驻留成本。
            fail("真实产品启动未取得唯一已配置热键".to_owned());
            return;
        }
        emit(
            "initialized product_scope=host_launcher_catalog_sdk_markdown physical_input=false present=false editing_verified=false memory_budget_pass=unmeasured",
        );
        super::show_launcher_surface(mtm);
    }
}

/// 呈现回调只固定原图观察身份；show栈的临时Native所有权尚未归还时，不据此推定就绪或关闭。
pub(super) fn launcher_presented(identity: LauncherProbeIdentity) {
    let claim = DRIVER.with_borrow(|slot| {
        let driver = slot.as_ref()?;
        if !matches!(
            driver.phase,
            Phase::LauncherReady | Phase::VerifyPluginRelease
        ) {
            // 插件其它阶段或停止后的show没有创建新验收等待的权限。
            return None;
        }
        Some((driver.epoch, driver.launcher_readiness.is_some()))
    });
    let Some((epoch, already_captured)) = claim else {
        return;
    };
    if !already_captured {
        // 原show返回时只绑定一次身份锚点；后继通知、重复呈现及getter都不能替换这份原图证据。
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
                // Native弱加载已经返回；借用内只移交弱元数据，不调用可能重入的Native操作。
                driver.launcher_readiness = Some(wait);
            }
        });
    }
    queue_launcher_readiness_check(epoch);
}

/// 只复制当前原等待的Rc；该对象仅保弱证据，不增加Native、正文或SDK会话所有权。
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

/// 同一排队观察合并真实通知；Pending没有新通知时不调用此入口自发重试。
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
        // 后继事件创建失败就固定失败，不能遗留已经登记却永远无法交付的观察权限。
        fail(error);
    }
}

/// 应用激活通知只授权观察一次；非激活式面板仍须满足正式就绪条件，不能仅凭通知名称通过。
pub(super) fn application_did_become_active(notification: &NSNotification, mtm: MainThreadMarker) {
    let Some(wait) = launcher_readiness_wait() else {
        return; // 原show身份尚未固定，提前通知不能凭空建立观察权限。
    };
    let Some(object) = notification.object() else {
        return; // 缺少发送对象的通知不能证明当前应用真正被激活。
    };
    let app = NSApplication::sharedApplication(mtm);
    let expected: &objc2::runtime::AnyObject = &app;
    if std::ptr::eq(expected, &*object) {
        // getter和通知临时持有只留在本回调栈内；队列中只放一个有限标量后继事件。
        queue_launcher_readiness_check(wait.epoch);
    }
}

/// key通知必须指向本轮原面板；其它窗口的通知不能推进当前验收。
pub(super) fn launcher_did_become_key(notification: &NSNotification) {
    if let Some(wait) = launcher_readiness_wait()
        && wait.matches_panel_notification(notification)
    {
        queue_launcher_readiness_check(wait.epoch);
    }
}

/// 实际开始编辑通知补足key通知早于field editor安装的合法顺序，仍只观察原输入对象。
pub(super) fn launcher_did_begin_editing(notification: &NSNotification) {
    if let Some(wait) = launcher_readiness_wait()
        && wait.matches_input_notification(notification)
    {
        queue_launcher_readiness_check(wait.epoch);
    }
}

/// 只接纳原Launcher关闭屏障的精确代次，再独立核验该图真实弱证据，不用收据代替资源归还。
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
            _ => None, // 其它验收的关闭或上一轮迟到回执都不能授权激活本轮插件。
        }
    });
    let Some((release, final_close, failed, epoch)) = observation else {
        return;
    };
    let released = autoreleasepool(|_| release.is_some_and(|release| release.is_released()));
    if !released || !foreground_is_empty() {
        fail("Launcher 原图弱证据与真实 Host 释放收据不一致".to_owned());
        // 原唯一关闭回执已经到达；证据不符必须结束失败，不能再等不存在的第二份回执。
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
            None // driver已经不存在，没有可转移的原图弱元数据。
        }
    });
    drop(old_release); // 最后一份Objective-C弱引用也在DRIVER借用外归还，避免析构重入仍被借用的状态。
    emit("launcher_released native_weak_empty=true host_barrier_consumed=true");
    if failed {
        // 失败后的正常清理仍是失败；无需再延长成功场景的驻留采样窗口。
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
        // 一次性时长只延长资源已释放后的驻留采样，不轮询或重试任何释放条件。
        if let Err(error) = scheduled {
            fail(error);
        }
    } else if let Err(error) = enqueue(Stage::ActivatePlugin) {
        fail(error);
    }
}

/// 匹配的finish_close、SDK会话Drop及Host屏障消费完成后，才记录本次真实前台返回回执。
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
            return None;
        };
        if generation != expected_generation || driver.target != Some(target) {
            return None; // 旧代次、其它目标或无关会话的回执都不能推进当前轮次。
        }
        if request != expected {
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

/// 保留首个失败，沿原插件返回与Launcher关闭路径清理，不以退出应用替代插件自身释放。
fn fail(error: String) {
    let first = DRIVER.with_borrow_mut(|slot| {
        let driver = slot.as_mut()?;
        if driver.phase == Phase::Finished || driver.first_failure.is_some() {
            return None; // 清理重入不能重复发起关闭，也不能覆盖已经保存的首个失败原因。
        }
        driver.first_failure = Some(error);
        driver.pending = None;
        driver.step_terminal = None; // 清理可能取消原请求；先撤销旧核验的排队权限，防止取消回调继续推进成功流程。
        Some((
            driver.phase,
            driver.hosted.as_ref().map(Rc::clone),
            driver.target,
            driver.resource.as_ref().map(Rc::clone),
            driver.table.as_ref().map(Rc::clone),
        ))
    });
    let Some((phase, bridge, expected_target, resource_bridge, table_bridge)) = first else {
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
            _ => None, // 上方已限定关闭阶段；其它类型不取得额外关闭权限。
        };
        if !matches_accepted_close(phase, expected_target, accepted) {
            // 关闭或保存预检被拒绝就不会产生后继释放回执；直接保留失败，不重放关闭或伪报释放。
            stop_loop();
        }
        // 仅为真实接纳且身份匹配的原关闭等待回执；缺失信号仍由父进程期限判失败。
        return;
    }
    if let Some(probe) = resource_bridge {
        let intent = if resource_case() == Some(NativeMarkdownHostedResourceCase::UnsubmittedSearch)
        {
            PluginCloseIntent::ExitToLauncher
        } else {
            PluginCloseIntent::DismissAndResume
        };
        if begin_plugin_close_observation(intent).is_ok() && probe.request_close().is_ok() {
            return; // 清理只等待真正被本次宿主接纳的原关闭，不能把低内存或清理成功当业务通过。
        }
    } else if let Some(probe) = table_bridge {
        if begin_plugin_close_observation(PluginCloseIntent::ExitToLauncher).is_ok()
            && probe.request_return(true).is_ok()
        {
            return; // 只清理本次原表格会话，成功计数不随清理增加。
        }
    } else if let Some(bridge) = bridge {
        let requested = begin_plugin_close_observation(PluginCloseIntent::ExitToLauncher)
            .and_then(|_| bridge.request_return());
        if requested.is_ok() {
            return; // 原释放回调到达后还须核验Native弱证据，随后才能关闭返回的Launcher。
        }
    } else if expected_target.is_some() {
        // 首帧/路径核验可能在桥创建前失败。原激活代次仍相同才允许直接由生产 Host 结束会话；
        // 此分支没有 MD 弱票，后继只作失败清理，不能靠进程退出或新造观察桥补成功。
        if begin_plugin_close_observation(PluginCloseIntent::DismissAndResume).is_err() {
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
            return; // 请求同步或异步接纳都只等原真实 lifecycle，不重试失败保存。
        }
    }
    if super::ACTIVE_SURFACE.with_borrow(Option::is_some) && close_launcher(true).is_ok() {
        return; // 清理同样等待真实前台关闭回执；close返回不等于资源已经释放。
    }
    // 原桥缺失或关闭被拒绝就停止并保留失败，不清空生产owner来伪造资源释放。
    stop_loop();
}

/// stop之后只投递唤醒事件使nextEvent返回，不合成输入，也不借菜单Quit改变产品关闭语义。
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

/// 在父进程提供的新空目录内执行三轮完整长文、巨段落和四种源模式，以及独立资源场景，随后采样5秒空前台驻留。
/// 调用方明确提供已经核验的现用快捷键；只写合成配置，不选备用键或修改用户设置。
///
/// 每轮编辑成功须核完整Unicode替换、撤销重做、原文恢复及真实保存；资源场景另核实际释放。
/// 本入口不证明物理输入、最终呈现或整产品内存达标；进程归属、采样和退出仍由父进程独立核验。
/// 文件创建与删除仅限该新目录中本入口拥有的合成样本，不触碰用户文稿。
///
/// # Errors
///
/// 非主线程、已有前台、目录非空及Native、步骤、保存、释放错误均明确失败，清理不抹掉首错。
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
        // 只初始化明确指定的新空目录；已有内容不属于本入口，不能删除或改写以满足验收前提。
        return Err("整产品验收目录必须是绝对路径的新空目录".to_owned());
    }
    let _pasteboard = super::acceptance_fixture::install_fixture(root)?;
    let fixture = FixtureScope;
    // 隔离样本没有用户配置，默认键可能冲突；复制调用方已核验的现用绑定，由正式启动流程注册一次。
    ShortcutSettingsStore::new(root.join("application-support"))
        .persist(configured_shortcut)
        .map_err(|_| "无法为合成验收持久化明确的既有快捷键".to_owned())?;
    // 只移除夹具刚创建的两个输入样本；随后保唯一长笔记，使选择不依赖目录枚举顺序。
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
    // SAFETY: 按NSApplication公开返回ABI接收单例消息；已存在的其它应用对象也满足该ABI及+1持有合同。
    // 随后独立动态转换核验实际事件路由类型，不把父类对象假定成本入口的子类。
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
        });
    });
    emit(
        "configured repeat_cycles_planned=3 document_flows_planned=8 full_steps_per_flow=45 table_cases_planned=8 resource_cases_planned=9 input=programmatic_native_actions editing_verified=false memory_budget_pass=unmeasured",
    );
    let result = super::run_application();
    // 正式run_application在停止时注销热键；仍独立核验实际空前台和注册已归还，不从代码路径推定释放。
    let released = foreground_is_empty() && super::shortcuts::product_probe_registration_released();
    drop(fixture);
    let state = DRIVER.take().ok_or("整产品 driver 提前丢失")?;
    result?;
    if let Some(error) = state.first_failure {
        return Err(error); // 清理成功不能抹掉此前渲染或Find步骤的真实失败。
    }
    if state.phase != Phase::Finished
        || state.completed_documents != DOCUMENT_FLOWS.len()
        || state.verified_editing_flows != CYCLES + 1
        || state.verified_table_cases != TABLE_CASES.len()
        || state.verified_resource_cases != RESOURCE_CASES.len()
        || !released
        || state.hosted.is_some()
        || state.resource.is_some()
        || state.table.is_some()
        || state.step_terminal.is_some()
    {
        return Err("整产品循环缺失终点、真实前台释放或热键注销证据".to_owned());
    }
    println!(
        "product_acceptance event=finished repeat_cycles=3 document_flows=8 full_editing_flows=4 table_cases=8 resource_cases=9 foreground_empty=true sdk_session_owned=false global_key_owner_released=true fixture_cleared=true editing_verified=true memory_budget_pass=unmeasured"
    );
    Ok(())
}
