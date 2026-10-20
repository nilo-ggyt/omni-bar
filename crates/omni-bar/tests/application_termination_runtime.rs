//! Subprocess-isolated production AppKit Quit/save/release integration over parent-owned synthetic files.

#[cfg(target_os = "macos")]
mod native {
    use std::fs::File;
    use std::io::Read;
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
    use std::path::Path;
    use std::process::{Child, Command, Stdio};
    use std::sync::mpsc;
    use std::time::Duration;

    use omni_native_runtime::{
        NATIVE_TERMINATION_PROBE_INSERTION, NativeTerminationProbeOrigin,
        NativeTerminationProbeScenario, run_native_termination_probe_child,
    };

    const CHILD_ARGUMENT: &str = "--termination-probe-child";
    const FAILURE_DEADLINE: Duration = Duration::from_secs(20);
    const MAX_LOG_BYTES: usize = 1024 * 1024;
    const ACCEPTANCE_BODY: &str = concat!(
        "# Synthetic acceptance note\n\nEnglish and 中文输入法 fixture. 😀\n\n",
        "- first item\n- second item\n\n> quotation\n\n",
        "| Column A | Column B |\n| --- | --- |\n| first | second |\n\n",
        "```rust\nfn main() {}\n```\n"
    );

    /// Keep each successful observation separate from the parent watchdog and its deliberate timeout failure.
    enum ChildCompletion {
        Exited(std::process::ExitStatus),
        TimedOut,
    }

    /// Run only exact child modes or four bounded parent scenarios; never initialize AppKit in the parent.
    pub(super) fn run() -> Result<(), String> {
        let arguments = std::env::args_os().collect::<Vec<_>>();
        if arguments
            .get(1)
            .is_some_and(|value| value == CHILD_ARGUMENT)
        {
            // The child receives only one declared scenario and its parent's fresh synthetic authority directory.
            if arguments.len() != 5 {
                // Malformed invocation cannot fall through into another parent or choose a guessed fixture.
                return Err(
                    "termination child requires exactly scenario, origin and fixture directory"
                        .into(),
                );
            }
            let scenario = match arguments[2].to_str() {
                // Each process starts production NSApplication and foreground state exactly once.
                Some("launcher") => NativeTerminationProbeScenario::Launcher,
                Some("markdown-dirty") => NativeTerminationProbeScenario::MarkdownDirty,
                // No user-provided plugin identity or alternate operation is accepted by this harness.
                _ => return Err("unknown termination probe scenario".into()),
            };
            let origin = match arguments[3].to_str() {
                // Ordinary app-local menu delivery and true dispatch origin have independent child evidence.
                Some("menu-action") => NativeTerminationProbeOrigin::MenuAction,
                Some("dispatch") => NativeTerminationProbeOrigin::Dispatch,
                // An alternate direct-selector fallback cannot mask a route-specific failure.
                _ => return Err("unknown termination probe origin".into()),
            };
            return run_native_termination_probe_child(Path::new(&arguments[4]), scenario, origin);
        }
        if arguments.len() != 1 {
            // This harness owns its process contract rather than silently ignoring unexpected automation arguments.
            return Err("run this dedicated test without additional harness arguments".into());
        }
        let mut failures = Vec::new();
        for scenario in ["launcher", "markdown-dirty"] {
            // Each foreground exercises both production entry routes with independent native process state.
            for origin in ["menu-action", "dispatch"] {
                // A fresh process owns one fixture, native event loop and failure-only parent watchdog.
                if let Err(error) = run_scenario(scenario, origin) {
                    // Preserve independent failures without retries, alternate closes or survivor allowances.
                    failures.push(error);
                }
            }
        }
        if failures.is_empty() {
            // These are programmatic native event/resource observations, never physical-input/product acceptance.
            println!("termination_integration=passed scenarios=4 physical_acceptance=false");
            Ok(())
        } else {
            // Exact private artifact locations and bounded captured output remain available for every failing child.
            Err(failures.join("\n"))
        }
    }

