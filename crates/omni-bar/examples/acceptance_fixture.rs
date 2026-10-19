//! Opt-in actual-product UI entry point using disposable synthetic storage and a private pasteboard.

/// Start one real main-thread product session; initialization errors leave a conventional failure status.
fn main() {
    #[cfg(target_os = "macos")]
    if let Err(error) = omni_native_runtime::run_isolated_acceptance_application() {
        // The adapter releases its partial fixture owner before returning a bounded setup failure.
        eprintln!("native acceptance fixture: {error}");
        std::process::exit(2);
    }
    #[cfg(not(target_os = "macos"))]
    {
        // This executable exercises actual AppKit; a portable build supplies no substitute runtime proof.
        eprintln!("native acceptance fixture requires macOS");
        std::process::exit(2);
    }
}
