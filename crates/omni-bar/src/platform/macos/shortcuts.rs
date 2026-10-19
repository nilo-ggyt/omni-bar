//! Main-thread Carbon ownership with one handler and bounded, explicit cleanup debt.
//!
//! Native registration and filesystem persistence cannot form one atomic transaction. Acquisition
//! and pre-rename failures keep the old key; post-rename directory-sync failures report an applied
//! change with uncertain durability. Native cleanup debt retains one inactive key and blocks acquisition.

use std::cell::{Cell, RefCell};
use std::ffi::c_void;
use std::io::Write;
use std::{mem, ptr};

use core_foundation::array::{CFArray, CFArrayRef};
use core_foundation::base::{CFType, TCFType};
use core_foundation::boolean::CFBoolean;
use core_foundation::dictionary::CFDictionary;
use core_foundation::number::CFNumber;
use core_foundation::string::CFString;
use objc2::MainThreadMarker;

use crate::capabilities::shortcut_preferences::{
    ShortcutBinding, ShortcutPreferences, ShortcutPreferencesError, ShortcutSettingsStore,
    ShortcutView,
};

const SIGNATURE: u32 = u32::from_be_bytes(*b"OMNI");
const KEYBOARD_CLASS: u32 = u32::from_be_bytes(*b"keyb");
const HOT_KEY_PRESSED: u32 = 5;
const DIRECT_OBJECT: u32 = u32::from_be_bytes(*b"----");
const HOT_KEY_ID_TYPE: u32 = u32::from_be_bytes(*b"hkid");
const HOT_KEY_EXCLUSIVE: u32 = 1;
const EVENT_NOT_HANDLED: i32 = -9874;
const PARAMETER_ERROR: i32 = -50;
const HOT_KEY_EXISTS: i32 = -9878;

/// Carbon's public records use two-byte packing on supported macOS targets.
#[repr(C, packed(2))]
struct EventTypeSpec {
    event_class: u32,
    event_kind: u32,
}

/// A never-reused process-local registration identity, independent of the physical key.
#[repr(C, packed(2))]
struct EventHotKeyId {
    signature: u32,
    id: u32,
}

type EventRef = *mut c_void;
type EventHandlerRef = *mut c_void;
type EventTargetRef = *mut c_void;
type EventHotKeyRef = *mut c_void;
type EventHandler = unsafe extern "C" fn(*mut c_void, EventRef, *mut c_void) -> i32;

thread_local! {
    /// The application retains its handler even when initial key acquisition fails.
    static REGISTRATION: RefCell<Option<Registration<CarbonApi>>> = const {
        RefCell::new(Some(Registration::new(CarbonApi)))
    };
    /// Callback dispatch never borrows the owner while Carbon or persistence can reenter.
    static ACTIVE_KEY_ID: Cell<Option<u32>> = const { Cell::new(None) };
}

/// Acceptance builds identify actual Carbon delivery; ordinary dispatch probes cannot impersonate a system shortcut.
#[cfg(feature = "native-acceptance")]
mod acceptance_diagnostics {
    use std::cell::Cell;
    use std::io::Write;

    /// Emit at most 64 fixed-stage records per process, retaining no keys, user text, addresses or native references.
    const RECORD_LIMIT: u8 = 64;

    /// Read the opt-in environment once at installation; callbacks consume a bounded scalar budget only.
    #[derive(Clone, Copy)]
    struct DiagnosticState {
        configured: bool,
        enabled: bool,
        emitted: u8,
    }

    impl DiagnosticState {
        /// Acceptance runs without explicit opt-in remain as silent as normal products on successful shortcut paths.
        const fn new() -> Self {
            Self {
                configured: false,
                enabled: false,
                emitted: 0,
            }
        }

        /// Consume budget before output so reentry or write failure cannot exceed the record limit.
        fn reserve_record(&mut self) -> Option<u8> {
            if !self.enabled || self.emitted == RECORD_LIMIT {
                // Disabled/exhausted state stays silent without buffer allocation or waiting for a retry.
                return None;
            }
            self.emitted += 1;
            Some(self.emitted)
        }
    }

    thread_local! {
        /// Scalar state shared by main-thread installation and Carbon callbacks; other threads start without opt-in authority.
        static STATE: Cell<DiagnosticState> = const { Cell::new(DiagnosticState::new()) };
        /// Only a matched actual Carbon callback authorizes the application root to record the real toggle branch.
        static NATIVE_DELIVERY_ACTIVE: Cell<bool> = const { Cell::new(false) };
    }

    /// Accept explicit `1` only; repeated installation neither rereads the environment nor resets output budget.
    pub(super) fn configure_once() {
        let mut state = STATE.get();
        if state.configured {
            // Installation reentry and repeated initialization share the original bounded diagnostic window.
            return;
        }
        state.configured = true;
        state.enabled = std::env::var_os("OMNI_NATIVE_SHORTCUT_DIAGNOSTICS").is_some_and(|value| {
            // Compare the fixed switch only; neither log nor retain its environment value.
            value == std::ffi::OsStr::new("1")
        });
        STATE.set(state);
        emit("diagnostics_enabled");
    }

    /// All fixed-label records share one budget; output holds no reentrant state borrow.
    pub(super) fn emit(stage: &'static str) {
        let sequence = STATE
            .try_with(|slot| {
                // Copy/commit Cell state before stderr output, without holding a state borrow across I/O.
                let mut state = slot.get();
                let sequence = state.reserve_record();
                slot.set(state);
                sequence
            })
            .ok()
            .flatten();
        if let Some(sequence) = sequence {
            // Output failure loses this observation only, with no retry or change to Carbon event-return policy.
            let _ = writeln!(
                std::io::stderr().lock(),
                "shortcut_acceptance sequence={sequence} stage={stage}"
            );
        }
    }

    /// The actual Carbon delivery lease saves one prior boolean and extends no application-resource lifetime.
    pub(super) struct NativeDeliveryScope {
        previous: bool,
    }

    impl NativeDeliveryScope {
        /// Enter scope only for opted-in acceptance callbacks; nested return restores the outer authorization.
        pub(super) fn enter() -> Self {
            let enabled = STATE.get().enabled;
            Self {
                previous: NATIVE_DELIVERY_ACTIVE.replace(enabled),
            }
        }
    }

    impl Drop for NativeDeliveryScope {
        fn drop(&mut self) {
            // Normal return and unwind restore the outer value; TLS destruction must not reinitialize diagnostics.
            let _ = NATIVE_DELIVERY_ACTIVE.try_with(|active| active.set(self.previous));
        }
    }

    /// Ordinary Launcher measurements check this false scalar only, with no environment query or output.
    pub(super) fn is_native_delivery() -> bool {
        NATIVE_DELIVERY_ACTIVE.try_with(Cell::get).unwrap_or(false)
    }

    #[cfg(test)]
    mod tests {
        use super::{
            DiagnosticState, NativeDeliveryScope, RECORD_LIMIT, STATE, is_native_delivery,
        };

        /// Explicit opt-in and total record budget bound diagnostics; exhaustion cannot wrap or reactivate the counter.
        #[test]
        fn records_require_enablement_and_stop_at_the_process_budget() {
            let mut state = DiagnosticState::new();
            assert_eq!(state.reserve_record(), None);
            state.enabled = true;
            for sequence in 1..=RECORD_LIMIT {
                // Each reservation consumes one grant, including output that later fails.
                assert_eq!(state.reserve_record(), Some(sequence));
            }
            assert_eq!(state.reserve_record(), None);
            assert_eq!(state.reserve_record(), None);
            assert_eq!(state.emitted, RECORD_LIMIT);
        }

        /// Nested return and unexpected unwind restore delivery authority; later ordinary dispatch cannot inherit it.
        #[test]
        fn native_delivery_scope_restores_after_nested_calls_and_unwind() {
            let previous = STATE.replace(DiagnosticState {
                configured: true,
                enabled: true,
                emitted: 0,
            });
            assert!(!is_native_delivery());
            let result = std::panic::catch_unwind(|| {
                // Model the matched callback's stack lease without invoking Carbon, the application root or stderr.
                let _outer = NativeDeliveryScope::enter();
                assert!(is_native_delivery());
                {
                    // Returning from nested delivery preserves the still-active outer callback's authorization.
                    let _inner = NativeDeliveryScope::enter();
                    assert!(is_native_delivery());
                }
                assert!(is_native_delivery());
                panic!("synthetic native delivery unwind");
            });
            assert!(result.is_err());
            assert!(!is_native_delivery());
            assert_eq!(STATE.get().emitted, 0);
            STATE.set(previous);
        }
    }
}

/// Expose fixed application-state roles/branches only, without plugin identities, keycodes or user input.
#[cfg(feature = "native-acceptance")]
pub(super) enum ShortcutDeliveryStage {
    ToggleEntered,
    SuppressedTermination,
    RejectedThread,
    SuppressedTransition,
    SuppressedOwnedAdapter,
    DismissLauncher,
    DismissPlugin,
    DismissBuiltIn,
    ResumeLauncher,
    ResumePlugin,
    ResumeBuiltIn,
}

