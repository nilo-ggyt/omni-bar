//! Bounded launcher selection and key-routing policy, independent of native control ownership.

use objc2_app_kit::NSEventModifierFlags;

use super::{
    MACOS_DOWN_ARROW_KEY_CODE, MACOS_KEYPAD_ENTER_KEY_CODE, MACOS_RETURN_KEY_CODE,
    MACOS_UP_ARROW_KEY_CODE, MAX_VISIBLE_LAUNCHER_RESULTS, QUIT_ACTIVATION_CODE,
};
use crate::installed_application_catalog::{InstalledApplication, search_installed_applications};
use crate::{LauncherAction, LauncherActionConversionError, LauncherCatalog, LauncherResult};

/// Queries longer than the visible search contract are rejected rather than interpreted as prefixes.
const MAX_QUERY_CHARACTERS: usize = 128;

/// One executable row; concrete plugin behavior remains behind the SDK catalog descriptor.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum NativeLauncherSelection {
    Catalog(LauncherResult),
    Application(InstalledApplication),
    Quit,
}

impl NativeLauncherSelection {
    /// Convert a snapshot row into an owned action before closing its native controls.
    pub(super) fn launcher_action(&self) -> Result<LauncherAction, LauncherActionConversionError> {
        match self {
            // Catalog conversion validates the registration/command pair at its owning boundary.
            Self::Catalog(result) => result.launcher_action(),
            // Installed applications retain their validated bundle path through the close handoff.
            Self::Application(application) => Ok(LauncherAction::application(
                application.name(),
                application.bundle_path(),
            )),
            // Quit is an explicit action, never an arbitrary query substring.
            Self::Quit => Ok(LauncherAction::quit()),
        }
    }

    /// Show a compact stable command without leaking bundle paths into the row.
    pub(super) fn activation_code(&self) -> &str {
        match self {
            // Descriptor commands apply equally to every statically registered plugin.
            Self::Catalog(result) => result.command,
            // Application rows share one non-executable visual category label.
            Self::Application(_) => "app",
            // Only the dedicated quit row exposes this command.
            Self::Quit => QUIT_ACTIVATION_CODE,
        }
    }

    /// Borrow a display label from the owned result or static descriptor.
    pub(super) fn title(&self) -> &str {
        match self {
            // Catalog titles are static localized product labels.
            Self::Catalog(result) => result.title,
            // Application names are owned by this result snapshot.
            Self::Application(application) => application.name(),
            // Quit does not depend on an installed application record.
            Self::Quit => "退出 Omni Bar",
        }
    }

    /// Explain the action category without reading plugin state or application contents.
    pub(super) fn subtitle(&self) -> &str {
        match self {
            // Use the plugin/built-in catalog's declared description verbatim.
            Self::Catalog(result) => result.description,
            // Application dispatch opens a validated macOS bundle.
            Self::Application(_) => "打开已安装的 macOS 应用",
            // Distinguish process termination from dismissing a foreground surface.
            Self::Quit => "完全退出应用",
        }
    }
}

/// A fixed-capacity visible snapshot; the launcher never allocates an unbounded result cache.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct NativeLauncherResults {
    entries: [Option<NativeLauncherSelection>; MAX_VISIBLE_LAUNCHER_RESULTS],
    pub(super) len: usize,
}

impl NativeLauncherResults {
    /// Initialize absent slots without retaining previous application paths.
    pub(super) fn empty() -> Self {
        // Every uninitialized slot owns no result, application path, or plugin resource.
        Self {
            entries: std::array::from_fn(|_| {
                // Construct each absent slot without a shared heap container or hidden overflow list.
                None
            }),
            len: 0,
        }
    }

    /// Preserve ranking while discarding results beyond the fixed visible capacity.
    pub(super) fn push(&mut self, selection: NativeLauncherSelection) {
        if self.len == self.entries.len() {
            // A full snapshot owns no hidden overflow list; the rejected result drops here.
            return;
        }
        // The initialized prefix grows only after ownership transfers into its next free slot.
        self.entries[self.len] = Some(selection);
        self.len += 1;
    }

