//! Explicit built-in object ledger and shared-editor detachment facts; typed close scope controls completion authority.

use objc2::rc::autoreleasepool;
use omni_plugin_sdk::PluginCloseScope;

use super::super::native_deallocation::{
    NativeDeallocationCohort, TerminalReleaseAuthority, verify_terminal_window,
};
use super::super::native_payload::NativePayloadSnapshot;
use super::*;

/// Receipts retain weak identities/scalars only; install deallocation observers after Host close admission.
pub(super) struct CloseObservation {
    session: std::rc::Weak<NativeBuiltInSession>,
    panel: Weak<BuiltInPanel>,
    payload: NativePayloadSnapshot,
    borrowed: BorrowedEditorSnapshot,
    failure: Option<String>,
    full: Option<NativeDeallocationCohort>,
    resources: Option<NativeDeallocationCohort>,
}

impl CloseObservation {
    /// Copy construction-ledger roles only; shared editors have separate weak detachment evidence.
    pub(super) fn capture(session: &Rc<NativeBuiltInSession>) -> Result<Self, String> {
        let mut observation = Self {
            session: Rc::downgrade(session),
            panel: Weak::from_retained(&session.panel),
            payload: session.controller.ivars().owned.snapshot(),
            borrowed: session.controller.ivars().borrowed.clone(),
            failure: None,
            full: None,
            resources: None,
        };
        // Capture neither clears input, creates a context nor attaches deallocation markers to borrowed objects.
        observation.capture_session_payload(session)?;
        Ok(observation)
    }

    /// Preflight may update menus/lazy undo; copy all still-live roles, including replaced identities.
    pub(super) fn capture_session_payload(
        &mut self,
        session: &NativeBuiltInSession,
    ) -> Result<(), String> {
        if let Some(error) = session
            .controller
            .ivars()
            .ownership_failure
            .borrow()
            .as_ref()
        {
            // A currently plausible snapshot cannot hide previously unrecorded roles during close preflight.
            return Err(error.clone());
        }
        self.payload = session.controller.ivars().owned.snapshot();
        self.borrowed.capture(&session.panel)
    }

    /// Clear text/history only after admission; irreversible-stage failure prevents the final receipt.
    pub(super) fn finish_accepted(&mut self, session: &NativeBuiltInSession) {
        self.borrowed.finish_accepted(
            &session.panel,
            &[&session.views.input, &session.views.output],
            &session.controller.ivars().undo,
            &session.controller.ivars().field_editor,
        );
        if let Err(error) = self.capture_session_payload(session) {
            // Continue accepted teardown without restoring Active, retrying or fabricating success.
            self.failure = Some(error);
        }
    }

    /// After admission, observe explicit roles and the additional actual panel release required by foreground handoff.
    pub(super) fn arm(&mut self, mtm: MainThreadMarker) {
        let mut full = NativeDeallocationCohort::new(mtm);
        let mut resources = NativeDeallocationCohort::new(mtm);
        if let Some(panel) = self.panel.load() {
            // Foreground handoff requires panel deallocation; terminal scope separately checks an empty framework window lease.
            full.track(&*panel, mtm);
        }
        // Both cohorts use the same explicit-role inventory, without tracking borrowed caches or retaining observed objects.
        self.payload.track_remaining(&mut full, mtm);
        self.payload.track_remaining(&mut resources, mtm);
        // Seal roles and release construction indices before native close; later identities cannot replace this receipt.
        full.seal();
        resources.seal();
        self.payload.seal();
        self.full = Some(full);
        self.resources = Some(resources);
    }

    /// Require Rust-owner release, actual explicit-role deallocation and independent shared-cache detachment.
    fn payload_released(&self) -> bool {
        // All project roles must be nil; borrowed editors must be empty/detached. Neither proof replaces the other.
        self.session.upgrade().is_none()
            && self.payload.is_released()
            && self.failure.is_none()
            && self.borrowed.is_detached()
    }

    /// Probes reuse session cache facts while still requiring actual foreground-panel and project-role release.
    pub(super) fn is_released(&self) -> bool {
        // Observation alone supplies no lifecycle authority; production additionally requires its actual cohort event.
        self.payload_released() && self.panel.load().is_none()
    }
}