#[cfg(feature = "native-acceptance")]
impl ShortcutDeliveryStage {
    /// Each branch names an actual state-machine path using fixed, nonsensitive compile-time labels.
    const fn label(self) -> &'static str {
        match self {
            // The callback entered the application root's synchronous toggle entry.
            Self::ToggleEntered => "toggle_entered",
            // Accepted application termination blocks another foreground open or close.
            Self::SuppressedTermination => "suppressed_termination",
            // A non-main thread cannot access AppKit or change foreground state.
            Self::RejectedThread => "rejected_thread",
            // Coalesce this delivery while old resources transition or await the destruction barrier.
            Self::SuppressedTransition => "suppressed_transition",
            // Logical idle with adapter-owned resources must await the existing release transaction.
            Self::SuppressedOwnedAdapter => "suppressed_owned_adapter",
            // Active Launcher uses its existing close preflight and lifecycle transaction.
            Self::DismissLauncher => "dismiss_launcher",
            // Active plugins close through the protocol, without plugin-specific diagnostic branches.
            Self::DismissPlugin => "dismiss_plugin",
            // Active built-ins use the shared built-in close transaction.
            Self::DismissBuiltIn => "dismiss_builtin",
            // With no old resources, reconstruct Launcher from the remembered descriptor.
            Self::ResumeLauncher => "resume_launcher",
            // With no old resources, activate the plugin through its generic registration identity.
            Self::ResumePlugin => "resume_plugin",
            // With no old resources, reconstruct the selected built-in session.
            Self::ResumeBuiltIn => "resume_builtin",
        }
    }
}

/// Record only on the current actual Carbon delivery stack, never from ordinary probes or direct calls.
#[cfg(feature = "native-acceptance")]
pub(super) fn record_native_delivery_stage(stage: ShortcutDeliveryStage) {
    if acceptance_diagnostics::is_native_delivery() {
        // This opted-in delivery lease still obeys the process-wide 64-record budget.
        acceptance_diagnostics::emit(stage.label());
    }
}

/// The narrow native boundary permits deterministic failure tests without installing global keys.
trait ShortcutNativeApi {
    type Handle: Copy;

    /// Install the sole application handler; a failed call transfers no reference.
    fn install_handler(&mut self) -> Result<Self::Handle, i32>;
    /// Acquire an exclusive key whose ID is not yet accepted by callback dispatch.
    fn register_key(&mut self, binding: ShortcutBinding, id: u32) -> Result<Self::Handle, i32>;
    /// A failed unregister keeps the owned reference available for one later explicit retry.
    fn unregister_key(&mut self, key: Self::Handle) -> Result<(), i32>;
    /// Carbon invalidates a handler reference after this call, including an error result.
    fn remove_handler(&mut self, handler: Self::Handle) -> Result<(), i32>;
    /// Publish only the current ID, before retiring an old key or tearing down its handler.
    fn publish_active_id(&mut self, id: Option<u32>);
    /// Report bounded operation/status metadata without user paths or document contents.
    fn report_failure(&mut self, operation: &'static str, status: i32);
}

/// Owns one native key and the view committed with that particular registration identity.
struct RegisteredKey<Handle> {
    handle: Handle,
    id: u32,
    view: ShortcutView,
}

/// Application-lifetime handler plus one active and at most one inactive native key.
struct Registration<Api: ShortcutNativeApi> {
    api: Api,
    handler: Option<Api::Handle>,
    active: Option<RegisteredKey<Api::Handle>>,
    retired: Option<RegisteredKey<Api::Handle>>,
    next_id: u32,
    // Retain the first startup failure as a Copy category for the footer, without files or native resources.
    startup_failure: Option<ShortcutPreferencesError>,
}

impl<Api: ShortcutNativeApi> Registration<Api> {
    /// An empty owner allocates no native resources until explicit application setup.
    const fn new(api: Api) -> Self {
        Self {
            api,
            handler: None,
            active: None,
            retired: None,
            next_id: 1,
            startup_failure: None,
        }
    }

    /// Read the committed active binding only; candidates and cleanup debt are not responsive registrations.
    fn current(&self) -> Option<ShortcutView> {
        self.active.as_ref().map(|key| {
            // Return Copy display metadata only; the current owner retains exclusive native-handle ownership.
            key.view
        })
    }

    /// Install one handler per owner; explicit binding changes do not install duplicate callbacks.
    fn ensure_handler(&mut self) -> Result<(), ShortcutPreferencesError> {
        if self.handler.is_none() {
            // A handler's uniqueness is (function,userData) per target; reinstalling it would fail.
            let handler = self.api.install_handler().map_err(|status| {
                // Installation transfers nothing on failure, so the existing key state is untouched.
                self.api.report_failure("install_handler", status);
                ShortcutPreferencesError::RegistrationUnavailable
            })?;
            self.handler = Some(handler);
        }
        Ok(())
    }

    /// Consume a fresh identity on every acquisition attempt, including failure, so queued old events cannot match replacements.
    fn acquire(
        &mut self,
        binding: ShortcutBinding,
    ) -> Result<RegisteredKey<Api::Handle>, ShortcutPreferencesError> {
        self.ensure_handler()?;
        let id = self.next_id;
        self.next_id = id
            .checked_add(1)
            .ok_or(ShortcutPreferencesError::RegistrationUnavailable)?;
        let handle = self.api.register_key(binding, id).map_err(|status| {
            // No successful handle was transferred; the previous key and persisted setting survive.
            self.api.report_failure("register_key", status);
            ShortcutPreferencesError::RegistrationUnavailable
        })?;
        Ok(RegisteredKey {
            handle,
            id,
            view: ShortcutView { binding },
        })
    }

    /// Startup attempts the configured binding once; conflicts grant no authority to acquire alternatives.
    fn install(&mut self, preferred: ShortcutBinding) -> Result<(), ShortcutPreferencesError> {
        if self.active.is_some() {
            // Repeated initialization preserves the published registration without another acquisition or settings rewrite.
            return Ok(());
        }
        let key = self.acquire(preferred)?;
        // Publish the sole successfully acquired candidate without writing the existing settings file.
        self.api.publish_active_id(Some(key.id));
        self.active = Some(key);
        self.startup_failure = None;
        Ok(())
    }

    /// Missing settings attempt the original default once; invalid settings preserve file/cause without an unconfigured hotkey.
    fn install_saved(
        &mut self,
        saved: Result<Option<ShortcutBinding>, ShortcutPreferencesError>,
    ) -> Result<(), ShortcutPreferencesError> {
        if self.active.is_some() {
            // Repeated initialization cannot replace an already committed binding or its startup status.
            return Ok(());
        }
        let result = match saved {
            // Valid explicit settings authorize this binding only; failure never iterates other picker choices.
            Ok(Some(binding)) => self.install(binding),
            // First installation without settings retains the original default attempt, with no alternate after conflict.
            Ok(None) => self.install(ShortcutBinding::default()),
            // Corrupt, unreadable and unsupported settings keep their typed errors without creating a handler or hotkey.
            Err(error) => Err(error),
        };
        self.startup_failure = result.as_ref().err().copied();
        result
    }

    /// Retry one retained key only on an explicit settings change, with no timer or growing queue.
    fn clear_cleanup_debt(&mut self) -> Result<(), ShortcutPreferencesError> {
        let Some(key) = self.retired.take() else {
            // No prior cleanup failure means acquisition can proceed immediately.
            return Ok(());
        };
        if let Err(status) = self.api.unregister_key(key.handle) {
            // The handle remains owned and inactive; reject acquisition instead of accumulating keys.
            self.retired = Some(key);
            self.api.report_failure("retry_unregister_key", status);
            return Err(ShortcutPreferencesError::CleanupPending);
        }
        Ok(())
    }