    /// Reject stale mouse/keyboard indices without indexing outside the fixed storage.
    pub(super) fn get(&self, index: usize) -> Option<&NativeLauncherSelection> {
        // Checked indexing preserves empty/unused slots as non-executable results.
        self.entries.get(index).and_then(Option::as_ref)
    }

    /// Bound downstream application scanning by the actual unfilled visible slots.
    pub(super) fn remaining_capacity(&self) -> usize {
        // Only push mutates the initialized prefix, keeping len within the fixed backing capacity.
        self.entries.len() - self.len
    }

    /// Select the no-results presentation without inspecting native controls.
    pub(super) fn is_empty(&self) -> bool {
        // A zero initialized prefix exposes only the non-executable first-row placeholder.
        self.len == 0
    }

    #[cfg(test)]
    /// Inspect only initialized rows when asserting catalog order.
    pub(super) fn iter(&self) -> impl Iterator<Item = &NativeLauncherSelection> + '_ {
        // Tests inspect only the initialized prefix and borrow no native controls.
        self.entries[..self.len].iter().flatten()
    }
}

/// Resolve explicit commands first and fill remaining visible slots with installed applications.
pub(super) fn resolve_launcher_results(
    catalog: &LauncherCatalog,
    raw_query: &str,
) -> NativeLauncherResults {
    // Reject over-budget input before catalog matching or filesystem scanning. Truncating an arbitrary
    // prefix could accidentally turn a long ordinary query into an exact capability or quit command.
    let mut results = NativeLauncherResults::empty();
    let Some(query) = accepted_launcher_query(raw_query) else {
        // Oversized input has no executable result and retains no candidate/application paths.
        return results;
    };

    if query.is_empty() {
        // Opening Launcher enumerates static capabilities without scanning application directories.
        for result in catalog.search("") {
            // Preserve catalog order in the bounded snapshot.
            results.push(NativeLauncherSelection::Catalog(result));
        }
        return results;
    }

    if query.eq_ignore_ascii_case(QUIT_ACTIVATION_CODE) {
        // Require an exact dedicated command so ordinary fuzzy text cannot offer accidental quit.
        results.push(NativeLauncherSelection::Quit);
        return results;
    }

    // An exact capability command wins over similarly named applications.
    let catalog_results = catalog.search(query);
    let has_exact_command = catalog_results.iter().any(|result| {
        // Inspect descriptors only; searching must never activate a plugin session.
        result.command.eq_ignore_ascii_case(query)
    });
    for result in catalog_results {
        // Capability matches occupy visible slots before application results.
        results.push(NativeLauncherSelection::Catalog(result));
    }
    if has_exact_command || results.remaining_capacity() == 0 {
        // Exact dispatch and full snapshots need no filesystem scan or extra retained paths.
        return results;
    }

    for application in search_installed_applications(query, results.remaining_capacity()) {
        // The platform scanner receives the remaining bound and transfers each result's ownership.
        results.push(NativeLauncherSelection::Application(application));
    }
    results
}

/// Borrow one trimmed query only when its Unicode scalar count fits the visible search contract.
fn accepted_launcher_query(raw_query: &str) -> Option<&str> {
    // Scalar iteration stops at the first excess character without allocating a second query string.
    let query = raw_query.trim();
    query
        .char_indices()
        .nth(MAX_QUERY_CHARACTERS)
        .is_none()
        .then_some(query)
}

/// Direction of an unmodified arrow key accepted outside IME composition.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum LauncherSelectionDirection {
    Previous,
    Next,
}

/// Wrap keyboard selection inside the initialized result prefix.
pub(super) fn move_launcher_selection_index(
    selected_index: usize,
    result_count: usize,
    direction: LauncherSelectionDirection,
) -> usize {
    if result_count == 0 {
        // An empty placeholder cannot become an executable selection.
        return 0;
    }
    // Normalize stale indices first. Explicit wrap branches avoid overflow even for extreme integers.
    let current = selected_index % result_count;
    match direction {
        // The previous row wraps from zero to the final initialized result.
        LauncherSelectionDirection::Previous => {
            if current == 0 {
                // Positive result_count makes subtraction valid even at usize::MAX capacity.
                result_count - 1
            } else {
                // A nonzero normalized index has a predecessor without unsigned underflow.
                current - 1
            }
        }
        // The next row wraps at the final initialized result without adding beyond usize::MAX.
        LauncherSelectionDirection::Next => {
            if current == result_count - 1 {
                // The final row's successor is the first initialized slot.
                0
            } else {
                // This branch is strictly below the final slot, so increment remains representable.
                current + 1
            }
        }
    }
}

