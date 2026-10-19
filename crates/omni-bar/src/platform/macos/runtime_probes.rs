//! Isolated native acceptance adapters; production state and the user's clipboard stay separate.

use std::cell::Cell;
use std::path::Path;
use std::rc::Rc;

use objc2::MainThreadMarker;

use crate::capabilities::shortcut_preferences::{
    ShortcutBinding, ShortcutPreferences, ShortcutPreferencesError, ShortcutView,
};
use crate::{
    BuiltInSurface, ForegroundSurface, ForegroundSurfaceState, ResumeTarget, SurfaceCloseIntent,
    SurfaceTransition,
};

use super::builtins::{self, BuiltInContext, BuiltInLifecycle};

/// A probe owns its own transition graph, never the production global foreground state.
struct ProbeLifecycle(ForegroundSurfaceState);

impl BuiltInLifecycle for ProbeLifecycle {
    fn commit_creation(&self, kind: BuiltInSurface) -> Result<(), String> {
        // The reusable harness constructs each surface from an empty isolated state.
        let target = ResumeTarget::BuiltIn(kind);
        self.0
            .begin_creation_for(target)
            .map_err(|error| error.to_string())?;
        self.0
            .complete_creation(target)
            .map(|_| ())
            .map_err(|error| error.to_string())
    }

    fn begin_close(
        &self,
        kind: BuiltInSurface,
        intent: SurfaceCloseIntent,
        scope: omni_plugin_sdk::PluginCloseScope,
    ) -> Result<(), String> {
        if scope != omni_plugin_sdk::PluginCloseScope::ForegroundTransition {
            // This ordinary graph probe has no application termination transaction or shell-lease authority.
            return Err("ordinary built-in probe rejects terminal scope".into());
        }
        let requested = SurfaceTransition::Destroying {
            surface: ForegroundSurface::BuiltIn(kind),
            restore_launcher: intent == SurfaceCloseIntent::ExitToLauncher,
        };
        if self.0.coalesces(requested) {
            // The existing close owner completes this accepted intent; do not acquire a second domain transition.
            return Ok(());
        }
        // Let the domain reject opposing intent or another owner; never synthesize native completion.
        self.0
            .begin_destruction(ForegroundSurface::BuiltIn(kind), intent)
            .map(|_| ())
            .map_err(|error| error.to_string())
    }

    fn finish_close(
        &self,
        kind: BuiltInSurface,
        _intent: SurfaceCloseIntent,
        scope: omni_plugin_sdk::PluginCloseScope,
    ) {
        // A standalone ordinary probe must never consume terminal-only evidence as a reusable foreground completion.
        assert_eq!(
            scope,
            omni_plugin_sdk::PluginCloseScope::ForegroundTransition
        );
        // A failed callback is an invariant violation in this harness, never a fabricated pass.
        let surface = ForegroundSurface::BuiltIn(kind);
        self.0
            .complete_destruction(surface)
            .expect("probe lifecycle completion");
        self.0
            .consume_destroyed_barrier(surface)
            .expect("probe lifecycle barrier");
    }
}

#[cfg(test)]
mod lifecycle_tests {
    use super::*;
    use omni_plugin_sdk::PluginCloseScope;

    #[test]
    fn repeated_probe_close_keeps_original_intent_and_next_creation_waits_for_receipt() {
        // Reuse production coalescing, but require native receipts to establish completion.
        let lifecycle = ProbeLifecycle(ForegroundSurfaceState::new());
        let kind = BuiltInSurface::Calculator;
        let scope = PluginCloseScope::ForegroundTransition;
        lifecycle.commit_creation(kind).unwrap();
        lifecycle
            .begin_close(kind, SurfaceCloseIntent::DismissAndResume, scope)
            .unwrap();
        lifecycle
            .begin_close(kind, SurfaceCloseIntent::DismissAndResume, scope)
            .unwrap();
        assert!(
            lifecycle
                .begin_close(kind, SurfaceCloseIntent::ExitToLauncher, scope)
                .is_err()
        );
        assert!(lifecycle.commit_creation(BuiltInSurface::Settings).is_err());
        // Simulate receipt-routing authority only; this state test does not prove object deallocation.
        lifecycle.finish_close(kind, SurfaceCloseIntent::DismissAndResume, scope);
        lifecycle.commit_creation(BuiltInSurface::Settings).unwrap();
    }
}

/// Model preference changes without registering a global shortcut or modifying user settings.
struct ProbeShortcuts(Cell<ShortcutBinding>);

impl ShortcutPreferences for ProbeShortcuts {
    fn current(&self) -> Result<Option<ShortcutView>, ShortcutPreferencesError> {
        // Return the fake adapter's actual active value; this probe registers no system shortcut.
        Ok(Some(ShortcutView {
            binding: self.0.get(),
        }))
    }

    fn update(&self, binding: &str) -> Result<ShortcutView, ShortcutPreferencesError> {
        // Parse before changing the fake registration; invalid input preserves the previous active binding.
        self.0.set(ShortcutBinding::parse(binding)?);
        Ok(ShortcutView {
            binding: self.0.get(),
        })
    }
}

/// Exercise real built-in controls, file semantics, timers and weak graph teardown on the main thread.
///
/// `directory` must be a caller-owned test directory; this writes only synthetic clipboard history
/// there. The probe explicitly disables system pasteboard capture and uses fake shortcut preferences.
/// Call before the dedicated process first creates NSApplication; reject another existing application class.
/// Real NSApplication.run dispatches bounded stages; only the production finish_close barrier advances successors.
pub fn run_native_builtins_runtime_probe(
    directory: &Path,
    mtm: MainThreadMarker,
) -> Result<(), String> {
    // Refuse live business surfaces or a non-Launcher resume target before presenting isolated graphs.
    require_probe_environment()?;
    // Inject isolated owners before any native feature graph or foreground monitor is constructed.
    let context = BuiltInContext {
        application_data: directory.to_owned(),
        lifecycle: Rc::new(ProbeLifecycle(ForegroundSurfaceState::new())),
        shortcuts: Rc::new(ProbeShortcuts(Cell::new(
            ShortcutBinding::ControlOptionSpace,
        ))),
    };
    builtins_probe::run(context, mtm)
}

mod builtins_probe;

#[cfg(feature = "native-acceptance")]
mod launcher_probe;
#[cfg(feature = "native-acceptance")]
pub use launcher_probe::{LauncherRuntimeProbeResult, run_native_launcher_runtime_probe};

/// Advance probe samples only after the native path consumes the actual foreground-close barrier.
#[cfg(feature = "native-acceptance")]
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
