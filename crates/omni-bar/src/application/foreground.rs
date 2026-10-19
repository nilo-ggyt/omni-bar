//! Exclusive foreground ownership, represented without any native object graph.
//!
//! The main-thread adapter performs native effects between these short transitions.
//! No state borrow is held while AppKit can synchronously reenter the application.

#![forbid(unsafe_code)]

use omni_plugin_sdk::PluginCommandTarget;
use std::cell::{Ref, RefCell, RefMut};

/// Stable built-in capabilities independent of a particular native control tree.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BuiltInSurface {
    /// Reopen clipboard persistence only when the new foreground session is activated.
    Clipboard,
    /// Start a fresh expression editor without retaining the previous control graph.
    Calculator,
    /// Reconstruct the conversion controls from the static unit catalog.
    UnitConversion,
    /// Read the actual persisted shortcut state through a newly activated settings surface.
    Settings,
}

/// The one business surface permitted to own foreground resources.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ForegroundSurface {
    /// The search surface currently owns the one foreground slot.
    Launcher,
    /// Static registration and command identity name the active plugin session.
    Plugin(PluginCommandTarget),
    /// A Host capability owns its session independently of any concrete native widget.
    BuiltIn(BuiltInSurface),
}

/// A fixed-size resume identity that retains neither windows nor document state.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ResumeTarget {
    /// Resume search instead of retaining the previous Launcher query or controls.
    #[default]
    Launcher,
    /// Reinvoke this static command; the previous document/session is not stored here.
    Plugin(PluginCommandTarget),
    /// Recreate this capability and let its adapter acquire any persisted data.
    BuiltIn(BuiltInSurface),
}

impl From<ForegroundSurface> for ResumeTarget {
    fn from(surface: ForegroundSurface) -> Self {
        // Preserve only the semantic identity after a foreground session is destroyed.
        match surface {
            // Launcher resumes as a fresh search surface.
            ForegroundSurface::Launcher => Self::Launcher,
            // A static target identifies plugin code without retaining its session.
            ForegroundSurface::Plugin(target) => Self::Plugin(target),
            // Built-in state is reconstructed from persistence on the next activation.
            ForegroundSurface::BuiltIn(feature) => Self::BuiltIn(feature),
        }
    }
}

/// Whether destruction leaves the app hidden or restores a fresh Launcher.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SurfaceCloseIntent {
    /// Hide the current surface and remember its capability for the next explicit summon.
    DismissAndResume,
    /// Release this session and request a fresh Launcher after the handoff barrier.
    ExitToLauncher,
}

/// A bounded transition claim consumed by one native creation/destruction owner.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SurfaceTransition {
    /// Reserve the target while construction may still own unpublished native resources.
    Creating {
        /// The capability reserved before the adapter acquires its unpublished resources.
        target: ResumeTarget,
    },
    /// Preserve the current owner and accepted successor policy throughout teardown.
    Destroying {
        /// The existing owner whose completion must match this close claim.
        surface: ForegroundSurface,
        /// Preserve the accepted successor decision even if another request arrives during close.
        restore_launcher: bool,
    },
}

impl SurfaceTransition {
    #[must_use]
    /// Resolve the semantic identity of the owner involved in either transition phase.
    pub fn target(self) -> ResumeTarget {
        // Both phases identify the eventual or previously owned semantic target.
        match self {
            // Creation already stores its intended resume identity.
            Self::Creating { target } => target,
            // Destruction discards the current graph but remembers its capability.
            Self::Destroying { surface, .. } => surface.into(),
        }
    }

    #[must_use]
    /// Name the native owner whose callback must match this transition.
    pub fn surface(self) -> ForegroundSurface {
        // Native adapters use the same identity in callbacks from either phase.
        match self {
            // Convert a creation target without creating or retaining any native object.
            Self::Creating { target } => target.into(),
            // A destruction callback must name the surface whose claim is active.
            Self::Destroying { surface, .. } => surface,
        }
    }
}

