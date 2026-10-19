//! Thin executable boundary; application composition owns initialization and teardown.

/// 命令边界只报告已返回的参数或运行错误；原生构造、事件循环和可失败清理由平台入口负责。
fn main() {
    if let Err(error) = omni_native_runtime::run_from_environment(std::env::args_os()) {
        // 参数错误尚未进入平台；运行错误沿平台返回后统一报告。exit 不运行剩余析构，
        // 不能把此进程状态当作前台屏障、保存成功或 Native 资源实际释放的证明。
        eprintln!("omni-native-runtime: {error}");
        std::process::exit(2);
    }
}