    /// Preserve prior state before commit; explicit user selection may also establish the first active registration.
    /// Persistence reports post-rename failure only as AppliedWithDurabilityUncertain and performs no Carbon cleanup;
    /// other errors are precommit failures. Never roll committed settings back to an unregistered state.
    fn update(
        &mut self,
        requested: ShortcutBinding,
        persist: impl FnOnce(ShortcutBinding) -> Result<(), ShortcutPreferencesError>,
    ) -> Result<ShortcutView, ShortcutPreferencesError> {
        self.clear_cleanup_debt()?;
        let previous = self.active.as_ref().map(|key| key.view);
        if previous.is_some_and(|view| requested == view.binding) {
            // Skip acquisition only for the same actual active binding; no-active state cannot treat a default as registered.
            let durability_uncertain = match persist(requested) {
                Ok(()) => {
                    // Settings and directory are synchronized; preserve the existing native identity.
                    false
                }
                Err(ShortcutPreferencesError::AppliedWithDurabilityUncertain) => {
                    // Rename committed the user's explicit choice; directory-sync failure cannot be reported as unchanged settings.
                    true
                }
                Err(error) => {
                    // Precommit failure preserves the old registration/file and existing failure evidence.
                    return Err(error);
                }
            };
            let view = ShortcutView { binding: requested };
            if let Some(key) = &mut self.active {
                // Keep the same published native identity without replacing it or reauthorizing old events.
                key.view = view;
            }
            self.startup_failure = None;
            return if durability_uncertain {
                // Written settings remain applied; callers refresh actual state before displaying the durability warning.
                Err(ShortcutPreferencesError::AppliedWithDurabilityUncertain)
            } else {
                // Existing registration identity and explicitly saved settings are both confirmed synchronized.
                Ok(view)
            };
        }
        // Candidates are not responsive before settings rename; directory durability does not reverse publication ordering.
        let candidate = self.acquire(requested)?;
        let durability_uncertain = match persist(requested) {
            Ok(()) => {
                // Settings and parent directory synchronized before publication.
                false
            }
            Err(ShortcutPreferencesError::AppliedWithDurabilityUncertain) => {
                // New settings are visible; rolling back Carbon alone would contradict the actual binding and persisted choice.
                true
            }
            Err(error) => {
                // Precommit failure preserves the previous active identity, possibly none, and attempts candidate cleanup once.
                if let Err(status) = self.api.unregister_key(candidate.handle) {
                    // Retain a failed-cleanup inactive handle and prohibit another acquisition until an explicit cleanup retry.
                    self.retired = Some(candidate);
                    self.api.report_failure("rollback_unregister_key", status);
                    return Err(ShortcutPreferencesError::CleanupPending);
                }
                return Err(error);
            }
        };
        // After rename, publish the sole new identity before unregistering the old key, immediately invalidating old queued events.
        let view = candidate.view;
        let id = candidate.id;
        let previous = self.active.replace(candidate);
        self.api.publish_active_id(Some(id));
        // The explicit selection committed; first-registration recovery and binding replacement share this publication point.
        self.startup_failure = None;
        if let Some(previous) = previous {
            // Even failed old-native unregistration leaves its queued events without foreground authority.
            if let Err(status) = self.api.unregister_key(previous.handle) {
                // New binding/settings committed; preserve both the unregistration failure and directory-sync warning.
                self.retired = Some(previous);
                self.api.report_failure("committed_unregister_key", status);
                return Err(if durability_uncertain {
                    // Report both independent postcommit failures together.
                    ShortcutPreferencesError::AppliedWithCleanupAndDurabilityUncertain
                } else {
                    // Settings are synchronized; only the old inactive native registration retains cleanup debt.
                    ShortcutPreferencesError::AppliedWithCleanupPending
                });
            }
        }
        if durability_uncertain {
            // Native cleanup completed, but directory durability of committed settings is still unconfirmed.
            Err(ShortcutPreferencesError::AppliedWithDurabilityUncertain)
        } else {
            // Native handoff and persistence both completed without cleanup debt.
            Ok(view)
        }
    }
}

impl<Api: ShortcutNativeApi> Drop for Registration<Api> {
    fn drop(&mut self) {
        // Make callbacks inert first. Cleanup is finite and OS failures cannot justify a release claim.
        self.api.publish_active_id(None);
        if let Some(key) = self.active.take() {
            // App termination provides the final system cleanup if this one bounded attempt fails.
            if let Err(status) = self.api.unregister_key(key.handle) {
                // Report the unreleased key honestly; shutdown never retries indefinitely or panics.
                self.api.report_failure("drop_active_key", status);
            }
        }
        if let Some(key) = self.retired.take() {
            // There is at most one inactive handle, never an unbounded retry list.
            if let Err(status) = self.api.unregister_key(key.handle) {
                // A failed final inactive-key release remains a native failure, not proven deallocation.
                self.api.report_failure("drop_retired_key", status);
            }
        }
        if let Some(handler) = self.handler.take() {
            // The SDK invalidates this ref after RemoveEventHandler; never retry it on error.
            if let Err(status) = self.api.remove_handler(handler) {
                // The reference is already invalid by contract; logging cannot retry or reuse it.
                self.api.report_failure("drop_handler", status);
            }
        }
    }
}

/// Real Carbon calls remain inside this main-thread adapter; no callback captures Rust ownership.
struct CarbonApi;

impl ShortcutNativeApi for CarbonApi {
    type Handle = *mut c_void;

    fn install_handler(&mut self) -> Result<Self::Handle, i32> {
        // Refuse foreign-thread calls before acquiring or inspecting Carbon state.
        let _mtm = MainThreadMarker::new().ok_or(PARAMETER_ERROR)?;
        // SAFETY: this public non-owning application target accessor runs on the main thread.
        let target = unsafe { GetApplicationEventTarget() };
        if target.is_null() {
            // A missing target transfers no handler ownership.
            return Err(PARAMETER_ERROR);
        }
        let event_type = EventTypeSpec {
            event_class: KEYBOARD_CLASS,
            event_kind: HOT_KEY_PRESSED,
        };
        let mut handler = ptr::null_mut();
        // SAFETY: Carbon copies the packed event descriptor, keeps a static ABI-correct callback,
        // receives no borrowed context, and writes a successful +owned reference to this valid slot.
        let status = unsafe {
            InstallEventHandler(
                target,
                receive,
                1,
                &event_type,
                ptr::null_mut(),
                &mut handler,
            )
        };
        native_reference(status, handler)
    }

    fn register_key(&mut self, binding: ShortcutBinding, id: u32) -> Result<Self::Handle, i32> {
        // Check main-thread access and reserved system combinations before acquisition; refusal creates no native owner.
        let _mtm = MainThreadMarker::new().ok_or(PARAMETER_ERROR)?;
        if system_reserves(binding)? {
            // Carbon may accept reserved shortcuts that the application would never receive.
            return Err(HOT_KEY_EXISTS);
        }
        // Deliver hotkey notifications to the standard dispatcher while installing the handler on the application target.
        // A local macOS 27 controlled keyboard comparison observed successful application-target registration without delivery.
        // SAFETY: Obtain the public borrowed target synchronously on main, without transferring or retaining its ownership.
        let target = unsafe { GetEventDispatcherTarget() };
        if target.is_null() {
            // Without a valid dispatcher, acquire no hotkey and preserve existing settings/registration state.
            return Err(PARAMETER_ERROR);
        }
        let identifier = EventHotKeyId {
            signature: SIGNATURE,
            id,
        };
        let mut key = ptr::null_mut();
        // SAFETY: validated public code/modifiers and packed ID are passed by value; Carbon writes
        // the owned handle into this live slot. Exclusive registration detects competing app owners.
        let status = unsafe {
            RegisterEventHotKey(
                binding.carbon_key_code(),
                binding.carbon_modifiers(),
                identifier,
                target,
                HOT_KEY_EXCLUSIVE,
                &mut key,
            )
        };
        native_reference(status, key)
    }

    fn unregister_key(&mut self, key: Self::Handle) -> Result<(), i32> {
        // All references originate from successful main-thread registration and stay in its TLS owner.
        let _mtm = MainThreadMarker::new().ok_or(PARAMETER_ERROR)?;
        // SAFETY: this live owned key is consumed only on success; failure leaves it tracked for retry.
        native_status(unsafe { UnregisterEventHotKey(key) })
    }

    fn remove_handler(&mut self, handler: Self::Handle) -> Result<(), i32> {
        // Teardown executes on the installing thread, and the static callback has no dangling context.
        let _mtm = MainThreadMarker::new().ok_or(PARAMETER_ERROR)?;
        // SAFETY: this is the sole removal of the successfully installed handler; its ref is never reused.
        native_status(unsafe { RemoveEventHandler(handler) })
    }

    fn publish_active_id(&mut self, id: Option<u32>) {
        // A destroyed TLS gate cannot accept events; teardown never resurrects that cell.
        let _ = ACTIVE_KEY_ID.try_with(|active| active.set(id));
    }

