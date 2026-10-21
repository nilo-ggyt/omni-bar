//! Native application composition and its portable foreground contracts.
//!
//! Domain/application decisions remain independent of AppKit; only the platform
//! adapter creates native objects. Plugins cross the shared SDK contract.

mod application;
mod capabilities;
mod composition;
mod platform;
mod runtime_command;

// Commands select the real platform entries without constructing AppKit or a foreground session here.
#[cfg(target_os = "macos")]
use platform::macos as macos_appkit;

// Synthetic verification APIs require explicit features and remain separate from command selection.
#[cfg(all(target_os = "macos", feature = "native-acceptance"))]
pub use platform::macos::{LauncherRuntimeProbeResult, run_native_launcher_runtime_probe};

#[cfg(all(target_os = "macos", feature = "native-acceptance"))]
pub use platform::macos::acceptance_fixture::{
    release_isolated_acceptance_pasteboard, run_isolated_acceptance_application,
};
#[cfg(all(target_os = "macos", feature = "native-product-acceptance"))]
pub use platform::macos::acceptance_product::{
    run_native_product_acceptance, run_native_product_complete_ui_acceptance,
    run_native_product_core_ui_acceptance, run_native_product_ime_acceptance,
};
#[cfg(all(target_os = "macos", feature = "native-acceptance"))]
pub use platform::macos::acceptance_termination::{
    NATIVE_TERMINATION_PROBE_INSERTION, NativeTerminationProbeOrigin,
    NativeTerminationProbeScenario, run_native_termination_probe_child,
};

use application::launcher as launcher_catalog;
#[cfg(test)]
use composition::plugins;

#[cfg(target_os = "macos")]
use platform::macos::applications as installed_application_catalog;

#[cfg(all(target_os = "macos", feature = "native-markdown-runtime-probe"))]
// Forward the linked plugin's verification entry without copying its implementation or document owner.
pub use omni_md_plugin::{
    native_markdown_runtime_probe_fixture, run_native_markdown_runtime_probe_application,
};

#[cfg(not(target_os = "macos"))]
// Portable contracts remain compilable; running on an unsupported OS fails without a simulated backend.
mod macos_appkit_stub;

// Expose pure decisions and typed actions without lending native objects or private platform modules.
pub use application::foreground::{
    BuiltInSurface, ForegroundSurface, ForegroundSurfaceState, ResumeTarget, SurfaceCloseIntent,
    SurfaceTransition,
};
pub use application::launcher::{
    ActivationSink, LauncherAction, LauncherActionConversionError, LauncherCatalog, LauncherResult,
    LauncherResultKind, dispatch_launcher_action,
};
pub use runtime_command::{RuntimeCommandError, run_from_environment};

// Both platform selections provide the same entry signatures; commands never choose a fallback runtime.
#[cfg(target_os = "macos")]
pub(crate) use macos_appkit::{run_application, run_benchmark_application, run_smoke_test};

#[cfg(not(target_os = "macos"))]
pub(crate) use macos_appkit_stub::{run_application, run_benchmark_application, run_smoke_test};
