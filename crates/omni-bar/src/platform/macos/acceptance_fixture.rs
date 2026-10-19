//! Synthetic authority injection for the opt-in acceptance executable's actual product event loop.

use std::cell::RefCell;
use std::os::unix::fs::PermissionsExt;

use objc2::MainThreadMarker;
use objc2_app_kit::{NSPasteboard, NSPasteboardTypeString};
use objc2_foundation::NSString;

use super::NativePluginStorageLocations;

/// Own only diagnostic authority metadata; all plugin documents/resources retain their ordinary owners.
struct FixtureState {
    locations: NativePluginStorageLocations,
    pasteboard_name: String,
    #[cfg(feature = "native-input-trace")]
    input_trace:
        Option<objc2::rc::Retained<super::acceptance_input_trace::AcceptanceInputTraceController>>,
}

thread_local! {
    /// Absent in normal product builds and empty until this explicit synthetic entry point is called.
    static FIXTURE: RefCell<Option<FixtureState>> = const { RefCell::new(None) };
}

/// Copy the small path authorities before any native callback can reenter the fixture state.
pub(super) fn storage_locations() -> Option<NativePluginStorageLocations> {
    // Owned paths cross this boundary; no TLS borrow extends into document acquisition or filesystem work.
    FIXTURE.with_borrow(|fixture| fixture.as_ref().map(|fixture| fixture.locations.clone()))
}

/// Return a unique private namespace rather than exposing a retained pasteboard or any personal body.
pub(super) fn pasteboard_name() -> Option<String> {
    // A copied namespace is finite fixture metadata and cannot borrow the global state during AppKit work.
    FIXTURE.with_borrow(|fixture| {
        fixture
            .as_ref()
            .map(|fixture| fixture.pasteboard_name.clone())
    })
}

/// End synthetic authority after a stopped/failed loop; normal AppKit process exit uses willTerminate cleanup.
pub(super) fn clear_fixture() {
    // Move the whole owner out first: native target/descriptor destruction cannot reenter a TLS borrow.
    let fixture = FIXTURE.take();
    if let Some(fixture) = fixture {
        // Release only the synthetic namespace, then drop optional trace authority outside the global slot.
        let pasteboard =
            NSPasteboard::pasteboardWithName(&NSString::from_str(&fixture.pasteboard_name));
        release_named_pasteboard(&pasteboard);
        drop(fixture);
    }
}

/// Install explicit trace controls only for an existing fixture, after the product menu is published.
#[cfg(feature = "native-input-trace")]
pub(super) fn install_input_trace_controls(mtm: MainThreadMarker) -> Result<(), String> {
    let root = FIXTURE.with_borrow(|fixture| {
        // Copy path authority only; no fixture borrow crosses native menu construction or filesystem acquisition.
        fixture.as_ref().and_then(|fixture| {
            fixture
                .locations
                .workspace_root
                .parent()
                .map(std::path::Path::to_owned)
        })
    });
    let Some(root) = root else {
        // A trace-capable build without the explicit fixture entry point acquires no diagnostic resources.
        return Ok(());
    };
    let controller = super::acceptance_input_trace::install(&root, mtm)
        .map_err(|_| "synthetic source-input controls unavailable".to_owned())?;
    let mut owner = Some(controller);
    FIXTURE.with_borrow_mut(|fixture| {
        if let Some(fixture) = fixture.as_mut().filter(|fixture| {
            // A different fixture cannot inherit the old controller's retained directory authority.
            fixture.input_trace.is_none()
                && fixture.locations.workspace_root.parent() == Some(root.as_path())
        }) {
            // Transfer the sole strong menu-target lifetime only into the still-installed fixture.
            fixture.input_trace = owner.take();
        }
    });
    if owner.is_some() {
        // A reentrant fixture removal/replacement cannot leave an unowned live target; drop after borrowing ends.
        return Err("synthetic source-input owner changed during installation".to_owned());
    }
    Ok(())
}