    fn report_failure(&mut self, operation: &'static str, status: i32) {
        // Stable operation names and OSStatus contain neither preference paths nor user content.
        let _ = writeln!(
            std::io::stderr().lock(),
            "shortcut_native operation={operation} status={status}"
        );
    }
}

/// Translate the public OSStatus contract without discarding its original numeric cause.
fn native_status(status: i32) -> Result<(), i32> {
    if status == 0 {
        // Zero is Carbon's successful completion code.
        Ok(())
    } else {
        // A native error is preserved for reporting at the single owning boundary.
        Err(status)
    }
}

/// A reference becomes owned only on a successful native call with a non-null output.
fn native_reference(status: i32, reference: *mut c_void) -> Result<*mut c_void, i32> {
    native_status(status)?;
    if reference.is_null() {
        // An impossible successful-null output cannot be presented as a usable registration.
        return Err(PARAMETER_ERROR);
    }
    Ok(reference)
}

/// Temporarily moves ownership out of TLS so native calls never cross a mutable cell borrow.
struct RegistrationLease(Option<Registration<CarbonApi>>);

impl Drop for RegistrationLease {
    fn drop(&mut self) {
        // Restoration also runs during Rust unwinding; never panic if TLS is already destroying or
        // an unexpected nested borrow violates the lease invariant. Normal operations restore the owner.
        let restored = REGISTRATION
            .try_with(|slot| {
                let Ok(mut slot) = slot.try_borrow_mut() else {
                    // A borrowed cell cannot accept ownership; leave bounded teardown to this lease's field.
                    return false;
                };
                if slot.is_some() {
                    // Never overwrite a different live owner or drop it under this mutable TLS borrow.
                    return false;
                }
                *slot = self.0.take();
                true
            })
            .unwrap_or(false);
        if !restored {
            // An invariant failure closes dispatch through Registration::drop and reports the loss of
            // normal app ownership; it never unwinds again or claims native release succeeded.
            if let Some(owner) = self.0.as_mut() {
                owner.api.report_failure("restore_owner", PARAMETER_ERROR);
            }
        }
    }
}

/// Execute one explicit operation with an owned registration, then restore it on every Rust exit.
fn with_registration<T>(
    operation: impl FnOnce(&mut Registration<CarbonApi>) -> Result<T, ShortcutPreferencesError>,
) -> Result<T, ShortcutPreferencesError> {
    let owner = REGISTRATION
        .with_borrow_mut(Option::take)
        .ok_or(ShortcutPreferencesError::StateUnavailable)?;
    let mut lease = RegistrationLease(Some(owner));
    // The lease was initialized above and remains live through all native/persistence callbacks.
    let owner = lease
        .0
        .as_mut()
        .ok_or(ShortcutPreferencesError::StateUnavailable)?;
    operation(owner)
}

/// Exposes small settings operations while the application retains the independent Carbon owner.
pub(super) struct NativeShortcutPreferences;

impl ShortcutPreferences for NativeShortcutPreferences {
    fn current(&self) -> Result<Option<ShortcutView>, ShortcutPreferencesError> {
        // TLS alone cannot establish legal Carbon/AppKit access.
        let _mtm = MainThreadMarker::new().ok_or(ShortcutPreferencesError::StateUnavailable)?;
        REGISTRATION.with_borrow(|owner| {
            let owner = owner
                .as_ref()
                .ok_or(ShortcutPreferencesError::StateUnavailable)?;
            // An owner without a key is recoverable; an owner currently lent to another call still reports explicit unavailability.
            Ok(owner.current())
        })
    }

