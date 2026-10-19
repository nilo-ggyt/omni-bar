//! Pure application-termination sequencing, independent of AppKit and any concrete plugin.
//!
//! A successful close request is not completion. The adapter must observe released native owners,
//! the dropped SDK session and the consumed foreground barrier before reporting `Released`.
//! The adapter establishes the applicable native release facts; this coordinator stores no native
//! handle and cannot turn a close notification or elapsed time into release authority.

#![forbid(unsafe_code)]

use super::foreground::SurfaceTransition;

/// Foreground readiness copied before a native effect; no window/session owner crosses this boundary.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TerminationReadiness {
    /// Every native adapter is empty and the foreground barrier has been consumed.
    Released,
    /// One stable active surface may execute its existing fallible dismissal path.
    Active,
    /// An accepted close already owns its immutable intent and eventual release signal.
    Closing,
    /// Creation or an unaccepted reentrant operation cannot safely accept application termination.
    Busy,
}

/// One copied owner inventory combines logical transition claims with adapters that may still hold resources.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct TerminationOwnership {
    /// A committed logical surface can run its existing fallible close preflight.
    pub(crate) active_surface: bool,
    /// Construction and destruction claims have different reentrant-termination consequences.
    pub(crate) transition: Option<SurfaceTransition>,
    /// A reported destruction still blocks successors until its handoff acknowledgement is consumed.
    pub(crate) destroyed_barrier: bool,
    /// Adapter ownership catches unpublished or retained resources absent from the logical surface slot.
    pub(crate) adapter_owned: bool,
    /// A native/SDK call still on the stack must return before AppKit enters a modal termination wait.
    pub(crate) plugin_call_in_flight: bool,
}

impl TerminationOwnership {
    /// Native calls still on the stack veto reentrant termination before an accepted close can be awaited.
    pub(crate) fn readiness(self) -> TerminationReadiness {
        if matches!(self.transition, Some(SurfaceTransition::Creating { .. })) {
            // A constructor may own an unpublished graph, so quitting cannot safely interrupt its local effects.
            return TerminationReadiness::Busy;
        }
        if self.plugin_call_in_flight {
            // A nested terminate call can enter AppKit's modal wait and block the original SDK/finalizer stack.
            return TerminationReadiness::Busy;
        }
        if matches!(self.transition, Some(SurfaceTransition::Destroying { .. }))
            || self.destroyed_barrier
        {
            // Existing accepted destruction may be awaited only after the original SDK call has returned.
            return TerminationReadiness::Closing;
        }
        if self.active_surface {
            // A stable committed surface can run its ordinary fallible generic dismissal preflight.
            return TerminationReadiness::Active;
        }
        if self.adapter_owned {
            // Logical emptiness cannot hide an unpublished/still-retained adapter graph.
            return TerminationReadiness::Busy;
        }
        // Only the joint absence of logical claims and adapter owners permits an immediate exit.
        TerminationReadiness::Released
    }
}

/// One preparation invocation's identity prevents stale returns from altering a newer request.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct TerminationTicket(u64);

/// Work selected for one application-delegate invocation after its short state borrow ends.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TerminationRequest {
    /// No foreground work remains; AppKit can terminate directly without a later reply.
    Now,
    /// Preserve the current usable owner because safe preflight cannot run or has failed.
    Cancel,
    /// AppKit is already waiting on the one accepted asynchronous release transaction.
    Later,
    /// The adapter must request close or attach to an existing accepted close, then report its outcome.
    Prepare(TerminationTicket),
}

/// A release signal may authorize exactly one reply after the delegate has returned `Later`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TerminationReply {
    /// Preparing, idle or duplicate/stale signals never send an asynchronous AppKit reply.
    None,
    /// The single outstanding asynchronous termination may now complete.
    Allow,
}

/// Preparing is deliberately distinct from Waiting because close may complete synchronously.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum Phase {
    /// No application quit owns the foreground; ordinary activation remains permitted.
    #[default]
    Idle,
    /// Native preflight is on the delegate stack, before any Later reply obligation can exist.
    Preparing {
        ticket: TerminationTicket,
        /// A synchronous completion is remembered until the caller supplies a fresh readiness snapshot.
        released: bool,
    },
    /// The delegate has selected Later and only one actual release signal can authorize its reply.
    Waiting { ticket: TerminationTicket },
    /// All ownership has ended and successor activation remains sealed until AppKit exits.
    Allowed { ticket: Option<TerminationTicket> },
}

/// One process-lifetime termination transaction stores only scalar identity and sequencing state.
#[derive(Debug, Default)]
pub(crate) struct ApplicationTermination {
    last_ticket: u64,
    phase: Phase,
}