#[derive(Debug, Default)]
struct ForegroundSurfaceSnapshot {
    /// Logical ownership remains occupied while the corresponding destruction claim is pending.
    active: Option<ForegroundSurface>,
    /// One construction or destruction claim prevents reentrant acquisition of a second surface.
    transition: Option<SurfaceTransition>,
    /// Dormant state retains a copyable capability identity rather than its old session payload.
    resume_target: ResumeTarget,
    /// Project handoff barrier after callback-stack-safe native cleanup. This is
    /// not an AppKit event name or independent evidence of object deallocation.
    destroyed_barrier: Option<ForegroundSurface>,
}

/// Serial main-thread ownership; effects execute only after each borrow ends.
#[derive(Debug, Default)]
pub struct ForegroundSurfaceState {
    snapshot: RefCell<ForegroundSurfaceSnapshot>,
}

impl ForegroundSurfaceState {
    #[must_use]
    /// Initialize an empty foreground with only a static Launcher resume identity.
    pub fn new() -> Self {
        // A fresh process has no surface and resumes Launcher by default.
        Self::default()
    }

    #[must_use]
    /// Copy current ownership without letting a state borrow cross a native call.
    pub fn active_surface(&self) -> Option<ForegroundSurface> {
        // Return a copied identity, never a borrow that can cross a native call.
        self.snapshot_guard().active
    }

    #[must_use]
    /// Observe the current creation/close claim for coalescing and reentrancy guards.
    pub fn pending_transition(&self) -> Option<SurfaceTransition> {
        // A copied claim lets callers decide to coalesce without retaining state access.
        self.snapshot_guard().transition
    }

    #[must_use]
    /// Resolve the bounded identity to reconstruct on the next explicit summon.
    pub fn resume_target(&self) -> ResumeTarget {
        // Idle state retains only this bounded target, not the prior resource graph.
        self.snapshot_guard().resume_target
    }

    #[must_use]
    /// Observe the once-only cleanup acknowledgement still blocking successor creation.
    pub fn destroyed_barrier(&self) -> Option<ForegroundSurface> {
        // A pending barrier prevents a successor until cleanup is explicitly consumed.
        self.snapshot_guard().destroyed_barrier
    }

    /// Record an already-prepared surface only when no other ownership claim exists.
    pub fn remember_surface(&self, surface: ForegroundSurface) -> Result<(), SurfaceStateError> {
        let mut snapshot = self.snapshot_guard_mut();
        if snapshot.active.is_some()
            || snapshot.transition.is_some()
            || snapshot.destroyed_barrier.is_some()
        {
            // Active, transitioning, or unreleased owners cannot be overwritten.
            return Err(SurfaceStateError::SurfaceAlreadyOwned);
        }
        // Commit foreground ownership and its future resume identity together.
        snapshot.active = Some(surface);
        snapshot.resume_target = surface.into();
        Ok(())
    }

    /// Claim creation for the remembered target before the adapter acquires resources.
    pub fn begin_creation(&self) -> Result<ResumeTarget, SurfaceStateError> {
        // End the read borrow before taking the separate transition mutation.
        let target = self.resume_target();
        self.begin_creation_for(target)
    }

    /// Claim a requested target after the previous project teardown barrier is consumed.
    pub fn begin_creation_for(
        &self,
        target: ResumeTarget,
    ) -> Result<ResumeTarget, SurfaceStateError> {
        let mut snapshot = self.snapshot_guard_mut();
        if snapshot.active.is_some() {
            // A visible/closing owner must finish its destruction path first.
            return Err(SurfaceStateError::SurfaceAlreadyOwned);
        }
        if snapshot.destroyed_barrier.is_some() {
            // Cleanup acknowledgement is distinct from clearing the active identity.
            return Err(SurfaceStateError::DestroyedBarrierPending);
        }
        if snapshot.transition.is_some() {
            // Reentrant activation cannot create a second transition owner.
            return Err(SurfaceStateError::TransitionAlreadyPending);
        }
        // Reserve creation while leaving resource acquisition to the platform adapter.
        snapshot.transition = Some(SurfaceTransition::Creating { target });
        Ok(target)
    }

