//! Native application composition and its portable foreground contracts.
//!
//! Domain/application decisions remain independent of AppKit; only the platform
//! adapter creates native objects. Plugins cross the shared SDK contract.

mod application;
mod capabilities;
mod composition;
mod platform;
mod runtime_command;

// 运行命令只依赖下面选择的三个平台入口；别名本身不创建 AppKit、快捷键或前台会话。
#[cfg(target_os = "macos")]
use platform::macos as macos_appkit;

// 内建验收入口在 macOS 可直接调用，但重新导出不会启动探针或把合成资源放进普通启动。
#[cfg(target_os = "macos")]
pub use platform::macos::run_native_builtins_runtime_probe;

// 这些专用验收 API 仅随显式 feature 编译；正式命令仍由 runtime_command 独立选择。
#[cfg(all(target_os = "macos", feature = "native-acceptance"))]
pub use platform::macos::{LauncherRuntimeProbeResult, run_native_launcher_runtime_probe};

#[cfg(all(target_os = "macos", feature = "native-acceptance"))]
pub use platform::macos::acceptance_fixture::{
    release_isolated_acceptance_pasteboard, run_isolated_acceptance_application,
};
#[cfg(all(target_os = "macos", feature = "native-product-acceptance"))]
pub use platform::macos::acceptance_product::run_native_product_acceptance;
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
// 只转出同一已链接插件的验收函数，不在 Host 复制插件实现、协议或文稿 owner。
pub use omni_md_plugin::{
    native_markdown_runtime_probe_fixture, run_native_markdown_runtime_probe_application,
};

#[cfg(not(target_os = "macos"))]
// 可移植领域测试仍可编译此 crate；实际运行必须明确失败，不能装配模拟原生后端。
mod macos_appkit_stub;

// 对外暴露纯状态与 typed 动作，使调用方不必持有 Native 对象或访问私有平台模块。
pub use application::foreground::{
    BuiltInSurface, ForegroundSurface, ForegroundSurfaceState, ResumeTarget, SurfaceCloseIntent,
    SurfaceTransition,
};
pub use application::launcher::{
    ActivationSink, LauncherAction, LauncherActionConversionError, LauncherCatalog, LauncherResult,
    LauncherResultKind, dispatch_launcher_action,
};
pub use runtime_command::{RuntimeCommandError, run_from_environment};

// 两个平台分支提供相同的内部签名；命令分发不判空、不重建依赖，也不另选备用运行时。
#[cfg(target_os = "macos")]
pub(crate) use macos_appkit::{run_application, run_benchmark_application, run_smoke_test};

#[cfg(not(target_os = "macos"))]
pub(crate) use macos_appkit_stub::{run_application, run_benchmark_application, run_smoke_test};