/// A closing operation retains only weak graphs, native signal counters and its original scalar request.
pub(super) struct PendingRelease {
    kind: BuiltInSurface,
    intent: SurfaceCloseIntent,
    scope: Cell<PluginCloseScope>,
    ticket: Cell<Option<TerminationTicket>>,
    lifecycle: Rc<dyn BuiltInLifecycle>,
    observation: CloseObservation,
    full: RefCell<Option<NativeDeallocationCohort>>,
    resources: NativeDeallocationCohort,
    terminal: RefCell<TerminalReleaseAuthority>,
    close_notified: bool,
    finished: Cell<bool>,
}

impl PendingRelease {
    /// Publish weak-only operation metadata before any actual native event may authorize completion.
    pub(super) fn new(
        kind: BuiltInSurface,
        intent: SurfaceCloseIntent,
        scope: PluginCloseScope,
        ticket: Option<TerminationTicket>,
        lifecycle: Rc<dyn BuiltInLifecycle>,
        mut observation: CloseObservation,
        close_notified: bool,
    ) -> Rc<Self> {
        // Separate callback authority from immutable weak graph fields; native getters never cross a state borrow.
        let full = observation.full.take().expect("armed full cohort");
        let resources = observation.resources.take().expect("armed resource cohort");
        Rc::new(Self {
            kind,
            intent,
            scope: Cell::new(scope),
            ticket: Cell::new(ticket),
            lifecycle,
            observation,
            full: RefCell::new(Some(full)),
            resources,
            terminal: RefCell::new(TerminalReleaseAuthority::default()),
            close_notified,
            finished: Cell::new(false),
        })
    }

    /// Install weak metadata callbacks; neither cohort owns the native graph it waits to observe.
    pub(super) fn observe(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        self.full
            .borrow()
            .as_ref()
            .expect("armed full cohort")
            .when_released(move || {
                if let Some(operation) = weak.upgrade() {
                    // Late delivery resolves only this exact operation, never the current built-in by kind alone.
                    operation.full_released();
                }
            });
        let weak = Rc::downgrade(self);
        self.resources.when_released(move || {
            if let Some(operation) = weak.upgrade() {
                // This real event is delivered after the last payload deallocation stack has returned.
                operation.try_terminal(true);
            }
        });
    }

    /// Preserve the original scalar request when actual quit overtakes an already finalized native owner.
    pub(super) fn request(
        &self,
        intent: SurfaceCloseIntent,
        scope: PluginCloseScope,
    ) -> Result<(), String> {
        if !self.current() {
            // A stale strong metadata borrow cannot promote a retired operation or borrow a newer close slot.
            return Err("Built-in close operation is no longer current".to_owned());
        }
        validate_close_scope(
            Some((self.intent, self.scope.get())),
            intent,
            scope,
            super::super::termination::application_termination_pending(),
        )?;
        let ticket = validate_close_ticket(
            self.ticket.get(),
            scope,
            super::super::termination::application_termination_ticket(),
        )?;
        // The lifecycle owner validates actual transaction authority before this operation replaces its scope.
        self.lifecycle.begin_close(self.kind, intent, scope)?;
        self.scope.set(scope);
        self.ticket.set(ticket);
        self.try_terminal(false);
        Ok(())
    }

    /// Return the unchanged accepted intent for an explicit application-termination promotion.
    pub(super) fn intent(&self) -> SurfaceCloseIntent {
        self.intent
    }

    /// Check this operation still owns closing state before any native verification or Host callback.
    fn current(&self) -> bool {
        !self.finished.get()
            && RELEASING.with_borrow(|current| {
                // Identity belongs to a live Rc allocation and cannot be borrowed by a later same-kind surface.
                current
                    .as_ref()
                    .is_some_and(|current| std::ptr::eq(&**current, self))
            })
    }

