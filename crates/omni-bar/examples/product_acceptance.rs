//! 独立合成目录上的真实 Host、Launcher、SDK 与 Markdown 原生循环；内存由父进程采样。

/// 只有本入口创建的空目录可以成为夹具；不接受个人库路径或复用已安装应用。
fn main() -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        use std::os::unix::fs::PermissionsExt;

        let mut arguments = std::env::args_os().skip(1);
        if arguments.next().as_deref() != Some(std::ffi::OsStr::new("--launcher-shortcut")) {
            return Err(
                "用法：product_acceptance --launcher-shortcut 本机既有启动器快捷键 [--system-ime|--core-ui|--complete-ui]".to_owned(),
            );
        }
        let shortcut = arguments.next().ok_or("缺少既有启动器快捷键")?;
        let shortcut = shortcut.to_str().ok_or("启动器快捷键必须使用有效UTF-8")?;
        // 入口分别保留原完整流程、原中文专项及同 PID 串接；这里只选择已有真实组合根，
        // 不在 example 复制工作负载步骤、创建第二个 Driver 或拼接不同进程的内存结果。
        let run: fn(&std::path::Path, &str) -> Result<(), String> = match arguments
            .next()
            .as_deref()
        {
            None => omni_native_runtime::run_native_product_acceptance,
            Some(value) if value == std::ffi::OsStr::new("--system-ime") => {
                omni_native_runtime::run_native_product_ime_acceptance
            }
            Some(value) if value == std::ffi::OsStr::new("--complete-ui") => {
                omni_native_runtime::run_native_product_complete_ui_acceptance
            }
            Some(value) if value == std::ffi::OsStr::new("--core-ui") => {
                omni_native_runtime::run_native_product_core_ui_acceptance
            }
            Some(_) => {
                return Err("只接受 --system-ime、--core-ui 或 --complete-ui 验收参数".to_owned());
            }
        };
        if arguments.next().is_some() {
            return Err("整产品验收不接受其他快捷键或未声明参数".to_owned()); // 不偷偷增加备用唤出键。
        }
        // TempDir 同时约束成功和普通错误出口；父 watchdog 杀进程时由打印出的唯一目录定位清理。
        let root = tempfile::Builder::new()
            .prefix("omni-product-acceptance-")
            .permissions(std::fs::Permissions::from_mode(0o700))
            .tempdir()
            .map_err(|_| "无法创建整产品合成验收目录".to_owned())?;
        run(root.path(), shortcut)
    }
    #[cfg(not(target_os = "macos"))]
    {
        // 非 macOS 不提供替代 Native 结果，不能算作执行过真实产品路径。
        Err("整产品原生验收只能在 macOS 主线程运行".to_owned())
    }
}
