//! Main-thread application composition, foreground handoff and native entry points.
//!
//! Controls, installed-application lookup, shortcut ownership and plugin/built-in adapters
//! have separate modules. This owner serializes their effects through one foreground state.

#[cfg(feature = "native-acceptance")]
pub(crate) mod acceptance_fixture;
#[cfg(feature = "native-input-trace")]
mod acceptance_input_trace;
#[cfg(feature = "native-product-acceptance")]
pub(crate) mod acceptance_product;
#[cfg(feature = "native-acceptance")]
pub(crate) mod acceptance_termination;
mod application_delegate;
mod application_menu;
pub(crate) mod applications;
mod builtin_host;
mod builtins;
mod dismissal_shortcut;
mod launcher_close;
mod launcher_results;
mod launcher_ui;
mod native_deallocation;
mod native_editor;
mod native_payload;
mod plugin_host;
mod runtime_probes;
mod shortcuts;
mod termination;

use launcher_results::*;
use launcher_ui::NativeLauncherSurface;

pub use runtime_probes::run_native_builtins_runtime_probe;
#[cfg(feature = "native-acceptance")]
pub use runtime_probes::{LauncherRuntimeProbeResult, run_native_launcher_runtime_probe};

use std::cell::RefCell;
use std::path::PathBuf;

use dispatch2::DispatchQueue;
use objc2::rc::autoreleasepool;
use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy};
use objc2_foundation::{
    MainThreadMarker, NSSearchPathDirectory, NSSearchPathDomainMask,
    NSSearchPathForDirectoriesInDomains,
};
use omni_plugin_sdk::PluginCloseIntent;

use crate::installed_application_catalog::{ApplicationLaunchError, launch_application_bundle};
use crate::launcher_catalog::{
    ActivationSink, LauncherAction, LauncherActionConversionError, dispatch_launcher_action,
};
use crate::{ForegroundSurface, ForegroundSurfaceState, ResumeTarget, SurfaceCloseIntent};

const LAUNCHER_WIDTH: f64 = 760.0;
const LAUNCHER_HEIGHT: f64 = 495.0;
const MAX_VISIBLE_LAUNCHER_RESULTS: usize = 6;
const MACOS_ESCAPE_KEY_CODE: u16 = 53;
const MACOS_RETURN_KEY_CODE: u16 = 36;
const MACOS_KEYPAD_ENTER_KEY_CODE: u16 = 76;
const MACOS_UP_ARROW_KEY_CODE: u16 = 126;
const MACOS_DOWN_ARROW_KEY_CODE: u16 = 125;
const QUIT_ACTIVATION_CODE: &str = "quit";
const OMNI_BUNDLE_IDENTIFIER: &str = "com.omnibar.desktop";

thread_local! {
    static ACTIVE_SURFACE: RefCell<Option<NativeLauncherSurface>> = const { RefCell::new(None) };
    // Native close is a request: retain the graph until its deferred will-close finalizer.
    static CLOSING_SURFACE: RefCell<Option<NativeLauncherSurface>> = const { RefCell::new(None) };
    static FOREGROUND_STATE: RefCell<ForegroundSurfaceState> =
        RefCell::new(ForegroundSurfaceState::new());
}

/// Lazily resolved paths; merely computing these locations opens no document or persistent file.
#[derive(Clone)]
struct NativePluginStorageLocations {
    workspace_root: PathBuf,
    application_support: PathBuf,
}

/// Resolve public user-domain directories without pre-creating a plugin workspace at idle.
fn native_plugin_storage_locations() -> Result<NativePluginStorageLocations, String> {
    #[cfg(feature = "native-acceptance")]
    if let Some(locations) = acceptance_fixture::storage_locations() {
        // Only the opt-in diagnostic executable can install these caller-owned synthetic authorities.
        return Ok(locations);
    }
    // Fail before plugin activation if either OS-provided base directory is unavailable.
    let documents = resolve_user_domain_directory(
        NSSearchPathDirectory::DocumentDirectory,
        "用户文稿目录不可用",
    )?;
    let application_support = resolve_user_domain_directory(
        NSSearchPathDirectory::ApplicationSupportDirectory,
        "用户应用支持目录不可用",
    )?;
    Ok(NativePluginStorageLocations {
        workspace_root: documents.join("OMNI"),
        application_support: application_support.join(OMNI_BUNDLE_IDENTIFIER),
    })
}

