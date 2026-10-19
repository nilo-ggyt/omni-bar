//! Execute the pinned plugin's shared standalone TextKit lifecycle contract.

/// Own the fixture and native process lifetime without Rust's worker-thread test harness.
fn main() {
    // Only a dedicated real-main-thread process may construct this probe's NSApplication subclass.
    let mtm = objc2_foundation::MainThreadMarker::new().expect("native AppKit main thread");
    let directory = tempfile::tempdir().expect("isolated native fixture directory");
    let document = directory.path().join("probe.md");
    std::fs::write(
        &document,
        omni_native_runtime::native_markdown_runtime_probe_fixture(),
    )
    .expect("write synthetic Unicode/table fixture");
    // SAFETY: no earlier code creates/accesses NSApplication. This dedicated main-thread probe
    // owns its synthetic fixture and never runs inside a product or nested application loop.
    unsafe { omni_native_runtime::run_native_markdown_runtime_probe_application(&document, mtm) }
        .expect("native Markdown event and strict release contract");
}