    /// Construction failed before a native surface became visible.  Clear only
    /// the matching creation transition so a recovery path can safely choose
    /// Launcher without disturbing an unrelated owner.
    pub fn cancel_creation(&self, target: ResumeTarget) -> Result<(), SurfaceStateError> {
        let mut snapshot = self.snapshot_guard_mut();
        if snapshot.transition != Some(SurfaceTransition::Creating { target }) {
            // A failure from an older or different request cannot erase the current claim.
            return Err(SurfaceStateError::UnexpectedLifecycleEvent);
        }
        // Failed construction releases only its own bounded transition state.
        snapshot.transition = None;
        if snapshot.active.is_none() {
            // Recovery defaults to Launcher rather than retrying a failed feature forever.
            snapshot.resume_target = ResumeTarget::Launcher;
        }
        Ok(())
    }

    /// Commit a prepared surface once, without invoking native code while state is borrowed.
    pub fn complete_creation(
        &self,
        target: ResumeTarget,
    ) -> Result<ForegroundSurface, SurfaceStateError> {
        let mut snapshot = self.snapshot_guard_mut();
        if snapshot.transition != Some(SurfaceTransition::Creating { target }) {
            // Completion cannot manufacture ownership without a matching creation claim.
            return Err(SurfaceStateError::UnexpectedLifecycleEvent);
        }
        // Replace the claim with one active identity and remember it for later resume.
        let surface = target.into();
        snapshot.transition = None;
        snapshot.active = Some(surface);
        snapshot.resume_target = target;
        Ok(surface)
    }

    /// Claim one close intent; successors remain forbidden until native cleanup finishes.
    /// Equivalent requests report the existing claim as an error; adapters use `coalesces`
    /// to acknowledge that same owner without beginning destruction a second time.
    pub fn begin_destruction(
        &self,
        surface: ForegroundSurface,
        intent: SurfaceCloseIntent,
    ) -> Result<SurfaceTransition, SurfaceStateError> {
        let mut snapshot = self.snapshot_guard_mut();
        if snapshot.active != Some(surface) {
            // Only the current native owner may begin its destruction.
            return Err(SurfaceStateError::SurfaceNotOwned);
        }
        if let Some(pending) = snapshot.transition {
            // Distinguish a repeated request from an opposing successor policy.
            let requested = SurfaceTransition::Destroying {
                surface,
                restore_launcher: intent == SurfaceCloseIntent::ExitToLauncher,
            };
            return if pending == requested {
                // The existing close owner handles this equivalent request.
                Err(SurfaceStateError::TransitionAlreadyPending)
            } else {
                // Preserve the original intent instead of replacing it during close.
                Err(SurfaceStateError::OpposingTransitionPending)
            };
        }
        // Retain active identity until the adapter's finalization path proves cleanup order.
        let transition = SurfaceTransition::Destroying {
            surface,
            restore_launcher: intent == SurfaceCloseIntent::ExitToLauncher,
        };
        snapshot.transition = Some(transition);
        Ok(transition)
    }

    /// A synchronous native close failure leaves the surface visible.  Cancel
    /// only the matching destroy request and keep ownership with that surface.
    pub fn cancel_destruction(
        &self,
        surface: ForegroundSurface,
    ) -> Result<SurfaceCloseIntent, SurfaceStateError> {
        let mut snapshot = self.snapshot_guard_mut();
        let Some(SurfaceTransition::Destroying {
            surface: expected,
            restore_launcher,
        }) = snapshot.transition
        else {
            // An unrelated callback cannot cancel a creation or an idle state.
            return Err(SurfaceStateError::UnexpectedLifecycleEvent);
        };
        if expected != surface || snapshot.active != Some(surface) {
            // Cancellation must match both the claim and its current resource owner.
            return Err(SurfaceStateError::UnexpectedLifecycleEvent);
        }
        // The close request failed synchronously; keep the original surface active.
        snapshot.transition = None;
        Ok(if restore_launcher {
            // Return the abandoned return-to-Launcher intent to its caller.
            SurfaceCloseIntent::ExitToLauncher
        } else {
            // Return the abandoned temporary-dismiss intent without losing ownership.
            SurfaceCloseIntent::DismissAndResume
        })
    }

