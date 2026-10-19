//! Product composition of independently versioned static plugin registrations.
//!
//! This is the only Host production module naming a concrete plugin implementation.
//! The catalog owns static descriptors only; sessions/resources belong to explicit activation.

pub use omni_md_plugin::REGISTRATION as MARKDOWN_REGISTRATION;
use omni_plugin_sdk::PluginRegistration;

/// Add independent plugins only here after pinning their reviewed source revision in Cargo.
pub const BUNDLED_PLUGIN_REGISTRATIONS: &[PluginRegistration] = &[MARKDOWN_REGISTRATION];

/// Explicit fixture-only measurement controls stay at composition, outside the generic plugin protocol.
#[cfg(all(target_os = "macos", feature = "native-input-trace"))]
pub(crate) mod input_trace {
    // Reuse the plugin's exact scalar types; Host diagnostics never copy its protocol or private implementation.
    pub(crate) use omni_md_plugin::{
        NATIVE_INPUT_TRACE_CAPACITY as CAPACITY, NativeInputTraceAccessError as AccessError,
        NativeInputTraceDocument as Document, NativeInputTraceKind as Kind,
        NativeInputTraceOutcome as Outcome, NativeInputTraceRecord as Record,
        NativeInputTraceRejection as Rejection, NativeInputTraceSelector as Selector,
        NativeInputTraceSnapshot as Snapshot, arm_native_markdown_input_trace as arm,
        drain_native_markdown_input_trace as drain,
    };
}

/// Zero-sized catalog over this product build's reviewed static plugin list.
#[derive(Clone, Copy, Debug, Default)]
pub struct PluginCatalog;

impl PluginCatalog {
    /// Construct without allocating caches, mappings, workers or plugin resources.
    #[must_use]
    pub const fn new() -> Self {
        // Construction is zero-sized; linked registrations remain immutable static data.
        Self
    }

    /// Borrow the immutable native registrations linked into this build.
    #[must_use]
    pub const fn registrations(&self) -> &'static [PluginRegistration] {
        // Borrow the assembled list directly rather than retaining another registry or invoking handlers.
        BUNDLED_PLUGIN_REGISTRATIONS
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use omni_plugin_sdk::PLUGIN_PROTOCOL_VERSION;

    /// Reject protocol drift and duplicate identities before any bundled plugin can be activated.
    #[test]
    fn bundled_catalog_uses_unique_exact_versioned_commands() {
        // Inspect every descriptor in the assembled product without invoking activation handlers.
        let descriptors = PluginCatalog::new()
            .registrations()
            .iter()
            .map(|registration| registration.descriptor())
            .collect::<Vec<_>>();
        // This product build promises bundled commands, while each registration must still be unique.
        assert!(!descriptors.is_empty());
        for (index, descriptor) in descriptors.iter().enumerate() {
            // Every independent crate must use the exact shared protocol and a nonempty stable identity.
            assert_eq!(descriptor.protocol_version, PLUGIN_PROTOCOL_VERSION);
            assert!(!descriptor.plugin_id.is_empty());
            assert!(!descriptor.commands.is_empty());
            for command in descriptor.commands {
                // Command names are unique within their descriptor; dispatch never guesses a duplicate.
                assert!(!command.id.is_empty());
                assert!(
                    descriptor
                        .commands
                        .iter()
                        .filter(|candidate| {
                            // Count identical command identities rather than localized display labels.
                            candidate.id == command.id
                        })
                        .count()
                        == 1,
                    "duplicate command id in descriptor {index}"
                );
            }
            // Registration identities are globally unique across all plugin repositories.
            assert_eq!(
                descriptors
                    .iter()
                    .filter(|candidate| {
                        // Independent repositories cannot shadow another linked plugin identity.
                        candidate.plugin_id == descriptor.plugin_id
                    })
                    .count(),
                1,
                "duplicate plugin id"
            );
        }
    }
}
