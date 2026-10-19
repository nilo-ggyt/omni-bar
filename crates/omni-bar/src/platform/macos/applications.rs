//! Bounded, query-owned macOS application discovery and explicit native launch requests.
//!
//! Only a nonempty Launcher query reads standard application roots. Directory handles, metadata,
//! and paths live for one bounded scan; no index, resident cache, observer, or background scan exists.

use std::cmp::Ordering;
use std::fs;
use std::path::{Path, PathBuf};

use omni_launcher_query::{launcher_fields_equal_case_insensitive, launcher_query_match_rank};

const MAX_APPLICATION_RESULTS: usize = 6;
const MAX_QUERY_CHARACTERS: usize = 128;
const MAX_ENTRIES_PER_ROOT: usize = 1_024;
const MAX_CATEGORIES_PER_ROOT: usize = 64;
const MAX_ENTRIES_PER_CATEGORY: usize = 256;
const MAX_TOTAL_ENTRIES: usize = 4_096;
const MAX_TOTAL_CATEGORIES: usize = 128;

const APPLICATION_ROOTS: &[&str] = &[
    "/Applications",
    "/System/Applications",
    "/System/Library/CoreServices/Applications",
    "/System/Cryptexes/App/System/Applications",
];

/// One bounded search match owns display/path metadata without opening or launching the bundle.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InstalledApplication {
    name: String,
    bundle_path: PathBuf,
    match_rank: u8,
}

impl InstalledApplication {
    /// Copy validated search metadata into the result retained by the current Launcher surface.
    pub(crate) fn from_search_match(
        name: impl Into<String>,
        bundle_path: impl Into<PathBuf>,
        match_rank: u8,
    ) -> Self {
        // Result ownership is limited to strings/scalars; filesystem handles close within the scanner.
        Self {
            name: name.into(),
            bundle_path: bundle_path.into(),
            match_rank,
        }
    }

    /// Borrow the visible application name for ranking/presentation without copying it.
    #[must_use]
    pub fn name(&self) -> &str {
        // The borrow cannot outlive the query result that owns the display metadata.
        &self.name
    }

    /// Borrow the discovered candidate path; launch revalidates absolute identity and current file type.
    #[must_use]
    pub fn bundle_path(&self) -> &Path {
        // Discovery is a snapshot, so this path is not proof that the bundle still exists at activation.
        &self.bundle_path
    }
}

/// Expected rejection before a native launch request is submitted to LaunchServices.
#[derive(Debug, Eq, PartialEq)]
pub enum ApplicationLaunchError {
    InvalidApplicationPath,
    MainThreadRequired,
}

impl std::fmt::Display for ApplicationLaunchError {
    /// Format bounded product feedback without exposing private filesystem paths.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            // Missing, relative, linked, or non-bundle candidates cannot be launched through this boundary.
            Self::InvalidApplicationPath => formatter.write_str("应用路径无效"),
            // AppKit access is confined to the process main thread even for an otherwise valid path.
            Self::MainThreadRequired => formatter.write_str("应用必须在主线程启动"),
        }
    }
}

impl std::error::Error for ApplicationLaunchError {}

/// One query shares finite entry/category budgets across every system and user root.
struct ScanBudget {
    entries: usize,
    categories: usize,
}

impl ScanBudget {
    /// Start one independent scan with the declared whole-query IO bounds.
    const fn new() -> Self {
        // Per-root/category limits apply in addition to these shared whole-query ceilings.
        Self {
            entries: MAX_TOTAL_ENTRIES,
            categories: MAX_TOTAL_CATEGORIES,
        }
    }

    /// Charge an examined metadata entry before any further classification or matching work.
    fn take_entry(&mut self) -> bool {
        let Some(remaining) = self.entries.checked_sub(1) else {
            // Exhaustion terminates traversal rather than wrapping an unsigned budget back to capacity.
            return false;
        };
        // Commit the successful charge only after underflow was excluded.
        self.entries = remaining;
        true
    }