    /// Record finalized native cleanup, leaving a separately consumed handoff barrier.
    pub fn complete_destruction(
        &self,
        surface: ForegroundSurface,
    ) -> Result<bool, SurfaceStateError> {
        let mut snapshot = self.snapshot_guard_mut();
        let Some(SurfaceTransition::Destroying {
            surface: expected,
            restore_launcher,
        }) = snapshot.transition
        else {
            // A close notification by itself is insufficient without a matching claim.
            return Err(SurfaceStateError::UnexpectedLifecycleEvent);
        };
        if expected != surface || snapshot.active != Some(surface) {
            // A stale finalizer must not release the identity of a newer surface.
            return Err(SurfaceStateError::UnexpectedLifecycleEvent);
        }
        // The adapter reports its cleanup phase before this update; this pure state cannot
        // inspect native objects or establish deallocation. The adapter still must
        // invalidate borrowed handles and drop its session before consuming the barrier.
        snapshot.active = None;
        snapshot.transition = None;
        snapshot.destroyed_barrier = Some(surface);
        Ok(restore_launcher)
    }

    /// Consume exactly one project teardown barrier before creating a
    /// successor.  Calling this twice is an error and cannot create a second
    /// business surface.
    pub fn consume_destroyed_barrier(
        &self,
        surface: ForegroundSurface,
    ) -> Result<(), SurfaceStateError> {
        let mut snapshot = self.snapshot_guard_mut();
        if snapshot.destroyed_barrier != Some(surface) {
            // Duplicate or mismatched acknowledgement cannot enable another successor.
            return Err(SurfaceStateError::UnexpectedLifecycleEvent);
        }
        // All ownership cleanup is acknowledged; a later creation may now claim the slot.
        snapshot.destroyed_barrier = None;
        Ok(())
    }

    /// Recognize an equivalent request without acquiring a second transition owner.
    #[must_use]
    pub fn coalesces(&self, transition: SurfaceTransition) -> bool {
        // Compare copied identities rather than retaining mutable transition access.
        self.pending_transition() == Some(transition)
    }

    /// Borrow state only for a pure transition/read with no framework effects.
    fn snapshot_guard(&self) -> Ref<'_, ForegroundSurfaceSnapshot> {
        // RefCell makes the single-thread boundary explicit without an idle mutex.
        self.snapshot.borrow()
    }

    /// Mutate one short transition before its caller performs any reentrant native work.
    fn snapshot_guard_mut(&self) -> RefMut<'_, ForegroundSurfaceSnapshot> {
        // No reference returned by a public method can carry this borrow into AppKit.
        self.snapshot.borrow_mut()
    }
}

impl From<ResumeTarget> for ForegroundSurface {
    fn from(target: ResumeTarget) -> Self {
        // This conversion names an owner but allocates no resource graph.
        match target {
            // Launcher always starts from fresh search state.
            ResumeTarget::Launcher => Self::Launcher,
            // Static plugin metadata is safe to retain while its session is absent.
            ResumeTarget::Plugin(target) => Self::Plugin(target),
            // Built-in persistent data is reopened by its new session owner.
            ResumeTarget::BuiltIn(feature) => Self::BuiltIn(feature),
        }
    }
}

/// Deterministic ownership/ordering failures, without private runtime payloads.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SurfaceStateError {
    /// Creation or replacement would overwrite an active owner or an outstanding claim.
    SurfaceAlreadyOwned,
    /// A close request names a different surface from the current foreground owner.
    SurfaceNotOwned,
    /// An equivalent operation already has the only accepted transition claim.
    TransitionAlreadyPending,
    /// A new request would replace the first close owner's successor policy.
    OpposingTransitionPending,
    /// Cleanup has been reported, but the adapter has not consumed the handoff acknowledgement.
    DestroyedBarrierPending,
    /// A stale, duplicate or out-of-order completion does not match the current claim.
    UnexpectedLifecycleEvent,
}

