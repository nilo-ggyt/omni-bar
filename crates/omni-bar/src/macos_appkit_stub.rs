//! Explicit unsupported-platform rejection; portable compilation does not imply a working native UI.

/// Reject all run modes without constructing substitute windows, shortcuts or platform resources.
pub(crate) fn run_application() -> Result<(), String> {
    // Keep the user-visible platform requirement consistent with the Chinese product interface.
    Err("Omni Bar 需要 macOS 14 或更高版本".to_owned())
}

/// Smoke verification needs real AppKit initialization, which an unsupported platform cannot provide.
pub(crate) fn run_smoke_test() -> Result<(), String> {
    run_application()
}

/// Reject measurement without a real platform workload rather than parking an empty process.
pub(crate) fn run_benchmark_application(_seconds: u64) -> Result<(), String> {
    run_application()
}