/// Resolve the platform pasteboard at the explicit capability boundary, without retaining its body.
fn application_pasteboard() -> objc2::rc::Retained<objc2_app_kit::NSPasteboard> {
    #[cfg(feature = "native-acceptance")]
    if let Some(name) = acceptance_fixture::pasteboard_name() {
        // Synthetic acceptance never reads personal history through Host capability adapters.
        return objc2_app_kit::NSPasteboard::pasteboardWithName(
            &objc2_foundation::NSString::from_str(&name),
        );
    }
    // Normal product calls use the public system pasteboard only in response to foreground work.
    objc2_app_kit::NSPasteboard::generalPasteboard()
}

/// Decode an OS-provided path once; never silently substitute a process working directory.
fn resolve_user_domain_directory(
    directory: NSSearchPathDirectory,
    unavailable_message: &'static str,
) -> Result<PathBuf, String> {
    let paths = NSSearchPathForDirectoriesInDomains(
        directory,
        NSSearchPathDomainMask::UserDomainMask,
        true,
    );
    let path = paths
        .firstObject()
        .ok_or_else(|| unavailable_message.to_owned())?;
    let path = PathBuf::from(path.to_string());
    if path.as_os_str().is_empty() {
        // An empty path would accidentally redirect persistence into the application's working directory.
        return Err(unavailable_message.to_owned());
    }
    Ok(path)
}

/// Snapshot the callback target before any action can reenter the global surface owner.
fn activate_active_launcher_query() {
    let controller = ACTIVE_SURFACE
        .with_borrow(|surface| surface.as_ref().map(|surface| surface.controller.clone()));
    if let Some(controller) = controller {
        // A live Launcher controller accepts one successor without a TLS borrow held across native work.
        controller.enqueue_selected_action();
    }
}

/// Retain the live controller for this synchronous event, ending global borrowing before setters.
fn move_active_launcher_selection(direction: LauncherSelectionDirection) {
    let controller = ACTIVE_SURFACE
        .with_borrow(|surface| surface.as_ref().map(|surface| surface.controller.clone()));
    if let Some(controller) = controller {
        // An event after close sees no active owner and cannot revive the previous control graph.
        controller.move_selection(direction);
    }
}

/// Dismiss through the same once-only ownership path as a result activation.
fn request_active_launcher_dismissal() {
    request_launcher_close();
}

/// Move active ownership to closing before requesting any reentrant native teardown effects.
fn request_launcher_close() {
    if let Err(error) = try_request_launcher_close() {
        // Ordinary actions keep the usable owner on capture/admission failure and report its bounded cause.
        eprintln!("Launcher close was rejected: {error}");
    }
}

/// Preserve the graph on fallible weak inventory capture before any responder/control teardown.
fn try_request_launcher_close() -> Result<(), String> {
    if CLOSING_SURFACE.with_borrow(|surface| surface.is_some()) {
        // A close is already accepted; repeated keyboard/button requests must coalesce.
        return Ok(());
    }
    ACTIVE_SURFACE.with_borrow(|surface| {
        // Read Rust scalars only; Busy rejects before logical close or focus changes, so no focus rollback is needed.
        surface.as_ref().map_or(Ok(()), |surface| {
            surface.controller.prepare_history_for_close()
        })
    })?;
    let Some(surface) = ACTIVE_SURFACE.take() else {
        // Stale events after ownership transfer have no graph left to close.
        return Ok(());
    };

    let state_result = FOREGROUND_STATE.with(|state| {
        state.borrow().begin_destruction(
            ForegroundSurface::Launcher,
            SurfaceCloseIntent::DismissAndResume,
        )
    });
    if state_result.is_err() {
        // Preserve the usable graph if the application state rejects this close claim.
        ACTIVE_SURFACE.with_borrow_mut(|active| active.replace(surface));
        return Err("Launcher foreground close admission failed".into());
    }

    launcher_close::begin_close();
    let panel = surface.panel.clone();
    // Publish the closing owner before prepare-to-close can reenter text-system callbacks.
    let controller = surface.controller.clone();
    let input = surface.input.clone();
    CLOSING_SURFACE.with_borrow_mut(|closing| closing.replace(surface));
    let presentation = launcher_ui::LauncherPresentation::from_handles(&panel, &input, &controller);
    let observation =
        launcher_close::capture_preflight(|| presentation.prepare_close_release_probe());
    let observation = match observation {
        Ok(observation) => observation,
        Err(error) => {
            // No control callback was detached. Cancel this exact logical close and restore the same usable owner.
            let cancelled = FOREGROUND_STATE
                .with_borrow(|state| state.cancel_destruction(ForegroundSurface::Launcher));
            if cancelled.is_ok() {
                // The Closing owner was already published, so no temporary/native getter ever saw an unowned graph.
                let surface = CLOSING_SURFACE.take();
                ACTIVE_SURFACE.with_borrow_mut(|active| *active = surface);
                if let Err(focus) = presentation.restore_after_close_refusal() {
                    // Native focus restoration runs outside owner/state borrows and retains its concrete failure cause.
                    return Err(format!("{error}; {focus}"));
                }
            }
            return Err(error);
        }
    };
    CLOSING_SURFACE.with_borrow_mut(|closing| {
        // Publishing weak metadata invokes no native call and retains none of the captured payload.
        closing
            .as_mut()
            .expect("accepted Launcher close keeps its owner")
            .close_observation = Some(observation);
    });
    drop(presentation);
    // Composition/responder/capture preflight completed outside owner borrows; close now delivers the real finalizer.
    panel.close();
    Ok(())
}

