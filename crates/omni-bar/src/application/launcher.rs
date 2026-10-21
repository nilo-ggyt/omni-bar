//! Portable catalog search and owned action descriptors, with static plugins injected by composition.

use std::path::PathBuf;

use omni_launcher_query::{LauncherQueryMatchRank, launcher_query_match_rank};
use omni_plugin_sdk::PluginRegistration;

/// Stable result categories shared by portable search and native activation.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum LauncherResultKind {
    /// A command owned by an independently registered native plugin.
    Plugin,
    /// A capability owned by the Host application.
    BuiltIn,
    /// An application entry whose executable action additionally needs a validated bundle path.
    InstalledApplication,
}

/// Owned action snapshot that survives destruction of the Launcher control graph.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LauncherAction {
    /// Invoke one declared command through the SDK's exact registration contract.
    Plugin {
        /// Static registration; retaining it owns no session resources.
        registration: PluginRegistration,
        /// Command identity validated against the descriptor at activation.
        command_id: &'static str,
    },
    /// Invoke a Host-owned capability through its bounded stable command.
    BuiltIn { command: &'static str },
    /// Open an application bundle discovered by the platform query boundary.
    Application { name: String, bundle_path: PathBuf },
    /// Explicitly exit the process after foreground teardown.
    Quit,
}

impl LauncherAction {
    #[must_use]
    /// Preserve registration and command identity without constructing the plugin.
    pub const fn plugin(registration: PluginRegistration, command_id: &'static str) -> Self {
        // Retain only static dispatch evidence; the SDK validates the command at activation.
        Self::Plugin {
            registration,
            command_id,
        }
    }

    #[must_use]
    /// Name an application-owned capability without conflating it with a plugin.
    pub const fn built_in(command: &'static str) -> Self {
        Self::BuiltIn { command }
    }

    #[must_use]
    /// Own the application label/path across the native close handoff.
    pub fn application(name: impl Into<String>, bundle_path: impl Into<PathBuf>) -> Self {
        // Own both values because the originating result row can be destroyed before launch.
        Self::Application {
            name: name.into(),
            bundle_path: bundle_path.into(),
        }
    }

    #[must_use]
    /// Represent explicit process exit separately from dismiss/back.
    pub const fn quit() -> Self {
        Self::Quit
    }

    /// Validate catalog evidence before execution; application actions require a separately validated path.
    pub fn from_result(result: LauncherResult) -> Result<Self, LauncherActionConversionError> {
        match result.kind {
            // Plugin execution requires both registration ownership and the advertised command.
            LauncherResultKind::Plugin => match (result.registration, result.plugin_command_id) {
                // Both static identities are present; preserve them as the generic dispatch target.
                (Some(registration), Some(command_id)) => {
                    Ok(Self::plugin(registration, command_id))
                }
                // An incomplete plugin descriptor cannot invent a default registration/command.
                _ => Err(LauncherActionConversionError::PluginRegistrationRequired),
            },
            // Built-ins use application-owned stable commands and need no plugin identity.
            LauncherResultKind::BuiltIn => Ok(Self::built_in(result.command)),
            // A static label cannot prove a concrete installed application bundle target.
            LauncherResultKind::InstalledApplication => {
                Err(LauncherActionConversionError::ApplicationTargetRequired)
            }
        }
    }
}

/// A descriptor lacks the evidence required to execute a stable action.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LauncherActionConversionError {
    /// A static label cannot substitute for a validated application bundle target.
    ApplicationTargetRequired,
    /// A plugin action requires both immutable registration and declared command identity.
    PluginRegistrationRequired,
}

impl std::fmt::Display for LauncherActionConversionError {
    /// Report missing activation evidence without starting any native or plugin effect.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            // Missing bundle evidence means no LaunchServices request has been made.
            Self::ApplicationTargetRequired => {
                formatter.write_str("应用结果缺少经过验证的启动目标")
            }
            // Missing static registration means no plugin resource has been acquired.
            Self::PluginRegistrationRequired => formatter.write_str("插件结果缺少静态注册项"),
        }
    }
}

impl std::error::Error for LauncherActionConversionError {}

/// Execution boundary; the platform implementation owns thread/lifecycle validation and side effects.
pub trait ActivationSink {
    /// Failure category owned by the actual execution boundary.
    type Error;

    /// Execute once after the previous foreground surface is released.
    fn activate(&self, action: LauncherAction) -> Result<(), Self::Error>;
}