    fn update(&self, binding: &str) -> Result<ShortcutView, ShortcutPreferencesError> {
        // Parse and resolve storage before borrowing or acquiring any native ownership.
        let _mtm = MainThreadMarker::new().ok_or(ShortcutPreferencesError::StateUnavailable)?;
        let requested = ShortcutBinding::parse(binding)?;
        let store = settings_store()?;
        with_registration(|owner| {
            // The owned lease releases the TLS borrow before Carbon or filesystem work can reenter.
            owner.update(requested, |binding| store.persist(binding))
        })
    }
}

/// Install only the configured binding; composition exposes settings on startup failure without acquiring alternatives.
pub(super) fn install(_mtm: MainThreadMarker) -> Result<(), String> {
    // Default products omit this diagnostic boundary; acceptance builds still require explicit opt-in.
    #[cfg(feature = "native-acceptance")]
    acceptance_diagnostics::configure_once();
    // Preserve the original settings-read error instead of masking refused configuration with a default key.
    let saved = settings_store().and_then(|store| {
        // Startup reads only the bounded historical preference and retains no file descriptor.
        store.load()
    });
    with_registration(|owner| {
        // Native acquisition/settings reads cross no TLS borrow; failure preserves unregistered state and the original file.
        owner.install_saved(saved)
    })
    .map_err(|error| {
        // The top-level application initialization boundary displays the stable product error.
        error.to_string()
    })
}

/// Explicit app termination cannot rely on Rust stack/TLS unwinding to release its Carbon registrations.
pub(super) fn uninstall(_mtm: MainThreadMarker) {
    // Disable queued callbacks before moving the registration owner out of its shared cell.
    ACTIVE_KEY_ID.set(None);
    let registration = REGISTRATION.take();
    // Native unregister/remove effects may reenter; no TLS borrow remains across bounded, reporting Drop.
    drop(registration);
}

/// After product run stops, verify registration ownership/delivery authority ended without reacquiring a hotkey.
#[cfg(feature = "native-product-acceptance")]
pub(super) fn product_probe_registration_released() -> bool {
    REGISTRATION.with_borrow(Option::is_none) && ACTIVE_KEY_ID.get().is_none()
}

/// Initialized workloads require one actual active registration; recovery UI is not normal hidden residency.
#[cfg(feature = "native-product-acceptance")]
pub(super) fn product_probe_registration_active() -> bool {
    REGISTRATION.with_borrow(|owner| {
        // current copies existing binding metadata only, without alternate acquisition or settings reads.
        owner
            .as_ref()
            .is_some_and(|owner| owner.current().is_some())
    }) && ACTIVE_KEY_ID.get().is_some()
}

/// The footer reports actual active bindings only, never an invented default or temporary key.
pub(super) fn footer() -> String {
    REGISTRATION.with_borrow(|owner| {
        let Some(owner) = owner.as_ref() else {
            // A lent owner exposes no readable active identity; candidates/defaults cannot be reported as registered.
            return "快捷键状态暂不可用".to_owned();
        };
        let Some(view) = owner.current() else {
            // First failures retain bounded localized categories, not paths, file contents or native error descriptions.
            return match owner.startup_failure {
                Some(error) => format!("快捷键未注册，请在设置中选择；{error}"),
                None => "快捷键未注册，请在设置中选择".to_owned(),
            };
        };
        view.binding.display_label()
    })
}

/// Resolve paths on demand; idle shortcut ownership retains no settings file or buffer.
fn settings_store() -> Result<ShortcutSettingsStore, ShortcutPreferencesError> {
    // Location resolution is fallible before any native candidate or settings file is acquired.
    let locations = super::native_plugin_storage_locations()
        .map_err(|_| ShortcutPreferencesError::StorageUnavailable)?;
    Ok(ShortcutSettingsStore::new(locations.application_support))
}

/// Respect enabled macOS shortcuts without requesting Accessibility or changing preferences.
fn system_reserves(binding: ShortcutBinding) -> Result<bool, i32> {
    let mut raw = ptr::null();
    // SAFETY: Carbon initializes the output with a +1 immutable CFArray on success.
    let status = unsafe { CopySymbolicHotKeys(&raw mut raw) };
    // Preserve CopySymbolicHotKeys' native cause instead of inventing key availability after failure.
    native_status(status)?;
    if raw.is_null() {
        // A successful-null array cannot establish key availability and transfers no CF ownership.
        return Err(PARAMETER_ERROR);
    }
    // SAFETY: ownership of the successful Copy result transfers exactly once to CFArray.
    let shortcuts = unsafe { CFArray::<CFType>::wrap_under_create_rule(raw) };
    let key = CFString::from_static_string("kHISymbolicHotKeyCode");
    let modifiers = CFString::from_static_string("kHISymbolicHotKeyModifiers");
    let enabled = CFString::from_static_string("kHISymbolicHotKeyEnabled");
    for entry in &shortcuts {
        // Ignore unrelated records while borrowing each value from the live array.
        let Some(untyped) = entry.downcast::<CFDictionary>() else {
            // The public CF container is heterogeneous, so non-dictionaries are irrelevant.
            continue;
        };
        // SAFETY: the dynamic CFDictionary type check above succeeded; get-rule
        // wrapping retains the borrowed dictionary for this local inspection.
        let dictionary = unsafe {
            CFDictionary::<CFString, CFType>::wrap_under_get_rule(untyped.as_concrete_TypeRef())
        };
        let active = dictionary
            .find(&enabled)
            .and_then(|value| {
                // Only the documented CFBoolean representation enables a reservation.
                value.downcast::<CFBoolean>()
            })
            .is_some_and(bool::from);
        if !active {
            // Disabled system bindings must not prevent a user's app shortcut.
            continue;
        }
        if number(&dictionary, &key) == Some(binding.carbon_key_code() as i32)
            && number(&dictionary, &modifiers) == Some(binding.carbon_modifiers() as i32)
        {
            // Both the physical key and full modifier set identify this reservation.
            return Ok(true);
        }
    }
    // No enabled system record claims this exact validated combination.
    Ok(false)
}

/// Decode the documented numeric property without assuming every dictionary value's type.
fn number(dictionary: &CFDictionary<CFString, CFType>, key: &CFString) -> Option<i32> {
    // Malformed or missing system fields are treated as unrelated records.
    dictionary
        .find(key)
        .and_then(|value| value.downcast::<CFNumber>())
        .and_then(|number| number.to_i32())
}

/// Accept only the committed registration; retired and queued candidate IDs remain inert.
fn accepts_event_id(signature: u32, id: u32) -> bool {
    // A destroyed TLS gate is closed, so callbacks after owner teardown cannot revive application UI.
    ACTIVE_KEY_ID
        .try_with(|active| matches_active_id(signature, id, active.get()))
        .unwrap_or(false)
}

/// Match only the committed identity; physical keys and native handles cannot authorize old events.
fn matches_active_id(signature: u32, id: u32, active: Option<u32>) -> bool {
    signature == SIGNATURE && active == Some(id)
}

/// Validate Carbon input/current registration identity before synchronously entering the existing foreground state machine.
unsafe extern "C" fn receive(_next: *mut c_void, event: EventRef, _context: *mut c_void) -> i32 {
    // Only opted-in acceptance observes callback arrival, without reading or logging other key contents.
    #[cfg(feature = "native-acceptance")]
    acceptance_diagnostics::emit("callback_received");
    if MainThreadMarker::new().is_none() || event.is_null() {
        // Reject non-main/null events before Carbon/AppKit access while preserving event-return policy.
        #[cfg(feature = "native-acceptance")]
        acceptance_diagnostics::emit("callback_rejected_input");
        return EVENT_NOT_HANDLED;
    }
    let mut id = EventHotKeyId {
        signature: 0,
        id: 0,
    };
    // SAFETY: Carbon provides an event valid for the callback and the packed output matches the public ABI;
    // its exclusive main-thread stack slot remains writable, and failed extraction never reads unverified identity data.
    let status = unsafe {
        GetEventParameter(
            event,
            DIRECT_OBJECT,
            HOT_KEY_ID_TYPE,
            ptr::null_mut(),
            mem::size_of::<EventHotKeyId>(),
            ptr::null_mut(),
            (&raw mut id).cast(),
        )
    };
    if status != 0 {
        // Preserve native parameter-extraction failure without interpreting an unverified registration identity.
        #[cfg(feature = "native-acceptance")]
        acceptance_diagnostics::emit("callback_parameter_failed");
        return status;
    }
    if !accepts_event_id(id.signature, id.id) {
        // The application target may serve other owners; stale/foreign identities cannot activate this project's UI.
        #[cfg(feature = "native-acceptance")]
        acceptance_diagnostics::emit("callback_identity_rejected");
        return EVENT_NOT_HANDLED;
    }
    // Only actual matching signature/id delivery obtains the diagnostic lease, without another queue or routing change.
    #[cfg(feature = "native-acceptance")]
    let _native_delivery = acceptance_diagnostics::NativeDeliveryScope::enter();
    #[cfg(feature = "native-acceptance")]
    acceptance_diagnostics::emit("callback_identity_matched");
    #[cfg(feature = "native-acceptance")]
    acceptance_diagnostics::emit("toggle_invoke_direct");

    // Rust panic cannot unwind through Carbon's C ABI; this boundary catches neither abort nor foreign exceptions,
    // preserving the existing unexpected-unwind return policy and restoring the diagnostic lease on stack exit.
    match std::panic::catch_unwind(super::toggle_foreground_surface) {
        Ok(()) => {
            // The synchronous call returned outside TLS borrows; this observation proves neither presentation nor release.
            #[cfg(feature = "native-acceptance")]
            acceptance_diagnostics::emit("toggle_returned");
            0
        }
        Err(_) => {
            // Preserve panic reporting and the unhandled return value without input disclosure or cross-ABI unwind.
            #[cfg(feature = "native-acceptance")]
            acceptance_diagnostics::emit("toggle_panicked");
            let _ = writeln!(
                std::io::stderr().lock(),
                "shortcut_native operation=dispatch status=rust_panic"
            );
            EVENT_NOT_HANDLED
        }
    }
}

#[link(name = "Carbon", kind = "framework")]
unsafe extern "C" {
    fn CopySymbolicHotKeys(shortcut_array: *mut CFArrayRef) -> i32;
    fn GetApplicationEventTarget() -> EventTargetRef;
    fn GetEventDispatcherTarget() -> EventTargetRef;
    fn InstallEventHandler(
        target: EventTargetRef,
        handler: EventHandler,
        // SDK ItemCount is unsigned long; its supported macOS LP64 width must match usize.
        count: usize,
        event_types: *const EventTypeSpec,
        context: *mut c_void,
        result: *mut EventHandlerRef,
    ) -> i32;
    fn RemoveEventHandler(handler: EventHandlerRef) -> i32;
    fn RegisterEventHotKey(
        key_code: u32,
        modifiers: u32,
        identifier: EventHotKeyId,
        target: EventTargetRef,
        options: u32,
        result: *mut EventHotKeyRef,
    ) -> i32;
    fn UnregisterEventHotKey(hot_key: EventHotKeyRef) -> i32;
    fn GetEventParameter(
        event: EventRef,
        name: u32,
        desired_type: u32,
        actual_type: *mut u32,
        size: usize,
        actual_size: *mut usize,
        output: *mut c_void,
    ) -> i32;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{BTreeMap, VecDeque};
    use std::rc::Rc;

    /// Observable native state and queued failures, with no Carbon/AppKit calls or global key changes.
    #[derive(Default)]
    struct FakeState {
        handler_installs: usize,
        handler_removals: usize,
        handler_live: bool,
        registrations: usize,
        // Record every attempted binding, including failure, so success-only accounting cannot hide alternate registrations.
        attempted: Vec<ShortcutBinding>,
        next_handle: usize,
        keys: BTreeMap<usize, (ShortcutBinding, u32)>,
        active_id: Option<u32>,
        register_results: VecDeque<i32>,
        unregister_results: VecDeque<i32>,
        remove_result: i32,
        reports: Vec<(&'static str, i32)>,
    }

    /// Shared observation is confined to the test's thread and is never a native callback context.
    struct FakeApi(Rc<RefCell<FakeState>>);

    impl ShortcutNativeApi for FakeApi {
        type Handle = usize;

        fn install_handler(&mut self) -> Result<Self::Handle, i32> {
            let mut state = self.0.borrow_mut();
            if state.handler_live {
                // Reproduce Carbon's exact uniqueness rule so repeated-handler regressions fail.
                return Err(-9866);
            }
            state.handler_live = true;
            state.handler_installs += 1;
            Ok(1)
        }

        fn register_key(&mut self, binding: ShortcutBinding, id: u32) -> Result<Self::Handle, i32> {
            let mut state = self.0.borrow_mut();
            state.registrations += 1;
            state.attempted.push(binding);
            if let Some(status) = state.register_results.pop_front() {
                // An injected OS failure transfers no key, matching the production ownership boundary.
                native_status(status)?;
            }
            state.next_handle += 1;
            let handle = state.next_handle;
            state.keys.insert(handle, (binding, id));
            Ok(handle)
        }

        fn unregister_key(&mut self, key: Self::Handle) -> Result<(), i32> {
            let mut state = self.0.borrow_mut();
            if let Some(status) = state.unregister_results.pop_front() {
                // Failed cleanup keeps the key live, making lost-handle bugs visible to the assertions.
                native_status(status)?;
            }
            assert!(
                state.keys.remove(&key).is_some(),
                "a native key was released twice"
            );
            Ok(())
        }

        fn remove_handler(&mut self, _handler: Self::Handle) -> Result<(), i32> {
            let mut state = self.0.borrow_mut();
            state.handler_removals += 1;
            state.handler_live = false;
            // Like Carbon, the reference is consumed even when the status must be reported.
            native_status(state.remove_result)
        }

        fn publish_active_id(&mut self, id: Option<u32>) {
            // The test sees the same one-ID gate used by the real static callback.
            self.0.borrow_mut().active_id = id;
        }

        fn report_failure(&mut self, operation: &'static str, status: i32) {
            // Preserve exact native causes for assertions without printing expected test failures.
            self.0.borrow_mut().reports.push((operation, status));
        }
    }

    /// Construct one isolated owner and expose only its fake operating-system observations.
    fn owner() -> (Registration<FakeApi>, Rc<RefCell<FakeState>>) {
        let state = Rc::new(RefCell::new(FakeState::default()));
        (Registration::new(FakeApi(Rc::clone(&state))), state)
    }

    /// A changed key must reuse the sole handler, commit a fresh ID, and reject already-queued old IDs.
    #[test]
    fn changes_reuse_handler_and_filter_old_and_foreign_events() {
        let (mut owner, state) = owner();
        owner.install(ShortcutBinding::CommandSpace).unwrap();
        let old_id = state.borrow().active_id.unwrap();
        let persisted = Cell::new(ShortcutBinding::CommandSpace);
        owner
            .update(ShortcutBinding::OptionSpace, |binding| {
                // Persistence occurs while the old ID is still the only dispatchable registration.
                assert_eq!(state.borrow().active_id, Some(old_id));
                assert_eq!(state.borrow().keys.len(), 2);
                persisted.set(binding);
                Ok(())
            })
            .unwrap();
        let state = state.borrow();
        let new_id = state.active_id.unwrap();
        assert_ne!(old_id, new_id);
        assert_eq!(state.handler_installs, 1);
        assert_eq!(state.handler_removals, 0);
        assert_eq!(state.keys.len(), 1);
        assert_eq!(persisted.get(), ShortcutBinding::OptionSpace);
        assert!(matches_active_id(SIGNATURE, new_id, state.active_id));
        assert!(!matches_active_id(SIGNATURE, old_id, state.active_id));
        assert!(!matches_active_id(0, new_id, state.active_id));
        assert!(!matches_active_id(SIGNATURE, new_id, None));
    }

    /// Registration conflict attempts the configured key only; an available alternative result must remain unused.
    #[test]
    fn startup_conflict_never_attempts_an_unconfigured_key() {
        let custom = ShortcutBinding::parse("control+alt+KeyK").unwrap();
        for binding in ShortcutBinding::ALL.into_iter().chain([custom]) {
            // Cover all original standard/custom saved keys; consuming the second result reveals unauthorized retry.
            let (mut owner, state) = owner();
            state
                .borrow_mut()
                .register_results
                .extend([HOT_KEY_EXISTS, 0]);
            assert_eq!(
                owner.install_saved(Ok(Some(binding))),
                Err(ShortcutPreferencesError::RegistrationUnavailable)
            );
            let observed = state.borrow();
            assert_eq!(observed.attempted, [binding]);
            assert_eq!(observed.register_results, [0]);
            assert!(observed.keys.is_empty());
            assert!(observed.active_id.is_none());
            assert!(owner.current().is_none());
            assert_eq!(
                owner.startup_failure,
                Some(ShortcutPreferencesError::RegistrationUnavailable)
            );
        }
    }

    /// Missing first-run settings attempt the existing default once; conflict leaves no settings or alternative registration.
    #[test]
    fn missing_configuration_attempts_only_default_once_without_writing() {
        for status in [0, HOT_KEY_EXISTS] {
            let root = tempfile::tempdir().unwrap();
            let store = ShortcutSettingsStore::new(root.path().to_owned());
            let (mut owner, state) = owner();
            state.borrow_mut().register_results.extend([status, 0]);
            let result = owner.install_saved(store.load());
            assert_eq!(result.is_ok(), status == 0);
            assert_eq!(state.borrow().attempted, [ShortcutBinding::default()]);
            assert_eq!(state.borrow().register_results, [0]);
            assert_eq!(store.load(), Ok(None));
            assert_eq!(owner.current().is_some(), status == 0);
        }
    }

    /// Startup failure preserves handler ownership for symmetric release without leaving a false active identity.
    #[test]
    fn failed_startup_releases_its_only_handler_without_a_key() {
        let (mut owner, state) = owner();
        state
            .borrow_mut()
            .register_results
            .push_back(HOT_KEY_EXISTS);
        assert_eq!(
            owner.install(ShortcutBinding::OptionSpace),
            Err(ShortcutPreferencesError::RegistrationUnavailable)
        );
        assert!(owner.current().is_none());
        assert!(state.borrow().active_id.is_none());
        assert_eq!(state.borrow().registrations, 1);
        assert!(state.borrow().keys.is_empty());
        drop(owner);
        assert_eq!(state.borrow().handler_installs, 1);
        assert_eq!(state.borrow().handler_removals, 1);
        assert!(!state.borrow().handler_live);
    }

    /// Rejected native acquisition must not write preferences or disturb the usable old key.
    #[test]
    fn acquisition_failure_preserves_old_native_and_persisted_state() {
        let (mut owner, state) = owner();
        owner.install(ShortcutBinding::CommandSpace).unwrap();
        let old_id = state.borrow().active_id;
        state.borrow_mut().register_results.push_back(-9878);
        let result = owner.update(ShortcutBinding::OptionSpace, |_| {
            // A failed acquisition must return before any filesystem commit is requested.
            panic!("persistence ran after native acquisition failed")
        });
        assert_eq!(
            result,
            Err(ShortcutPreferencesError::RegistrationUnavailable)
        );
        assert_eq!(
            owner.current().unwrap().binding,
            ShortcutBinding::CommandSpace
        );
        assert_eq!(state.borrow().active_id, old_id);
        assert_eq!(state.borrow().keys.len(), 1);
        assert!(owner.retired.is_none());
    }

    /// An actual failed pending-file write must preserve the old file and release the candidate.
    #[test]
    fn persistence_failure_preserves_file_and_old_key() {
        let (mut owner, state) = owner();
        owner.install(ShortcutBinding::CommandSpace).unwrap();
        let root = tempfile::tempdir().unwrap();
        let store = ShortcutSettingsStore::new(root.path().to_owned());
        store.persist(ShortcutBinding::CommandSpace).unwrap();
        // Fail the real unique sibling's file sync before rename, independent of filename or permissions.
        assert_eq!(
            owner.update(ShortcutBinding::OptionSpace, |binding| store
                .persist_with_file_sync_failure(binding)),
            Err(ShortcutPreferencesError::StorageUnavailable)
        );
        assert_eq!(store.load(), Ok(Some(ShortcutBinding::CommandSpace)));
        assert_eq!(
            owner.current().unwrap().binding,
            ShortcutBinding::CommandSpace
        );
        assert_eq!(state.borrow().keys.len(), 1);
        assert!(owner.retired.is_none());
    }

    /// A postcommit directory-sync warning must publish the new native ID instead of rolling it back.
    #[test]
    fn committed_directory_sync_failure_keeps_real_new_file_and_registration() {
        let (mut owner, state) = owner();
        owner.install(ShortcutBinding::CommandSpace).unwrap();
        let old_id = state.borrow().active_id.unwrap();
        let root = tempfile::tempdir().unwrap();
        let store = ShortcutSettingsStore::new(root.path().to_owned());
        store.persist(ShortcutBinding::CommandSpace).unwrap();
        assert_eq!(
            owner.update(ShortcutBinding::OptionSpace, |binding| {
                // A candidate remains inactive while the actual rename/sync operation is running.
                assert_eq!(state.borrow().active_id, Some(old_id));
                store.persist_with_directory_sync_failure(binding)
            }),
            Err(ShortcutPreferencesError::AppliedWithDurabilityUncertain)
        );
        // Both live Carbon state and readable bytes describe the committed choice despite its warning.
        assert_eq!(
            owner.current().unwrap().binding,
            ShortcutBinding::OptionSpace
        );
        assert_eq!(store.load(), Ok(Some(ShortcutBinding::OptionSpace)));
        let state = state.borrow();
        assert_ne!(state.active_id, Some(old_id));
        assert!(!matches_active_id(SIGNATURE, old_id, state.active_id));
        assert_eq!(state.keys.len(), 1);
        assert!(state.reports.is_empty());
        assert!(owner.retired.is_none());
    }

    /// Independent postcommit native cleanup and directory-sync failures must both survive the result.
    #[test]
    fn committed_cleanup_and_directory_sync_failures_keep_both_facts_and_bounded_debt() {
        let (mut owner, state) = owner();
        owner.install(ShortcutBinding::CommandSpace).unwrap();
        let old_id = state.borrow().active_id.unwrap();
        let root = tempfile::tempdir().unwrap();
        let store = ShortcutSettingsStore::new(root.path().to_owned());
        store.persist(ShortcutBinding::CommandSpace).unwrap();
        state.borrow_mut().unregister_results.extend([-18, -19, 0]);
        assert_eq!(
            owner.update(ShortcutBinding::OptionSpace, |binding| {
                // The real file rename commits before both injected postcommit failures are reported.
                store.persist_with_directory_sync_failure(binding)
            }),
            Err(ShortcutPreferencesError::AppliedWithCleanupAndDurabilityUncertain)
        );
        let committed_id = state.borrow().active_id.unwrap();
        assert_ne!(committed_id, old_id);
        assert_eq!(
            owner.current().unwrap().binding,
            ShortcutBinding::OptionSpace
        );
        assert_eq!(store.load(), Ok(Some(ShortcutBinding::OptionSpace)));
        assert_eq!(owner.retired.as_ref().unwrap().id, old_id);
        assert_eq!(state.borrow().keys.len(), 2);
        assert!(!matches_active_id(SIGNATURE, old_id, Some(committed_id)));
        assert_eq!(state.borrow().reports, [("committed_unregister_key", -18)]);
        // A failed explicit debt retry cannot acquire another key or invoke storage for a new request.
        assert_eq!(
            owner.update(ShortcutBinding::CommandShiftK, |_| {
                panic!("retained cleanup debt must block a new persistence request")
            }),
            Err(ShortcutPreferencesError::CleanupPending)
        );
        assert_eq!(state.borrow().registrations, 2);
        assert_eq!(state.borrow().active_id, Some(committed_id));
        // A later same-binding retry releases the old inactive owner and successfully synchronizes
        // the committed preference, without fabricating a rollback or allocating a third registration.
        owner
            .update(ShortcutBinding::OptionSpace, |binding| {
                store.persist(binding)
            })
            .unwrap();
        assert_eq!(state.borrow().active_id, Some(committed_id));
        assert_eq!(state.borrow().registrations, 2);
        assert_eq!(state.borrow().keys.len(), 1);
        assert!(owner.retired.is_none());
        assert_eq!(
            state.borrow().reports,
            [
                ("committed_unregister_key", -18),
                ("retry_unregister_key", -19)
            ]
        );
    }

    /// Explicitly saving the same active key preserves its identity/written settings after directory-sync failure.
    #[test]
    fn committed_same_binding_directory_sync_failure_keeps_id() {
        let (mut owner, state) = owner();
        owner.install(ShortcutBinding::OptionSpace).unwrap();
        let previous = owner.current().unwrap();
        let id = state.borrow().active_id;
        let root = tempfile::tempdir().unwrap();
        let store = ShortcutSettingsStore::new(root.path().to_owned());
        assert_eq!(
            owner.update(previous.binding, |binding| store
                .persist_with_directory_sync_failure(binding)),
            Err(ShortcutPreferencesError::AppliedWithDurabilityUncertain)
        );
        assert_eq!(owner.current(), Some(previous));
        assert_eq!(store.load(), Ok(Some(previous.binding)));
        assert_eq!(state.borrow().active_id, id);
        assert_eq!(state.borrow().registrations, 1);
        assert_eq!(state.borrow().keys.len(), 1);
    }

    /// Failed rollback cleanup remains owned and prevents any unbounded accumulation of native keys.
    #[test]
    fn failed_candidate_cleanup_blocks_acquisition_until_explicit_retry_succeeds() {
        let (mut owner, state) = owner();
        owner.install(ShortcutBinding::CommandSpace).unwrap();
        state.borrow_mut().unregister_results.extend([-11, -12, 0]);
        assert_eq!(
            owner.update(ShortcutBinding::OptionSpace, |_| Err(
                ShortcutPreferencesError::StorageUnavailable
            )),
            Err(ShortcutPreferencesError::CleanupPending)
        );
        let candidate_id = owner.retired.as_ref().unwrap().id;
        assert_eq!(state.borrow().keys.len(), 2);
        assert!(!matches_active_id(
            SIGNATURE,
            candidate_id,
            state.borrow().active_id
        ));
        assert_eq!(
            owner.update(ShortcutBinding::CommandShiftK, |_| panic!(
                "cleanup debt must block persistence"
            )),
            Err(ShortcutPreferencesError::CleanupPending)
        );
        assert_eq!(state.borrow().registrations, 2);
        assert_eq!(
            owner.current().unwrap().binding,
            ShortcutBinding::CommandSpace
        );
        // A third explicit request retries once, releases the debt, and may then acquire a successor.
        owner
            .update(ShortcutBinding::CommandShiftK, |_| Ok(()))
            .unwrap();
        assert!(owner.retired.is_none());
        assert_eq!(state.borrow().keys.len(), 1);
        assert_eq!(state.borrow().handler_installs, 1);
        assert_eq!(state.borrow().registrations, 3);
        assert_eq!(
            state.borrow().reports,
            vec![
                ("rollback_unregister_key", -11),
                ("retry_unregister_key", -12)
            ]
        );
    }

    /// A failed old-key release after persistence must report an applied binding, never a fake rollback.
    #[test]
    fn committed_cleanup_error_reports_real_new_view_and_retries_without_new_registration() {
        let (mut owner, state) = owner();
        owner.install(ShortcutBinding::CommandSpace).unwrap();
        let old_id = state.borrow().active_id.unwrap();
        let persisted = Cell::new(ShortcutBinding::CommandSpace);
        state.borrow_mut().unregister_results.push_back(-13);
        assert_eq!(
            owner.update(ShortcutBinding::OptionSpace, |binding| {
                persisted.set(binding);
                Ok(())
            }),
            Err(ShortcutPreferencesError::AppliedWithCleanupPending)
        );
        assert_eq!(
            owner.current().unwrap().binding,
            ShortcutBinding::OptionSpace
        );
        assert_eq!(persisted.get(), ShortcutBinding::OptionSpace);
        assert_eq!(state.borrow().keys.len(), 2);
        assert!(!matches_active_id(
            SIGNATURE,
            old_id,
            state.borrow().active_id
        ));
        // Selecting the actual key retries cleanup and persists that view without another acquisition.
        owner
            .update(ShortcutBinding::OptionSpace, |_| Ok(()))
            .unwrap();
        assert!(owner.retired.is_none());
        assert_eq!(state.borrow().registrations, 2);
        assert_eq!(state.borrow().keys.len(), 1);
    }

    /// Final cleanup closes dispatch first and reports each failure without retrying an invalid handler.
    #[test]
    fn drop_closes_gate_and_reports_bounded_native_failures() {
        let (mut owner, state) = owner();
        owner.install(ShortcutBinding::CommandSpace).unwrap();
        state.borrow_mut().unregister_results.push_back(-14);
        assert_eq!(
            owner.update(ShortcutBinding::OptionSpace, |_| Ok(())),
            Err(ShortcutPreferencesError::AppliedWithCleanupPending)
        );
        state.borrow_mut().unregister_results.extend([-15, -16]);
        state.borrow_mut().remove_result = -17;
        drop(owner);
        let state = state.borrow();
        assert_eq!(state.active_id, None);
        assert_eq!(state.handler_removals, 1);
        assert_eq!(
            state.reports,
            vec![
                ("committed_unregister_key", -14),
                ("drop_active_key", -15),
                ("drop_retired_key", -16),
                ("drop_handler", -17)
            ]
        );
        // Failed native cleanup remains an observed failure; no test claims those OS resources released.
        assert_eq!(state.keys.len(), 2);
    }

    /// Failed same-key save preserves active identity; another explicit save does not duplicate native registration.
    #[test]
    fn same_binding_precommit_failure_preserves_active_id_without_acquisition() {
        let (mut owner, state) = owner();
        owner.install(ShortcutBinding::CommandSpace).unwrap();
        let previous = owner.current().unwrap();
        let id = state.borrow().active_id;
        assert_eq!(
            owner.update(previous.binding, |_| Err(
                ShortcutPreferencesError::StorageUnavailable
            )),
            Err(ShortcutPreferencesError::StorageUnavailable)
        );
        assert_eq!(owner.current(), Some(previous));
        owner.update(previous.binding, |_| Ok(())).unwrap();
        assert_eq!(state.borrow().active_id, id);
        assert_eq!(state.borrow().registrations, 1);
    }

    /// Reentrant access fails closed, and a Rust unwind restores the owner without a live TLS borrow.
    #[test]
    fn ownership_lease_restores_after_reentrant_rejection_and_unwind() {
        let result = std::panic::catch_unwind(|| {
            // The empty application owner acquires no Carbon resources in this pure lease test.
            let _ = with_registration::<()>(|_| {
                assert!(REGISTRATION.with_borrow(Option::is_none));
                assert_eq!(
                    with_registration::<()>(|_| {
                        // Reentrant access must be rejected before a second owner operation can execute.
                        panic!("reentrant operation acquired the leased owner")
                    }),
                    Err(ShortcutPreferencesError::StateUnavailable)
                );
                panic!("synthetic failure after native ownership was leased")
            });
        });
        assert!(result.is_err());
        assert!(REGISTRATION.with_borrow(Option::is_some));
        // A returned failure likewise restores the same empty owner for subsequent explicit work.
        assert_eq!(
            with_registration::<()>(|_| { Err(ShortcutPreferencesError::StorageUnavailable) }),
            Err(ShortcutPreferencesError::StorageUnavailable)
        );
        assert!(REGISTRATION.with_borrow(Option::is_some));
    }

    /// Exhausting the finite ID space must fail closed rather than accepting a previous queued event.
    #[test]
    fn exhausted_ids_preserve_old_key_without_reuse() {
        let (mut owner, state) = owner();
        owner.install(ShortcutBinding::CommandSpace).unwrap();
        let old_id = state.borrow().active_id;
        owner.next_id = u32::MAX;
        assert_eq!(
            owner.update(ShortcutBinding::OptionSpace, |_| panic!(
                "ID exhaustion must block persistence"
            )),
            Err(ShortcutPreferencesError::RegistrationUnavailable)
        );
        assert_eq!(state.borrow().active_id, old_id);
        assert_eq!(state.borrow().registrations, 1);
    }

    /// Real custom preference reload selects its own key first without touching the stored representation.
    #[test]
    fn custom_saved_binding_survives_startup_and_reconstructed_owner() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("launcher-shortcut");
        let original = b"CTRL+OPTION+KeyK\n";
        std::fs::write(&path, original).unwrap();
        let store = ShortcutSettingsStore::new(root.path().to_owned());
        let custom = ShortcutBinding::parse("control+alt+KeyK").unwrap();
        for _ in 0..2 {
            // A fresh owner models application registration reconstruction after releasing the prior owner.
            let (mut owner, state) = owner();
            owner.install_saved(store.load()).unwrap();
            assert_eq!(owner.current().unwrap(), ShortcutView { binding: custom });
            assert_eq!(state.borrow().registrations, 1);
            assert_eq!(std::fs::read(&path).unwrap(), original);
        }
    }

    /// Custom-binding conflict preserves the original file and no-active state, without substituting a standard choice.
    #[test]
    fn custom_startup_conflict_preserves_file_and_remains_unregistered() {
        let root = tempfile::tempdir().unwrap();
        let store = ShortcutSettingsStore::new(root.path().to_owned());
        let custom = ShortcutBinding::parse("control+alt+KeyK").unwrap();
        store.persist(custom).unwrap();
        let (mut owner, state) = owner();
        state
            .borrow_mut()
            .register_results
            .push_back(HOT_KEY_EXISTS);
        assert_eq!(
            owner.install_saved(store.load()),
            Err(ShortcutPreferencesError::RegistrationUnavailable)
        );
        assert!(owner.current().is_none());
        assert_eq!(store.load(), Ok(Some(custom)));
        assert_eq!(state.borrow().attempted, [custom]);
    }

    /// Corrupt/unsupported settings register nothing; unreadable paths follow no symlink and attempt no default.
    #[test]
    fn rejected_saved_preferences_preserve_bytes_without_native_acquisition() {
        for (contents, error) in [
            (
                "control+F21\n",
                ShortcutPreferencesError::UnsupportedBinding,
            ),
            ("not+a+shortcut\n", ShortcutPreferencesError::InvalidBinding),
            (
                "shift+KeyK\n",
                ShortcutPreferencesError::MissingPrimaryModifier,
            ),
        ] {
            let root = tempfile::tempdir().unwrap();
            let path = root.path().join("launcher-shortcut");
            std::fs::write(&path, contents).unwrap();
            let store = ShortcutSettingsStore::new(root.path().to_owned());
            let (mut owner, state) = owner();
            assert_eq!(owner.install_saved(store.load()), Err(error));
            assert_eq!(owner.startup_failure, Some(error));
            assert!(owner.current().is_none());
            assert_eq!(std::fs::read_to_string(&path).unwrap(), contents);
            assert_eq!(state.borrow().handler_installs, 0);
            assert!(state.borrow().attempted.is_empty());
        }
        // Actual symlink rejection preserves its target and acquires no alternate binding.
        let root = tempfile::tempdir().unwrap();
        let victim = root.path().join("fixture-settings-source");
        std::fs::write(&victim, b"control+alt+KeyK\n").unwrap();
        std::os::unix::fs::symlink(&victim, root.path().join("launcher-shortcut")).unwrap();
        let store = ShortcutSettingsStore::new(root.path().to_owned());
        let (mut owner, state) = owner();
        assert_eq!(
            owner.install_saved(store.load()),
            Err(ShortcutPreferencesError::StorageUnavailable)
        );
        assert!(owner.current().is_none());
        assert_eq!(state.borrow().handler_installs, 0);
        assert!(state.borrow().attempted.is_empty());
        assert_eq!(std::fs::read(&victim).unwrap(), b"control+alt+KeyK\n");
        assert!(root.path().join("launcher-shortcut").is_symlink());
    }

    /// Explicit selection can recover no-active state; precommit failure stays unregistered, postcommit warnings do not roll back.
    #[test]
    fn explicit_repair_from_unregistered_keeps_transaction_boundaries() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("launcher-shortcut");
        let original = b"control+F21\n";
        std::fs::write(&path, original).unwrap();
        let store = ShortcutSettingsStore::new(root.path().to_owned());
        let (mut owner, state) = owner();
        assert_eq!(
            owner.install_saved(store.load()),
            Err(ShortcutPreferencesError::UnsupportedBinding)
        );
        let requested = ShortcutBinding::OptionSpace;
        assert_eq!(
            owner.update(requested, |binding| {
                assert!(state.borrow().active_id.is_none());
                assert_eq!(state.borrow().keys.len(), 1);
                store.persist_with_file_sync_failure(binding)
            }),
            Err(ShortcutPreferencesError::StorageUnavailable)
        );
        assert!(owner.current().is_none());
        assert!(state.borrow().keys.is_empty());
        assert_eq!(std::fs::read(&path).unwrap(), original);
        assert_eq!(
            owner.startup_failure,
            Some(ShortcutPreferencesError::UnsupportedBinding)
        );
        assert_eq!(
            owner.update(requested, |binding| {
                assert!(state.borrow().active_id.is_none());
                store.persist_with_directory_sync_failure(binding)
            }),
            Err(ShortcutPreferencesError::AppliedWithDurabilityUncertain)
        );
        assert_eq!(owner.current().unwrap().binding, requested);
        assert!(state.borrow().active_id.is_some());
        assert_eq!(state.borrow().keys.len(), 1);
        assert_eq!(store.load(), Ok(Some(requested)));
        assert!(owner.startup_failure.is_none());
    }