    /// Charge a single permitted category descent without permitting recursive directory walking.
    fn take_category(&mut self) -> bool {
        let Some(remaining) = self.categories.checked_sub(1) else {
            // A depleted descent allowance still permits direct root bundles but no further categories.
            return false;
        };
        // Commit one accepted descent so every later category shares the reduced allowance.
        self.categories = remaining;
        true
    }

    /// Report when no further metadata entry may be examined in this query.
    const fn exhausted(&self) -> bool {
        // Category exhaustion alone does not forbid direct bundles in later roots.
        self.entries == 0
    }
}

/// Search the fixed application roots once and return at most six ranked metadata results.
#[must_use]
pub fn search_installed_applications(
    raw_query: &str,
    requested_limit: usize,
) -> Vec<InstalledApplication> {
    // Normalize before touching the filesystem: empty queries and zero-result requests stay IO-free.
    let query = bounded_query(raw_query);
    if query.is_empty() || requested_limit == 0 {
        // The idle Launcher never scans application directories or allocates a result buffer.
        return Vec::new();
    }

    // Retain only the requested bounded best candidates while sharing one finite scan budget.
    let limit = requested_limit.min(MAX_APPLICATION_RESULTS);
    let mut results = Vec::with_capacity(limit);
    let mut budget = ScanBudget::new();
    for root in APPLICATION_ROOTS.iter().map(Path::new) {
        // The fixed root list prevents filesystem-wide discovery or an unbounded recursive walk.
        scan_root(root, query, limit, &mut results, &mut budget);
        if has_exact_match(&results) || budget.exhausted() {
            // A unique exact-name candidate or the shared IO ceiling ends system-root traversal.
            break;
        }
    }

    if !budget.exhausted()
        && let Some(home) = std::env::var_os("HOME")
    {
        // The user's standard Applications directory receives only the remaining whole-query budget.
        scan_root(
            &PathBuf::from(home).join("Applications"),
            query,
            limit,
            &mut results,
            &mut budget,
        );
    }
    if has_exact_match(&results) {
        // Exact-name activation offers one highest-ranked candidate rather than ambiguous weaker matches.
        results.truncate(1);
    }
    results
}

/// Validate a current bundle candidate and submit a main-thread LaunchServices activation request.
///
/// Success means the request was submitted; the asynchronous system launch result is not awaited here.
#[cfg(target_os = "macos")]
pub(crate) fn launch_application_bundle(bundle_path: &Path) -> Result<(), ApplicationLaunchError> {
    use objc2::{MainThreadMarker, rc::autoreleasepool};
    use objc2_app_kit::{NSWorkspace, NSWorkspaceOpenConfiguration};
    use objc2_foundation::{NSString, NSURL};

    // Revalidate the discovered snapshot before materializing a native file URL; no shell is involved.
    validate_application_bundle_path(bundle_path)?;
    let _mtm = MainThreadMarker::new().ok_or(ApplicationLaunchError::MainThreadRequired)?;
    let path = bundle_path
        .to_str()
        .ok_or(ApplicationLaunchError::InvalidApplicationPath)?;
    autoreleasepool(|_| {
        // Native URL/configuration temporaries die within this explicit activation call. LaunchServices
        // owns the asynchronous request after submission; the host retains no application object graph.
        let path = NSString::from_str(path);
        let url = NSURL::fileURLWithPath_isDirectory(&path, true);
        let configuration = NSWorkspaceOpenConfiguration::configuration();
        configuration.setActivates(true);
        configuration.setAddsToRecentItems(true);
        NSWorkspace::sharedWorkspace().openApplicationAtURL_configuration_completionHandler(
            &url,
            &configuration,
            None,
        );
    });
    Ok(())
}