/// Forward an already-owned action once, preserving the sink's typed failure.
pub fn dispatch_launcher_action<S>(sink: &S, action: LauncherAction) -> Result<(), S::Error>
where
    S: ActivationSink + ?Sized,
{
    // The sink owns sequencing and side effects; this forwarding boundary does not retry a failure.
    sink.activate(action)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// A copyable static descriptor; installed-application paths belong to a separate owned result.
pub struct LauncherResult {
    /// Category used to validate the evidence required for activation.
    pub kind: LauncherResultKind,
    /// Static discoverability code; this is not a plugin-specific dispatch switch.
    pub command: &'static str,
    /// Product-facing localized action label.
    pub title: &'static str,
    /// Short localized explanation of the action.
    pub description: &'static str,
    /// Plugin evidence, absent for application-owned built-ins.
    pub registration: Option<PluginRegistration>,
    /// Exact descriptor command identity paired with the registration.
    pub plugin_command_id: Option<&'static str>,
}

impl LauncherResult {
    /// Convert only if this descriptor contains the evidence for its declared kind.
    pub fn launcher_action(self) -> Result<LauncherAction, LauncherActionConversionError> {
        LauncherAction::from_result(self)
    }
}

/// Static application capabilities do not allocate feature resources until explicit activation.
const BUILT_IN_RESULTS: [LauncherResult; 4] = [
    LauncherResult {
        kind: LauncherResultKind::BuiltIn,
        command: "clipboard",
        title: "超级剪切板",
        description: "查看最近复制的内容",
        registration: None,
        plugin_command_id: None,
    },
    LauncherResult {
        kind: LauncherResultKind::BuiltIn,
        command: "calc",
        title: "计算器",
        description: "快速计算表达式",
        registration: None,
        plugin_command_id: None,
    },
    LauncherResult {
        kind: LauncherResultKind::BuiltIn,
        command: "convert",
        title: "单位换算",
        description: "转换常见单位",
        registration: None,
        plugin_command_id: None,
    },
    LauncherResult {
        kind: LauncherResultKind::BuiltIn,
        command: "settings",
        title: "设置",
        description: "配置 Omni Bar",
        registration: None,
        plugin_command_id: None,
    },
];

/// Stateless search over application capabilities and composition-injected static registrations.
#[derive(Clone, Copy, Debug)]
pub struct LauncherCatalog {
    registrations: &'static [PluginRegistration],
}

impl LauncherCatalog {
    #[must_use]
    /// Inject the static catalog from the composition root; the application has no concrete plugin edge.
    pub const fn new(registrations: &'static [PluginRegistration]) -> Self {
        // Borrow descriptors only: catalog creation must not allocate plugin sessions or runtime state.
        Self { registrations }
    }

    #[must_use]
    /// Rank static descriptor fields with stable tie order; empty input enumerates capabilities only.
    pub fn search(&self, query: &str) -> Vec<LauncherResult> {
        // Allocate only a per-query descriptor snapshot; nothing is retained by the catalog.
        let bundled_results = self.bundled_results();
        let trimmed = query.trim();
        if trimmed.is_empty() {
            // Opening Launcher exposes capabilities without application filesystem work.
            return bundled_results;
        }

        // Rank once per candidate, preserving original catalog order among equal ranks.
        let mut matched = bundled_results
            .iter()
            .copied()
            .filter_map(|result| {
                // Nonmatching static descriptors consume no result slot.
                launcher_result_match_rank(&result, trimmed).map(|rank| (result, rank))
            })
            .collect::<Vec<_>>();
        // Stable sorting retains deterministic order for equal matching levels.
        matched.sort_by_key(|(_, rank)| *rank);
        // Remove transient rank metadata; the caller owns the final per-query descriptor vector.
        matched.into_iter().map(|(result, _)| result).collect()
    }

    #[must_use]
    /// Materialize an ephemeral search snapshot from descriptors and application-owned built-ins.
    pub fn bundled_results(&self) -> Vec<LauncherResult> {
        // Size the ephemeral vector from descriptors without activating any implementation.
        let plugin_command_count = self
            .registrations
            .iter()
            .map(|registration| registration.descriptor().commands.len())
            .sum::<usize>();
        let mut results = Vec::with_capacity(BUILT_IN_RESULTS.len() + plugin_command_count);
        for registration in self.registrations {
            // Each independent plugin contributes only the commands in its static SDK descriptor.
            let descriptor = registration.descriptor();
            for command in descriptor.commands {
                // Preserve each command identity; the host never branches on a plugin-specific page/ID.
                results.push(LauncherResult {
                    kind: LauncherResultKind::Plugin,
                    command: descriptor.activation_code,
                    title: command.title,
                    description: command.subtitle,
                    registration: Some(*registration),
                    plugin_command_id: Some(command.id),
                });
            }
        }
        // Built-ins retain their declared order after the static plugin commands.
        results.extend(BUILT_IN_RESULTS);
        results
    }
}

/// Resolve generic plugin search fields or the built-in label fields through one shared matcher.
fn launcher_result_match_rank(
    result: &LauncherResult,
    query: &str,
) -> Option<LauncherQueryMatchRank> {
    if let Some(registration) = result.registration {
        // A plugin defines its own searchable aliases through the SDK, not host-specific branches.
        let descriptor = registration.descriptor();
        // A result without its paired command cannot borrow another command's aliases or rank.
        let command_id = result.plugin_command_id?;
        // Unknown command evidence yields no match; discovery never falls back to activation.
        let command = descriptor.command(command_id)?;
        // Choose the strongest matching descriptor field instead of favoring a particular alias order.
        return descriptor
            .search_fields(command)
            .filter_map(|field| launcher_query_match_rank(field, query))
            .min();
    }

    // Application capabilities share the same matching semantics across command/title/description.
    [result.command, result.title, result.description]
        .into_iter()
        .filter_map(|field| launcher_query_match_rank(field, query))
        .min()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Ensure the assembled catalog remains dormant while enumerating all current product entries.
    #[test]
    fn empty_query_discovers_all_mvp_capabilities() {
        // Search only static descriptors; application paths and native sessions are not catalog results.
        let results = crate::composition::launcher_catalog().search("");
        assert_eq!(results.len(), 5);
        assert_eq!(results[0].kind, LauncherResultKind::Plugin);
        assert_eq!(results[0].plugin_command_id, Some("open"));
        assert!(results.iter().any(|result| result.command == "clipboard"));
        assert!(results.iter().any(|result| result.command == "calc"));
        assert!(results.iter().any(|result| result.command == "convert"));
        assert!(results.iter().any(|result| result.command == "settings"));
        assert!(
            !results
                .iter()
                .any(|result| result.kind == LauncherResultKind::InstalledApplication)
        );
    }

    /// The shared matcher must ignore case and reject unrelated text without creating results.
    #[test]
    fn query_matching_is_case_insensitive_and_bounded_to_catalog_entries() {
        // An exact alias selects the registered plugin while unknown text yields an empty snapshot.
        let results = crate::composition::launcher_catalog().search("MD");
        assert_eq!(results.len(), 1);
        assert!(
            results
                .iter()
                .all(|result| result.kind == LauncherResultKind::Plugin)
        );
        assert!(
            crate::composition::launcher_catalog()
                .search("does-not-exist")
                .is_empty()
        );
    }

    /// Aliases, prefixes, substrings and localized keywords share a single ranking contract.
    #[test]
    fn query_uses_shared_match_levels_and_stable_ties() {
        // Descriptor-driven matching has the same levels as the pure launcher-query crate.
        assert_eq!(
            launcher_result_match_rank(
                &crate::composition::launcher_catalog().bundled_results()[0],
                "md"
            ),
            Some(LauncherQueryMatchRank::Exact)
        );
        assert_eq!(
            launcher_result_match_rank(
                &crate::composition::launcher_catalog().bundled_results()[0],
                "MARK"
            ),
            Some(LauncherQueryMatchRank::Prefix)
        );
        assert_eq!(
            launcher_result_match_rank(
                &crate::composition::launcher_catalog().bundled_results()[0],
                "down"
            ),
            Some(LauncherQueryMatchRank::Contains)
        );
        assert_eq!(
            launcher_result_match_rank(
                &crate::composition::launcher_catalog().bundled_results()[0],
                "笔记"
            ),
            Some(LauncherQueryMatchRank::Exact)
        );

        // A complete case-insensitive keyword still preserves the original command identity.
        let markdown = crate::composition::launcher_catalog().search("MARKDOWN");
        assert_eq!(markdown.len(), 1);
        assert_eq!(markdown[0].plugin_command_id, Some("open"));
    }

    /// Every bundled command must round-trip its own registration without concrete plugin branching.
    #[test]
    fn every_plugin_result_resolves_to_its_static_registration() {
        for result in crate::composition::launcher_catalog()
            .bundled_results()
            .iter()
            .filter(|result| result.kind == LauncherResultKind::Plugin)
        {
            // Only plugin descriptors require the paired static registration/command evidence.
            let LauncherAction::Plugin {
                registration,
                command_id,
            } = result.launcher_action().expect("static plugin result")
            else {
                // A category mismatch is a failed contract, not a fallback to a particular plugin.
                panic!("plugin result must become a plugin action");
            };
            // Preserve the descriptor's actual identity so this assertion works for any plugin count.
            assert_eq!(result.registration, Some(registration));
            assert_eq!(result.plugin_command_id, Some(command_id));
            assert!(registration.descriptor().command(command_id).is_some());
        }
    }
}