impl ApplicationTermination {
    /// An idle resident process accepts future termination without allocating background resources.
    pub(crate) const fn new() -> Self {
        // Ticket zero is never issued; the first actual foreground preparation receives a fresh identity.
        Self {
            last_ticket: 0,
            phase: Phase::Idle,
        }
    }

    /// Block all successor activation from the first accepted preparation through actual process exit.
    pub(crate) fn suppress_successors(&self) -> bool {
        self.phase != Phase::Idle
    }

    /// Return accepted coordinator identity, excluding both idle state and an owner-free immediate quit.
    pub(crate) fn transaction_ticket(&self) -> Option<TerminationTicket> {
        match self.phase {
            // Preparing and Waiting preserve the same exact transaction across synchronous or delayed release.
            Phase::Preparing { ticket, .. } | Phase::Waiting { ticket } => Some(ticket),
            // Allowed retains the completed identity only when a real foreground preparation existed.
            Phase::Allowed { ticket } => ticket,
            // A merely queued programmatic reservation is outside this pure coordinator and has no ticket.
            Phase::Idle => None,
        }
    }

    /// Decide a delegate response or reserve one fallible close preparation without native work.
    pub(crate) fn request(&mut self, readiness: TerminationReadiness) -> TerminationRequest {
        match self.phase {
            Phase::Allowed { .. } => {
                // A repeated native request after completion cannot reopen foreground ownership.
                return TerminationRequest::Now;
            }
            Phase::Waiting { .. } => {
                // Repeated Quit attaches to the existing single reply obligation rather than creating another one.
                return TerminationRequest::Later;
            }
            Phase::Preparing { .. } => {
                // A nested event during save/composition cannot acquire another close or premature reply obligation.
                return TerminationRequest::Cancel;
            }
            Phase::Idle => {
                // Only an idle coordinator may evaluate a fresh foreground readiness snapshot.
            }
        }
        match readiness {
            TerminationReadiness::Released => {
                // Native/session/barrier release is already proven, so no deferred completion is necessary.
                self.phase = Phase::Allowed { ticket: None };
                TerminationRequest::Now
            }
            TerminationReadiness::Busy => {
                // Creation and unaccepted native reentry preserve their original owner and allow a later explicit retry.
                TerminationRequest::Cancel
            }
            TerminationReadiness::Active | TerminationReadiness::Closing => {
                // Both a new dismissal and an existing accepted close need one protected preparation return.
                let Some(next) = self.last_ticket.checked_add(1) else {
                    // Identity exhaustion rejects termination rather than accepting a stale completion alias.
                    return TerminationRequest::Cancel;
                };
                self.last_ticket = next;
                let ticket = TerminationTicket(next);
                self.phase = Phase::Preparing {
                    ticket,
                    released: false,
                };
                TerminationRequest::Prepare(ticket)
            }
        }
    }

    /// Publish the close result after native effects, before the delegate returns its first reply.
    pub(crate) fn prepared(
        &mut self,
        ticket: TerminationTicket,
        accepted: bool,
        readiness: TerminationReadiness,
    ) -> TerminationRequest {
        let Phase::Preparing {
            ticket: current,
            released,
        } = self.phase
        else {
            // A stale/unmatched return cannot convert a different phase into termination authority.
            return TerminationRequest::Cancel;
        };
        if ticket != current {
            // Preserve a newer transaction when a historical preparation reports late.
            return TerminationRequest::Cancel;
        }
        if !accepted {
            // Fallible save/composition/close refusal restores normal interaction without discarding its owner.
            self.phase = Phase::Idle;
            return TerminationRequest::Cancel;
        }
        if readiness == TerminationReadiness::Released {
            // Synchronous completion returns Now; no asynchronous reply may precede AppKit's wait.
            self.phase = Phase::Allowed {
                ticket: Some(ticket),
            };
            return TerminationRequest::Now;
        }
        if released || readiness != TerminationReadiness::Closing {
            // An inconsistent post-close owner cannot be hidden by an earlier signal or an accepted-method result.
            self.phase = Phase::Idle;
            return TerminationRequest::Cancel;
        }
        // A real accepted close now owns the only future release signal and AppKit reply obligation.
        self.phase = Phase::Waiting { ticket };
        TerminationRequest::Later
    }