    /// A full native event can authorize only the exact ordinary scope that remains accepted now.
    fn full_released(&self) {
        if !self.current() || self.scope.get() != PluginCloseScope::ForegroundTransition {
            // Promotion revokes the queued ordinary callback's authority without changing its original intent.
            return;
        }
        let released = autoreleasepool(|_| {
            let completed = self
                .full
                .borrow()
                .as_ref()
                .is_some_and(NativeDeallocationCohort::has_completed);
            // A real cohort event authorizes one role/cache check; the event itself does not prove every boundary.
            completed
                && self.observation.payload_released()
                && self.observation.panel.load().is_none()
        });
        if !released {
            // Release signals cannot hide surviving roles or cache residue; failure keeps handoff blocked without retry.
            eprintln!(
                "Built-in owned-resource verification failed: roles={:?}, cache={:?}, capture={:?}",
                self.observation.payload.surviving_roles(),
                self.observation.borrowed.failure(),
                self.observation.failure,
            );
            return;
        }
        self.complete(PluginCloseScope::ForegroundTransition);
    }

    /// Reserve one terminal check only from a delivered actual payload event, in either promotion order.
    fn try_terminal(&self, actual_resource_event: bool) {
        if !self.current() {
            // Retired operation metadata cannot verify or complete another owner after delayed dispatch.
            return;
        }
        let reserved = self
            .terminal
            .borrow_mut()
            .reserve(self.scope.get(), actual_resource_event);
        if !reserved {
            // Ordinary scope remembers the actual event; promotion waits for it rather than guessing a queue turn.
            return;
        }
        if !super::super::termination::application_termination_pending() {
            // A queued shortcut/quit reservation alone never grants terminal completion authority.
            eprintln!("Built-in terminal close has no pending application termination");
            return;
        }
        if self.ticket.get().is_none()
            || self.ticket.get() != super::super::termination::application_termination_ticket()
        {
            // A cancelled earlier quit cannot lend its already accepted terminal receipt to a later transaction.
            eprintln!("Built-in terminal close has a stale application termination ticket");
            return;
        }
        let result = autoreleasepool(|_| {
            if !self.resources.has_completed() || !self.observation.payload_released() {
                // Require actual release events, absent Rust owners and cleared public cache relationships together.
                return Err(format!(
                    "Built-in terminal resource verification failed: roles={:?}, cache={:?}, capture={:?}",
                    self.observation.payload.surviving_roles(),
                    self.observation.borrowed.failure(),
                    self.observation.failure,
                ));
            }
            let panel = self.observation.panel.load();
            let receipt = verify_terminal_window(
                panel.as_deref().map(|panel| -> &NSWindow { panel }),
                self.close_notified,
                self.observation.session.upgrade().is_some(),
            )?;
            // Move callback/association authority out before the native cancellation call can destroy its marker.
            let mut full = self.full.borrow_mut().take().expect("armed full cohort");
            full.cancel_observation(panel.as_deref());
            Ok(receipt)
        });
        match result {
            Ok(receipt) => {
                // This scalar is an empty framework-window lease receipt, explicitly not native deallocation.
                eprintln!(
                    "Built-in terminal framework window pending={}",
                    receipt.framework_panel_pending
                );
                self.complete(PluginCloseScope::ApplicationTermination);
            }
            Err(error) => {
                // One failed terminal verification stays closing; no retry, timer or ordinary successor is admitted.
                eprintln!("{error}");
            }
        }
    }

    /// Release the exact metadata slot before the Host consumes its barrier or replies to AppKit.
    fn complete(&self, scope: PluginCloseScope) {
        if !self.current() || self.scope.get() != scope {
            // Reentrant promotion or a stale callback cannot consume a receipt under another scope.
            return;
        }
        if scope == PluginCloseScope::ApplicationTermination
            && (self.ticket.get().is_none()
                || self.ticket.get() != super::super::termination::application_termination_ticket())
        {
            // The receipt and Host barrier must still belong to the same real coordinator transaction after native work.
            eprintln!("Built-in terminal receipt lost its application termination ticket");
            return;
        }
        self.finished.set(true);
        let owner = RELEASING.with_borrow_mut(|current| {
            // `current` was validated above with no intervening native call; transfer ownership outside TLS.
            current.take()
        });
        drop(owner);
        // No native upgrade or mutable borrow survives this call; terminal scope can advance quit only.
        self.lifecycle.finish_close(self.kind, self.intent, scope);
    }
}