/// Release native edges outside the close callback stack, then authorize one pending successor.
fn finalize_launcher_close() {
    // The actual deallocation cohorts own barrier publication after this native close callback has unwound.
    launcher_close::finalize();
}

/// Route owned catalog actions to the OS, generic plugin boundary or application-owned capability.
#[derive(Clone, Copy, Debug, Default)]
struct NativeActivationSink;

/// Preserve the owning boundary's failure category without storing user content in diagnostics.
#[derive(Debug, Eq, PartialEq)]
enum NativeLauncherActivationError {
    PluginActivation(String),
    BuiltInActivation(String),
    ApplicationTermination(String),
    Application(ApplicationLaunchError),
    MainThreadUnavailable,
}

impl std::fmt::Display for NativeLauncherActivationError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            // Plugin failures retain their SDK category and bounded message.
            Self::PluginActivation(message) => write!(formatter, "插件激活失败: {message}"),
            Self::BuiltInActivation(message) => {
                // Native built-in errors contain only bounded product status, never user data.
                write!(formatter, "内建功能无法打开: {message}")
            }
            Self::ApplicationTermination(message) => {
                // Failed run-loop scheduling preserves the live process and exposes its explicit request failure.
                write!(formatter, "无法安排应用退出：{message}")
            }
            // Application launch already supplies a bounded platform error description.
            Self::Application(error) => error.fmt(formatter),
            // Native entry points cannot operate from a non-main thread.
            Self::MainThreadUnavailable => formatter.write_str("AppKit 主线程不可用"),
        }
    }
}

impl std::error::Error for NativeLauncherActivationError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            // Platform launch failures preserve their typed underlying cause.
            Self::Application(error) => Some(error),
            // Other variants already represent the complete bounded boundary failure.
            Self::PluginActivation(_)
            | Self::BuiltInActivation(_)
            | Self::ApplicationTermination(_)
            | Self::MainThreadUnavailable => None,
        }
    }
}

impl ActivationSink for NativeActivationSink {
    type Error = NativeLauncherActivationError;

    fn activate(&self, action: LauncherAction) -> Result<(), Self::Error> {
        if termination::suppress_successors() {
            // Queued catalog actions cannot launch applications or acquire foreground owners during app quit.
            return Ok(());
        }
        match action {
            LauncherAction::Plugin {
                registration,
                command_id,
            } => {
                // The SDK adapter owns activation/session cleanup without concrete plugin branches.
                plugin_host::activate(registration, command_id).map_err(|error| {
                    NativeLauncherActivationError::PluginActivation(error.to_string())
                })
            }
            LauncherAction::BuiltIn { command } => {
                // Translate only application-owned capabilities; plugin dispatch stays generic.
                let kind = builtin_host::from_command(command).ok_or_else(|| {
                    NativeLauncherActivationError::BuiltInActivation("未知内建功能".to_owned())
                })?;
                let mtm = MainThreadMarker::new()
                    .ok_or(NativeLauncherActivationError::MainThreadUnavailable)?;
                builtin_host::activate(kind, mtm)
                    .map_err(NativeLauncherActivationError::BuiltInActivation)
            }
            LauncherAction::Application { bundle_path, .. } => {
                // The validated bundle path goes directly to the platform launch adapter.
                launch_application_bundle(&bundle_path)
                    .map_err(NativeLauncherActivationError::Application)
            }
            LauncherAction::Quit => {
                // Launcher action dispatch may itself run on serial main GCD; leave that stack before modal termination.
                termination::request_application_termination()
                    .map_err(NativeLauncherActivationError::ApplicationTermination)
            }
        }
    }
}

/// Separate a malformed catalog selection from a valid action rejected by its execution boundary.
#[derive(Debug, Eq, PartialEq)]
enum NativeLauncherDispatchError<E> {
    Conversion(LauncherActionConversionError),
    Sink(E),
}

