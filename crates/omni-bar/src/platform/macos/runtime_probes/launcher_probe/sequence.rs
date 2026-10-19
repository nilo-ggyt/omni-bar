//! Separate probe progression from native objects; pure state cannot establish readiness or release.

/// Accept one readiness observation and one exact Host close receipt per sample.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Phase {
    Scheduled,
    AwaitingReady,
    AwaitingRelease { host_generation: u64 },
    Finished,
}

/// Deferred callbacks carry an epoch so a previous invocation cannot advance a later probe.
pub(super) struct Sequence {
    pub(super) epoch: u64,
    pub(super) sample: usize,
    pub(super) warm_samples: usize,
    pub(super) phase: Phase,
    queued_check: bool,
}

impl Sequence {
    /// Sample zero is cold; exactly warm_samples complete reconstructions follow it.
    pub(super) fn new(epoch: u64, warm_samples: usize) -> Self {
        Self {
            epoch,
            sample: 0,
            warm_samples,
            phase: Phase::Scheduled,
            queued_check: false,
        }
    }

    /// Only the scheduled phase may reserve this sample's creation authority.
    pub(super) fn begin_sample(&mut self, epoch: u64) -> bool {
        if self.epoch != epoch || self.phase != Phase::Scheduled {
            // Stale blocks and duplicate entry have no second creation authority.
            return false;
        }
        self.phase = Phase::AwaitingReady;
        true
    }

    /// Coalesce notifications from the same native call stack without polling or recurring work.
    pub(super) fn queue_check(&mut self, epoch: u64) -> bool {
        if self.epoch != epoch || self.phase != Phase::AwaitingReady || self.queued_check {
            // Before creation, during close or when already queued, grant no additional check.
            return false;
        }
        self.queued_check = true;
        true
    }

    /// Consume one check authorized by an actual event or creation return.
    pub(super) fn begin_check(&mut self, epoch: u64) -> bool {
        if self.epoch != epoch || self.phase != Phase::AwaitingReady || !self.queued_check {
            // A phase change revokes already queued checks from the old phase.
            return false;
        }
        self.queued_check = false;
        true
    }

    /// After the caller proves readiness, bind the sole expected receipt before native close starts.
    pub(super) fn accept_ready(&mut self, epoch: u64, host_generation: u64) -> bool {
        if self.epoch != epoch || self.phase != Phase::AwaitingReady {
            // Another readiness signal cannot overwrite the accepted close generation.
            return false;
        }
        self.queued_check = false;
        self.phase = Phase::AwaitingRelease { host_generation };
        true
    }

    /// Accept only current-close success; duplicate or stale receipts cannot consume the next sample.
    pub(super) fn accept_release(&mut self, host_generation: u64) -> bool {
        if self.phase != (Phase::AwaitingRelease { host_generation }) {
            // If close has not begun or generation differs, preserve the current wait unchanged.
            return false;
        }
        if self.sample == self.warm_samples {
            // Publish statistics only after the final warm sample proves resource release too.
            self.phase = Phase::Finished;
        } else {
            // The entry bounds sample count; advance only after an exact successful receipt.
            self.sample += 1;
            self.phase = Phase::Scheduled;
        }
        true
    }

    /// First failure revokes queued checks; a later successful receipt cannot erase it.
    pub(super) fn fail(&mut self) {
        self.phase = Phase::Finished;
        self.queued_check = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn events_coalesce_and_stale_epochs_cannot_create_check_or_close() {
        // Test every stale-epoch entry in the same phase, rather than relying on an accidentally empty slot.
        let mut sequence = Sequence::new(7, 1);
        assert!(!sequence.begin_sample(6));
        assert!(sequence.begin_sample(7));
        assert!(!sequence.queue_check(6));
        assert!(sequence.queue_check(7));
        assert!(!sequence.queue_check(7));
        assert!(!sequence.begin_check(6));
        assert!(sequence.begin_check(7));
        assert!(!sequence.begin_check(7));
        assert!(!sequence.accept_ready(6, 30));
        assert!(sequence.accept_ready(7, 30));
        assert!(!sequence.accept_ready(7, 31));
        assert!(!sequence.queue_check(7));
        assert!(!sequence.accept_release(29));
        assert_eq!(
            sequence.phase,
            Phase::AwaitingRelease {
                host_generation: 30
            }
        );
    }

    #[test]
    fn next_creation_and_final_result_require_each_exact_release() {
        let mut sequence = Sequence::new(9, 1);
        assert!(sequence.begin_sample(9));
        assert!(sequence.accept_ready(9, 40));
        assert!(!sequence.begin_sample(9));
        assert!(sequence.accept_release(40));
        assert_eq!(sequence.sample, 1);
        assert!(!sequence.accept_release(40));
        assert!(sequence.begin_sample(9));
        assert!(sequence.accept_ready(9, 41));
        assert!(!sequence.accept_release(40));
        assert!(sequence.accept_release(41));
        assert_eq!(sequence.phase, Phase::Finished);
        assert!(!sequence.accept_release(41));
        assert!(!sequence.begin_sample(9));
    }

    #[test]
    fn failure_revokes_pending_checks_and_receipts() {
        let mut sequence = Sequence::new(10, 1);
        assert!(sequence.begin_sample(10));
        assert!(sequence.queue_check(10));
        sequence.fail();
        assert!(!sequence.begin_check(10));
        assert!(!sequence.accept_ready(10, 8));
        assert!(!sequence.accept_release(8));
        assert!(!sequence.begin_sample(10));
    }

    #[test]
    fn failure_after_ready_cannot_publish_success_from_later_cleanup_receipt() {
        // Failures before or after readiness revoke sample authority; cleanup receipts permit probe exit only.
        let mut sequence = Sequence::new(11, 1);
        assert!(sequence.begin_sample(11));
        assert!(sequence.accept_ready(11, 70));
        sequence.fail();
        assert!(!sequence.accept_release(70));
        assert_eq!(sequence.sample, 0);
        assert!(!sequence.begin_sample(11));
    }
}
