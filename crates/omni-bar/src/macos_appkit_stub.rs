//! 非 macOS 目标只提供明确的运行拒绝；领域代码可编译不表示 AppKit 或产品界面已受支持。

/// 三种运行模式共用这一不支持结果；不创建模拟窗口、快捷键或备用平台实现。
pub(crate) fn run_application() -> Result<(), String> {
    Err("Omni Bar requires macOS 14 or later".to_owned())
}

/// smoke 必须取得真实 AppKit 构造结果；非 macOS 不能借可移植测试成功来报告原生初始化通过。
pub(crate) fn run_smoke_test() -> Result<(), String> {
    run_application()
}

/// 不消费期限来空等或创建驻留进程；缺少真实平台时没有可作为产品采样的工作负载。
pub(crate) fn run_benchmark_application(_seconds: u64) -> Result<(), String> {
    run_application()
}
