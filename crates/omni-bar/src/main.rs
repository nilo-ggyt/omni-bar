//! Thin executable boundary; application composition owns initialization and teardown.

/// Report returned command failures after the platform owner has handled initialization and cleanup.
fn main() {
    if let Err(error) = omni_native_runtime::run_from_environment(std::env::args_os()) {
        // Invalid arguments never entered AppKit; runtime failures have returned through its owner.
        // Process exit does not unwind, so this status proves neither saving nor native release.
        eprintln!("omni-native-runtime: {error}");
        std::process::exit(2);
    }
}