impl<E: std::fmt::Display> std::fmt::Display for NativeLauncherDispatchError<E> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            // Conversion errors mean no native action was attempted.
            Self::Conversion(error) => error.fmt(formatter),
            // Execution failures retain the sink's own diagnostic policy.
            Self::Sink(error) => error.fmt(formatter),
        }
    }
}

/// Convert before dispatch; tests can observe routing without opening applications or native UI.
fn activate_launcher_selection_with_sink<S>(
    selection: NativeLauncherSelection,
    sink: &S,
) -> Result<(), NativeLauncherDispatchError<S::Error>>
where
    S: ActivationSink + ?Sized,
{
    // Reject malformed selections before the sink acquires any resources.
    let action = selection
        .launcher_action()
        .map_err(NativeLauncherDispatchError::Conversion)?;
    dispatch_launcher_action(sink, action).map_err(NativeLauncherDispatchError::Sink)
}

/// Execute the successor only after the Launcher project teardown barrier was consumed.
fn activate_launcher_selection(selection: NativeLauncherSelection) {
    if let Err(error) = activate_launcher_selection_with_sink(selection, &NativeActivationSink) {
        // Log bounded action/system failure categories without content or native object dumps.
        eprintln!("native launcher activation failed: {error}");
    }
}

/// Claim and publish one complete Launcher owner before allowing reentrant native presentation.
fn show_launcher_surface(mtm: MainThreadMarker) {
    if termination::suppress_successors() {
        // An accepted app quit forbids restoration even when an earlier close intent requested Launcher.
        return;
    }
    if CLOSING_SURFACE.with_borrow(|surface| surface.is_some()) {
        // A previous Launcher graph still owns callback targets until its finalizer releases them.
        return;
    }
    if ACTIVE_SURFACE.with_borrow(|surface| surface.is_some()) {
        // Repeated summon cannot create a competing Launcher window.
        return;
    }
    if plugin_host::exists() {
        // An active or closing plugin session owns the foreground slot and its native view.
        return;
    }
    if builtins::is_open() {
        // A built-in closing graph still owns the foreground slot until finalization.
        return;
    }

    let target = match FOREGROUND_STATE
        .with(|state| state.borrow().begin_creation_for(ResumeTarget::Launcher))
    {
        // The state machine accepted this exact Launcher creation claim.
        Ok(target) => target,
        Err(error) => {
            // Report the rejected transition without allocating a native graph.
            eprintln!("native launcher creation transition rejected: {error}");
            return;
        }
    };

    let surface = match NativeLauncherSurface::new(mtm) {
        // All controls are prepared while hidden; ownership is still local until commit.
        Ok(surface) => surface,
        Err(error) => {
            // Failed construction drops its local native claims and cancels only its own state claim.
            let _ = FOREGROUND_STATE.with(|state| state.borrow().cancel_creation(target));
            eprintln!("native launcher creation failed: {error}");
            return;
        }
    };
    if let Err(error) =
        FOREGROUND_STATE.with(|state| state.borrow().complete_creation(ResumeTarget::Launcher))
    {
        // Never present a graph whose matching application ownership could not be committed.
        let _ = FOREGROUND_STATE.with(|state| state.borrow().cancel_creation(target));
        eprintln!("native launcher ownership commit failed: {error}");
        return;
    }
    // Publish the unique owner before presentation can synchronously reenter native callbacks.
    let presentation = surface.presentation();
    ACTIVE_SURFACE.with_borrow_mut(|active| active.replace(surface));
    presentation.show(mtm);
}

