//! Bind native weak identities at presentation; actual notifications authorize observation, not readiness itself.

use objc2::rc::Weak;
use objc2::runtime::AnyObject;
use objc2_app_kit::NSTextField;
use objc2_foundation::NSNotification;

use super::super::launcher_ui::{LauncherProbeIdentity, NativeLauncherPanel};

/// Getters can synchronously deliver notifications; coalesce one scalar reason until observation ends to avoid stale Stage1.
#[derive(Default)]
pub(super) struct ReadinessSchedule {
    observing: bool,
    notified_during_observation: bool,
}

impl ReadinessSchedule {
    /// Observe after Stage1 consumes its event; repeated reentry cannot start another getter sequence.
    pub(super) fn begin_check(&mut self) -> Result<(), String> {
        if self.observing {
            // Preserve the first observation and its received reason instead of overwriting them on reentry.
            return Err("Launcher 就绪观察发生重入".to_owned());
        }
        self.observing = true;
        self.notified_during_observation = false;
        Ok(())
    }

    /// Notifications outside observation may queue events; inside, record one reason without leaking stale events to later cycles.
    pub(super) fn signal(&mut self) -> bool {
        if self.observing {
            self.notified_during_observation = true;
            false
        } else {
            true // The outer pending slot coalesces adjacent actual notifications into one event.
        }
    }

    /// Consume the independent reason only if this result is still Pending and an actual notification reentered.
    pub(super) fn end_check(&mut self, pending: bool) -> bool {
        let notified = std::mem::take(&mut self.notified_during_observation);
        self.observing = false;
        pending && notified
    }
}

/// Presentation authority contains scalars/Weak only and extends no Launcher or shared-editor lifetime.
pub(super) struct LauncherReadinessWait {
    pub(super) epoch: u64,
    pub(super) host_generation: u64,
    panel: Weak<NativeLauncherPanel>,
    input: Weak<NSTextField>,
}

impl LauncherReadinessWait {
    /// Bind identities once from actual show's original handles; notifications and later checks cannot replace them.
    pub(super) fn new(epoch: u64, host_generation: u64, identity: LauncherProbeIdentity) -> Self {
        Self {
            epoch,
            host_generation,
            panel: identity.panel,
            input: identity.input,
        }
    }

    /// Load original weak identities before borrowing Rust ownership; send no native messages during global borrows.
    pub(super) fn matches_active_owner(&self) -> bool {
        if self.host_generation != super::super::launcher_close::probe_generation() {
            // A later actual close revokes the old presentation's authority to observe or close any successor graph.
            return false;
        }
        let Some(panel) = self.panel.load() else {
            // A nil original window Weak cannot point to a replacement even if its address is reused.
            return false;
        };
        let Some(input) = self.input.load() else {
            // Losing the original input revokes authority independently; its panel alone cannot prove input readiness.
            return false;
        };
        super::super::ACTIVE_SURFACE.with_borrow(|slot| {
            slot.as_ref().is_some_and(|owner| {
                // Match both original objects to reject an intermediate graph with the same window but replaced search field.
                std::ptr::eq(&*owner.panel, &*panel) && std::ptr::eq(&*owner.input, &*input)
            })
        })
    }

    /// Window notifications require the original panel identity; equal contents or another key window grant no authority.
    pub(super) fn matches_panel_notification(&self, notification: &NSNotification) -> bool {
        let Some(object) = notification.object() else {
            // A notification without a sender cannot establish window identity.
            return false;
        };
        self.panel.load().is_some_and(|panel| {
            // Temporary retains protect synchronous comparison only; no additional native owner remains in the driver.
            same_object(&panel, &object)
        })
    }

    /// Editing-begin accepts this original input only; another field's shared editor cannot wake this wait.
    pub(super) fn matches_input_notification(&self, notification: &NSNotification) -> bool {
        let Some(object) = notification.object() else {
            // A notification name cannot substitute for a missing input-object identity.
            return false;
        };
        self.input.load().is_some_and(|input| {
            // Read no strings, editors or body text; the next stage's original getters establish actual input facts.
            same_object(&input, &object)
        })
    }
}

/// Compare actual Objective-C identity after normal superclass conversion, without isEqual:.
fn same_object(left: &AnyObject, right: &AnyObject) -> bool {
    std::ptr::eq(left, right)
}

#[cfg(test)]
mod tests {
    use super::ReadinessSchedule;

    #[test]
    /// A pending observation cannot manufacture a successor without another external notification.
    fn pending_without_a_new_real_signal_cannot_schedule_its_own_retry() {
        let mut schedule = ReadinessSchedule::default();
        assert!(schedule.begin_check().is_ok());
        assert!(!schedule.end_check(true));
        // An initial not-ready observation creates no new authority; only another actual external notification permits queueing.
        assert!(schedule.signal());
        assert!(schedule.begin_check().is_ok());
        assert!(!schedule.end_check(true));
    }

    #[test]
    /// Duplicate notifications coalesce, and completed observations revoke any unconsumed reason.
    fn reentrant_notifications_coalesce_and_cannot_outlive_a_ready_or_failed_check() {
        let mut schedule = ReadinessSchedule::default();
        assert!(schedule.begin_check().is_ok());
        assert!(!schedule.signal());
        assert!(!schedule.signal());
        assert!(schedule.begin_check().is_err());
        // Only Pending consumes the actual getter-stack notification; duplicates cannot create another successor.
        assert!(schedule.end_check(true));
        assert!(schedule.begin_check().is_ok());
        assert!(!schedule.end_check(true));
        for _ in 0..2 {
            // Ready and failure both pass false, ending the reason before close or the next cycle.
            assert!(schedule.begin_check().is_ok());
            assert!(!schedule.signal());
            assert!(!schedule.end_check(false));
            assert!(schedule.begin_check().is_ok());
            assert!(!schedule.end_check(true));
        }
    }
}