    /// Keep the child unreaped while the parent waits, so timeout kill cannot address a reused numeric PID.
    fn wait_owned_child(child: &mut Child) -> Result<ChildCompletion, String> {
        let pid = child.id();
        let (sender, receiver) = mpsc::sync_channel(1);
        let waiter = std::thread::spawn(move || {
            let observed = loop {
                // SAFETY: waitid receives a valid writable siginfo_t allocation and this process's owned child PID.
                // WNOWAIT observes exit without reaping; only the parent Child handle below consumes the wait status.
                let result = unsafe {
                    let mut information = std::mem::MaybeUninit::<libc::siginfo_t>::zeroed();
                    libc::waitid(
                        libc::P_PID,
                        pid,
                        information.as_mut_ptr(),
                        libc::WEXITED | libc::WNOWAIT,
                    )
                };
                if result == 0 {
                    // The zombie remains owned and cannot have its PID reused before Child::wait performs reaping.
                    break Ok(());
                }
                let error = std::io::Error::last_os_error();
                if error.kind() == std::io::ErrorKind::Interrupted {
                    // A signal interruption restarts a blocking wait, not a timed native polling or lifecycle retry.
                    continue;
                }
                break Err(error.to_string());
            };
            // The one bounded channel carries process completion only; it never controls native foreground state.
            let _ = sender.send(observed);
        });
        let observation = receiver.recv_timeout(FAILURE_DEADLINE);
        let timed_out = matches!(observation, Err(mpsc::RecvTimeoutError::Timeout));
        if !matches!(observation, Ok(Ok(()))) {
            // Kill only this still-unreaped Child after timeout/wait failure; never signal another app or guessed helper.
            let _ = child.kill();
        }
        let status = child
            .wait()
            .map_err(|error| format!("owned termination child could not be reaped: {error}"))?;
        waiter
            .join()
            .map_err(|_| "the owned-child exit observer panicked".to_owned())?;
        if timed_out {
            // Timeout always fails even if killing the child produced a normal-looking final process record.
            return Ok(ChildCompletion::TimedOut);
        }
        match observation {
            // Exit has now been consumed exactly once by the unique Child handle.
            Ok(Ok(())) => Ok(ChildCompletion::Exited(status)),
            // A failed public wait contract cannot be recast as successful native termination.
            Ok(Err(error)) => Err(format!("owned-child waitid failed: {error}")),
            // Lost channel ownership is a harness failure, without a second native run.
            Err(error) => Err(format!("owned-child observer disconnected: {error}")),
        }
    }

    /// Bound diagnostics before materializing or reporting any unexpected system/native output.
    fn read_log(path: &Path) -> Result<String, String> {
        let mut bytes = Vec::new();
        File::open(path)
            .map_err(|error| error.to_string())?
            .take(MAX_LOG_BYTES as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|error| error.to_string())?;
        if bytes.len() > MAX_LOG_BYTES {
            // Preserve the full private artifact on disk without copying unbounded diagnostics to the parent output.
            return Err("native child log exceeded the one-MiB report budget".into());
        }
        Ok(String::from_utf8_lossy(&bytes).into_owned())
    }