    /// A failed candidate in no-active state still creates bounded cleanup debt, blocking another acquisition before retry.
    #[test]
    fn inactive_repair_cleanup_debt_blocks_then_allows_only_explicit_retry() {
        let (mut owner, state) = owner();
        state.borrow_mut().unregister_results.extend([-18, -19, 0]);
        assert_eq!(
            owner.update(ShortcutBinding::OptionSpace, |_| {
                // The candidate is uncommitted, so Carbon callbacks still have no active identity.
                assert!(state.borrow().active_id.is_none());
                Err(ShortcutPreferencesError::StorageUnavailable)
            }),
            Err(ShortcutPreferencesError::CleanupPending)
        );
        assert!(owner.current().is_none());
        assert_eq!(state.borrow().keys.len(), 1);
        assert_eq!(
            owner.update(ShortcutBinding::CommandShiftK, |_| panic!(
                "清理失败不允许持久化"
            )),
            Err(ShortcutPreferencesError::CleanupPending)
        );
        assert_eq!(state.borrow().attempted, [ShortcutBinding::OptionSpace]);
        assert!(state.borrow().active_id.is_none());
        // Only a later explicit user's successful cleanup retry permits acquisition/publication of the selected binding.
        owner
            .update(ShortcutBinding::CommandShiftK, |_| Ok(()))
            .unwrap();
        assert_eq!(
            state.borrow().attempted,
            [ShortcutBinding::OptionSpace, ShortcutBinding::CommandShiftK]
        );
        assert_eq!(
            owner.current().unwrap().binding,
            ShortcutBinding::CommandShiftK
        );
        assert_eq!(state.borrow().keys.len(), 1);
        assert!(owner.retired.is_none());
    }