/// Dismiss the current surface or recreate the bounded remembered target from a hidden state.
fn toggle_foreground_surface() {
    #[cfg(feature = "native-acceptance")]
    use shortcuts::{ShortcutDeliveryStage as DeliveryStage, record_native_delivery_stage};
    #[cfg(feature = "native-acceptance")]
    // Output requires actual Carbon delivery scope; ordinary probes query no environment and default products omit this code.
    record_native_delivery_stage(DeliveryStage::ToggleEntered);
    if termination::suppress_successors() {
        // Carbon repeats cannot dismiss a second time or reactivate a surface during accepted app termination.
        #[cfg(feature = "native-acceptance")]
        record_native_delivery_stage(DeliveryStage::SuppressedTermination);
        return;
    }
    let Some(mtm) = MainThreadMarker::new() else {
        // Carbon/main-queue delivery is expected; a foreign worker call must perform no AppKit effects.
        #[cfg(feature = "native-acceptance")]
        record_native_delivery_stage(DeliveryStage::RejectedThread);
        return;
    };
    let transition_pending = FOREGROUND_STATE.with(|state| {
        let state = state.borrow();
        state.pending_transition().is_some() || state.destroyed_barrier().is_some()
    });
    if transition_pending {
        // Coalesce shortcut repeats while native teardown or its project barrier is incomplete.
        #[cfg(feature = "native-acceptance")]
        record_native_delivery_stage(DeliveryStage::SuppressedTransition);
        return;
    }

    let active_surface = FOREGROUND_STATE.with(|state| state.borrow().active_surface());
    match active_surface {
        // Launcher dismissal keeps only its descriptor; the entire control graph is released.
        Some(ForegroundSurface::Launcher) => {
            // Diagnostics tag bounded actions only; the existing close entry and barrier still determine outcomes.
            #[cfg(feature = "native-acceptance")]
            record_native_delivery_stage(DeliveryStage::DismissLauncher);
            request_active_launcher_dismissal();
        }
        Some(ForegroundSurface::Plugin(_target)) => {
            // Plugin save validation can reject close; the owning SDK session preserves its state.
            #[cfg(feature = "native-acceptance")]
            record_native_delivery_stage(DeliveryStage::DismissPlugin);
            if let Err(error) = plugin_host::request_close(PluginCloseIntent::DismissAndResume) {
                // A failed save/close cannot be treated as hidden or allow a new surface.
                eprintln!("native plugin shortcut dismissal failed: {error}");
            }
        }
        Some(ForegroundSurface::BuiltIn(_)) => {
            // A shortcut dismisses the session fully while preserving only its resume kind.
            #[cfg(feature = "native-acceptance")]
            record_native_delivery_stage(DeliveryStage::DismissBuiltIn);
            if let Err(error) = builtins::request_close(SurfaceCloseIntent::DismissAndResume) {
                // Failed foreground cleanup keeps its current owner usable.
                eprintln!("native built-in shortcut dismissal failed: {error}");
            }
        }
        None => {
            // Logical emptiness alone is insufficient while any adapter still retains a graph.
            if ACTIVE_SURFACE.with_borrow(|surface| surface.is_some())
                || CLOSING_SURFACE.with_borrow(|surface| surface.is_some())
                || plugin_host::exists()
                || builtins::is_open()
            {
                // Wait for the existing explicit finalizer rather than creating a retry timer/window.
                #[cfg(feature = "native-acceptance")]
                record_native_delivery_stage(DeliveryStage::SuppressedOwnedAdapter);
                return;
            }
            let target = FOREGROUND_STATE.with(|state| state.borrow().resume_target());
            match target {
                // The default or explicit back target recreates a fresh Launcher graph.
                ResumeTarget::Launcher => {
                    // Restore descriptors only, never borrowing the previous native graph.
                    #[cfg(feature = "native-acceptance")]
                    record_native_delivery_stage(DeliveryStage::ResumeLauncher);
                    show_launcher_surface(mtm);
                }
                ResumeTarget::Plugin(target) => {
                    // Generic registration/command identity selects every plugin without a concrete ID branch.
                    #[cfg(feature = "native-acceptance")]
                    record_native_delivery_stage(DeliveryStage::ResumePlugin);
                    if let Err(error) = NativeActivationSink.activate(LauncherAction::plugin(
                        target.registration,
                        target.command_id,
                    )) {
                        // Failed plugin activation already rolls back its own creation claim.
                        eprintln!("native plugin shortcut resume failed: {error}");
                    }
                }
                ResumeTarget::BuiltIn(kind) => {
                    // Recreate a fresh native session rather than retaining hidden controls.
                    #[cfg(feature = "native-acceptance")]
                    record_native_delivery_stage(DeliveryStage::ResumeBuiltIn);
                    if let Err(error) = builtin_host::activate(kind, mtm) {
                        // Failed creation releases its claim so Launcher is a safe recovery target.
                        eprintln!("native built-in resume failed: {error}");
                        show_launcher_surface(mtm);
                    }
                }
            }
        }
    }
}

/// Enter the real resident application's event loop with one app-owned shortcut registration.
pub(crate) fn run_application() -> Result<(), String> {
    run_application_until(None)
}

/// Measure the initialized production resident state rather than an uninitialized parked process.
pub(crate) fn run_benchmark_application(seconds: u64) -> Result<(), String> {
    // The external sampler observes the same initialization and hidden event loop as normal launch.
    run_application_until(Some(seconds))
}

