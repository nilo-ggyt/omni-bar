//! Resident process policy, separate from the lifetime of any foreground or input-method window.

use objc2::rc::Retained;
use objc2::{MainThreadMarker, MainThreadOnly, define_class, msg_send};
use objc2_app_kit::{NSApplication, NSApplicationDelegate, NSApplicationTerminateReply};
use objc2_foundation::{NSObject, NSObjectProtocol};

define_class!(
    /// The application-lifetime delegate keeps a windowless shortcut host resident until explicit quit.
    // SAFETY: this NSObject subclass has no Rust ivars and implements only the exact
    // main-thread NSApplicationDelegate selectors declared by the public AppKit protocol.
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    pub(super) struct ResidentApplicationDelegate;

    // SAFETY: the subclass inherits NSObject's identity and initialization invariants unchanged.
    unsafe impl NSObjectProtocol for ResidentApplicationDelegate {}

    // SAFETY: the application owner retains this main-thread delegate for the complete run loop.
    unsafe impl NSApplicationDelegate for ResidentApplicationDelegate {
        #[unsafe(method(applicationShouldHandleReopen:hasVisibleWindows:))]
        /// Reopening the app uses the existing Launcher entry without acquiring another system shortcut.
        fn should_handle_reopen(
            &self,
            _application: &NSApplication,
            _has_visible_windows: bool,
        ) -> bool {
            // The single-foreground entry preserves editing/closing/terminating sessions and rejects duplicate windows.
            // Without an active shortcut, reopening still restores settings access after the recovery window was dismissed.
            super::show_launcher_surface(self.mtm());
            // Recovery is handled here; prevent AppKit window restoration from creating another business entry.
            false
        }

        #[cfg(feature = "native-acceptance")]
        #[unsafe(method(applicationDidFinishLaunching:))]
        /// Start only the explicitly installed synthetic termination driver after production initialization.
        fn did_finish_launching(&self, _notification: &objc2_foundation::NSNotification) {
            // An absent opt-in driver is inert; manual acceptance retains its existing queued Launcher startup.
            super::acceptance_termination::application_did_finish_launching(self.mtm());
            #[cfg(feature = "native-product-acceptance")]
            {
                // Without the standalone product driver this is inert; only actual launch starts its first Launcher.
                super::acceptance_product::application_did_finish_launching(self.mtm());
            }
        }

        #[cfg(feature = "native-product-acceptance")]
        #[unsafe(method(applicationDidBecomeActive:))]
        /// Actual app-activation notification wakes the original Launcher wait without changing product activation policy.
        fn did_become_active(&self, notification: &objc2_foundation::NSNotification) {
            // No driver means no operation; the notification itself proves neither input readiness nor stage success.
            super::acceptance_product::application_did_become_active(notification, self.mtm());
        }

        #[unsafe(method(applicationShouldTerminateAfterLastWindowClosed:))]
        /// Keep the shortcut host resident when its last foreground or input-method window closes.
        fn should_terminate_after_last_window(&self, _application: &NSApplication) -> bool {
            // Closing Launcher, a plugin or an IME candidate must leave the shortcut host resident.
            false
        }

        #[unsafe(method(applicationShouldTerminate:))]
        /// Veto unsaved input or delay ordinary process exit until generic foreground release completes.
        fn should_terminate(&self, _application: &NSApplication) -> NSApplicationTerminateReply {
            // Ordinary app termination must commit plugin-visible input and await actual generic graph release.
            let reply = super::termination::application_should_terminate();
            #[cfg(feature = "native-acceptance")]
            {
                // Record the actual decision without changing it or calling close a second time.
                super::acceptance_termination::record_delegate_reply(reply);
            }
            reply
        }

        #[unsafe(method(applicationWillTerminate:))]
        /// Release process registrations explicitly because terminate does not unwind the Rust run-loop stack.
        fn will_terminate(&self, _notification: &objc2_foundation::NSNotification) {
            // AppKit exits without unwinding main; take/drop the Carbon owner outside TLS borrowing now.
            super::shortcuts::uninstall(self.mtm());
            #[cfg(feature = "native-acceptance")]
            {
                // Synthetic acceptance releases its private pasteboard and optional trace target/directory authority.
                super::acceptance_fixture::clear_fixture();
                // The actual final notification observes weak identities only after ordinary process cleanup.
                super::acceptance_termination::application_will_terminate();
            }
        }
    }
);

impl ResidentApplicationDelegate {
    /// Initialize before installing the application's non-owning delegate property.
    pub(super) fn new(mtm: MainThreadMarker) -> Retained<Self> {
        // Establish the empty Rust payload before the native designated initialization consumes allocation.
        let allocated = Self::alloc(mtm).set_ivars(());
        // SAFETY: the unit ivars are initialized and the public NSObject initializer
        // consumes the +1 subclass allocation, returning the same retained main-thread type.
        unsafe { msg_send![super(allocated), init] }
    }
}

/// Temporary resident policy restores the prior non-owning delegate on success and every error exit.
///
/// The borrowed application and retained delegate keep this scope confined to its AppKit thread.
/// Restoring the link precedes releasing the temporary owner, including during normal Rust unwinding.
pub(super) struct ResidentDelegateScope<'a> {
    application: &'a NSApplication,
    previous: Option<Retained<objc2::runtime::ProtocolObject<dyn NSApplicationDelegate>>>,
    _resident: Retained<ResidentApplicationDelegate>,
}

impl<'a> ResidentDelegateScope<'a> {
    /// Install a bounded probe policy without permanently replacing an existing application delegate.
    pub(super) fn install(application: &'a NSApplication, mtm: MainThreadMarker) -> Self {
        // Retain the previous weak delegate before replacing its link so it remains restorable.
        let previous = application.delegate();
        let resident = ResidentApplicationDelegate::new(mtm);
        application.setDelegate(Some(objc2::runtime::ProtocolObject::from_ref(&*resident)));
        Self {
            application,
            previous,
            _resident: resident,
        }
    }
}

impl Drop for ResidentDelegateScope<'_> {
    fn drop(&mut self) {
        // AppKit must stop referring to this temporary delegate before its retained owner is dropped.
        // A previous delegate remains retained through the synchronous restoration call.
        self.application.setDelegate(self.previous.as_deref());
    }
}