    /// Acquisition failure without an active key writes no settings; later explicit success publishes the sole responsive identity.
    #[test]
    fn inactive_acquisition_failure_and_success_preserve_exact_commit_boundary() {
        let (mut owner, state) = owner();
        state
            .borrow_mut()
            .register_results
            .push_back(HOT_KEY_EXISTS);
        assert_eq!(
            owner.update(ShortcutBinding::OptionSpace, |_| panic!(
                "取得失败不允许持久化"
            )),
            Err(ShortcutPreferencesError::RegistrationUnavailable)
        );
        assert!(owner.current().is_none());
        assert!(state.borrow().keys.is_empty());
        let root = tempfile::tempdir().unwrap();
        let store = ShortcutSettingsStore::new(root.path().to_owned());
        owner
            .update(ShortcutBinding::OptionSpace, |binding| {
                assert!(state.borrow().active_id.is_none());
                assert_eq!(state.borrow().keys.len(), 1);
                store.persist(binding)
            })
            .unwrap();
        assert_eq!(store.load(), Ok(Some(ShortcutBinding::OptionSpace)));
        assert_eq!(
            owner.current().unwrap().binding,
            ShortcutBinding::OptionSpace
        );
        let active_id = state.borrow().active_id;
        // Repeated initialization cannot replace the user's committed active binding with another preferred value.
        owner
            .install_saved(Ok(Some(ShortcutBinding::CommandShiftK)))
            .unwrap();
        assert_eq!(state.borrow().active_id, active_id);
        assert_eq!(state.borrow().attempted, [ShortcutBinding::OptionSpace; 2]);
    }

