//! 独立主线程工具以真实 AppKit 事件观察就绪和释放，不测量物理按键或 presented frame。

/// 失败返回普通工具 exit=1，不把可预期环境或资源失败伪装成 abort 信号。
fn main() -> Result<(), String> {
    // 明确启用的 example 避免在普通单元测试中反复打开真实窗口。
    let samples = std::env::args()
        .nth(1)
        .map(|value| {
            // 非数字属于配置错误，不偷偷恢复为另一份样本量。
            value
                .parse()
                .map_err(|_| "Launcher probe sample count must be numeric".to_owned())
        })
        .transpose()?
        .unwrap_or(1000);
    let mtm = objc2_foundation::MainThreadMarker::new().ok_or("native main thread required")?;
    // 父 watchdog 约束缺失事件；本工具不以 timer、sleep 或轮询完成就绪/资源证明。
    let result = omni_native_runtime::run_native_launcher_runtime_probe(samples, mtm)?;
    println!("{result:#?}");
    Ok(())
}
