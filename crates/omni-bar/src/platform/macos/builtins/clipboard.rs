//! Main-queue pasteboard adaptation and a strictly foreground-owned dispatch timer.

use std::cell::Cell;
use std::time::Duration;

use block2::RcBlock;
use dispatch2::{
    _dispatch_source_type_timer, DispatchObject, DispatchQueue, DispatchRetained, DispatchSource,
    DispatchTime,
};
use objc2::rc::autoreleasepool;
use objc2_app_kit::{NSPasteboard, NSPasteboardTypeString};
use objc2_foundation::NSString;

use crate::capabilities::clipboard::{
    CHECK_INTERVAL_MS, CHECK_LEEWAY_MS, ClipboardCaptureDecision, ClipboardClearReceipt,
    ClipboardClearSuppression, ClipboardSession, ClipboardSourceGeneration,
    ClipboardSourceIdentity, MAX_ENTRY_BYTES, MAX_SOURCE_NAME_UTF16,
};

thread_local! {
    // The process retains one fixed namespace/generation value, never an inactive session or native resource.
    static CLEAR_SUPPRESSION: Cell<ClipboardClearSuppression> = const {
        Cell::new(ClipboardClearSuppression::new())
    };
}

/// Copy process-only clear metadata before the new foreground owner acquires its initial snapshot.
pub(super) fn clear_suppression() -> ClipboardClearSuppression {
    CLEAR_SUPPRESSION.with(Cell::get)
}

/// Publish only a successful storage receipt, with no native operation under the TLS access.
pub(super) fn remember_clear(receipt: ClipboardClearReceipt) {
    CLEAR_SUPPRESSION.with(|slot| {
        // Copy/update/store touches only bounded values and retains only the last cleared namespace.
        let mut suppression = slot.get();
        suppression.remember(receipt);
        slot.set(suppression);
    });
}

/// A foreground session owns this activated source and cancels it before becoming idle.
pub(super) struct ClipboardMonitor {
    source: DispatchRetained<DispatchSource>,
}

impl ClipboardMonitor {
    /// Install one coalescible timer on the main queue; callers perform the initial snapshot first.
    pub(super) fn start(callback: impl Fn() + 'static) -> Result<Self, String> {
        // Validate the deadline before creating a source, so failures cannot drop a suspended source.
        let deadline = DispatchTime::try_from(Duration::from_millis(CHECK_INTERVAL_MS))
            .map_err(|()| "剪贴板监测计时器不可用".to_owned())?;
        let source_type = std::ptr::addr_of!(_dispatch_source_type_timer).cast_mut();
        // SAFETY: the public timer source takes zero handle/mask; the process-owned main
        // queue guarantees that this non-Send callback is invoked only on the AppKit thread.
        let source = unsafe { DispatchSource::new(source_type, 0, 0, Some(DispatchQueue::main())) };
        let block: RcBlock<dyn Fn()> = RcBlock::new(callback);
        // SAFETY: libdispatch copies this heap block; the source's queue and callback captures
        // remain valid until cancellation, and no captured native value leaves the main queue.
        unsafe { source.set_event_handler_with_block(RcBlock::as_ptr(&block)) };
        source.set_timer(
            deadline,
            CHECK_INTERVAL_MS * 1_000_000,
            CHECK_LEEWAY_MS * 1_000_000,
        );
        source.activate();
        Ok(Self { source })
    }
}

impl Drop for ClipboardMonitor {
    /// Cancel this foreground-owned timer before its session becomes idle; no background monitor survives close.
    fn drop(&mut self) {
        // Cancellation happens on the same main queue that owns the callback's native accesses.
        self.source.cancel();
    }
}

/// Read a body only after exact source/activity/clear metadata authorizes this stable generation.
pub(super) fn capture_changed(
    history: &ClipboardSession,
    last_seen: &Cell<isize>,
) -> Result<Option<(ClipboardSourceGeneration, String)>, String> {
    autoreleasepool(|_| {
        // This one authority supplies namespace, count and optional body; no TLS borrow crosses its getters.
        let pasteboard = super::super::application_pasteboard();
        let generation = source_generation_for(&pasteboard)?;
        match history.capture_decision(generation, last_seen.get()) {
            // Unchanged or cleared generations touch metadata only, even when clear occurred before the next tick.
            ClipboardCaptureDecision::Skip => return Ok(None),
            // An authority replacement cannot be hidden by an equal count or attributed to the old session.
            ClipboardCaptureDecision::SourceChanged => {
                return Err("剪贴板来源已改变，请重新打开面板".to_owned());
            }
            // Only this exact live source and fresh unsuppressed count may expose types or contents.
            ClipboardCaptureDecision::Read => {}
        }
        let content = permitted_text(&pasteboard);
        if pasteboard.changeCount() != generation.change_count {
            // A concurrent system copy invalidated the body; retry the stable generation next tick.
            return Ok(None);
        }
        // Forbidden/empty generations are also remembered so they are not repeatedly materialized.
        last_seen.set(generation.change_count);
        Ok(content.map(|body| (generation, body)))
    })
}