impl std::fmt::Display for SurfaceStateError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Keep product messages bounded and independent from pointers/document paths.
        let message = match self {
            // Another active or transitional owner blocks this request.
            Self::SurfaceAlreadyOwned => "前台 surface 已被占用",
            // The requested capability cannot close another owner's resource graph.
            Self::SurfaceNotOwned => "请求的 surface 不属于当前 owner",
            // Equivalent requests coalesce at the adapter's existing close owner.
            Self::TransitionAlreadyPending => "前台 surface 转换正在进行",
            // Keep the first intent when another request asks for a different successor.
            Self::OpposingTransitionPending => "相反方向的前台 surface 转换正在进行",
            // Final cleanup acknowledgement must precede the next acquisition.
            Self::DestroyedBarrierPending => "上一个 native surface 尚未完成 Destroyed 屏障",
            // A stale, duplicate, or out-of-order callback cannot mutate current ownership.
            Self::UnexpectedLifecycleEvent => "原生生命周期事件与当前转换不匹配",
        };
        // Rendering the selected static string cannot expose application content.
        formatter.write_str(message)
    }
}

impl std::error::Error for SurfaceStateError {}

#[cfg(test)]
mod tests {
    //! Transition ordering and recovery are verified without creating native windows.
    use super::*;

    const PLUGIN_TARGET: PluginCommandTarget = PluginCommandTarget {
        registration: crate::plugins::MARKDOWN_REGISTRATION,
        command_id: "open",
    };

    /// Creation and completion cannot produce two concurrent foreground owners.
    #[test]
    fn creation_and_destroy_use_one_surface_owner() {
        // A fresh state grants Launcher one creation claim and rejects a competing owner.
        let state = ForegroundSurfaceState::new();
        assert_eq!(state.begin_creation().unwrap(), ResumeTarget::Launcher);
        assert_eq!(
            state.complete_creation(ResumeTarget::Launcher).unwrap(),
            ForegroundSurface::Launcher
        );
        assert!(matches!(
            state.begin_creation(),
            Err(SurfaceStateError::SurfaceAlreadyOwned)
        ));

        // Accepted exit intent fixes the successor policy until this owner completes cleanup.
        let transition = state
            .begin_destruction(
                ForegroundSurface::Launcher,
                SurfaceCloseIntent::ExitToLauncher,
            )
            .unwrap();
        assert_eq!(
            transition,
            SurfaceTransition::Destroying {
                surface: ForegroundSurface::Launcher,
                restore_launcher: true,
            }
        );
        assert!(
            state
                .complete_destruction(ForegroundSurface::Launcher)
                .unwrap()
        );
        assert_eq!(state.active_surface(), None);
    }

    /// Repeated closes preserve their accepted intent while opposing successors are rejected.
    #[test]
    fn repeated_same_direction_requests_coalesce() {
        // Seed one static plugin identity without constructing a session or native graph.
        let state = ForegroundSurfaceState::new();
        state
            .remember_surface(ForegroundSurface::Plugin(PLUGIN_TARGET))
            .unwrap();
        let transition = state
            .begin_destruction(
                ForegroundSurface::Plugin(PLUGIN_TARGET),
                SurfaceCloseIntent::DismissAndResume,
            )
            .unwrap();
        // Repeats are recognized while an opposite intent cannot replace the existing close claim.
        assert!(state.coalesces(transition));
        assert!(matches!(
            state.begin_destruction(
                ForegroundSurface::Plugin(PLUGIN_TARGET),
                SurfaceCloseIntent::DismissAndResume
            ),
            Err(SurfaceStateError::TransitionAlreadyPending)
        ));
        assert!(matches!(
            state.begin_destruction(
                ForegroundSurface::Plugin(PLUGIN_TARGET),
                SurfaceCloseIntent::ExitToLauncher
            ),
            Err(SurfaceStateError::OpposingTransitionPending)
        ));
    }

