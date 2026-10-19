//! Standalone harness keeps AppKit on the process main thread and all persistence in a tempdir.

use objc2_foundation::MainThreadMarker;

fn main() {
    // Rust's ordinary threaded test harness cannot supply AppKit's process-main-thread guarantee.
    let mtm = MainThreadMarker::new().expect("native built-ins require the macOS main thread");
    let directory = tempfile::tempdir().expect("create isolated built-in probe directory");
    // 专用入口必须先于普通 NSApplication 构造；真正 run 分派输入、Undo、close 与资源收据。
    // 合成目录/偏好不读写用户剪贴板；应用类不匹配或实际事件缺失都必须失败。
    omni_native_runtime::run_native_builtins_runtime_probe(directory.path(), mtm)
        .expect("native built-in functional, data and resource probe");
    println!("native_builtins_probe=ok");
}