/// Share product initialization so benchmark mode cannot silently omit framework/shortcut costs.
fn run_application_until(benchmark_seconds: Option<u64>) -> Result<(), String> {
    // Reject a worker-thread caller before any AppKit object or Carbon registration is acquired.
    let mtm =
        MainThreadMarker::new().ok_or_else(|| "原生运行时必须在 AppKit 主线程启动".to_owned())?;
    let result = autoreleasepool(|_| {
        // Resolve the real shared application, then install the same accessory policy in both modes.
        let application = NSApplication::sharedApplication(mtm);
        // The app delegate belongs to the resident process, never to a transient business surface.
        let _delegate = application_delegate::ResidentDelegateScope::install(&application, mtm);
        let accessory_policy = NSApplicationActivationPolicy::Accessory;
        if application.activationPolicy() != accessory_policy
            && !application.setActivationPolicy(accessory_policy)
            && application.activationPolicy() != accessory_policy
        {
            // Preserve an explicit initialization failure instead of claiming a resident benchmark.
            return Err("无法设置 accessory 激活策略".to_owned());
        }
        if let Err(error) = shortcuts::install(mtm) {
            if benchmark_seconds.is_some() {
                // Hidden-resident benchmarks require a real active registration; a visible recovery window is a different workload.
                return Err(error);
            }
            // Normal startup preserves settings failure and offers existing settings access, without acquiring another hotkey.
            eprintln!("启动器快捷键未注册：{error}");
            DispatchQueue::main().exec_async(|| {
                if let Some(marker) = MainThreadMarker::new() {
                    // After initialization returns, request the normal Launcher once; its foreground state rejects duplicate windows.
                    show_launcher_surface(marker);
                }
            });
        }
        // Standard Quit and editing selectors share the same resident delegate and focused native responder.
        let _menu = application_menu::ApplicationMenuScope::install(&application, mtm);
        #[cfg(feature = "native-input-trace")]
        {
            // Explicit synthetic entry points retain their weak menu targets; ordinary product builds omit this path.
            acceptance_fixture::install_input_trace_controls(mtm)?;
        }
        if let Some(seconds) = benchmark_seconds {
            // This one-shot shutdown exists only in the explicit benchmark command; product idle
            // owns no periodic timer. A main-queue callback avoids moving AppKit objects across threads.
            println!("benchmark_mode=production_resident_hidden");
            println!("benchmark_seconds={seconds}");
            println!("measurement=external_whole_process_physical_footprint");
            let deadline =
                dispatch2::DispatchTime::try_from(std::time::Duration::from_secs(seconds))
                    .map_err(|_| "Benchmark deadline is out of range".to_owned())?;
            DispatchQueue::main()
                .after(deadline, || {
                    // Return from the deadline's main-dispatch block before AppKit can wait for native release.
                    if let Err(error) = termination::request_application_termination() {
                        // A failed request remains an explicit benchmark failure instead of bypassing save/release policy.
                        eprintln!("benchmark termination could not be scheduled: {error}");
                    }
                })
                .map_err(|_| "Benchmark shutdown could not be scheduled".to_owned())?;
        }
        // AppKit owns event-loop autorelease pools and sleeps waiting for real events at hidden idle.
        application.run();
        // Explicit run-loop stop also releases the Carbon owner; normal terminate uses applicationWillTerminate:.
        shortcuts::uninstall(mtm);
        // Menu/delegate scopes restore prior non-owning process links on stopped-loop and error exits.
        Ok(())
    });
    if result.is_err() {
        // A partial initialization may already own Carbon resources; error exit must release them explicitly too.
        shortcuts::uninstall(mtm);
    }
    result
}

