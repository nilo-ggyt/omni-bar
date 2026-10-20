//! Opt-in production-loop termination evidence over a fresh, parent-owned synthetic fixture.
//!
//! The driver owns weak native identities and scalar observations only. Actual save, scope-specific
//! native payload release, SDK-session Drop and the foreground barrier retain their production implementation.

use std::cell::RefCell;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use objc2::rc::{Weak, autoreleasepool};
use objc2::runtime::{AnyObject, ProtocolObject};
use objc2::{MainThreadMarker, msg_send, sel};
use objc2_app_kit::{
    NSApplication, NSApplicationTerminateReply, NSMenuItem, NSTextInputClient, NSWindow,
};
use objc2_foundation::{NSNotFound, NSRange, NSString, ns_string};
use omni_md_plugin::native_surface::paged_text_view::{
    NativeMarkdownPagedTextView, NativeMarkdownPagedTextViewError,
};

use crate::capabilities::shortcut_preferences::{ShortcutBinding, ShortcutSettingsStore};

/// Two explicit synthetic scenarios run in separate processes, each with a fresh NSApplication.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NativeTerminationProbeScenario {
    /// Terminate with the actual Launcher still owning its foreground graph.
    Launcher,
    /// Insert into the actual SDK Markdown responder and immediately terminate before autosave can run.
    MarkdownDirty,
}

/// 显式菜单动作与串行主队列退出各走真实产品入口；均不注册或发送退出快捷键。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NativeTerminationProbeOrigin {
    /// 由原NSMenu公开action入口消费实际退出项，菜单仍拥有原target与selector。
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
    window: Option<Weak<NSWindow>>,
    editor: Option<Weak<NativeMarkdownPagedTextView>>,
    // 只观察本场景实际调用的原菜单项，不保菜单树、target或任何前台强边。
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
    // 四案使用与本轮整产品验收相同、已明确选择的Alt+Space，只写本子进程拥有的隔离配置。
    // 空配置会尝试默认Cmd+Space；系统拒绝后正式恢复Launcher已占前台，不能再假定插件可直接创建。
    // 此处不注册第二个键，也不更改用户偏好；实际注册及系统拒绝后的恢复仍只走原启动入口。
    ShortcutSettingsStore::new(directory.join("application-support"))
        .persist(ShortcutBinding::OptionSpace)
        .map_err(|_| {
            // 尚未发布观察者或启动应用；配置失败先归还本夹具的路径与命名剪贴板权限，保留失败。
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
        Some(NativeTerminationProbeOrigin::MenuAction) => {
            // 显式执行已安装的真实菜单项；没有键盘事件、事件重播或直接coordinator兜底。
            if let Err(error) = perform_quit_menu_action(mtm) {
                fail(error, mtm);
            }
        }
        None => {
            // The configured owner must survive through its one selected entry; a missing observer cannot pass.
            fail("the termination probe origin disappeared".into(), mtm);
        }
    }
}