/// Observe exact namespace and generation together without materializing any clipboard contents.
pub(super) fn source_generation() -> Result<ClipboardSourceGeneration, String> {
    // This pool ends every temporary native claim before callers read or publish their scalar state.
    autoreleasepool(|_| source_generation_for(&super::super::application_pasteboard()))
}

/// Read exact bounded metadata from the same immutable namespace handle used for a body or clear.
fn source_generation_for(pasteboard: &NSPasteboard) -> Result<ClipboardSourceGeneration, String> {
    let name = pasteboard.name();
    let length = name.length();
    if length > MAX_SOURCE_NAME_UTF16 {
        // Refuse before creating or clearing history: truncation could suppress an unrelated namespace.
        return Err("剪贴板来源名称过长".to_owned());
    }
    let mut units = [0; MAX_SOURCE_NAME_UTF16];
    for (index, unit) in units[..length].iter_mut().enumerate() {
        // The immutable retained name bounds every public UTF-16 access; no heap string is retained.
        *unit = name.characterAtIndex(index);
    }
    let source = ClipboardSourceIdentity::from_utf16(&units[..length])
        .ok_or_else(|| "剪贴板来源名称过长".to_owned())?;
    Ok(ClipboardSourceGeneration {
        source,
        change_count: pasteboard.changeCount(),
    })
}

/// Replace system plain text from a transient validated history body or calculated result.
pub(super) fn write_text(content: &str) -> Result<(), String> {
    autoreleasepool(|_| {
        // The system pasteboard becomes the body owner; this adapter retains no Rust copy.
        let pasteboard = super::super::application_pasteboard();
        let string = NSString::from_str(content);
        pasteboard.clearContents();
        // SAFETY: this is an immutable AppKit framework constant with process lifetime.
        let plain_text = unsafe { NSPasteboardTypeString };
        pasteboard
            .setString_forType(&string, plain_text)
            .then_some(())
            .ok_or_else(|| "无法写入系统剪贴板".to_owned())
    })
}

/// Refuse sensitive, unbounded and empty values before the history adapter sees a body.
fn permitted_text(pasteboard: &NSPasteboard) -> Option<String> {
    // Unknown or excessive type lists are treated conservatively as non-capturable content.
    let types = pasteboard.types()?;
    if types.count() > 128 {
        // Bound both type inspection and allocations for untrusted pasteboard producers.
        return None;
    }
    for index in 0..types.count() {
        // Each inspected public type must be bounded and free of privacy-sensitive markers.
        let kind = types.objectAtIndex(index);
        if kind.length() > 256 || sensitive_type(&kind.to_string()) {
            // Password managers and concealed/transient/generated types never enter history.
            return None;
        }
    }
    // SAFETY: the public immutable plain-text type constant is valid for this process.
    let plain_text = unsafe { NSPasteboardTypeString };
    let string = pasteboard.stringForType(plain_text)?;
    if string.length() > MAX_ENTRY_BYTES {
        // UTF-16 length is a lower bound on UTF-8 bytes, so reject huge payloads before conversion.
        return None;
    }
    let body = string.to_string();
    if body.is_empty() || body.len() > MAX_ENTRY_BYTES {
        // Encoding expansion can exceed the byte budget even after the pre-conversion check.
        return None;
    }
    Some(body)
}

/// Recognize the existing excluded pasteboard conventions case-insensitively.
fn sensitive_type(kind: &str) -> bool {
    // These public conventions preserve the previous product's password/privacy exclusions.
    let kind = kind.to_ascii_lowercase();
    [
        "org.nspasteboard.transienttype",
        "org.nspasteboard.concealedtype",
        "org.nspasteboard.autogeneratedtype",
        "com.agilebits.onepassword",
        "com.1password",
        "keepass",
        "password",
    ]
    .iter()
    .any(|marker| kind.contains(marker))
}

#[cfg(test)]
mod tests {
    use super::sensitive_type;

    /// Privacy markers are filtered without reading a live pasteboard during pure tests.
    #[test]
    fn excludes_private_types_and_accepts_plain_text() {
        // Case-insensitive matching also covers the historical keepassxc prefix.
        assert!(sensitive_type("org.nspasteboard.ConcealedType"));
        assert!(sensitive_type("org.keepassxc.keepass"));
        assert!(sensitive_type("com.1password.item"));
        assert!(!sensitive_type("public.utf8-plain-text"));
    }
}
