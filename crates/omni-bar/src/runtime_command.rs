//! 在平台资源分配前区分正式启动、初始化检查和限时驻留采样，不持有 Native 对象或回调。

use std::ffi::OsString;

use crate::{run_application, run_benchmark_application, run_smoke_test};

/// 参数拒绝与平台运行失败分开归类；携带原诊断，不把失败转成另一种启动模式。
#[derive(Debug)]
pub enum RuntimeCommandError {
    /// Invalid or ambiguous arguments rejected before native initialization.
    InvalidArguments(String),
    /// 平台入口返回的初始化或运行失败；资源清理和原始原因仍由该入口负责。
    Runtime(String),
}

impl std::fmt::Display for RuntimeCommandError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            // 两类错误共享文字输出，但保留原 variant 给调用方区分是否已进入平台；这里不再重试。
            Self::InvalidArguments(message) | Self::Runtime(message) => {
                formatter.write_str(message)
            }
        }
    }
}

impl std::error::Error for RuntimeCommandError {}

/// 只分发已有命令；先检查该命令的参数，再进入唯一平台入口，避免错误输入先注册快捷键。
///
/// # Errors
/// 参数编码、数量或采样时长不合法时返回 InvalidArguments；平台已执行后返回其 Runtime 原因。
pub fn run_from_environment<I>(arguments: I) -> Result<(), RuntimeCommandError>
where
    I: IntoIterator<Item = OsString>,
{
    // 入口按进程 argv 约定先消费程序名；保留 OsString 原字节，不能先有损转换再识别命令。
    let mut arguments = arguments.into_iter();
    let _program = arguments.next();
    let Some(command) = arguments.next() else {
        // 没有命令才是正常驻留启动；不套用采样期限，也不分配合成验收控件。
        return run_application().map_err(RuntimeCommandError::Runtime);
    };

    match command.to_str() {
        // smoke 只走平台的最小构造与状态转换检查，不把它当作用户笔记或完整资源验收。
        Some("--smoke-test") => {
            if arguments.next().is_some() {
                // 无参数命令不能忽略后续输入；在 AppKit 构造前完整拒绝本次请求。
                return Err(RuntimeCommandError::InvalidArguments(
                    "--smoke-test 不接受额外参数".to_owned(),
                ));
            }
            run_smoke_test().map_err(RuntimeCommandError::Runtime)
        }
        // 采样复用真实驻留初始化；先确定期限和唯一时长参数，再允许平台安排结束回调。
        Some("--benchmark") => {
            let seconds = parse_benchmark_seconds(arguments.next())?;
            if arguments.next().is_some() {
                // 时长合法也不能吞掉第二个参数；此处尚未启动事件循环或申请快捷键。
                return Err(RuntimeCommandError::InvalidArguments(
                    "--benchmark 仅接受一个时长参数".to_owned(),
                ));
            }
            run_benchmark_application(seconds).map_err(RuntimeCommandError::Runtime)
        }
        // 帮助只打印当前命令，不调用平台或持久化配置；不创建供采样用的空进程。
        Some("--help" | "-h") => {
            print_usage();
            Ok(())
        }
        // 未知命令保留原文本用于定位错误，不猜近似选项或落回正式启动；尚无 Native 资源。
        Some(unknown) => Err(RuntimeCommandError::InvalidArguments(format!(
            "未知命令 {unknown:?}；请使用 --help 查看用法"
        ))),
        // 命令字节不是 UTF-8 时明确拒绝，避免替换字符把不同输入映射成同一个受支持命令。
        None => Err(RuntimeCommandError::InvalidArguments(
            "命令参数必须采用 UTF-8 编码".to_owned(),
        )),
    }
}

/// 将明确采样限制在 1 至 3600 秒；仅返回时长，不分配 timer、线程或应用 owner。
fn parse_benchmark_seconds(argument: Option<OsString>) -> Result<u64, RuntimeCommandError> {
    let Some(argument) = argument else {
        // 缺少时长是参数错误，不能把显式采样静默变成无限驻留的正式应用。
        return Err(RuntimeCommandError::InvalidArguments(
            "--benchmark 需要以秒为单位的时长参数".to_owned(),
        ));
    };
    // 编码和数值分别保留失败含义；验证完成前平台不会安排任何结束回调。
    let value = argument.to_str().ok_or_else(|| {
        // 非 UTF-8 的原字节不能经有损转换后重新尝试解析成数字。
        RuntimeCommandError::InvalidArguments("采样时长必须是 UTF-8 编码的整数".to_owned())
    })?;
    let seconds = value.parse::<u64>().map_err(|_| {
        // 负数、非整数和 u64 溢出均拒绝；不能截断或钳制成另一个有效采样期限。
        RuntimeCommandError::InvalidArguments("采样时长必须是无符号整数".to_owned())
    })?;
    if seconds == 0 || seconds > 3_600 {
        // 零不能代表无限等待；超过一小时也不代为缩短，调用方须明确给出可接受期限。
        return Err(RuntimeCommandError::InvalidArguments(
            "采样时长必须介于 1 至 3600 秒之间".to_owned(),
        ));
    }
    // 到这里仅移交已检查的秒数；实际计时与正常保存、终止协议仍归平台。
    Ok(seconds)
}

/// 帮助与上面的命令集合对应，只写标准输出，不初始化原生运行时或读取用户数据。
fn print_usage() {
    println!("用法：omni-native-runtime [选项]");
    println!("  --help, -h             显示帮助");
    println!("  --smoke-test           验证原生初始化，不使用用户数据");
    println!("  --benchmark <seconds>  隐藏运行已初始化的应用，供外部采样");
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 直接验证期限与多余参数的业务拒绝，不以实际等待或 AppKit 初始化替代参数检查。
    #[test]
    fn measurement_arguments_are_bounded_before_native_initialization() {
        // 非法数值覆盖零、负数、超期、非整数和溢出；普通测试线程不进入 AppKit。
        for input in ["0", "-1", "3601", "invalid", "18446744073709551616"] {
            assert!(parse_benchmark_seconds(Some(input.into())).is_err());
        }
        // 缺失与合法上下界分别验证，避免拒绝非法值的同时误伤明确的采样请求。
        assert!(parse_benchmark_seconds(None).is_err());
        assert_eq!(parse_benchmark_seconds(Some("1".into())).unwrap(), 1);
        assert_eq!(parse_benchmark_seconds(Some("3600".into())).unwrap(), 3600);
        // Extra flags must fail before smoke could touch AppKit or benchmark could register a shortcut.
        for arguments in [
            vec!["omni", "--smoke-test", "extra"],
            vec!["omni", "--benchmark", "1", "extra"],
        ] {
            assert!(matches!(
                run_from_environment(arguments.into_iter().map(OsString::from)),
                Err(RuntimeCommandError::InvalidArguments(_))
            ));
        }
    }
}
