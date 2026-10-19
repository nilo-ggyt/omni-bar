//! Static assembly of reviewed plugin registrations for this product build.

pub(crate) mod plugins;

/// Inject the concrete bundled registrations into otherwise portable application search policy.
pub(crate) fn launcher_catalog() -> crate::LauncherCatalog {
    // Inject one static slice; discovery receives descriptors without loading a plugin session or cache.
    crate::LauncherCatalog::new(plugins::PluginCatalog::new().registrations())
}
