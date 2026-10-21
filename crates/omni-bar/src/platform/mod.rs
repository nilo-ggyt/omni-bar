//! Operating-system adapters; portable domain code never imports this layer.

// Compile the real adapter only on macOS; unsupported targets cannot assemble a substitute UI.
#[cfg(target_os = "macos")]
pub(crate) mod macos;
