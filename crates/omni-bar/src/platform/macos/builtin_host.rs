//! Compose built-in sessions with the application's single foreground state machine.

use std::cell::Cell;
use std::rc::Rc;

use objc2::MainThreadMarker;
use omni_plugin_sdk::PluginCloseScope;

use crate::application::foreground::SurfaceTransition;
use crate::application::termination::TerminationTicket;
use crate::{BuiltInSurface, ForegroundSurface, ResumeTarget, SurfaceCloseIntent};

use super::builtins::{self, BuiltInContext, BuiltInLifecycle};
use super::shortcuts::NativeShortcutPreferences;

/// One activation's accepted request is scalar metadata and never retains a native/session resource graph.
#[derive(Clone, Copy)]
struct AcceptedClose {
    intent: SurfaceCloseIntent,
    scope: PluginCloseScope,
    ticket: Option<TerminationTicket>,
}

/// Per-activation callbacks reject stale directions/scopes even when a later surface has the same kind.
struct Lifecycle {
    kind: BuiltInSurface,
    accepted: Cell<Option<AcceptedClose>>,
    finished: Cell<bool>,
}

impl Lifecycle {
    /// Bind one activation's callbacks without acquiring a native object or process-global observer.
    fn new(kind: BuiltInSurface) -> Self {
        Self {
            kind,
            accepted: Cell::new(None),
            finished: Cell::new(false),
        }
    }
}

impl BuiltInLifecycle for Lifecycle {
    /// Commit only this activation's prepared kind without crossing state borrowing into AppKit.
    fn commit_creation(&self, kind: BuiltInSurface) -> Result<(), String> {
        if kind != self.kind || self.finished.get() || self.accepted.get().is_some() {
            // A stale callback cannot recreate or borrow another built-in activation's foreground claim.
            return Err("Built-in creation does not match its lifecycle owner".to_owned());
        }
        // The surface is prepared before it commits ownership or becomes visible.
        super::FOREGROUND_STATE.with_borrow(|state| {
            // Only the matching creation claim can become the active built-in.
            state
                .complete_creation(ResumeTarget::BuiltIn(kind))
                .map(|_| ())
                .map_err(|error| error.to_string())
        })
    }

    /// Validate exact intent/scope/quit identity before acknowledging the one foreground destruction owner.
    fn begin_close(
        &self,
        kind: BuiltInSurface,
        intent: SurfaceCloseIntent,
        scope: PluginCloseScope,
    ) -> Result<(), String> {
        if kind != self.kind || self.finished.get() {
            // Completed or different-kind callbacks cannot begin another destruction with stale authority.
            return Err("Built-in close does not match its lifecycle owner".to_owned());
        }
        let accepted = self.accepted.get();
        builtins::validate_close_scope(
            accepted.map(|accepted| (accepted.intent, accepted.scope)),
            intent,
            scope,
            super::termination::application_termination_pending(),
        )?;
        let ticket = builtins::validate_close_ticket(
            accepted.and_then(|accepted| accepted.ticket),
            scope,
            super::termination::application_termination_ticket(),
        )?;
        // The session stops new work only after the single foreground owner accepts close.
        super::FOREGROUND_STATE.with_borrow(|state| {
            if accepted.is_some() {
                // The pure foreground graph already owns this accepted destruction. Repeats and promotion
                // validate its exact direction; beginning destruction again would reject the existing claim.
                return if state.pending_transition()
                    == Some(SurfaceTransition::Destroying {
                        surface: ForegroundSurface::BuiltIn(kind),
                        restore_launcher: intent == SurfaceCloseIntent::ExitToLauncher,
                    }) {
                    // Scalar scope/ticket may advance only while the original destruction remains current.
                    Ok(())
                } else {
                    // A consumed/replaced claim cannot lend its old accepted request to a later activation.
                    Err("Built-in accepted close lost its foreground claim".to_owned())
                };
            }
            // Repeated/opposing requests cannot replace another close owner or intent.
            state
                .begin_destruction(ForegroundSurface::BuiltIn(kind), intent)
                .map(|_| ())
                .map_err(|error| error.to_string())
        })?;
        // Admission succeeds before either participant updates its accepted request or terminal transaction ticket.
        self.accepted.set(Some(AcceptedClose {
            intent,
            scope,
            ticket,
        }));
        Ok(())
    }