    /// Validate exact event order and final synthetic bytes after real NSApplication process termination.
    fn run_scenario(scenario: &str, origin: &str) -> Result<(), String> {
        let directory = tempfile::Builder::new()
            .prefix(&format!("omni-termination-{scenario}-{origin}-"))
            // The authority is private from creation, before this parent writes logs or starts its child.
            .permissions(std::fs::Permissions::from_mode(0o700))
            .tempdir()
            .map_err(|error| error.to_string())?;
        let fixture = directory.path().join("fixture");
        // Set the child authority's owner-only mode in its creation operation, before any fixture/plugin access.
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&fixture)
            .map_err(|error| error.to_string())?;
        let stdout_path = directory.path().join("child.stdout");
        let stderr_path = directory.path().join("child.stderr");
        // Log files stay outside the fresh fixture and are owned by this parent, never by a native destructor.
        let stdout = File::create(&stdout_path).map_err(|error| error.to_string())?;
        let stderr = File::create(&stderr_path).map_err(|error| error.to_string())?;
        let mut child = Command::new(std::env::current_exe().map_err(|error| error.to_string())?)
            .args([CHILD_ARGUMENT, scenario, origin])
            .arg(&fixture)
            .stdin(Stdio::null())
            .stdout(stdout)
            .stderr(stderr)
            .spawn()
            .map_err(|error| error.to_string())?;
        let completion = wait_owned_child(&mut child);
        let stdout = read_log(&stdout_path);
        let stderr = read_log(&stderr_path);
        let execution = (|| {
            match completion? {
                ChildCompletion::Exited(status) if status.success() => {
                    // Process exit alone is insufficient; strict scalar ordering and saved bytes are checked below.
                }
                ChildCompletion::Exited(status) => {
                    // Preserve a crash, signal or explicit failed diagnostic as its exact process status.
                    return Err(format!("child exited unsuccessfully: {status}"));
                }
                ChildCompletion::TimedOut => {
                    // Only this owned child was killed; the missing native completion remains a failing observation.
                    return Err(
                        "child exceeded the single 20-second parent failure deadline".into(),
                    );
                }
            }
            Ok(())
        })();
        // A timed-out child may have saved correctly; distinguish that from the failed native release/exit gate.
        let proof = stdout.as_ref().map_err(Clone::clone).and_then(|stdout| {
            // This read-only verifier never restarts the child or changes its retained synthetic fixture.
            validate_proof(stdout, scenario, origin, &fixture)
        });
        let validation = match (execution, proof) {
            // Process completion and every independent scalar/data assertion are jointly necessary.
            (Ok(()), Ok(())) => Ok(()),
            // A successful process status cannot excuse a failed graph/order/data proof.
            (Ok(()), Err(error)) => Err(error),
            // Valid-looking evidence cannot excuse a signal, crash or parent-watchdog timeout.
            (Err(error), Ok(())) => Err(error),
            // Retain both concrete causes instead of hiding saved-data evidence behind the first timeout.
            (Err(execution), Err(proof)) => Err(format!("{execution}; {proof}")),
        };
        if let Err(error) = validation {
            // Keep exactly this parent-created directory; do not remove or overwrite any other artifact path.
            let preserved = directory.keep();
            return Err(format!(
                "scenario={scenario} origin={origin} failure={error}\nartifacts={}\nstdout:\n{}\nstderr:\n{}",
                preserved.display(),
                stdout.unwrap_or_else(|error| format!("unavailable: {error}")),
                stderr.unwrap_or_else(|error| format!("unavailable: {error}"))
            ));
        }
        println!(
            "termination_scenario={scenario} origin={origin} saved_bytes=verified native_release=verified reply_order=verified"
        );
        // TempDir removes only this successfully verified parent's synthetic fixture/log authority.
        Ok(())
    }

    /// A maintained scalar protocol rejects missing/duplicate/reordered completion and validates exact file bodies.
    fn validate_proof(
        stdout: &str,
        scenario: &str,
        origin: &str,
        fixture: &Path,
    ) -> Result<(), String> {
        let evidence = stdout
            .lines()
            .filter(|line| {
                // System startup output has no authority over this probe's explicitly prefixed scalar protocol.
                line.starts_with("termination_probe ")
            })
            .collect::<Vec<_>>();
        let mut expected = vec![
            "termination_probe event=configured input=programmatic_responder physical_acceptance=false",
            if origin == "dispatch" {
                // The parent independently declares a serial-main-dispatch regression case.
                "termination_probe event=origin value=dispatch"
            } else {
                // 原菜单动作不依赖退出快捷键；target/action必须仍由已安装的原NSMenu拥有。
                "termination_probe event=origin value=menu_action"
            },
            "termination_probe event=foreground_ready production_owner=true",
        ];
        let selected_note = if scenario == "markdown-dirty" {
            let dirty = evidence.get(3).ok_or("dirty-input proof is missing")?;
            let note = match *dirty {
                // Both known fixture notes are legitimate filesystem-order selections; no arbitrary path is parsed.
                "termination_probe event=dirty_input note=Acceptance.md disk_still_old=true" => {
                    "Acceptance.md"
                }
                "termination_probe event=dirty_input note=SourcePerformance.md disk_still_old=true" => {
                    "SourcePerformance.md"
                }
                // The exact old-disk proof is necessary to exercise immediate-Quit save rather than prior autosave.
                _ => {
                    return Err("unexpected or missing unsaved native responder input proof".into());
                }
            };
            expected.push(dirty);
            Some(note)
        } else {
            // Launcher owns no document and cannot manufacture an input/save proof.
            None
        };
        expected.push("termination_probe event=terminate_requested temporary_graph_retains=none");
        if origin == "dispatch" {
            // The originating GCD callback must return before any modal delegate wait is entered.
            expected.push("termination_probe event=dispatch_request_returned");
        } else {
            // 仍保真实菜单入口与action退栈顺序，不能去掉两场景或改成直接coordinator调用。
            expected.extend([
                "termination_probe event=menu_action_requested key_equivalent_empty=true",
                "termination_probe event=menu_action original_item_matches=true",
                "termination_probe event=menu_request_returned",
                "termination_probe event=menu_action_returned",
            ]);
        }
        let terminal = evidence
            .last()
            .copied()
            .unwrap_or("missing terminal payload/shell proof");
        // 父端限定每个场景允许的独立正文观察；不接受 attached/unobserved 或由子端任选身份。
        let editor_observations: &[&str] = if scenario == "markdown-dirty" {
            // 实际 nil 与完整收据共同证明的具体空壳分别报告，不能把后者写成析构。
            &["nil", "inert"]
        } else {
            // Launcher 场景没有正文，出现任何正文身份都代表错误的资源范围。
            &["not_applicable"]
        };
        let terminal_valid = editor_observations.iter().any(|observation| {
            // 只枚举 terminal scope 明确允许的两个窗口结果；其余标量仍要求完整且各出现一次。
            ["pending", "none"].into_iter().any(|lease| {
                terminal == format!(
                    "termination_probe event=will_terminate terminal_receipt=true payload_clear=true editor_payload={observation} shell_valid=true framework_window_lease={lease} foreground_released=true later_replies=1 async_replies=1"
                )
            })
        });
        expected.extend([
            "termination_probe event=delegate_reply value=later",
            "termination_probe event=async_reply foreground_released=true sdk_session_owned=false",
            terminal,
        ]);
        // Saved data is a separate forensic observation even when a child never completes native release.
        let saved = verify_fixture_bytes(fixture, selected_note);
        if evidence != expected || !terminal_valid {
            // Exact order rejects Now, Cancel, missing graph completion, premature reply and duplicate async authority.
            let saved = match saved {
                // Exact fixture bytes can succeed while native termination still fails; neither overrides the other.
                Ok(()) => "verified".to_owned(),
                // Preserve the concrete data mismatch/unavailable cause beside the failed native protocol.
                Err(error) => format!("failed: {error}"),
            };
            return Err(format!(
                "termination scalar protocol differed: {evidence:?}; saved_bytes={saved}"
            ));
        }
        saved
    }

    /// Compare exact selected/untouched bodies independently of process exit or native release success.
    fn verify_fixture_bytes(fixture: &Path, selected_note: Option<&str>) -> Result<(), String> {
        for note in ["Acceptance.md", "SourcePerformance.md"] {
            // Check both the selected document and its untouched neighbor, preserving exact synthetic Unicode/body bytes.
            let mut expected = if selected_note == Some(note) {
                // The canonical document must include precisely the delivered prefix once after save-before-close.
                NATIVE_TERMINATION_PROBE_INSERTION.as_bytes().to_vec()
            } else {
                // Launcher and the unselected note must retain their original initialized bytes exactly.
                Vec::new()
            };
            if note == "Acceptance.md" {
                // This independent fixture expectation includes the original table, fence, Unicode and final newline.
                expected.extend_from_slice(ACCEPTANCE_BODY.as_bytes());
            } else {
                // Reconstruct the declared one-MiB ASCII workload under a fixed maximum, never from child output.
                expected.extend_from_slice(b"OmniFrameOldA9\n");
                let mut remaining = 1_048_576 - b"OmniFrameOldA9\n".len();
                let mut row = [b'x'; 64];
                row[63] = b'\n';
                while remaining >= row.len() {
                    // Every complete row has the exact initializer's 63 ASCII bytes plus newline.
                    expected.extend_from_slice(&row);
                    remaining -= row.len();
                }
                if remaining != 0 {
                    // Preserve the initializer's exact partial-row termination and one-MiB length.
                    row[remaining - 1] = b'\n';
                    expected.extend_from_slice(&row[..remaining]);
                }
            }
            // One extra byte detects erroneous growth without materializing an unbounded failed output document.
            let mut actual = Vec::new();
            File::open(fixture.join("workspace").join(note))
                .and_then(|file| {
                    // The parent reads at most this exact known fixture's expected size plus one rejection byte.
                    file.take(expected.len() as u64 + 1)
                        .read_to_end(&mut actual)
                })
                .map_err(|error| format!("saved fixture {note} unavailable: {error}"))?;
            if actual != expected {
                // A successful exit/release cannot excuse lost, duplicated or silently changed canonical bytes.
                return Err(format!("saved fixture bytes differ for {note}"));
            }
        }
        Ok(())
    }
}

/// The parent never creates AppKit; each real-main-thread child owns precisely one production application loop.
fn main() {
    #[cfg(target_os = "macos")]
    if let Err(error) = native::run() {
        // Preserve exact bounded failure evidence before reporting a failed dedicated integration executable.
        eprintln!("application termination integration failed: {error}");
        std::process::exit(1);
    }
    #[cfg(not(target_os = "macos"))]
    {
        // Unsupported runtime targets cannot be presented as passed AppKit integration.
        eprintln!("application termination integration requires macOS");
        std::process::exit(1);
    }
}