/// Reject native activation on unsupported platforms without acquiring platform resources.
#[cfg(not(target_os = "macos"))]
pub(crate) fn launch_application_bundle(_bundle_path: &Path) -> Result<(), ApplicationLaunchError> {
    // A bundle request cannot be fulfilled without the macOS LaunchServices adapter.
    Err(ApplicationLaunchError::InvalidApplicationPath)
}

/// Refuse relative, missing, linked, and non-directory final components before native activation.
#[cfg(target_os = "macos")]
fn validate_application_bundle_path(path: &Path) -> Result<(), ApplicationLaunchError> {
    // Inspect the final component without following symlinks; discovery may have become stale.
    let metadata =
        fs::symlink_metadata(path).map_err(|_| ApplicationLaunchError::InvalidApplicationPath)?;
    if !path.is_absolute()
        || !has_app_extension(path)
        || !metadata.file_type().is_dir()
        || metadata.file_type().is_symlink()
    {
        // Launch only current absolute .app directories; failure leaves Launcher ownership unchanged.
        return Err(ApplicationLaunchError::InvalidApplicationPath);
    }
    Ok(())
}

/// Borrow at most the first 128 trimmed Unicode scalar values without splitting a UTF-8 sequence.
fn bounded_query(raw_query: &str) -> &str {
    // The scalar boundary protects localized app queries from byte-index panics without a second copy.
    let query = raw_query.trim();
    let end = query
        .char_indices()
        .nth(MAX_QUERY_CHARACTERS)
        .map_or(query.len(), |(index, _)| {
            // The 129th scalar's byte offset excludes it while preserving every accepted scalar whole.
            index
        });
    &query[..end]
}

/// Inspect bounded direct bundles and at most one bounded category level beneath an allowed root.
fn scan_root(
    root: &Path,
    query: &str,
    limit: usize,
    results: &mut Vec<InstalledApplication>,
    budget: &mut ScanBudget,
) {
    let Ok(entries) = fs::read_dir(root) else {
        // Missing/inaccessible optional roots contribute no results and acquire no persistent state.
        return;
    };
    let mut category_count = 0;
    for entry in entries.take(MAX_ENTRIES_PER_ROOT).flatten() {
        // The iterator cap includes failed entries; the shared budget additionally bounds valid metadata.
        if !budget.take_entry() {
            // No candidate or category classification may run once this query's entry allowance is gone.
            break;
        }
        let Ok(file_type) = entry.file_type() else {
            // A removed/inaccessible candidate is skipped without aborting other bounded results.
            continue;
        };
        if !file_type.is_dir() || file_type.is_symlink() {
            // Ordinary files and linked directories are never entered or returned as launch candidates.
            continue;
        }
        let file_name = entry.file_name();
        let Some(file_name) = file_name.to_str() else {
            // Native presentation/ranking requires a valid UTF-8 name; malformed names remain invisible.
            continue;
        };
        if let Some(name) = application_name(file_name) {
            // A direct .app bundle is one atomic candidate; never recurse into its Contents directory.
            consider_entry(&entry, name, query, limit, results);
            if has_exact_match(results) {
                // Exact display identity ends this root without scanning additional bundle contents.
                break;
            }
            continue;
        }
        if file_name.starts_with('.')
            || category_count >= MAX_CATEGORIES_PER_ROOT
            || !budget.take_category()
        {
            // Hidden folders and exhausted per-root/global category budgets cannot trigger descent.
            continue;
        }
        // Descend one category level only; the child scanner never calls this root scanner recursively.
        category_count += 1;
        scan_category(&entry.path(), query, limit, results, budget);
        if has_exact_match(results) {
            // Once this category found the exact candidate, no weaker root/category result is needed.
            break;
        }
    }
}