/// Bridge the public oneway selector omitted by the pinned generated objc2 bindings.
fn release_named_pasteboard(pasteboard: &NSPasteboard) {
    // SAFETY: NSPasteboard.h declares this public zero-argument oneway void method. The caller
    // holds a live named pasteboard and executes on the main thread; no Rust ownership is transferred.
    unsafe { objc2::msg_send![pasteboard, releaseGlobally] }
}

/// Stream an exact one-MiB ASCII workload without allocating a second document-sized fixture string.
fn initialize_source_input_fixture(root: &std::path::Path) -> Result<(), String> {
    use std::io::Write;

    const DOCUMENT_BYTES: usize = 1_048_576;
    const BASELINE_LINE: &[u8] = b"OmniFrameOldA9\n";
    // A fresh exclusive file keeps this diagnostic initializer from replacing an existing note.
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(root.join("workspace/SourcePerformance.md"))
        .map_err(|_| "synthetic source-input fixture acquisition failed".to_owned())?;
    file.write_all(BASELINE_LINE)
        .map_err(|_| "synthetic source-input baseline write failed".to_owned())?;
    // Fixed 64-byte rows represent a multiline document; this is explicitly not a giant-line workload.
    let mut row = [b'x'; 64];
    row[63] = b'\n';
    let mut remaining = DOCUMENT_BYTES - BASELINE_LINE.len();
    while remaining >= row.len() {
        // Streaming rows bounds initializer scratch independently of the declared document size.
        file.write_all(&row)
            .map_err(|_| "synthetic source-input row write failed".to_owned())?;
        remaining -= row.len();
    }
    if remaining != 0 {
        // End the final partial ASCII row with a newline while preserving the exact byte count.
        row[remaining - 1] = b'\n';
        file.write_all(&row[..remaining])
            .map_err(|_| "synthetic source-input tail write failed".to_owned())?;
    }
    file.sync_all()
        .map_err(|_| "synthetic source-input fixture sync failed".to_owned())?;
    Ok(())
}

/// Seed only a newly acquired temporary directory; every partial failure remains owned by its TempDir.
fn initialize_fixture(root: &std::path::Path) -> Result<NativePluginStorageLocations, String> {
    for directory in ["workspace", "application-support", "external-assets"] {
        // Three fixed real children isolate note, settings/history and import-source mutations.
        std::fs::create_dir(root.join(directory))
            .map_err(|_| "synthetic fixture directory initialization failed".to_owned())?;
    }
    // The attachment source contains no personal data and is intentionally outside the synthetic vault.
    std::fs::write(
        root.join("external-assets/fixture.txt"),
        b"Synthetic attachment\n",
    )
    .map_err(|_| "synthetic import source initialization failed".to_owned())?;
    // One finite fixture exposes Unicode, lists, tables and fences through actual plugin controls.
    std::fs::write(
        root.join("workspace/Acceptance.md"),
        concat!(
            "# Synthetic acceptance note\n\nEnglish and 中文输入法 fixture. 😀\n\n",
            "- first item\n- second item\n\n> quotation\n\n",
            "| Column A | Column B |\n| --- | --- |\n| first | second |\n\n",
            "```rust\nfn main() {}\n```\n"
        ),
    )
    .map_err(|_| "synthetic note initialization failed".to_owned())?;
    // The second opt-in note supplies a declared, reproducible input workload; opening remains explicit.
    initialize_source_input_fixture(root)?;
    Ok(NativePluginStorageLocations {
        workspace_root: root.join("workspace"),
        application_support: root.join("application-support"),
    })
}

