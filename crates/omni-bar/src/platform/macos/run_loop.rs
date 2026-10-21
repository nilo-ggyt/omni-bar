//! Own process initialization and the AppKit run loop, without owning a document surface.
//!
//! Production, explicit measurement and smoke commands share the native adapters. The
//! foreground coordinator in the parent module remains the sole authority for visible sessions.

use dispatch2::DispatchQueue;
use objc2::MainThreadMarker;
use objc2::rc::autoreleasepool;
use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy};

#[cfg(feature = "native-input-trace")]
use super::acceptance_fixture;
use super::launcher_ui::NativeLauncherSurface;
use super::{
    application_delegate, application_menu, shortcuts, show_launcher_surface, termination,
};

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
            return Err("无法设置后台常驻激活模式".to_owned());
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

/// Verify minimal native control construction and teardown without using user documents.
pub(crate) fn run_smoke_test() -> Result<(), String> {
    // A standalone main-thread command is required; Rust's threaded unit harness is unsuitable.
    let mtm = MainThreadMarker::new()
        .ok_or_else(|| "原生 smoke test 必须在 AppKit 主线程运行".to_owned())?;
    autoreleasepool(|_| {
        // This intentionally does not install a shortcut or read real clipboard/workspace content.
        let application = NSApplication::sharedApplication(mtm);
        let _ = application.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
        let surface = NativeLauncherSurface::new(mtm)?;
        // Exercise the actual constructed controls without duplicating the plugin catalog size
        // or building a second foreground state graph solely for smoke verification.
        surface.prepare_to_close()?;
        surface.release_native_ownership();
        drop(surface);
        // Describe the limited observation so automation cannot confuse smoke with complete acceptance.
        println!("smoke_test=ok");
        println!("platform=macos-appkit");
        println!("webview_dependency=none");
        Ok(())
    })
}