/// Inspect direct .app children of one category without following links or deeper directories.
fn scan_category(
    category: &Path,
    query: &str,
    limit: usize,
    results: &mut Vec<InstalledApplication>,
    budget: &mut ScanBudget,
) {
    let Ok(entries) = fs::read_dir(category) else {
        // Optional categories may disappear or deny access during a query; skip without retaining a handle.
        return;
    };
    for entry in entries.take(MAX_ENTRIES_PER_CATEGORY).flatten() {
        // Both the child iterator cap and the whole-query metadata ceiling bound this one-level scan.
        if !budget.take_entry() {
            // Exhaustion prevents the category from consuming more shared work allowance.
            break;
        }
        let Ok(file_type) = entry.file_type() else {
            // A removed/inaccessible candidate is skipped without aborting other bounded results.
            continue;
        };
        if !file_type.is_dir() || file_type.is_symlink() {
            // Ordinary files and linked directories are never entered or returned as launch candidates.
            continue;
        }
        let file_name = entry.file_name();
        let Some(name) = file_name.to_str().and_then(application_name) else {
            // Non-bundle directories and invalid/hidden names are neither results nor deeper scan roots.
            continue;
        };
        consider_entry(&entry, name, query, limit, results);
        if has_exact_match(results) {
            // Once this category found the exact candidate, no weaker root/category result is needed.
            break;
        }
    }
}

/// Insert a matching unique candidate into the bounded ordered best-result set.
fn consider_entry(
    entry: &fs::DirEntry,
    name: &str,
    query: &str,
    limit: usize,
    results: &mut Vec<InstalledApplication>,
) {
    let Some(match_rank) = launcher_query_match_rank(name, query) else {
        // Nonmatching names retain no path/result allocation beyond the directory iterator's entry.
        return;
    };
    if results.iter().any(|existing| {
        // Duplicate paths or case-equivalent labels must not crowd out distinct applications.
        existing.bundle_path == entry.path()
            || launcher_fields_equal_case_insensitive(existing.name.as_str(), name)
    }) {
        // Preserve the first matching registration for an already represented visible application.
        return;
    }
    // Build the owned result only for a semantic match, then place it by deterministic rank/name/path.
    let candidate = InstalledApplication::from_search_match(name, entry.path(), match_rank as u8);
    let insertion_index = results.partition_point(|existing| {
        // Equal keys keep a stable insertion position without sorting/retaining an unbounded catalog.
        compare(existing, &candidate) != Ordering::Greater
    });
    if insertion_index >= limit {
        // A weaker candidate outside the retained prefix cannot improve the caller's bounded result set.
        return;
    }
    results.insert(insertion_index, candidate);
    if results.len() > limit {
        // Inserting a better candidate releases the previous weakest result immediately.
        results.pop();
    }
}

/// Rank semantic matches first, then use stable name/path tie breakers for deterministic selection.
fn compare(left: &InstalledApplication, right: &InstalledApplication) -> Ordering {
    // No locale service or bundle content lookup is needed to compare the already-owned metadata.
    left.match_rank
        .cmp(&right.match_rank)
        .then_with(|| {
            // Equal semantic ranks prefer deterministic display-name order.
            left.name.cmp(&right.name)
        })
        .then_with(|| {
            // Equal names have a deterministic final path tie breaker without extra filesystem IO.
            left.bundle_path.cmp(&right.bundle_path)
        })
}

/// Inspect the ordered result prefix for the shared query contract's exact-name rank.
fn has_exact_match(results: &[InstalledApplication]) -> bool {
    // Exact matching is the highest rank, so only the first bounded candidate can determine this exit.
    results.first().is_some_and(|result| {
        // The shared launcher query model defines exact equality as rank zero.
        result.match_rank == 0
    })
}

/// Extract a visible .app basename through an ASCII delimiter without invalid Unicode slicing.
fn application_name(file_name: &str) -> Option<&str> {
    // Split at the final ASCII extension boundary instead of slicing by four
    // bytes.  Bundle names may contain multi-byte UTF-8 characters (for
    // example, a localized application name), so `len() - 4` can land inside
    // a code point and panic during a query.
    let (name, extension) = file_name.rsplit_once('.')?;
    if !extension.eq_ignore_ascii_case("app") {
        // Sibling directories with unrelated extensions are not application candidates.
        return None;
    }
    // Empty and dot-prefixed names stay hidden even when they use the supported extension.
    (!name.is_empty() && !name.starts_with('.')).then_some(name)
}