/// 只执行进程已安装的原退出项；空快捷键与真实action同时通过，不能为了验收另造菜单。
fn perform_quit_menu_action(mtm: MainThreadMarker) -> Result<(), String> {
    let application = NSApplication::sharedApplication(mtm);
    let main = application.mainMenu().ok_or("原应用菜单尚未安装")?;
    if main.numberOfItems() == 0 {
        return Err("原应用菜单没有实际条目".into());
    }
    let menu = main
        .itemAtIndex(0)
        .and_then(|item| item.submenu())
        .ok_or("原应用退出子菜单缺失")?;
    let index = menu.indexOfItemWithTitle(ns_string!("退出 Omni Bar"));
    if index < 0 || index >= menu.numberOfItems() {
        return Err("原应用退出项不存在".into());
    }
    let item = menu.itemAtIndex(index).ok_or("原应用退出项已经移除")?;
    if item.keyEquivalent().length() != 0
        || item.action() != Some(sel!(requestOmniTermination:))
        || item.target().is_none()
    {
        return Err("原退出项仍绑定快捷键或不再拥有正式退出action".into());
    }
    let weak = Weak::from_retained(&item);
    PROBE.with_borrow_mut(|slot| {
        let state = slot.as_mut().ok_or("原菜单退出观察已结束")?;
        if state.origin != NativeTerminationProbeOrigin::MenuAction || state.menu_item.is_some() {
            return Err("原菜单退出动作不能重复接纳".to_owned());
        }
        state.menu_item = Some(weak);
        Ok(())
    })?;
    emit("event=menu_action_requested key_equivalent_empty=true");
    // AppKit同步向这个原菜单项的target发送action；原产品包装只排正式退出，必须先返回。
    // 菜单和item的临时强边在本函数结束归还；它们不持插件、文稿或窗口资源。
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
    let window = window
        .and_then(|window| {
            // A released source cannot be revived; an existing source is retained only inside the draining input pool.
            window.load()
        })
        .ok_or("the Markdown window disappeared before synthetic input")?;
    let editor = window
        .initialFirstResponder()
        .and_then(|responder| {
            // 只接实际构造器挂载的分页客户；不接受字段editor，也不保旧TextKit兼容路径。
            responder.downcast::<NativeMarkdownPagedTextView>().ok()
        })
        .ok_or("原Markdown初始responder不是正式分页正文")?;
    let before = editor.input_state().map_err(|cause| cause.to_string())?;
    if before.marked.is_some()
        || before.primary_range != NSRange::new(0, 0)
        || before.selection_range_count != 1
    {
        return Err("原分页正文不是预声明的空选区输入前态".into());
    }
    if !window.makeFirstResponder(Some(&editor))
        || !window.firstResponder().is_some_and(|actual| {
            let expected: &objc2_app_kit::NSResponder = &editor;
            std::ptr::eq(&*actual, expected)
        })
        || editor.input_state().map_err(|cause| cause.to_string())? != before
    {
        // Native焦点调用可能重入；原完整Input变化后不能给后来客户补签这次插字。
        return Err("原分页正文焦点或完整输入已经变化".into());
    }
    let client = ProtocolObject::<dyn NSTextInputClient>::from_ref(&*editor);
    let requested = NSRange::new(0, "# Synthetic acceptance note".len());
    let mut actual = NSRange::new(NSNotFound as usize, 0);
    // SAFETY: 原公开协议只读固定ASCII前缀，actual为本栈可写range；两夹具此处均非代理对边界。
    let prefix =
        unsafe { client.attributedSubstringForProposedRange_actualRange(requested, &mut actual) }
            .ok_or("原分页客户没有返回有限夹具前缀")?;
    if actual != requested || editor.input_state().map_err(|cause| cause.to_string())? != before {
        return Err("有限夹具前缀并非同一原Input的完整请求范围".into());
    }
    let prefix = prefix.string(); // 只取得上面有限协议结果的NSString，不读取整篇正文。
    let note = if prefix.isEqualToString(ns_string!("# Synthetic acceptance note")) {
        // 两个预声明文件的枚举顺序不固定；只认固定头部，不能接纳任意用户文稿。
        "Acceptance.md"
    } else if prefix.hasPrefix(ns_string!("OmniFrameOldA9\n")) {
        "SourcePerformance.md"
    } else {
        return Err("the active document is not one of the declared synthetic fixtures".into());
    };
    drop(prefix);
    let insertion = NSString::from_str(NATIVE_TERMINATION_PROBE_INSERTION);
    if editor.input_state().map_err(|cause| cause.to_string())? != before {
        return Err("原退出插字在Native载荷构造期间已失去输入权限".into());
    }
    // SAFETY: 同一原分页客户接受NSString及显式0..0替换；只模拟公开输入协议，不冒称真实IME或物理键盘。
    unsafe { client.insertText_replacementRange(&insertion, NSRange::new(0, 0)) };
    let after = editor.input_state().map_err(|cause| cause.to_string())?;
    let inserted_units = NATIVE_TERMINATION_PROBE_INSERTION.len(); // 本预声明前缀严格为ASCII。
    // 原字符事务也撤旧折叠布局票，空折叠集合的 revision 同样可合法推进；
    // 此处验证用户实际插字后态，不把内部布局票不变当作保存前置条件。
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
        // void 协议回调的真实首错只能从原客户取回；这里只保同一次输入的固定元数据，
        // 不重放插字、不等待后继状态冒充本次提交，也不读取正文或改变原失败关闭路径。
        let failure = editor.take_failure();
        return Err(format!(
            "原分页插字未形成准确一次预声明后态 before={before:?} after={after:?} view_failure={failure:?}"
        ));
    }
    let requested = NSRange::new(0, inserted_units);
    let mut actual = NSRange::new(NSNotFound as usize, 0);
    // SAFETY: 仍只读原客户的固定新增ASCII片段，绝不借.string()物化整篇原文。
    let prefix =
        unsafe { client.attributedSubstringForProposedRange_actualRange(requested, &mut actual) }
            .ok_or("原分页插字没有返回有限实际前缀")?;
    if actual != requested
        || !prefix.string().isEqualToString(&insertion)
        || editor.input_state().map_err(|cause| cause.to_string())? != after
    {
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
        // 只保原客户的零化弱身份；所有前缀、Native协议结果与强view均随本次输入池归还。
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

/// 实际action必须收到原NSMenu所持的同一菜单项；没有NSEvent也不会伪造按键来源。
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
        // 两个真实入口从这里起共用同一退出事务，不借后继currentEvent补造原动作来源。
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

/// 独立观察原分页客户是否实际封闭；完整SDK收据另证Doc、Source、字体与worker等全部资源归还。
fn observe_terminated_editor(editor: &Weak<NativeMarkdownPagedTextView>) -> (bool, &'static str) {
    let Some(editor) = editor.load() else {
        // The original weak identity ended; a new or replacement control cannot substitute for this observation.
        return (true, "nil");
    };
    let client = ProtocolObject::<dyn NSTextInputClient>::from_ref(&*editor);
    let selection = client.selectedRange();
    let selection_empty = selection.length == 0
        && (selection.location == 0 || selection.location == NSNotFound as usize);
    // SAFETY: 原弱身份只在本池升级；三个公开NSView/NSResponder getter只比较nil，不能重建正文或输入owner。
    let links_detached = unsafe {
        let window: *mut objc2::runtime::AnyObject = msg_send![&*editor, window];
        let parent: *mut objc2::runtime::AnyObject = msg_send![&*editor, superview];
        let responder: *mut objc2::runtime::AnyObject = msg_send![&*editor, nextResponder];
        window.is_null() && parent.is_null() && responder.is_null()
    };
    // Revoked只说明前台失权，不能冒充实际close；必须是公开Closed错误，且真实CPU拥有槽已归还。
    // inputContext在Closed时只返回None，不创建新上下文；不再借TextKit.string()/layout/undo检查旧图。
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