/// Enter the actual production application with a fresh vault/settings/history directory and named pasteboard.
///
/// The executable prints its synthetic root for inspection. AppKit's explicit process termination may
/// bypass Rust destructors, so the OS temporary directory remains inspectable after Quit; remove only
/// that printed generated directory after collecting results. Standard AppKit Copy/Paste and file chooser
/// actions retain OS behavior: use synthetic contents and select only files under this printed root.
///
/// # Errors
///
/// Requires the actual main thread and a fresh process with no preexisting foreground/fixture owner.
pub fn run_isolated_acceptance_application() -> Result<(), String> {
    // The temporary authority remains inspectable after AppKit's process exit, or is removed on a stopped loop.
    let root = tempfile::Builder::new()
        .prefix("omni-native-acceptance-")
        // TempDir otherwise uses 0777 filtered by umask; diagnostic export authority requires explicit 0700.
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir()
        .map_err(|_| "synthetic fixture directory unavailable".to_owned())?;
    let _pasteboard = install_fixture(root.path())?;
    // Queue ordinary Launcher creation after product initialization; no test-only window or fake lifecycle.
    dispatch2::DispatchQueue::main().exec_async(|| {
        if let Some(marker) = MainThreadMarker::new() {
            // Main-queue delivery enters the same Launcher code as an actual global shortcut callback.
            super::show_launcher_surface(marker);
        }
    });
    let result = super::run_application();
    // A stopped/failed loop releases the private namespace outside TLS borrowing before fixture removal.
    clear_fixture();
    drop(root);
    result
}

/// Install identical isolated authorities for either manual acceptance or a parent-owned subprocess fixture.
pub(super) fn install_fixture(
    root: &std::path::Path,
) -> Result<objc2::rc::Retained<NSPasteboard>, String> {
    let _mtm = MainThreadMarker::new()
        .ok_or_else(|| "acceptance fixture requires the AppKit main thread".to_owned())?;
    if FIXTURE.with_borrow(Option::is_some)
        || super::ACTIVE_SURFACE.with_borrow(Option::is_some)
        || super::CLOSING_SURFACE.with_borrow(Option::is_some)
        || super::plugin_host::exists()
        || super::builtins::is_open()
        || super::FOREGROUND_STATE.with_borrow(|state| {
            // Unpublished creation, accepted destruction and an unconsumed barrier also own the original authorities.
            state.active_surface().is_some()
                || state.pending_transition().is_some()
                || state.destroyed_barrier().is_some()
        })
    {
        // Never replace another session's authorities or redirect already-acquired native resources.
        return Err("acceptance fixture requires a fresh foreground owner".to_owned());
    }
    // Establish only owned synthetic files; never substitute HOME or inspect a real vault/settings file.
    let locations = initialize_fixture(root)?;
    // AppKit chooses a unique private namespace even when separate subprocess fixtures share the basename "fixture".
    let pasteboard = NSPasteboard::pasteboardWithUniqueName();
    let name = pasteboard.name().to_string();
    // Initialize known synthetic content before a real clipboard-history surface can capture it.
    pasteboard.clearContents();
    // SAFETY: this immutable public AppKit type constant has process lifetime.
    let plain_text = unsafe { NSPasteboardTypeString };
    if !pasteboard.setString_forType(
        &NSString::from_str("Synthetic acceptance clipboard"),
        plain_text,
    ) {
        // A failed private seed cannot silently fall back to reading the personal system pasteboard.
        release_named_pasteboard(&pasteboard);
        return Err("private acceptance pasteboard unavailable".to_owned());
    }
    FIXTURE.with_borrow_mut(|fixture| {
        // Publish complete immutable authorities only after all fallible fixture acquisition has succeeded.
        fixture.replace(FixtureState {
            locations,
            pasteboard_name: name,
            #[cfg(feature = "native-input-trace")]
            input_trace: None,
        });
    });
    println!("acceptance_fixture_root={}", root.display());
    println!("acceptance_clipboard=private_named_pasteboard");
    println!("acceptance_source_input=1048576_bytes_ascii_64_byte_rows_baseline_OmniFrameOldA9");
    Ok(pasteboard)
}