    /// Accept only a complete adapter/session/barrier release observation from the current process owner.
    pub(crate) fn foreground_released(&mut self) -> TerminationReply {
        match self.phase {
            Phase::Preparing { ticket, .. } => {
                // Save/close can finish on its caller stack; remember it without replying before Later returns.
                self.phase = Phase::Preparing {
                    ticket,
                    released: true,
                };
                TerminationReply::None
            }
            Phase::Waiting { ticket } => {
                // Consume the one asynchronous reply authority before native dispatch can reenter the coordinator.
                self.phase = Phase::Allowed {
                    ticket: Some(ticket),
                };
                TerminationReply::Allow
            }
            Phase::Idle | Phase::Allowed { .. } => {
                // Normal dismissal, stale or duplicate completion cannot create another reply obligation.
                TerminationReply::None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Extract only an accepted preparation ticket so tests exercise the full state protocol explicitly.
    fn prepare(
        state: &mut ApplicationTermination,
        readiness: TerminationReadiness,
    ) -> TerminationTicket {
        let TerminationRequest::Prepare(ticket) = state.request(readiness) else {
            // A test setup that failed to reserve preparation cannot meaningfully exercise its return protocol.
            panic!("expected one preparation");
        };
        ticket
    }

    /// An idle empty app exits immediately and duplicate release callbacks never send a later reply.
    #[test]
    fn empty_application_needs_no_deferred_reply() {
        // A fresh coordinator sees no foreground ownership and needs no asynchronous reply obligation.
        let mut state = ApplicationTermination::new();
        assert_eq!(state.transaction_ticket(), None);
        assert_eq!(
            state.request(TerminationReadiness::Released),
            TerminationRequest::Now
        );
        assert!(state.suppress_successors());
        assert_eq!(state.transaction_ticket(), None);
        assert_eq!(state.foreground_released(), TerminationReply::None);
        assert_eq!(
            state.request(TerminationReadiness::Released),
            TerminationRequest::Now
        );
    }

    /// A close completing on the delegate stack returns Now instead of replying to a wait that does not exist yet.
    #[test]
    fn synchronous_close_never_replies_before_the_delegate_returns() {
        // Hold preparation open while the simulated native completion arrives on its original call stack.
        let mut state = ApplicationTermination::new();
        let ticket = prepare(&mut state, TerminationReadiness::Active);
        assert_eq!(state.transaction_ticket(), Some(ticket));
        assert!(state.suppress_successors());
        assert_eq!(state.foreground_released(), TerminationReply::None);
        assert_eq!(
            state.prepared(ticket, true, TerminationReadiness::Released),
            TerminationRequest::Now
        );
        // Completed identity survives Allowed for exact late-receipt rejection while all new activation remains sealed.
        assert_eq!(state.transaction_ticket(), Some(ticket));
        assert_eq!(state.foreground_released(), TerminationReply::None);
    }

    /// Ordinary asynchronous release allows exactly one reply after Later, including repeated quit requests.
    #[test]
    fn asynchronous_close_coalesces_quit_and_consumes_one_reply() {
        // Begin with a real accepted close that still owns its eventual resource-release signal.
        let mut state = ApplicationTermination::new();
        let ticket = prepare(&mut state, TerminationReadiness::Active);
        assert_eq!(
            state.prepared(ticket, true, TerminationReadiness::Closing),
            TerminationRequest::Later
        );
        // Repeated Quit shares the first obligation; only the later real release consumes it.
        assert_eq!(
            state.request(TerminationReadiness::Closing),
            TerminationRequest::Later
        );
        assert!(state.suppress_successors());
        assert_eq!(state.foreground_released(), TerminationReply::Allow);
        assert_eq!(state.foreground_released(), TerminationReply::None);
        assert_eq!(
            state.request(TerminationReadiness::Released),
            TerminationRequest::Now
        );
    }

    /// Failure while visible input remains editable cancels termination and removes only its activation gate.
    #[test]
    fn save_or_composition_veto_preserves_interaction_and_can_retry() {
        // Refusal leaves foreground input active and must remove only the temporary application quit gate.
        let mut state = ApplicationTermination::new();
        let first = prepare(&mut state, TerminationReadiness::Active);
        assert_eq!(
            state.prepared(first, false, TerminationReadiness::Active),
            TerminationRequest::Cancel
        );
        assert!(!state.suppress_successors());
        assert_eq!(state.transaction_ticket(), None);
        assert_eq!(state.foreground_released(), TerminationReply::None);
        // A later explicit retry receives fresh authority; an old preparation return cannot cancel or approve it.
        let second = prepare(&mut state, TerminationReadiness::Active);
        assert_ne!(first, second);
        assert_eq!(
            state.prepared(first, true, TerminationReadiness::Released),
            TerminationRequest::Cancel
        );
        assert!(state.suppress_successors());
        assert_eq!(
            state.prepared(second, true, TerminationReadiness::Closing),
            TerminationRequest::Later
        );
    }

    /// Creation and nested save reentry cannot manufacture a second accepted close or overwrite its ticket.
    #[test]
    fn busy_creation_and_reentrant_quit_cancel_safely() {
        // A constructor owns unpublished resources and cannot be interrupted by a new quit transaction.
        let mut state = ApplicationTermination::new();
        assert_eq!(
            state.request(TerminationReadiness::Busy),
            TerminationRequest::Cancel
        );
        assert!(!state.suppress_successors());
        // The outer save owns preparation; a nested Quit must neither replace it nor establish a modal wait.
        let ticket = prepare(&mut state, TerminationReadiness::Active);
        assert_eq!(
            state.request(TerminationReadiness::Active),
            TerminationRequest::Cancel
        );
        assert!(state.suppress_successors());
        assert_eq!(
            state.prepared(ticket, false, TerminationReadiness::Active),
            TerminationRequest::Cancel
        );
        assert!(!state.suppress_successors());
    }

    /// An already accepted exit-to-Launcher close is awaited without changing its native intent or permitting a successor.
    #[test]
    fn existing_close_keeps_its_intent_while_successor_is_suppressed() {
        // An existing close is already authoritative; this transaction only consumes its eventual successor slot.
        let mut state = ApplicationTermination::new();
        let ticket = prepare(&mut state, TerminationReadiness::Closing);
        assert!(state.suppress_successors());
        assert_eq!(
            state.prepared(ticket, true, TerminationReadiness::Closing),
            TerminationRequest::Later
        );
        assert_eq!(state.foreground_released(), TerminationReply::Allow);
        assert!(state.suppress_successors());
    }

    /// A plugin returning success without beginning close cannot authorize a pending quit or lost owner.
    #[test]
    fn accepted_method_result_is_not_a_release_or_close_barrier() {
        // A buggy accepted-method response still leaves its owner active, which must veto application exit.
        let mut state = ApplicationTermination::new();
        let ticket = prepare(&mut state, TerminationReadiness::Active);
        assert_eq!(
            state.prepared(ticket, true, TerminationReadiness::Active),
            TerminationRequest::Cancel
        );
        assert!(!state.suppress_successors());
    }

    /// Releasing and then reacquiring ownership during preparation is rejected instead of trusting a historical signal.
    #[test]
    fn inconsistent_owner_after_synchronous_release_never_exits() {
        // A historical release signal cannot authorize exit after the live ownership snapshot contradicts it.
        let mut state = ApplicationTermination::new();
        let ticket = prepare(&mut state, TerminationReadiness::Active);
        assert_eq!(state.foreground_released(), TerminationReply::None);
        assert_eq!(
            state.prepared(ticket, true, TerminationReadiness::Closing),
            TerminationRequest::Cancel
        );
        assert!(!state.suppress_successors());
    }
    /// Real ownership classification prevents Creating and an unaccepted local SDK call from reaching close effects.
    #[test]
    fn logical_and_adapter_ownership_are_joint_termination_authority() {
        use super::super::foreground::{ForegroundSurface, ResumeTarget};
        // Adapter presence and an active logical owner must both participate in empty-app classification.
        let mut ownership = TerminationOwnership::default();
        assert_eq!(ownership.readiness(), TerminationReadiness::Released);
        ownership.adapter_owned = true;
        assert_eq!(ownership.readiness(), TerminationReadiness::Busy);
        ownership.active_surface = true;
        assert_eq!(ownership.readiness(), TerminationReadiness::Active);
        // A local SDK/native call blocks modal quit reentry whether activation or close has begun.
        ownership.plugin_call_in_flight = true;
        assert_eq!(ownership.readiness(), TerminationReadiness::Busy);
        ownership.transition = Some(SurfaceTransition::Creating {
            target: ResumeTarget::Launcher,
        });
        assert_eq!(ownership.readiness(), TerminationReadiness::Busy);
        ownership.transition = Some(SurfaceTransition::Destroying {
            surface: ForegroundSurface::Launcher,
            restore_launcher: true,
        });
        // Reentrant terminate must return Cancel so its modal wait cannot strand the original closing SDK stack.
        assert_eq!(ownership.readiness(), TerminationReadiness::Busy);
        ownership.plugin_call_in_flight = false;
        assert_eq!(ownership.readiness(), TerminationReadiness::Closing);
        // Even after logical ownership clears, an unconsumed handoff barrier is still pending destruction.
        ownership = TerminationOwnership {
            destroyed_barrier: true,
            ..TerminationOwnership::default()
        };
        assert_eq!(ownership.readiness(), TerminationReadiness::Closing);
    }

    /// Exhaustion refuses a new identity without suppressing normal interaction or wrapping a stale ticket.
    #[test]
    fn ticket_exhaustion_is_a_nonmutating_refusal() {
        // Exhaustion is synthetic but must fail before changing either phase or activation authority.
        let mut state = ApplicationTermination {
            last_ticket: u64::MAX,
            phase: Phase::Idle,
        };
        assert_eq!(
            state.request(TerminationReadiness::Active),
            TerminationRequest::Cancel
        );
        assert!(!state.suppress_successors());
    }
}
