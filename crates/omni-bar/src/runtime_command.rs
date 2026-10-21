//! Select production, smoke or bounded measurement before acquiring platform resources.

use std::ffi::OsString;

use crate::{run_application, run_benchmark_application, run_smoke_test};

/// Distinguish rejected arguments from returned runtime failures without changing the requested mode.
#[derive(Debug)]
pub enum RuntimeCommandError {
    /// Invalid or ambiguous arguments rejected before native initialization.
    InvalidArguments(String),
    /// Initialization or runtime failure returned after the platform boundary handled its owned resources.
    Runtime(String),
}

impl std::fmt::Display for RuntimeCommandError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            // Preserve the category for callers while displaying its original bounded diagnostic; never retry here.
            Self::InvalidArguments(message) | Self::Runtime(message) => {
                formatter.write_str(message)
            }
        }
    }
}

impl std::error::Error for RuntimeCommandError {}

/// Validate the selected command completely before platform initialization can register a shortcut.
///
/// # Errors
/// Return InvalidArguments for invalid encoding, arity or duration, or the actual Runtime failure.
pub fn run_from_environment<I>(arguments: I) -> Result<(), RuntimeCommandError>
where
    I: IntoIterator<Item = OsString>,
{
    // Consume argv[0] and retain original OsString bytes until exact command decoding succeeds.
    let mut arguments = arguments.into_iter();
    let _program = arguments.next();
    let Some(command) = arguments.next() else {
        // Only an absent command means normal residency, with no measurement deadline or synthetic controls.
        return run_application().map_err(RuntimeCommandError::Runtime);
    };

    match command.to_str() {
        // Smoke checks minimal native construction and transitions, without claiming complete product acceptance.
        Some("--smoke-test") => {
            if arguments.next().is_some() {
                // Reject excess arguments before any AppKit construction instead of silently ignoring them.
                return Err(RuntimeCommandError::InvalidArguments(
                    "--smoke-test 不接受额外参数".to_owned(),
                ));
            }
            run_smoke_test().map_err(RuntimeCommandError::Runtime)
        }
        // Measurement uses real residency, but validates its sole deadline before scheduling shutdown.
        Some("--benchmark") => {
            let seconds = parse_benchmark_seconds(arguments.next())?;
            if arguments.next().is_some() {
                // A valid duration cannot authorize extra arguments; no event loop or shortcut exists yet.
                return Err(RuntimeCommandError::InvalidArguments(
                    "--benchmark 仅接受一个时长参数".to_owned(),
                ));
            }
            run_benchmark_application(seconds).map_err(RuntimeCommandError::Runtime)
        }
        // Help writes usage only, without entering the platform or creating persistent configuration.
        Some("--help" | "-h") => {
            print_usage();
            Ok(())
        }
        // Reject unknown text as supplied instead of guessing a command or falling through to production.
        Some(unknown) => Err(RuntimeCommandError::InvalidArguments(format!(
            "未知命令 {unknown:?}；请使用 --help 查看用法"
        ))),
        // Reject invalid UTF-8 rather than aliasing distinct command bytes through replacement characters.
        None => Err(RuntimeCommandError::InvalidArguments(
            "命令参数必须采用 UTF-8 编码".to_owned(),
        )),
    }
}

/// Parse an explicit duration of 1 through 3600 seconds without acquiring timers or application state.
fn parse_benchmark_seconds(argument: Option<OsString>) -> Result<u64, RuntimeCommandError> {
    let Some(argument) = argument else {
        // A missing duration cannot turn explicit measurement into indefinite production residency.
        return Err(RuntimeCommandError::InvalidArguments(
            "--benchmark 需要以秒为单位的时长参数".to_owned(),
        ));
    };
    // Preserve distinct encoding and numeric failures before the platform schedules a shutdown callback.
    let value = argument.to_str().ok_or_else(|| {
        // Invalid original bytes must not become a numeric argument through lossy conversion.
        RuntimeCommandError::InvalidArguments("采样时长必须是 UTF-8 编码的整数".to_owned())
    })?;
    let seconds = value.parse::<u64>().map_err(|_| {
        // Reject negatives, non-integers and overflow instead of clamping them to a different deadline.
        RuntimeCommandError::InvalidArguments("采样时长必须是无符号整数".to_owned())
    })?;
    if seconds == 0 || seconds > 3_600 {
        // Zero cannot mean infinite waiting; the caller must also explicitly choose a duration within one hour.
        return Err(RuntimeCommandError::InvalidArguments(
            "采样时长必须介于 1 至 3600 秒之间".to_owned(),
        ));
    }
    // Transfer validated seconds only; the platform still owns timing, saving and ordinary termination.
    Ok(seconds)
}

/// Describe exactly the accepted commands without initializing AppKit or reading user data.
fn print_usage() {
    println!("用法：omni-native-runtime [选项]");
    println!("  --help, -h             显示帮助");
    println!("  --smoke-test           验证原生初始化，不使用用户数据");
    println!("  --benchmark <seconds>  隐藏运行已初始化的应用，供外部采样");
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verify invalid durations and excess arguments without substituting waiting or native initialization.
    #[test]
    fn measurement_arguments_are_bounded_before_native_initialization() {
        // Exercise invalid numeric input directly; ordinary test threads must never initialize AppKit.
        for input in ["0", "-1", "3601", "invalid", "18446744073709551616"] {
            assert!(parse_benchmark_seconds(Some(input.into())).is_err());
        }
        // Preserve valid endpoints as well as missing-input rejection for explicit measurement requests.
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
