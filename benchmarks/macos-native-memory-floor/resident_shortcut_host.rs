#![no_main]
#![no_std]

// This standalone Carbon floor control is compiled directly by the benchmark script, never
// linked into the product. Its fixed shortcut is independent of the user's stored binding.

use core::ffi::c_void;
use core::mem;
use core::panic::PanicInfo;
use core::ptr;
#[cfg(omni_shortcut_probe)]
use core::sync::atomic::{AtomicBool, Ordering};

type EventHandlerCallRef = *mut c_void;
type EventRef = *mut c_void;
type EventTargetRef = *mut c_void;
type EventHandlerRef = *mut c_void;
type EventHotKeyRef = *mut c_void;

const OMNI_HOT_KEY_SIGNATURE: u32 = 0x4f4d_4e49;
const OMNI_HOT_KEY_IDENTIFIER: u32 = 1;
const OMNI_HOT_KEY_CODE: u32 = 0x31;
const OMNI_HOT_KEY_MODIFIERS: u32 = (1 << 8) | (1 << 9);
#[cfg(omni_shortcut_probe)]
static BENCHMARK_HOT_KEY_RECEIVED: AtomicBool = AtomicBool::new(false);

#[repr(C, packed(2))]
/// Match Carbon's two-byte-packed event descriptor without relying on Rust's default layout.
struct EventTypeSpec {
    event_class: u32,
    event_kind: u32,
}

#[repr(C, packed(2))]
/// Keep both registration identity fields; one matching scalar cannot authorize delivery.
struct EventHotKeyId {
    signature: u32,
    id: u32,
}

type EventHandler = unsafe extern "C" fn(EventHandlerCallRef, EventRef, *mut c_void) -> i32;

#[link(name = "Carbon", kind = "framework")]
unsafe extern "C" {
    fn abort() -> !;
    fn GetEventDispatcherTarget() -> EventTargetRef;
    fn InstallEventHandler(
        target: EventTargetRef,
        handler: EventHandler,
        event_type_count: u32,
        event_types: *const EventTypeSpec,
        context: *mut c_void,
        installed_handler: *mut EventHandlerRef,
    ) -> i32;
    fn RegisterEventHotKey(
        key_code: u32,
        modifiers: u32,
        identifier: EventHotKeyId,
        target: EventTargetRef,
        options: u32,
        registered_hot_key: *mut EventHotKeyRef,
    ) -> i32;
    fn GetEventParameter(
        event: EventRef,
        parameter_name: u32,
        desired_type: u32,
        actual_type: *mut u32,
        buffer_size: usize,
        actual_size: *mut usize,
        data: *mut c_void,
    ) -> i32;
    fn ReceiveNextEvent(
        event_type_count: usize,
        event_types: *const EventTypeSpec,
        timeout: f64,
        pull_event: u8,
        received_event: *mut EventRef,
    ) -> i32;
    fn SendEventToEventTarget(event: EventRef, target: EventTargetRef) -> i32;
    fn ReleaseEvent(event: EventRef);
}

#[panic_handler]
/// A no-std comparison has no recovery runtime; abort rather than unwind across Carbon callbacks.
fn abort_on_panic(_panic: &PanicInfo<'_>) -> ! {
    // SAFETY: The platform abort primitive never returns and borrows no Rust-owned value.
    unsafe { abort() }
}

/// Read only the original typed event payload and reject any other registration's notification.
unsafe extern "C" fn accept_registered_hot_key(
    _next_handler: EventHandlerCallRef,
    event: EventRef,
    _context: *mut c_void,
) -> i32 {
    const EVENT_NOT_HANDLED: i32 = -9874;
    const EVENT_PARAM_DIRECT_OBJECT: u32 = 0x2d2d_2d2d;
    const TYPE_EVENT_HOT_KEY_ID: u32 = 0x686b_6964;
    let mut received_hot_key = EventHotKeyId {
        signature: 0,
        id: 0,
    };
    // SAFETY: Carbon lent this event for the callback. The exact typed stack buffer remains
    // allocated for the synchronous parameter read; optional metadata outputs are intentionally null.
    let status = unsafe {
        GetEventParameter(
            event,
            EVENT_PARAM_DIRECT_OBJECT,
            TYPE_EVENT_HOT_KEY_ID,
            ptr::null_mut(),
            mem::size_of::<EventHotKeyId>(),
            ptr::null_mut(),
            (&raw mut received_hot_key).cast(),
        )
    };
    if status != 0 {
        return status;
    }
    // A successful payload read still needs both fields to match this control's registration.
    if received_hot_key.signature != OMNI_HOT_KEY_SIGNATURE
        || received_hot_key.id != OMNI_HOT_KEY_IDENTIFIER
    {
        return EVENT_NOT_HANDLED;
    }

    #[cfg(omni_shortcut_probe)]
    // This single scalar is only an exit condition for the explicit probe build, never a product receipt.
    BENCHMARK_HOT_KEY_RECEIVED.store(true, Ordering::Relaxed);

    0
}

#[unsafe(no_mangle)]
/// Own one handler and one hot key for the process lifetime, without constructing AppKit or an allocator.
pub extern "C" fn main() -> i32 {
    const EVENT_CLASS_KEYBOARD: u32 = 0x6b65_7962;
    const EVENT_HOT_KEY_PRESSED: u32 = 5;
    let event_type = EventTypeSpec {
        event_class: EVENT_CLASS_KEYBOARD,
        event_kind: EVENT_HOT_KEY_PRESSED,
    };
    let hot_key_identifier = EventHotKeyId {
        signature: OMNI_HOT_KEY_SIGNATURE,
        id: OMNI_HOT_KEY_IDENTIFIER,
    };
    let mut installed_handler = ptr::null_mut();
    let mut registered_hot_key = ptr::null_mut();

    // SAFETY: The event descriptor and registration buffers remain live throughout the native
    // calls. Every received event is released before the next iteration; no pointer escapes to Rust.
    unsafe {
        let event_dispatcher = GetEventDispatcherTarget();
        if InstallEventHandler(
            event_dispatcher,
            accept_registered_hot_key,
            1,
            &event_type,
            ptr::null_mut(),
            &mut installed_handler,
        ) != 0
        {
            return 1;
        }
        // Failure ends this standalone process. Carbon registrations cannot outlive their process;
        // this historical comparison does not provide the product's explicit close-release evidence.
        if RegisterEventHotKey(
            OMNI_HOT_KEY_CODE,
            OMNI_HOT_KEY_MODIFIERS,
            hot_key_identifier,
            event_dispatcher,
            0,
            &mut registered_hot_key,
        ) != 0
        {
            return 1;
        }
        // Block in Carbon instead of polling. This floor control deliberately owns no text or window.
        loop {
            let mut received_event = ptr::null_mut();
            if ReceiveNextEvent(1, &event_type, -1.0, 1, &mut received_event) != 0
                || received_event.is_null()
            {
                return 1;
            }
            // Dispatch synchronously while this event is live, then return the retained Carbon event.
            let _ = SendEventToEventTarget(received_event, event_dispatcher);
            ReleaseEvent(received_event);

            #[cfg(omni_shortcut_probe)]
            // Only the original matching callback ends the explicit delivery probe successfully.
            if BENCHMARK_HOT_KEY_RECEIVED.load(Ordering::Relaxed) {
                return 0;
            }
        }
    }
}