/// Validate the final extension independently of basename/display-name extraction.
fn has_app_extension(path: &Path) -> bool {
    // Checked native-path conversion rejects an invalid extension without materializing a full path string.
    path.extension()
        .and_then(|extension| {
            // Non-UTF-8 suffixes cannot satisfy the public ASCII bundle-extension contract.
            extension.to_str()
        })
        .is_some_and(|extension| {
            // Native app extensions are recognized case-insensitively without changing the path itself.
            extension.eq_ignore_ascii_case("app")
        })
}

#[cfg(test)]
mod tests {
    use std::fs;
    use tempfile::tempdir;

    use super::{
        ScanBudget, application_name, bounded_query, scan_root, search_installed_applications,
    };

    /// Idle or zero-limit queries return before any standard application directory is accessed.
    #[test]
    fn empty_query_does_not_read_application_roots() {
        // Both exits are independent of machine-specific installed applications or filesystem permissions.
        assert!(search_installed_applications("  ", 6).is_empty());
        assert!(search_installed_applications("Safari", 0).is_empty());
    }

    /// A bundle is atomic and category descent stops after exactly one metadata layer.
    #[test]
    fn scans_only_direct_bundles_and_one_category_layer() {
        // TempDir uses an exclusive random directory, so concurrent processes cannot share fixtures.
        let directory = tempdir().unwrap();
        let root = directory.path();
        fs::create_dir_all(root.join("Example.app")).unwrap();
        fs::create_dir_all(root.join("浏览器/Google Chrome.app")).unwrap();
        fs::create_dir_all(root.join("浏览器/Deeper/Hidden.app")).unwrap();
        fs::create_dir_all(root.join("Container.app/Contents/Tool.app")).unwrap();

        // Localized category names must not interfere with the shared case-insensitive query path.
        let mut results = Vec::new();
        scan_root(root, "chrome", 6, &mut results, &mut ScanBudget::new());
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].name(), "Google Chrome");

        // Deeper folders are intentionally unreachable instead of becoming a recursive file scan.
        let mut hidden = Vec::new();
        scan_root(root, "hidden", 6, &mut hidden, &mut ScanBudget::new());
        assert!(hidden.is_empty());

        // A bundle's own Contents directory is equally outside application discovery's descent contract.
        let mut nested_bundle = Vec::new();
        scan_root(root, "tool", 6, &mut nested_bundle, &mut ScanBudget::new());
        assert!(nested_bundle.is_empty());
    }

    /// Bundle-name parsing preserves Unicode boundaries while rejecting hidden/unsupported entries.
    #[test]
    fn application_name_requires_a_visible_app_suffix() {
        // Case-insensitive ASCII suffix recognition must not byte-slice localized or emoji basenames.
        assert_eq!(application_name("Safari.app"), Some("Safari"));
        assert_eq!(application_name(".hidden.app"), None);
        assert_eq!(application_name("Safari"), None);
        assert_eq!(application_name("浏览器😀.APP"), Some("浏览器😀"));
        assert_eq!(application_name("😀app"), None);
    }

    /// Shared entry and category ceilings stop traversal before directory populations can grow work.
    #[test]
    fn shared_budgets_bound_candidate_and_category_scans() {
        // Every direct entry matches, so result count independently exposes the exact charged allowance.
        let directory = tempdir().unwrap();
        for index in 0..12 {
            // The fixture is deliberately larger than the injected whole-query entry budget.
            fs::create_dir(directory.path().join(format!("BudgetBundle-{index}.app"))).unwrap();
        }
        let mut results = Vec::new();
        let mut budget = ScanBudget {
            entries: 3,
            categories: 0,
        };
        scan_root(directory.path(), "budget", 6, &mut results, &mut budget);
        assert_eq!(results.len(), 3);
        assert_eq!(budget.entries, 0);

        // Distinct categories each contain one match; one descent token permits only one such result.
        let category_root = tempdir().unwrap();
        for index in 0..4 {
            // Full child paths make the depth boundary observable without depending on read_dir order.
            fs::create_dir_all(
                category_root
                    .path()
                    .join(format!("Category-{index}/CategoryBundle-{index}.app")),
            )
            .unwrap();
        }
        let mut categorized_results = Vec::new();
        let mut category_budget = ScanBudget {
            entries: 32,
            categories: 1,
        };
        scan_root(
            category_root.path(),
            "categorybundle",
            6,
            &mut categorized_results,
            &mut category_budget,
        );
        assert_eq!(categorized_results.len(), 1);
        assert_eq!(category_budget.categories, 0);
    }

    /// Hidden bundles/directories and symbolic links cannot expand or pollute the candidate set.
    #[test]
    fn skips_hidden_entries_and_linked_bundles_or_categories() {
        use std::os::unix::fs::symlink;

        // Only one visible real bundle should survive despite several apparently matching aliases.
        let directory = tempdir().unwrap();
        let outside = tempdir().unwrap();
        fs::create_dir(directory.path().join("VisibleTarget.app")).unwrap();
        fs::create_dir(directory.path().join(".HiddenTarget.app")).unwrap();
        fs::create_dir_all(
            directory
                .path()
                .join(".HiddenCategory/HiddenCategoryTarget.app"),
        )
        .unwrap();
        fs::create_dir(outside.path().join("OutsideTarget.app")).unwrap();
        symlink(
            outside.path().join("OutsideTarget.app"),
            directory.path().join("LinkedTarget.app"),
        )
        .unwrap();
        symlink(outside.path(), directory.path().join("LinkedCategory")).unwrap();

        // Neither final bundle aliases nor linked category descent can become retained application results.
        let mut results = Vec::new();
        scan_root(
            directory.path(),
            "target",
            6,
            &mut results,
            &mut ScanBudget::new(),
        );
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].name(), "VisibleTarget");
    }

    /// Trimming and query limits preserve Unicode scalar boundaries before any filesystem scan.
    #[test]
    fn unicode_queries_are_bounded_without_splitting_utf8() {
        // A supplementary scalar occupies four bytes but counts as one character in the query budget.
        let query = format!("  {}suffix  ", "😀".repeat(128));
        let bounded = bounded_query(&query);
        assert_eq!(bounded.chars().count(), 128);
        assert_eq!(bounded.len(), 512);
        assert!(!bounded.ends_with("suffix"));
    }

    /// Activation revalidates final filesystem type instead of trusting a stale discovery path.
    #[cfg(target_os = "macos")]
    #[test]
    fn bundle_path_validation_rejects_links_and_replaced_candidates() {
        use super::{ApplicationLaunchError, validate_application_bundle_path};
        use std::os::unix::fs::symlink;

        // A real absolute directory with the supported suffix is a launch candidate before replacement.
        let directory = tempdir().unwrap();
        let bundle = directory.path().join("Candidate.app");
        fs::create_dir(&bundle).unwrap();
        assert_eq!(validate_application_bundle_path(&bundle), Ok(()));
        let alias = directory.path().join("Alias.app");
        symlink(&bundle, &alias).unwrap();
        assert_eq!(
            validate_application_bundle_path(&alias),
            Err(ApplicationLaunchError::InvalidApplicationPath)
        );

        // Replacing a discovered directory with a file must make the same path invalid at activation.
        fs::remove_dir(&bundle).unwrap();
        fs::write(&bundle, b"not a bundle directory").unwrap();
        assert_eq!(
            validate_application_bundle_path(&bundle),
            Err(ApplicationLaunchError::InvalidApplicationPath)
        );
    }
}