    /// Consume this activation's once-only barrier and route only the accepted scope's allowed successor.
    fn finish_close(
        &self,
        kind: BuiltInSurface,
        intent: SurfaceCloseIntent,
        scope: PluginCloseScope,
    ) {
        let Some(accepted) = self.accepted.get() else {
            // A raw native callback has no completion authority without a previously accepted scoped request.
            eprintln!("Built-in completion has no accepted close request");
            return;
        };
        if kind != self.kind
            || self.finished.get()
            || accepted.intent != intent
            || accepted.scope != scope
        {
            // An old/pre-promotion or duplicate completion cannot consume the current owner's exact barrier.
            eprintln!("Built-in completion does not match its accepted close request");
            return;
        }
        if scope == PluginCloseScope::ApplicationTermination
            && (accepted.ticket.is_none()
                || accepted.ticket != super::termination::application_termination_ticket())
        {
            // A cancelled quit's terminal receipt cannot be reused during a later real quit transaction.
            eprintln!("Built-in completion has a stale application termination ticket");
            return;
        }
        // The built-in supplied actual full graph release or the stricter terminal payload/empty-shell receipt.
        let completion = super::FOREGROUND_STATE.with_borrow(|state| {
            let surface = ForegroundSurface::BuiltIn(kind);
            if state.pending_transition()
                != Some(SurfaceTransition::Destroying {
                    surface,
                    restore_launcher: intent == SurfaceCloseIntent::ExitToLauncher,
                })
            {
                // Validate direction before consuming state: identity alone cannot authorize a different successor.
                return Err("Built-in foreground close direction changed".to_owned());
            }
            let restore = state
                .complete_destruction(surface)
                .map_err(|error| error.to_string())?;
            // One consumed project barrier enables a successor after resource ownership ends.
            state
                .consume_destroyed_barrier(surface)
                .map_err(|error| error.to_string())?;
            Ok::<bool, String>(restore)
        });
        match completion {
            Ok(restore) => {
                // Consume once before any native successor or termination reply can synchronously reenter this owner.
                self.finished.set(true);
                // An intent mismatch is a contract failure, never permission for a second window.
                if restore != (intent == SurfaceCloseIntent::ExitToLauncher) {
                    // Preserve zero surfaces rather than guessing an inconsistent successor.
                    eprintln!("built-in close intent disagrees with foreground ownership");
                    return;
                }
                #[cfg(feature = "native-acceptance")]
                if scope == PluginCloseScope::ApplicationTermination {
                    // Observe only the real built-in release receipt after the exact native
                    // cohort completed and this activation consumed its Host destruction barrier.
                    super::acceptance_termination::record_terminal_release();
                }
                let quitting = super::termination::foreground_did_release();
                if scope == PluginCloseScope::ApplicationTermination || quitting {
                    // The accepted original close finishes, but application termination consumes its successor slot.
                    return;
                }
                if restore && let Some(mtm) = MainThreadMarker::new() {
                    // Enter native effects only after all foreground state borrows ended.
                    super::show_launcher_surface(mtm);
                }
            }
            Err(error) => {
                // Report only a bounded lifecycle category, with no content or private paths.
                eprintln!("built-in finalization rejected: {error}");
            }
        }
    }
}

/// Translate the catalog's built-in command into its stable domain capability.
pub(super) fn from_command(command: &str) -> Option<BuiltInSurface> {
    // Built-ins are application capabilities; plugins retain a separate generic protocol path.
    match command {
        // Clipboard owns foreground-only monitoring and bounded persisted history.
        "clipboard" => Some(BuiltInSurface::Clipboard),
        // Calculator evaluates bounded expressions without launching a process.
        "calc" => Some(BuiltInSurface::Calculator),
        // Conversion uses explicit supported units and dimensions.
        "convert" => Some(BuiltInSurface::UnitConversion),
        // Settings operates on the host's existing shortcut registration.
        "settings" => Some(BuiltInSurface::Settings),
        // Unknown catalog commands cannot acquire native resources.
        _ => None,
    }
}

/// Prepare and activate one built-in, rolling back only its own failed creation claim.
pub(super) fn activate(kind: BuiltInSurface, mtm: MainThreadMarker) -> Result<(), String> {
    if super::termination::suppress_successors() {
        // A queued built-in action cannot allocate a competing graph after app quit preparation starts.
        return Err("Application termination is in progress".to_owned());
    }

    // Claim the foreground slot before any feature-specific resource allocation.
    let target = ResumeTarget::BuiltIn(kind);
    super::FOREGROUND_STATE
        .with_borrow(|state| state.begin_creation_for(target))
        .map_err(|error| error.to_string())?;
    let prepared = (|| {
        // Directory resolution remains lazy and does not create a Markdown workspace.
        let locations = super::native_plugin_storage_locations()?;
        let context = BuiltInContext {
            application_data: locations.application_support,
            lifecycle: Rc::new(Lifecycle::new(kind)),
            shortcuts: Rc::new(NativeShortcutPreferences),
        };
        builtins::show(kind, context, mtm)
    })();
    if prepared.is_err() {
        // A rejected construction cannot leave a pending claim that blocks Launcher recovery.
        let _ = super::FOREGROUND_STATE.with_borrow(|state| state.cancel_creation(target));
    }
    prepared
}