/// Verify minimal native construction and pure transition ordering without using user documents.
pub(crate) fn run_smoke_test() -> Result<(), String> {
    // A standalone main-thread command is required; Rust's threaded unit harness is unsuitable.
    let mtm = MainThreadMarker::new()
        .ok_or_else(|| "原生 smoke test 必须在 AppKit 主线程运行".to_owned())?;
    autoreleasepool(|_| {
        // This intentionally does not install a shortcut or read real clipboard/workspace content.
        let application = NSApplication::sharedApplication(mtm);
        let _ = application.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
        let state = ForegroundSurfaceState::new();
        let target = state.begin_creation().map_err(|error| error.to_string())?;
        if target != crate::ResumeTarget::Launcher {
            // A fresh state must never restore an uninitialized feature or plugin session.
            return Err("初始恢复目标必须是 Launcher".to_owned());
        }
        let surface = NativeLauncherSurface::new(mtm)?;
        if surface.controller.result_count() != 5 {
            // The current static composition must expose all five initial capabilities.
            return Err("Launcher 空查询必须初始化五个内建入口".to_owned());
        }
        // Commit the hidden prepared graph before exercising explicit native edge removal.
        state
            .complete_creation(crate::ResumeTarget::Launcher)
            .map_err(|error| error.to_string())?;
        surface.prepare_to_close()?;
        surface.release_native_ownership();
        drop(surface);
        // Pure state ordering complements graph probes; it does not itself prove native deallocation.
        state
            .begin_destruction(
                ForegroundSurface::Launcher,
                SurfaceCloseIntent::DismissAndResume,
            )
            .map_err(|error| error.to_string())?;
        state
            .complete_destruction(ForegroundSurface::Launcher)
            .map_err(|error| error.to_string())?;
        state
            .consume_destroyed_barrier(ForegroundSurface::Launcher)
            .map_err(|error| error.to_string())?;
        if state.begin_creation().is_err() {
            // Consuming the previous surface's once-only barrier must enable the next creation.
            return Err("Destroyed 屏障消费后应允许下一次创建".to_owned());
        }
        // Describe the limited observation so automation cannot confuse smoke with complete acceptance.
        println!("smoke_test=ok");
        println!("platform=macos-appkit");
        println!("webview_dependency=none");
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::path::PathBuf;

    use super::*;
    use crate::installed_application_catalog::InstalledApplication;
    use crate::launcher_catalog::{LauncherResult, LauncherResultKind};
    use objc2_app_kit::NSEventModifierFlags;

    /// Observe owned dispatch values without starting processes or touching AppKit.
    #[derive(Default)]
    struct RecordingActivationSink {
        actions: RefCell<Vec<LauncherAction>>,
    }

    impl ActivationSink for RecordingActivationSink {
        type Error = ();

        fn activate(&self, action: LauncherAction) -> Result<(), Self::Error> {
            // Test-only recording preserves action order without running the production sink.
            self.actions.borrow_mut().push(action);
            Ok(())
        }
    }

    /// Opening Launcher enumerates static capabilities without filesystem application results.
    #[test]
    fn empty_query_exposes_all_bundled_entries_without_app_scan() {
        // Opening Launcher enumerates static capabilities without filesystem application results.
        let results = resolve_launcher_results(&crate::composition::launcher_catalog(), "");
        assert_eq!(results.len, 5);
        assert!(
            results
                .iter()
                .all(|entry| { matches!(entry, NativeLauncherSelection::Catalog(_)) })
        );
    }

    /// Only the explicit case-insensitive quit command offers process termination.
    #[test]
    fn exact_quit_is_the_only_quit_entry() {
        // Only the explicit case-insensitive quit command offers process termination.
        let quit = resolve_launcher_results(&crate::composition::launcher_catalog(), "QUIT");
        assert_eq!(quit.len, 1);
        assert!(matches!(quit.get(0), Some(NativeLauncherSelection::Quit)));
        assert!(
            resolve_launcher_results(&crate::composition::launcher_catalog(), "quitting")
                .iter()
                .all(|entry| !matches!(entry, NativeLauncherSelection::Quit))
        );
    }

    /// Keyboard navigation wraps visible rows and preserves IME editing authority.
    #[test]
    fn navigation_wraps_and_ignores_composition_or_modifiers() {
        // Keyboard navigation wraps visible rows and preserves IME editing authority.
        assert_eq!(
            move_launcher_selection_index(0, 5, LauncherSelectionDirection::Previous),
            4
        );
        assert_eq!(
            move_launcher_selection_index(4, 5, LauncherSelectionDirection::Next),
            0
        );
        assert_eq!(
            launcher_selection_navigation_direction(
                MACOS_DOWN_ARROW_KEY_CODE,
                NSEventModifierFlags::empty(),
                false,
            ),
            Some(LauncherSelectionDirection::Next)
        );
        assert_eq!(
            launcher_selection_navigation_direction(
                MACOS_UP_ARROW_KEY_CODE,
                NSEventModifierFlags::empty(),
                true,
            ),
            None
        );
    }

    /// Return cannot activate a result while composition or action modifiers are present.
    #[test]
    fn return_requires_unmodified_non_composing_input() {
        // Return cannot activate a result while composition or action modifiers are present.
        assert!(is_launcher_activation_key(
            MACOS_RETURN_KEY_CODE,
            NSEventModifierFlags::empty(),
            false,
        ));
        assert!(!is_launcher_activation_key(
            MACOS_RETURN_KEY_CODE,
            NSEventModifierFlags::Command,
            false,
        ));
        assert!(!is_launcher_activation_key(
            MACOS_RETURN_KEY_CODE,
            NSEventModifierFlags::empty(),
            true,
        ));
    }

    /// The same conversion boundary dispatches every supported category without native side effects.
    #[test]
    fn launcher_selection_dispatches_plugins_built_ins_applications_and_quit() {
        // The same conversion boundary dispatches every supported category without native side effects.
        let sink = RecordingActivationSink::default();
        let application = InstalledApplication::from_search_match(
            "示例应用",
            PathBuf::from("/Applications/示例应用.app"),
            0,
        );
        let selections = vec![
            NativeLauncherSelection::Catalog(
                crate::composition::launcher_catalog().bundled_results()[0],
            ),
            NativeLauncherSelection::Catalog(
                crate::composition::launcher_catalog().bundled_results()[1],
            ),
            NativeLauncherSelection::Catalog(
                crate::composition::launcher_catalog().bundled_results()[2],
            ),
            NativeLauncherSelection::Catalog(
                crate::composition::launcher_catalog().bundled_results()[3],
            ),
            NativeLauncherSelection::Catalog(
                crate::composition::launcher_catalog().bundled_results()[4],
            ),
            NativeLauncherSelection::Application(application),
            NativeLauncherSelection::Quit,
        ];

        for selection in selections {
            // Convert each owned snapshot after the hypothetical Launcher close handoff.
            activate_launcher_selection_with_sink(selection, &sink)
                .expect("recording sink must accept every launcher action");
        }

        // The exact order and static command evidence survive every category conversion.
        assert_eq!(
            sink.actions.into_inner(),
            vec![
                LauncherAction::plugin(crate::plugins::MARKDOWN_REGISTRATION, "open"),
                LauncherAction::built_in("clipboard"),
                LauncherAction::built_in("calc"),
                LauncherAction::built_in("convert"),
                LauncherAction::built_in("settings"),
                LauncherAction::application("示例应用", "/Applications/示例应用.app"),
                LauncherAction::quit(),
            ]
        );
    }

    /// Malformed static application labels cannot substitute for a validated bundle target.
    #[test]
    fn launcher_selection_reports_catalog_conversion_failure() {
        // Malformed static application labels cannot substitute for a validated bundle target.
        let malformed = NativeLauncherSelection::Catalog(LauncherResult {
            kind: LauncherResultKind::InstalledApplication,
            command: "app",
            title: "应用",
            description: "打开应用",
            registration: None,
            plugin_command_id: None,
        });

        let error =
            activate_launcher_selection_with_sink(malformed, &RecordingActivationSink::default())
                .expect_err("an application catalog row lacks a verified bundle path");
        assert_eq!(
            error,
            NativeLauncherDispatchError::Conversion(
                LauncherActionConversionError::ApplicationTargetRequired
            )
        );
    }

    /// Unknown actions fail before resource acquisition or operating-system application launch.
    #[test]
    fn native_sink_rejects_unknown_plugin_and_unknown_builtin_commands() {
        // Unknown actions fail before resource acquisition or operating-system application launch.
        let sink = NativeActivationSink;
        let unknown_plugin = NativeLauncherSelection::Catalog(LauncherResult {
            kind: LauncherResultKind::Plugin,
            command: "unknown-plugin",
            title: "未知插件",
            description: "测试未知插件动作",
            registration: None,
            plugin_command_id: Some("unknown"),
        });
        assert_eq!(
            activate_launcher_selection_with_sink(unknown_plugin, &sink),
            Err(NativeLauncherDispatchError::Conversion(
                LauncherActionConversionError::PluginRegistrationRequired
            ))
        );
        assert_eq!(
            activate_launcher_selection_with_sink(
                NativeLauncherSelection::Catalog(LauncherResult {
                    kind: LauncherResultKind::BuiltIn,
                    command: "unknown",
                    title: "Unknown",
                    description: "Rejected built-in",
                    registration: None,
                    plugin_command_id: None,
                }),
                &sink,
            ),
            Err(NativeLauncherDispatchError::Sink(
                NativeLauncherActivationError::BuiltInActivation("未知内建功能".to_owned())
            ))
        );
        assert_eq!(
            activate_launcher_selection_with_sink(
                NativeLauncherSelection::Application(InstalledApplication::from_search_match(
                    "不存在的应用",
                    "/definitely/not-an-application.app",
                    0,
                )),
                &sink,
            ),
            Err(NativeLauncherDispatchError::Sink(
                NativeLauncherActivationError::Application(
                    ApplicationLaunchError::InvalidApplicationPath
                )
            ))
        );
    }
}
