//! Resolve native storage and pasteboard authorities at explicit capability boundaries.
//!
//! Paths are metadata until activation. Resolving the production vault must not create,
//! enumerate or open Documents/OMNI, and synthetic builds lend their own separate authority.

use std::path::PathBuf;

use objc2_foundation::{
    NSSearchPathDirectory, NSSearchPathDomainMask, NSSearchPathForDirectoriesInDomains,
};

const OMNI_BUNDLE_IDENTIFIER: &str = "com.omnibar.desktop";

/// Lazily resolved paths; merely computing these locations opens no document or persistent file.
#[derive(Clone)]
pub(super) struct NativePluginStorageLocations {
    pub(super) workspace_root: PathBuf,
    pub(super) application_support: PathBuf,
}

/// Resolve public user-domain directories without pre-creating a plugin workspace at idle.
pub(super) fn native_plugin_storage_locations() -> Result<NativePluginStorageLocations, String> {
    #[cfg(feature = "native-acceptance")]
    if let Some(locations) = super::acceptance_fixture::storage_locations() {
        // Only the opt-in diagnostic executable can install these caller-owned synthetic authorities.
        return Ok(locations);
    }
    // Fail before plugin activation if either OS-provided base directory is unavailable.
    let documents = resolve_user_domain_directory(
        NSSearchPathDirectory::DocumentDirectory,
        "用户文稿目录不可用",
    )?;
    let application_support = resolve_user_domain_directory(
        NSSearchPathDirectory::ApplicationSupportDirectory,
        "用户应用支持目录不可用",
    )?;
    Ok(NativePluginStorageLocations {
        workspace_root: documents.join("OMNI"),
        application_support: application_support.join(OMNI_BUNDLE_IDENTIFIER),
    })
}

/// Resolve the platform pasteboard at the explicit capability boundary, without retaining its body.
pub(super) fn application_pasteboard() -> objc2::rc::Retained<objc2_app_kit::NSPasteboard> {
    #[cfg(feature = "native-acceptance")]
    if let Some(name) = super::acceptance_fixture::pasteboard_name() {
        // Synthetic acceptance never reads personal history through Host capability adapters.
        return objc2_app_kit::NSPasteboard::pasteboardWithName(
            &objc2_foundation::NSString::from_str(&name),
        );
    }
    // Normal product calls use the public system pasteboard only in response to foreground work.
    objc2_app_kit::NSPasteboard::generalPasteboard()
}

/// Decode an OS-provided path once; never silently substitute a process working directory.
fn resolve_user_domain_directory(
    directory: NSSearchPathDirectory,
    unavailable_message: &'static str,
) -> Result<PathBuf, String> {
    let paths = NSSearchPathForDirectoriesInDomains(
        directory,
        NSSearchPathDomainMask::UserDomainMask,
        true,
    );
    let path = paths
        .firstObject()
        .ok_or_else(|| unavailable_message.to_owned())?;
    let path = PathBuf::from(path.to_string());
    if path.as_os_str().is_empty() {
        // An empty path would accidentally redirect persistence into the application's working directory.
        return Err(unavailable_message.to_owned());
    }
    Ok(path)
}