    /// Failed explicit changes preserve a loaded custom binding and its exact existing file.
    #[test]
    fn failed_change_from_custom_retains_current_key_and_existing_bytes() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("launcher-shortcut");
        let original = b"CONTROL+ALT+KeyK\n";
        std::fs::write(&path, original).unwrap();
        let store = ShortcutSettingsStore::new(root.path().to_owned());
        let (mut owner, state) = owner();
        owner.install_saved(store.load()).unwrap();
        let previous = owner.current().unwrap();
        let old_id = state.borrow().active_id;
        state
            .borrow_mut()
            .register_results
            .push_back(HOT_KEY_EXISTS);
        assert_eq!(
            owner.update(ShortcutBinding::OptionSpace, |_| panic!(
                "conflict must not persist"
            )),
            Err(ShortcutPreferencesError::RegistrationUnavailable)
        );
        assert_eq!(owner.current().unwrap(), previous);
        assert_eq!(
            owner.update(ShortcutBinding::OptionSpace, |binding| store
                .persist_with_file_sync_failure(binding)),
            Err(ShortcutPreferencesError::StorageUnavailable)
        );
        assert_eq!(owner.current().unwrap(), previous);
        assert_eq!(state.borrow().active_id, old_id);
        assert_eq!(state.borrow().keys.len(), 1);
        assert_eq!(std::fs::read(&path).unwrap(), original);
    }
}
