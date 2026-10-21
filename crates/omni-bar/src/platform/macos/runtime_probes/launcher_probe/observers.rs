//! Symmetric public notification/KVO registration for a standalone probe, without owning the observed graph.

use std::ffi::c_void;

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{
    NSApplicationDidBecomeActiveNotification, NSControlTextDidBeginEditingNotification,
    NSWindowDidBecomeKeyNotification,
};
use objc2_foundation::{
    NSDictionary, NSKeyValueChangeKey, NSKeyValueObservingOptions, NSNotification,
    NSNotificationCenter, NSObject, NSObjectNSKeyValueObserverRegistration, NSObjectProtocol,
    NSString, ns_string,
};

use super::super::super::launcher_ui::{LauncherProbeIdentity, NativeLauncherPanel};
use super::{matches_panel, notification_matches, queue_ready_check};

/// A unique static address identifies this KVO registration only and is never dereferenced as storage.
static FIRST_RESPONDER_CONTEXT: u8 = 0;

/// Observer ivars store an epoch only, retaining no window, field, driver or closure owner.
pub(super) struct ObserverIvars {
    epoch: u64,
}

define_class!(
    /// Named AppKit notifications and firstResponder KVO request bounded, coalesced observations only.
    // SAFETY: NSObject adds no subclass obligations; scalar ivars, registration, callbacks and removal stay on the main thread.
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    #[ivars = ObserverIvars]
    pub(super) struct ReadinessObserver;

    // SAFETY: Preserve NSObject identity, initialization and reference-counting ABI.
    unsafe impl NSObjectProtocol for ReadinessObserver {}

    impl ReadinessObserver {
        /// Treat a matching native event as a reason to inspect readiness, never as readiness evidence itself.
        #[unsafe(method(launcherReadinessEvent:))]
        fn event(&self, notification: &NSNotification) {
            // These AppKit application/window/control notifications are delivered synchronously on their main thread.
            let epoch = self.ivars().epoch;
            if notification_matches(epoch, notification) {
                // Events from unrelated windows or input controls cannot advance the current sample.
                queue_ready_check(epoch);
            }
        }

        /// Handle this registration's first-responder changes and preserve superclass handling for every other context.
        #[unsafe(method(observeValueForKeyPath:ofObject:change:context:))]
        fn changed(
            &self,
            key_path: Option<&NSString>,
            object: Option<&AnyObject>,
            change: Option<&NSDictionary<NSKeyValueChangeKey, AnyObject>>,
            context: *mut c_void,
        ) {
            if context == context_pointer() {
                // Require both the dedicated context and exact window identity; change contents do not establish readiness.
                if key_path.is_some_and(|key| key.isEqualToString(ns_string!("firstResponder")))
                    && object.is_some_and(|object| matches_panel(self.ivars().epoch, object))
                {
                    // Actual firstResponder changes justify a check but do not independently prove success.
                    queue_ready_check(self.ivars().epoch);
                }
            } else {
                // Forward unrelated KVO through NSObject's standard handling instead of swallowing framework errors.
                // SAFETY: Preserve nullable KVO parameter types and context when invoking the matching superclass ABI.
                unsafe {
                    let _: () = msg_send![
                        super(self), observeValueForKeyPath: key_path,
                        ofObject: object, change: change, context: context
                    ];
                }
            }
        }
    }
);

impl ReadinessObserver {
    /// Install scalar ivars before invoking NSObject's designated initialization.
    pub(super) fn new(epoch: u64, mtm: MainThreadMarker) -> Retained<Self> {
        let allocated = Self::alloc(mtm).set_ivars(ObserverIvars { epoch });
        // SAFETY: Fully initialized ivars precede NSObject init, which consumes the sole +1 allocation without extra prerequisites.
        unsafe { msg_send![super(allocated), init] }
    }
}

/// KVO stores a weak window identity; retain the observer owner until explicit removal completes.
pub(super) struct FirstResponderObservation {
    panel: objc2::rc::Weak<NativeLauncherPanel>,
    observer: Retained<ReadinessObserver>,
}

impl FirstResponderObservation {
    /// Observe publicly KVO-compliant NSWindow.firstResponder, never private TextKit properties.
    pub(super) fn install(
        identity: &LauncherProbeIdentity,
        observer: &Retained<ReadinessObserver>,
    ) -> Result<Self, String> {
        let panel = identity
            .panel
            .load()
            .ok_or("Launcher probe owner disappeared before KVO")?;
        // SAFETY: The main-thread panel is live, the observer implements the exact selector and the key path is public KVO;
        // the unique static context identifies one registration, removed before window destruction or close request.
        unsafe {
            panel.addObserver_forKeyPath_options_context(
                observer,
                ns_string!("firstResponder"),
                NSKeyValueObservingOptions::empty(),
                context_pointer(),
            );
        }
        Ok(Self {
            panel: identity.panel.clone(),
            observer: observer.clone(),
        })
    }
}

impl Drop for FirstResponderObservation {
    /// Revoke the exact registration before releasing its observer, without keeping the measured panel alive.
    fn drop(&mut self) {
        if let Some(panel) = self.panel.load() {
            // Remove only this registration; if the weak identity is nil, do not guess a replacement observed object.
            // SAFETY: Construction registered this exact observer/key/context; this main-thread owner drops once only.
            unsafe {
                panel.removeObserver_forKeyPath_context(
                    &self.observer,
                    ns_string!("firstResponder"),
                    context_pointer(),
                );
            }
        }
    }
}

/// Named notification registrations have one owner and retain no notifications or callbacks after stop.
pub(super) struct Notifications {
    center: Retained<NSNotificationCenter>,
    observer: Retained<ReadinessObserver>,
}

impl Notifications {
    /// Three actual event types cover activation, key status and editing begin; creation return checks initial synchronous state.
    pub(super) fn install(observer: &Retained<ReadinessObserver>) -> Self {
        let center = NSNotificationCenter::defaultCenter();
        // SAFETY: Public notification names are valid NSString constants and the selector accepts one NSNotification.
        // This owner and driver keep the observer alive; object=None broadens delivery, but callbacks verify exact identity.
        unsafe {
            for name in [
                NSApplicationDidBecomeActiveNotification,
                NSWindowDidBecomeKeyNotification,
                NSControlTextDidBeginEditingNotification,
            ] {
                // Keep three fixed registrations instead of accumulating observers per sample.
                center.addObserver_selector_name_object(
                    observer,
                    sel!(launcherReadinessEvent:),
                    Some(name),
                    None,
                );
            }
        }
        Self {
            center,
            observer: observer.clone(),
        }
    }
}

impl Drop for Notifications {
    /// Remove all three registrations while the retained observer is still live; queued checks remain epoch-guarded.
    fn drop(&mut self) {
        // SAFETY: This observer owns these three named registrations; remove them before release without modifying others.
        unsafe { self.center.removeObserver(&self.observer) };
    }
}

/// Use this address only as a comparison token, never to read or write the underlying static byte.
fn context_pointer() -> *mut c_void {
    std::ptr::addr_of!(FIRST_RESPONDER_CONTEXT)
        .cast_mut()
        .cast()
}