/// Let IME and modified Return shortcuts retain their normal text-system behavior.
pub(super) fn is_launcher_activation_key(
    key_code: u16,
    modifiers: NSEventModifierFlags,
    has_marked_text: bool,
) -> bool {
    // Device-independent action modifiers matter; Caps Lock alone does not block activation.
    let activation_modifiers = NSEventModifierFlags::Shift
        | NSEventModifierFlags::Control
        | NSEventModifierFlags::Option
        | NSEventModifierFlags::Command;
    matches!(
        key_code,
        MACOS_RETURN_KEY_CODE | MACOS_KEYPAD_ENTER_KEY_CODE
    ) && !modifiers.intersects(activation_modifiers)
        && !has_marked_text
}

/// Claim only plain arrow keys; all other events continue through the AppKit responder chain.
pub(super) fn launcher_selection_navigation_direction(
    key_code: u16,
    modifiers: NSEventModifierFlags,
    has_marked_text: bool,
) -> Option<LauncherSelectionDirection> {
    let navigation_modifiers = NSEventModifierFlags::Shift
        | NSEventModifierFlags::Control
        | NSEventModifierFlags::Option
        | NSEventModifierFlags::Command;
    if has_marked_text || modifiers.intersects(navigation_modifiers) {
        // Composition navigation and modified editing commands belong to the field editor.
        return None;
    }
    match key_code {
        // Plain Up chooses the preceding visible result with wrapping.
        MACOS_UP_ARROW_KEY_CODE => Some(LauncherSelectionDirection::Previous),
        // Plain Down chooses the following visible result with wrapping.
        MACOS_DOWN_ARROW_KEY_CODE => Some(LauncherSelectionDirection::Next),
        // Non-navigation keys remain available to the native responder.
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Unicode budgets reject long command prefixes instead of producing an unintended exact action.
    #[test]
    fn long_queries_do_not_turn_into_prefix_commands() {
        // Truncation would incorrectly reinterpret these prefixes as dedicated quit/built-in actions.
        let catalog = crate::composition::launcher_catalog();
        for command in ["quit", "calc"] {
            // The accepted prefix fits the limit, but the complete visible query has additional input.
            let query = format!("{command}{}more", " ".repeat(MAX_QUERY_CHARACTERS));
            assert!(resolve_launcher_results(&catalog, &query).is_empty());
        }
        // The budget counts Unicode scalars, preserving a complete non-BMP query at its exact limit.
        let unicode = "😀".repeat(MAX_QUERY_CHARACTERS);
        assert_eq!(accepted_launcher_query(&unicode), Some(unicode.as_str()));
        assert_eq!(accepted_launcher_query(&(unicode + "x")), None);
    }

    /// Stale/native indices wrap safely without integer overflow or an executable empty placeholder.
    #[test]
    fn navigation_normalizes_extreme_indices_before_wrapping() {
        // A stale maximum index must behave like its normalized visible row.
        for direction in [
            LauncherSelectionDirection::Previous,
            LauncherSelectionDirection::Next,
        ] {
            assert_eq!(
                move_launcher_selection_index(usize::MAX, 6, direction),
                move_launcher_selection_index(usize::MAX % 6, 6, direction)
            );
            assert_eq!(move_launcher_selection_index(usize::MAX, 0, direction), 0);
        }
        // These arithmetic-only cases exercise representable boundaries without allocating a huge result set.
        assert_eq!(
            move_launcher_selection_index(
                usize::MAX,
                usize::MAX,
                LauncherSelectionDirection::Previous
            ),
            usize::MAX - 1
        );
        assert_eq!(
            move_launcher_selection_index(usize::MAX, usize::MAX, LauncherSelectionDirection::Next),
            1
        );
    }
}
