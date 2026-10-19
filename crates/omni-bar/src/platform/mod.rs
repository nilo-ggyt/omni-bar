//! Operating-system adapters; portable domain code never imports this layer.

// AppKit 模块只在 macOS 编译；其他目标由 crate 根的明确拒绝入口处理，不装配替身图。
#[cfg(target_os = "macos")]
pub(crate) mod macos;
