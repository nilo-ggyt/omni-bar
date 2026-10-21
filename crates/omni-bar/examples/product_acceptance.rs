//! Run the real Host, Launcher, SDK and Markdown event loop over an isolated synthetic directory.
//! The parent independently measures process memory instead of trusting an in-process budget claim.

/// Acquire only a fresh empty directory created here, never a personal vault or an installed application's state.
fn main() -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        use std::os::unix::fs::PermissionsExt;

        let mut arguments = std::env::args_os().skip(1);
        if arguments.next().as_deref() != Some(std::ffi::OsStr::new("--launcher-shortcut")) {
            // Require the existing explicit launcher binding before any fixture or native owner is acquired.
            return Err(
                "用法：product_acceptance --launcher-shortcut 本机既有启动器快捷键 [--system-ime|--core-ui|--complete-ui]".to_owned(),
            );
        }
        let shortcut = arguments.next().ok_or("缺少既有启动器快捷键")?;
        let shortcut = shortcut.to_str().ok_or("启动器快捷键必须使用有效UTF-8")?;
        // Select an existing production composition entry without copying its workload, constructing
        // another driver or combining memory observations from different processes.
        let run: fn(&std::path::Path, &str) -> Result<(), String> = match arguments
            .next()
            .as_deref()
        {
            // The argument-free workload retains the original full product sequence.
            None => omni_native_runtime::run_native_product_acceptance,
            Some(value) if value == std::ffi::OsStr::new("--system-ime") => {
                // The focused IME sequence still owns its real input and cleanup implementation.
                omni_native_runtime::run_native_product_ime_acceptance
            }
            Some(value) if value == std::ffi::OsStr::new("--complete-ui") => {
                // Complete UI includes the external handoff contract in the same real product process.
                omni_native_runtime::run_native_product_complete_ui_acceptance
            }
            Some(value) if value == std::ffi::OsStr::new("--core-ui") => {
                // Core UI preserves native editing/input/resource flows without acquiring browser resources.
                omni_native_runtime::run_native_product_core_ui_acceptance
            }
            Some(_) => {
                // An unknown workload cannot fall back to another scenario and publish misleading evidence.
                return Err("只接受 --system-ime、--core-ui 或 --complete-ui 验收参数".to_owned());
            }
        };
        if arguments.next().is_some() {
            // Reject extra chords or arguments instead of adding an undeclared fallback summon key.
            return Err("整产品验收不接受其他快捷键或未声明参数".to_owned());
        }
        // TempDir owns normal success/error cleanup. After watchdog termination, the parent identifies
        // only this printed private directory and is responsible for its remaining cleanup.
        let root = tempfile::Builder::new()
            .prefix("omni-product-acceptance-")
            .permissions(std::fs::Permissions::from_mode(0o700))
            .tempdir()
            .map_err(|_| "无法创建整产品合成验收目录".to_owned())?;
        run(root.path(), shortcut)
    }
    #[cfg(not(target_os = "macos"))]
    {
        // Unsupported platforms produce no substitute native result and cannot count as product execution.
        Err("整产品原生验收只能在 macOS 主线程运行".to_owned())
    }
}
