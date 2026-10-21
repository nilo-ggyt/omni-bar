//! Native launcher observations reuse the application's actual foreground owner.

use crate::{ForegroundSurfaceState, ResumeTarget};

use super::builtins;

mod launcher_probe;
pub use launcher_probe::{LauncherRuntimeProbeResult, run_native_launcher_runtime_probe};

/// Advance probe samples only after the native path consumes the actual foreground-close barrier.
pub(super) fn launcher_probe_foreground_released(generation: u64) {
    launcher_probe::foreground_released(generation);
}

/// Confirm a probe cannot dismiss real user work or resume a persisted plugin instead of Launcher.
fn require_probe_environment() -> Result<(), String> {
    let idle_launcher = super::FOREGROUND_STATE.with_borrow(probe_state_is_idle_launcher);
    if !idle_launcher
        || super::ACTIVE_SURFACE.with_borrow(Option::is_some)
        || super::CLOSING_SURFACE.with_borrow(Option::is_some)
        || super::plugin_host::exists()
        || builtins::is_open()
    {
        // Native probes are explicit isolated work; existing resource/transition ownership is preserved.
        return Err(
            "Native probes require idle Launcher state and no existing foreground graph".to_owned(),
        );
    }
    Ok(())
}

/// Inspect pure state only; no caller can keep a RefCell borrow across a native operation.
fn probe_state_is_idle_launcher(state: &ForegroundSurfaceState) -> bool {
    // An idle non-Launcher resume target could open real user data when the benchmark toggles foreground.
    state.active_surface().is_none()
        && state.pending_transition().is_none()
        && state.destroyed_barrier().is_none()
        && state.resume_target() == ResumeTarget::Launcher
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{BuiltInSurface, ForegroundSurface, SurfaceCloseIntent};

    /// Eligibility rejects every ownership phase and non-Launcher resume identity before native work.
    #[test]
    fn probe_eligibility_rejects_active_pending_barrier_and_non_launcher_resume() {
        // Fresh pure state is the only allowed baseline; no native application is constructed here.
        let state = ForegroundSurfaceState::new();
        assert!(probe_state_is_idle_launcher(&state));
        state.begin_creation_for(ResumeTarget::Launcher).unwrap();
        assert!(!probe_state_is_idle_launcher(&state));
        state.complete_creation(ResumeTarget::Launcher).unwrap();
        assert!(!probe_state_is_idle_launcher(&state));
        state
            .begin_destruction(
                ForegroundSurface::Launcher,
                SurfaceCloseIntent::DismissAndResume,
            )
            .unwrap();
        assert!(!probe_state_is_idle_launcher(&state));
        state
            .complete_destruction(ForegroundSurface::Launcher)
            .unwrap();
        assert!(!probe_state_is_idle_launcher(&state));
        state
            .consume_destroyed_barrier(ForegroundSurface::Launcher)
            .unwrap();
        assert!(probe_state_is_idle_launcher(&state));

        // A closed built-in retains only a resume descriptor, but toggling it would still use real user data.
        let target = ResumeTarget::BuiltIn(BuiltInSurface::Clipboard);
        state.begin_creation_for(target).unwrap();
        state.complete_creation(target).unwrap();
        let surface = ForegroundSurface::BuiltIn(BuiltInSurface::Clipboard);
        state
            .begin_destruction(surface, SurfaceCloseIntent::DismissAndResume)
            .unwrap();
        state.complete_destruction(surface).unwrap();
        state.consume_destroyed_barrier(surface).unwrap();
        assert!(!probe_state_is_idle_launcher(&state));
    }
}