    /// A failed construction clears its own claim and restores a usable recovery target.
    #[test]
    fn failed_creation_recovers_to_launcher_without_retaining_surface() {
        // Fail a claimed plugin construction before ownership becomes active.
        let state = ForegroundSurfaceState::new();
        assert_eq!(
            state
                .begin_creation_for(ResumeTarget::Plugin(PLUGIN_TARGET))
                .unwrap(),
            ResumeTarget::Plugin(PLUGIN_TARGET)
        );
        state
            .cancel_creation(ResumeTarget::Plugin(PLUGIN_TARGET))
            .unwrap();

        assert_eq!(state.active_surface(), None);
        // Recovery discards only the failed descriptor claim and allows a fresh Launcher creation.
        assert_eq!(state.pending_transition(), None);
        assert_eq!(state.resume_target(), ResumeTarget::Launcher);
        assert_eq!(state.begin_creation().unwrap(), ResumeTarget::Launcher);
    }

    /// Save/close rejection leaves the current surface and its data usable.
    #[test]
    fn synchronous_destruction_failure_keeps_surface_owned() {
        // Establish one live plugin owner whose native save/close may reject the requested exit.
        let state = ForegroundSurfaceState::new();
        state
            .remember_surface(ForegroundSurface::Plugin(PLUGIN_TARGET))
            .unwrap();
        state
            .begin_destruction(
                ForegroundSurface::Plugin(PLUGIN_TARGET),
                SurfaceCloseIntent::ExitToLauncher,
            )
            .unwrap();

        // Cancelling the exact close restores use of the same owner rather than dropping its data.
        assert_eq!(
            state
                .cancel_destruction(ForegroundSurface::Plugin(PLUGIN_TARGET))
                .unwrap(),
            SurfaceCloseIntent::ExitToLauncher
        );
        assert_eq!(
            state.active_surface(),
            Some(ForegroundSurface::Plugin(PLUGIN_TARGET))
        );
        assert_eq!(state.pending_transition(), None);
    }

    /// A will-close state update alone cannot authorize a successor before resource cleanup.
    #[test]
    fn destruction_barrier_must_be_consumed_before_successor_creation() {
        // Complete logical destruction while retaining the explicit project handoff barrier.
        let state = ForegroundSurfaceState::new();
        state.remember_surface(ForegroundSurface::Launcher).unwrap();
        state
            .begin_destruction(
                ForegroundSurface::Launcher,
                SurfaceCloseIntent::ExitToLauncher,
            )
            .unwrap();
        assert!(
            state
                .complete_destruction(ForegroundSurface::Launcher)
                .unwrap()
        );
        assert_eq!(state.destroyed_barrier(), Some(ForegroundSurface::Launcher));
        assert!(matches!(
            state.begin_creation_for(ResumeTarget::Plugin(PLUGIN_TARGET)),
            Err(SurfaceStateError::DestroyedBarrierPending)
        ));

        // Only the once-only acknowledgement permits a new plugin creation claim.
        state
            .consume_destroyed_barrier(ForegroundSurface::Launcher)
            .unwrap();
        assert_eq!(
            state
                .begin_creation_for(ResumeTarget::Plugin(PLUGIN_TARGET))
                .unwrap(),
            ResumeTarget::Plugin(PLUGIN_TARGET)
        );
    }

    /// Idle resume metadata identifies a capability without retaining its resource graph.
    #[test]
    fn resume_target_contains_only_the_surface_kind() {
        // The clipboard resume identity contains no controls, mappings, timers or history payload.
        let state = ForegroundSurfaceState::new();
        state
            .remember_surface(ForegroundSurface::BuiltIn(BuiltInSurface::Clipboard))
            .unwrap();
        assert_eq!(
            state.resume_target(),
            ResumeTarget::BuiltIn(BuiltInSurface::Clipboard)
        );
    }
}
