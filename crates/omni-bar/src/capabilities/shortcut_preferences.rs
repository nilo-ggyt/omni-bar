//! Validated shortcut choices and small atomic preference files, independent of AppKit.

use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::PathBuf;

mod keys;

const COMMAND: u32 = 1 << 8;
const SHIFT: u32 = 1 << 9;
const OPTION: u32 = 1 << 11;
const CONTROL: u32 = 1 << 12;

/// Staging files are uniquely created siblings; the historical predictable name is never opened.
const STAGING_PREFIX: &str = ".launcher-shortcut-";

/// Standard choices plus a previously saved chord backed by a public Carbon virtual key.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum ShortcutBinding {
    #[default]
    CommandSpace,
    CommandShiftSpace,
    ControlOptionSpace,
    CommandShiftK,
    ControlShiftSpace,
    OptionSpace,
    Custom(ShortcutChord),
}

/// A validated modifier/key pair owns static key metadata only, never an idle heap string.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ShortcutChord {
    key: &'static keys::ShortcutKey,
    modifiers: u32,
}

impl ShortcutBinding {
    /// Stable settings order, matching all choices previously offered by the product.
    pub(crate) const ALL: [Self; 6] = [
        Self::CommandSpace,
        Self::CommandShiftSpace,
        Self::ControlOptionSpace,
        Self::CommandShiftK,
        Self::ControlShiftSpace,
        Self::OptionSpace,
    ];

    /// Preserve the old modifier/key grammar while refusing keys without this adapter's public mapping.
    pub(crate) fn parse(binding: &str) -> Result<Self, ShortcutPreferencesError> {
        // Persisted/external input remains bounded before normalization; no raw native keycode is accepted.
        if binding.len() > 128 {
            // Persisted or external oversized input is rejected before allocating tokens.
            return Err(ShortcutPreferencesError::InvalidBinding);
        }
        let mut modifiers = 0;
        let mut key = None;
        for raw in binding.trim().split('+') {
            // The preserved parser requires modifiers before one final key; duplicate modifiers are idempotent.
            let token = raw.trim().to_uppercase();
            if token.is_empty() || key.is_some() {
                // Empty components, multiple keys and a modifier after the key cannot identify one chord.
                return Err(ShortcutPreferencesError::InvalidBinding);
            }
            match token.as_str() {
                // Both historical Option spellings select the same Carbon modifier.
                "OPTION" | "ALT" => modifiers |= OPTION,
                // Preserve the short Control spelling accepted by the baseline parser.
                "CONTROL" | "CTRL" => modifiers |= CONTROL,
                // Command-or-Control resolves to Command on the supported macOS target.
                "COMMAND" | "CMD" | "SUPER" | "COMMANDORCONTROL" | "COMMANDORCTRL"
                | "CMDORCTRL" | "CMDORCONTROL" => modifiers |= COMMAND,
                // Shift supplements a primary modifier but never replaces that product requirement.
                "SHIFT" => modifiers |= SHIFT,
                // A key must have both a baseline name and a corroborated public Carbon mapping.
                _ => {
                    key = Some(keys::find(&token).ok_or_else(|| {
                        if keys::known_without_public_mapping(&token) {
                            // Recognized baseline keys remain distinguishable from malformed settings.
                            ShortcutPreferencesError::UnsupportedBinding
                        } else {
                            // Unknown names are invalid input rather than permission to invent a keycode.
                            ShortcutPreferencesError::InvalidBinding
                        }
                    })?);
                }
            }
        }
        let key = key.ok_or(ShortcutPreferencesError::InvalidBinding)?;
        if modifiers & (COMMAND | CONTROL | OPTION) == 0 {
            // Keep the existing product rule even though Carbon itself also accepts unmodified keys.
            return Err(ShortcutPreferencesError::MissingPrimaryModifier);
        }
        // Normalize the six standard combinations so equivalent aliases retain one value identity.
        for candidate in Self::ALL {
            if candidate.carbon_key_code() == key.code && candidate.carbon_modifiers() == modifiers
            {
                // A saved standard choice must not appear a second time as a custom picker option.
                return Ok(candidate);
            }
        }
        Ok(Self::Custom(ShortcutChord { key, modifiers }))
    }

    /// Return the stable text persisted by this and the previous settings implementation.
    pub(crate) fn canonical(self) -> String {
        match self {
            // Preserve the default serializer spelling for existing installations.
            Self::CommandSpace => "super+Space".to_owned(),
            // The previous library orders Shift before Super when serializing.
            Self::CommandShiftSpace => "shift+super+Space".to_owned(),
            // Option is represented as Alt in the portable persisted format.
            Self::ControlOptionSpace => "control+alt+Space".to_owned(),
            // KeyK maps to the physical ANSI K key used by the old settings choices.
            Self::CommandShiftK => "shift+super+KeyK".to_owned(),
            // Control and Shift require no Command modifier.
            Self::ControlShiftSpace => "control+shift+Space".to_owned(),
            // Option alone is a valid primary modifier.
            Self::OptionSpace => "alt+Space".to_owned(),
            // Custom values use the baseline serializer's Shift/Control/Alt/Super order.
            Self::Custom(chord) => {
                let mut result = String::new();
                for (flag, name) in [
                    (SHIFT, "shift+"),
                    (CONTROL, "control+"),
                    (OPTION, "alt+"),
                    (COMMAND, "super+"),
                ] {
                    // Only this transient result owns bytes; the application-lifetime binding stays Copy.
                    if chord.modifiers & flag != 0 {
                        // Emit exactly the validated active modifiers before the final physical key name.
                        result.push_str(name);
                    }
                }
                result.push_str(chord.key.name);
                result
            }
        }
    }

    /// Use Chinese key names and standard modifier symbols without changing persistence or registration.
    pub(crate) fn display_label(self) -> String {
        match self {
            // The default chord retains the system Command symbol and localizes only the Space name.
            Self::CommandSpace => "⌘空格".to_owned(),
            // Show Shift explicitly so this selectable alternative cannot be confused with Command-Space.
            Self::CommandShiftSpace => "⇧⌘空格".to_owned(),
            // Both modifier symbols identify the selected Control-Option chord without changing its key.
            Self::ControlOptionSpace => "⌃⌥空格".to_owned(),
            // K stays a keycap letter; localizing the label must not change its physical key mapping.
            Self::CommandShiftK => "⇧⌘K".to_owned(),
            // Control-Shift remains visually distinct from the Command-based alternatives.
            Self::ControlShiftSpace => "⌃⇧空格".to_owned(),
            // The Option-only choice displays its sole modifier and the localized Space name.
            Self::OptionSpace => "⌥空格".to_owned(),
            // A saved custom chord changes presentation only; parsing and Carbon still use its static key facts.
            Self::Custom(chord) => {
                let mut result = String::new();
                for (flag, symbol) in [(SHIFT, "⇧"), (CONTROL, "⌃"), (OPTION, "⌥"), (COMMAND, "⌘")]
                {
                    // Inspect the same four modifier flags used by registration in a stable display order.
                    if chord.modifiers & flag != 0 {
                        // An unset flag must not introduce an extra modifier into the displayed chord.
                        result.push_str(symbol);
                    }
                }
                result.push_str(chord.key.label());
                result
            }
        }
    }

    /// Keep the six standard positions stable and append only the actual current custom chord.
    pub(crate) fn picker_choices(self) -> Vec<Self> {
        let mut choices = Self::ALL.to_vec();
        if !choices.contains(&self) {
            // A current saved custom chord remains selectable without expanding the product's editor scope.
            choices.push(self);
        }
        choices
    }

    /// Return the public Carbon virtual key code for the validated choice.
    pub(crate) const fn carbon_key_code(self) -> u32 {
        match self {
            // This key was validated through the static public-mapping table during parsing.
            Self::Custom(chord) => chord.key.code,
            // ANSI K is the sole non-Space key among the six standard choices.
            Self::CommandShiftK => 40,
            // All other choices use the public Space key code.
            _ => 49,
        }
    }

    /// Return public Carbon modifier bits without depending on the Tauri shortcut plugin.
    pub(crate) const fn carbon_modifiers(self) -> u32 {
        match self {
            // Parsing admitted only the four supported Carbon modifier flags.
            Self::Custom(chord) => chord.modifiers,
            // Command is bit 8 in Carbon's documented modifier representation.
            Self::CommandSpace => 1 << 8,
            // Combine Command and Shift without left/right-specific device flags.
            Self::CommandShiftSpace | Self::CommandShiftK => (1 << 8) | (1 << 9),
            // Control and Option map to bits 12 and 11 respectively.
            Self::ControlOptionSpace => (1 << 12) | (1 << 11),
            // Control-Shift uses the same Space key with these two modifiers.
            Self::ControlShiftSpace => (1 << 12) | (1 << 9),
            // Option-only remains a distinct registration.
            Self::OptionSpace => 1 << 11,
        }
    }
}

/// The sole published launcher chord; the enclosing Option represents an absent registration explicitly.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ShortcutView {
    pub(crate) binding: ShortcutBinding,
}

/// Own the exact picker rows; a placeholder has no binding and cannot register the default by index.
pub(crate) struct ShortcutPickerModel {
    rows: Vec<Option<ShortcutBinding>>,
    selected: usize,
}

impl ShortcutPickerModel {
    /// Select only the active chord; without one, show a placeholder until the user chooses a real row.
    pub(crate) fn new(current: Option<ShortcutView>) -> Self {
        match current {
            Some(view) => {
                // Preserve the six standard positions and append the current custom chord at most once.
                let choices = view.binding.picker_choices();
                // A built-in chord selects its stable position; a custom chord selects the single appended row.
                let selected = ShortcutBinding::ALL
                    .iter()
                    .position(|binding| *binding == view.binding)
                    .unwrap_or(ShortcutBinding::ALL.len());
                Self {
                    rows: choices.into_iter().map(Some).collect(),
                    selected,
                }
            }
            None => {
                // This row reports no registration and must not become a key when the save action reads it.
                Self {
                    rows: std::iter::once(None)
                        .chain(ShortcutBinding::ALL.map(Some))
                        .collect(),
                    selected: 0,
                }
            }
        }
    }

    /// Build the native menu and resolve its later selection against this same owned row snapshot.
    pub(crate) fn rows(&self) -> &[Option<ShortcutBinding>] {
        &self.rows
    }

    /// Return the active row captured at construction, or the placeholder when no chord was registered.
    pub(crate) fn selected(&self) -> usize {
        self.selected
    }

    /// Return a binding only for a real row; placeholders and out-of-range indices return None.
    /// Callers must reject a failed conversion from a signed native index before reaching this method.
    pub(crate) fn binding_at(&self, index: usize) -> Option<ShortcutBinding> {
        self.rows.get(index).copied().flatten()
    }
}

/// Bounded product failures shared between the native preferences UI and Carbon owner.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub(crate) enum ShortcutPreferencesError {
    #[error("快捷键格式无效")]
    InvalidBinding,
    #[error("快捷键至少需要 Command、Control 或 Option 修饰键")]
    MissingPrimaryModifier,
    #[error("该快捷键没有受支持的原生按键映射，原配置未改动")]
    UnsupportedBinding,
    #[error("快捷键配置状态不可用")]
    StateUnavailable,
    #[error("无法注册该快捷键，可能已被系统或其他应用占用")]
    RegistrationUnavailable,
    #[error("快捷键配置无法保存")]
    StorageUnavailable,
    /// The new file replaced the old preference, but containing-directory synchronization failed.
    #[error("新快捷键已生效并写入，但配置目录同步失败，断电后的保存状态无法确认；请重试")]
    AppliedWithDurabilityUncertain,
    /// This request did not commit; one inactive native registration still needs bounded cleanup.
    #[error("快捷键未更改，系统注册清理尚未完成；请重试")]
    CleanupPending,
    /// The requested binding and file committed, but the old inactive native registration remains owned.
    #[error("新快捷键已生效并保存，但旧系统注册清理尚未完成；请重试")]
    AppliedWithCleanupPending,
    /// Both committed-file durability and release of the old inactive registration remain uncertain.
    #[error("新快捷键已生效，但配置目录同步失败且旧系统注册清理尚未完成；请重试")]
    AppliedWithCleanupAndDurabilityUncertain,
}

/// Main-thread registration owner implemented by the application composition root.
pub(crate) trait ShortcutPreferences {
    /// Read the actual registration: Ok(None) means inactive, while Err must never imply a default chord.
    fn current(&self) -> Result<Option<ShortcutView>, ShortcutPreferencesError>;
    /// Acquire and persist an explicit user choice, including recovery from an inactive registration.
    /// The candidate starts responding only after the settings commit.
    ///
    /// CleanupPending preserves the previous active chord, possibly None, and blocks another acquisition
    /// until the old inactive registration is released. AppliedWithCleanupPending reports a committed
    /// chord and file with cleanup still owed. AppliedWithDurabilityUncertain reports a committed rename
    /// whose directory sync failed; the combined variant retains both outstanding effects.
    /// Callers must refresh current() after any error because a post-commit failure does not roll back.
    /// Native registration and the filesystem rename cannot form one atomic transaction.
    fn update(&self, binding: &str) -> Result<ShortcutView, ShortcutPreferencesError>;
}

/// Own paths only; descriptors and buffers exist solely during explicit load/persist operations.
pub(crate) struct ShortcutSettingsStore {
    settings_path: PathBuf,
}

impl ShortcutSettingsStore {
    /// Preserve the existing Application Support filenames so upgrades keep the user's setting.
    pub(crate) fn new(application_data: PathBuf) -> Self {
        // Resolve filenames below the already-resolved application data directory.
        Self {
            settings_path: application_data.join("launcher-shortcut"),
        }
    }

    /// Load a small regular file; absence is distinct from malformed or inaccessible settings.
    pub(crate) fn load(&self) -> Result<Option<ShortcutBinding>, ShortcutPreferencesError> {
        // Never follow a final-component link or read an unbounded external file into memory.
        let mut file = match OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
            .open(&self.settings_path)
        {
            // Existing settings must be bounded and structurally valid before parsing.
            Ok(file) => file,
            // A new installation legitimately has no persisted override.
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            // Other failures must be visible to the startup policy rather than silently discarded.
            Err(_) => return Err(ShortcutPreferencesError::StorageUnavailable),
        };
        let metadata = file
            .metadata()
            .map_err(|_| ShortcutPreferencesError::StorageUnavailable)?;
        if !metadata.is_file() || metadata.len() > 128 {
            // Reject special files and oversized data before the read can block or allocate freely.
            return Err(ShortcutPreferencesError::InvalidBinding);
        }
        // Read at most one byte beyond the accepted limit to detect growth without unbounded memory.
        let mut binding = String::new();
        Read::by_ref(&mut file)
            .take(129)
            .read_to_string(&mut binding)
            .map_err(|_| ShortcutPreferencesError::InvalidBinding)?;
        // The parser also enforces the bound if the file grew between metadata and read.
        ShortcutBinding::parse(&binding).map(Some)
    }

    /// Flush a unique sibling and atomically replace the setting, then synchronize its directory.
    ///
    /// `StorageUnavailable` is strictly before rename and preserves the previous destination.
    /// `AppliedWithDurabilityUncertain` means the new destination is already visible; it cannot
    /// promise rollback or survival of an unexpected power loss. Success observes both sync calls,
    /// rather than providing a broader guarantee about hardware caches or newly created ancestors.
    pub(crate) fn persist(&self, binding: ShortcutBinding) -> Result<(), ShortcutPreferencesError> {
        // Create only the application data directory needed by this explicit user change.
        let directory = self
            .settings_path
            .parent()
            .ok_or(ShortcutPreferencesError::StorageUnavailable)?;
        fs::create_dir_all(directory).map_err(|_| ShortcutPreferencesError::StorageUnavailable)?;
        // Acquire the directory before committing anything. Reject a final directory symlink and
        // retain this transient descriptor until directory synchronization completes.
        let directory_file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(directory)
            .map_err(|_| ShortcutPreferencesError::StorageUnavailable)?;
        // tempfile uses exclusive creation and a private Unix mode. A fresh random sibling cannot
        // truncate a preexisting hardlink/symlink, and its owner cleans only its own staging path.
        // The resolved application directory must remain private to its user during this operation.
        let mut staged = tempfile::Builder::new()
            .prefix(STAGING_PREFIX)
            .suffix(".pending")
            .rand_bytes(16)
            .tempfile_in(directory)
            .map_err(|_| ShortcutPreferencesError::StorageUnavailable)?;
        // Finish the bounded canonical bytes and synchronize the file before exposing its name as
        // committed state. Every failure here drops the staging owner, leaving the old file intact.
        writeln!(staged, "{}", binding.canonical())
            .map_err(|_| ShortcutPreferencesError::StorageUnavailable)?;
        staged
            .flush()
            .map_err(|_| ShortcutPreferencesError::StorageUnavailable)?;
        staged
            .as_file()
            .sync_all()
            .map_err(|_| ShortcutPreferencesError::StorageUnavailable)?;
        // Same-directory persist atomically replaces the destination entry, never the contents of
        // a file reached through a destination hardlink/symlink. On failure PersistError retains
        // the unique temporary file, whose destructor removes only the path acquired above.
        let committed_file = staged.persist(&self.settings_path).map_err(|error| {
            // Retain the precommit error meaning while ending this operation's staging ownership.
            drop(error);
            ShortcutPreferencesError::StorageUnavailable
        })?;
        drop(committed_file);
        // Rename is the commit point: a failed directory sync must not trigger a native rollback
        // or report that the previous preference survived. No cleanup owns the destination now.
        directory_file
            .sync_all()
            .map_err(|_| ShortcutPreferencesError::AppliedWithDurabilityUncertain)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
    use std::path::Path;
    use tempfile::tempdir;

    /// Snapshot only fixture entry names so every operation has an exact residue expectation.
    fn entry_names(directory: &Path) -> Vec<String> {
        let mut names = fs::read_dir(directory)
            .unwrap()
            .map(|entry| {
                // Controlled fixture names are ASCII; a failed read must fail rather than hide residue.
                entry.unwrap().file_name().into_string().unwrap()
            })
            .collect::<Vec<_>>();
        names.sort_unstable();
        names
    }

    /// Keep all supported registration mappings and historical serializer spellings stable.
    #[test]
    fn choices_round_trip_and_have_distinct_native_mappings() {
        // Each UI choice must reconstruct exactly the same registration after restart.
        for binding in ShortcutBinding::ALL {
            assert_eq!(ShortcutBinding::parse(&binding.canonical()), Ok(binding));
        }
        // Historical modifier order remains readable while unknown/duplicate key sets are rejected.
        assert_eq!(
            ShortcutBinding::parse("super+shift+Space"),
            Ok(ShortcutBinding::CommandShiftSpace)
        );
        assert_eq!(ShortcutBinding::CommandShiftK.carbon_key_code(), 40);
        assert_eq!(ShortcutBinding::ControlOptionSpace.carbon_modifiers(), 6144);
        assert!(ShortcutBinding::parse("shift+Space").is_err());
        assert_eq!(
            ShortcutBinding::parse("super+super+Space"),
            Ok(ShortcutBinding::CommandSpace)
        );
        assert!(ShortcutBinding::parse("super+F99").is_err());
    }

    /// Missing and malformed persisted data must remain distinguishable to the startup owner.
    #[test]
    fn settings_are_bounded_and_persist_without_extra_files() {
        // Use an isolated root so tests never inspect or modify real application preferences.
        let root = tempdir().unwrap();
        let store = ShortcutSettingsStore::new(root.path().to_owned());
        assert_eq!(store.load(), Ok(None));
        store.persist(ShortcutBinding::ControlShiftSpace).unwrap();
        assert_eq!(store.load(), Ok(Some(ShortcutBinding::ControlShiftSpace)));
        assert_eq!(entry_names(root.path()), ["launcher-shortcut"]);
        assert_eq!(
            fs::metadata(&store.settings_path)
                .unwrap()
                .permissions()
                .mode()
                & 0o077,
            0
        );
        // A malformed external replacement is distinguished from a valid persisted user choice.
        fs::write(&store.settings_path, "x".repeat(129)).unwrap();
        assert_eq!(store.load(), Err(ShortcutPreferencesError::InvalidBinding));
    }

    /// Historical hardlinks/symlinks are unrelated entries, never write targets or cleanup candidates.
    #[test]
    fn historical_staging_links_preserve_victim_bytes_and_preexisting_entries() {
        for symbolic in [false, true] {
            // Each attack fixture has its own existing setting and separate valuable file.
            let root = tempdir().unwrap();
            let store = ShortcutSettingsStore::new(root.path().to_owned());
            store.persist(ShortcutBinding::CommandSpace).unwrap();
            let victim = root.path().join("victim");
            let legacy_stage = root.path().join(".launcher-shortcut.pending");
            fs::write(&victim, b"unrelated valuable bytes").unwrap();
            if symbolic {
                // O_NOFOLLOW used to reject this link, after which cleanup incorrectly deleted it.
                symlink(&victim, &legacy_stage).unwrap();
            } else {
                // O_NOFOLLOW does not protect a hardlink: the prior truncate would overwrite its victim.
                fs::hard_link(&victim, &legacy_stage).unwrap();
            }
            let expected_entries = entry_names(root.path());
            let original_inode = fs::metadata(&victim).unwrap().ino();
            // The real commit must ignore the predictable legacy name instead of accepting its write authority.
            store.persist(ShortcutBinding::OptionSpace).unwrap();
            assert_eq!(store.load(), Ok(Some(ShortcutBinding::OptionSpace)));
            assert_eq!(fs::read(&victim).unwrap(), b"unrelated valuable bytes");
            assert_eq!(fs::metadata(&legacy_stage).unwrap().ino(), original_inode);
            assert_eq!(
                fs::symlink_metadata(&legacy_stage)
                    .unwrap()
                    .file_type()
                    .is_symlink(),
                symbolic
            );
            assert_eq!(entry_names(root.path()), expected_entries);
        }
    }

    /// Replacing a linked destination changes its directory entry without writing through to its victim.
    #[test]
    fn destination_hardlinks_and_symlinks_never_receive_in_place_writes() {
        for symbolic in [false, true] {
            // A destination may have been externally replaced since the last read; rename must remain safe.
            let root = tempdir().unwrap();
            let store = ShortcutSettingsStore::new(root.path().to_owned());
            let victim = root.path().join("victim");
            fs::write(&victim, b"super+Space\n").unwrap();
            if symbolic {
                // The committed name points elsewhere, but replacing that link must not follow it.
                symlink(&victim, &store.settings_path).unwrap();
            } else {
                // A hardlink has no symlink flag, so safe replacement must avoid truncation entirely.
                fs::hard_link(&victim, &store.settings_path).unwrap();
            }
            let original_inode = fs::metadata(&victim).unwrap().ino();
            store.persist(ShortcutBinding::ControlShiftSpace).unwrap();
            assert_eq!(fs::read(&victim).unwrap(), b"super+Space\n");
            assert_ne!(
                fs::metadata(&store.settings_path).unwrap().ino(),
                original_inode
            );
            assert!(
                fs::symlink_metadata(&store.settings_path)
                    .unwrap()
                    .is_file()
            );
            assert_eq!(store.load(), Ok(Some(ShortcutBinding::ControlShiftSpace)));
            assert_eq!(entry_names(root.path()), ["launcher-shortcut", "victim"]);
        }
    }

    /// A directory occupying the legacy staging name no longer blocks legitimate configuration changes.
    #[test]
    fn historical_staging_directory_remains_untouched() {
        let root = tempdir().unwrap();
        let store = ShortcutSettingsStore::new(root.path().to_owned());
        let legacy_stage = root.path().join(".launcher-shortcut.pending");
        fs::create_dir(&legacy_stage).unwrap();
        fs::write(legacy_stage.join("owned-by-someone-else"), b"preserve").unwrap();
        // New staging is exclusive and independent; it never cleans a preexisting directory tree.
        store.persist(ShortcutBinding::OptionSpace).unwrap();
        assert_eq!(
            fs::read(legacy_stage.join("owned-by-someone-else")).unwrap(),
            b"preserve"
        );
        assert_eq!(
            entry_names(root.path()),
            [".launcher-shortcut.pending", "launcher-shortcut"]
        );
    }

    /// Failure before acquiring staging cannot delete any entry in an unaccepted directory alias.
    #[test]
    fn failed_directory_acquisition_preserves_preexisting_configuration_and_staging_link() {
        let root = tempdir().unwrap();
        let real_directory = root.path().join("real-application-data");
        let real_store = ShortcutSettingsStore::new(real_directory.clone());
        real_store.persist(ShortcutBinding::CommandSpace).unwrap();
        let victim = real_directory.join("victim");
        let legacy_stage = real_directory.join(".launcher-shortcut.pending");
        fs::write(&victim, b"unrelated bytes").unwrap();
        fs::hard_link(&victim, &legacy_stage).unwrap();
        let original_entries = entry_names(&real_directory);
        let alias = root.path().join("unresolved-application-data");
        symlink(&real_directory, &alias).unwrap();
        // The constructor expects a resolved private directory. A final symlink fails acquisition
        // before any random sibling exists, and cannot authorize cleanup in the aliased directory.
        let aliased_store = ShortcutSettingsStore::new(alias);
        assert_eq!(
            aliased_store.persist(ShortcutBinding::OptionSpace),
            Err(ShortcutPreferencesError::StorageUnavailable)
        );
        assert_eq!(real_store.load(), Ok(Some(ShortcutBinding::CommandSpace)));
        assert_eq!(fs::read(&victim).unwrap(), b"unrelated bytes");
        assert_eq!(
            fs::metadata(&legacy_stage).unwrap().ino(),
            fs::metadata(&victim).unwrap().ino()
        );
        assert_eq!(entry_names(&real_directory), original_entries);
    }

    /// A rename failure releases the acquired sibling without changing the destination directory.
    #[test]
    fn failed_rename_cleans_staging_without_deleting_destination_contents() {
        let root = tempdir().unwrap();
        let store = ShortcutSettingsStore::new(root.path().to_owned());
        fs::create_dir(&store.settings_path).unwrap();
        fs::write(store.settings_path.join("occupant"), b"preserve").unwrap();
        // A regular sibling cannot atomically replace this directory, so commit must fail.
        assert_eq!(
            store.persist(ShortcutBinding::OptionSpace),
            Err(ShortcutPreferencesError::StorageUnavailable)
        );
        assert_eq!(
            fs::read(store.settings_path.join("occupant")).unwrap(),
            b"preserve"
        );
        assert_eq!(entry_names(root.path()), ["launcher-shortcut"]);
    }

    /// Existing custom keys keep their physical mapping and historical modifier/key aliases.
    #[test]
    fn saved_custom_chords_preserve_baseline_grammar_and_public_mappings() {
        let cases = [
            (
                "control+alt+KeyK",
                "control+alt+KeyK",
                0x28,
                CONTROL | OPTION,
            ),
            (
                " CmdOrCtrl + shift + q ",
                "shift+super+KeyQ",
                0x0c,
                COMMAND | SHIFT,
            ),
            (
                "CTRL+OPTION+1",
                "control+alt+Digit1",
                0x12,
                CONTROL | OPTION,
            ),
            ("Command+F20", "super+F20", 0x5a, COMMAND),
            ("alt+NUMPLUS", "alt+NumpadAdd", 0x45, OPTION),
            ("CTRL+LEFT", "control+ArrowLeft", 0x7b, CONTROL),
            ("Command+Insert", "super+Insert", 0x72, COMMAND),
            ("control+VolumeUp", "control+AudioVolumeUp", 0x48, CONTROL),
            ("super+super+ſ", "super+KeyS", 0x01, COMMAND),
        ];
        for (input, canonical, code, modifiers) in cases {
            // Independent expected keycodes/modifiers cover letter, function, keypad, navigation and volume families.
            let binding = ShortcutBinding::parse(input).unwrap();
            assert_eq!(binding.canonical(), canonical, "{input}");
            assert_eq!(binding.carbon_key_code(), code, "{input}");
            assert_eq!(binding.carbon_modifiers(), modifiers, "{input}");
            assert_eq!(ShortcutBinding::parse(&binding.canonical()), Ok(binding));
        }
        // Grammar errors cannot be reinterpreted as a different ordering or an arbitrary raw keycode.
        for invalid in [
            "ctrl+KeyK+shift",
            "ctrl++KeyK",
            "ctrl+KeyK+KeyQ",
            "ctrl+0x28",
        ] {
            assert_eq!(
                ShortcutBinding::parse(invalid),
                Err(ShortcutPreferencesError::InvalidBinding)
            );
        }
        assert_eq!(
            ShortcutBinding::parse("shift+KeyK"),
            Err(ShortcutPreferencesError::MissingPrimaryModifier)
        );
        assert!(!std::mem::needs_drop::<ShortcutBinding>());
    }

    /// The complete static grammar remains canonical and collision-free across primary modifier combinations.
    #[test]
    fn every_public_key_and_supported_modifier_set_round_trips() {
        let flags = [SHIFT, CONTROL, OPTION, COMMAND];
        for key in keys::KEYS {
            // Each key fact is finite source data rather than a resident dynamic registry.
            for mask in 1u32..16 {
                let modifiers = flags.iter().enumerate().fold(0, |value, (index, flag)| {
                    // Convert the finite four-bit test mask into Carbon's documented bit positions.
                    let selected = if mask & (1 << index) != 0 {
                        // This test combination includes the current documented modifier flag.
                        *flag
                    } else {
                        // An absent test bit contributes no native modifier.
                        0
                    };
                    value | selected
                });
                if modifiers & (COMMAND | CONTROL | OPTION) == 0 {
                    // Shift-only keys violate the product's primary-modifier rule and are covered separately.
                    continue;
                }
                let chord = ShortcutBinding::Custom(ShortcutChord { key, modifiers });
                let parsed = ShortcutBinding::parse(&chord.canonical()).unwrap();
                assert_eq!(parsed.carbon_key_code(), key.code);
                assert_eq!(parsed.carbon_modifiers(), modifiers);
                assert_eq!(ShortcutBinding::parse(&parsed.canonical()), Ok(parsed));
                assert!(parsed.canonical().len() <= 128);
            }
        }
    }

    /// Existing unsupported native keys stay distinguishable and their stored bytes remain unchanged.
    #[test]
    fn unsupported_saved_keys_are_rejected_without_changing_the_file() {
        let root = tempdir().unwrap();
        let store = ShortcutSettingsStore::new(root.path().to_owned());
        for name in [
            "F21",
            "PrintScreen",
            "Pause",
            "MediaPlayPause",
            "ScrollLock",
        ] {
            // These names are recognized by the baseline parser but excluded from the public Carbon scope.
            let original = format!("control+{name}\n");
            fs::write(&store.settings_path, &original).unwrap();
            assert_eq!(
                store.load(),
                Err(ShortcutPreferencesError::UnsupportedBinding)
            );
            assert_eq!(fs::read_to_string(&store.settings_path).unwrap(), original);
        }
    }

    /// Loading preserves old user bytes; only an explicit successful save canonicalizes the setting.
    #[test]
    fn custom_preference_reload_preserves_old_bytes_until_explicit_save() {
        let root = tempdir().unwrap();
        let store = ShortcutSettingsStore::new(root.path().to_owned());
        let old = b"CONTROL+OPTION+KeyK\n";
        fs::write(&store.settings_path, old).unwrap();
        let custom = store.load().unwrap().unwrap();
        assert_eq!(custom.canonical(), "control+alt+KeyK");
        assert_eq!(fs::read(&store.settings_path).unwrap(), old);
        // Explicitly saving the custom choice canonicalizes only this requested commit, then remains reloadable.
        store.persist(custom).unwrap();
        assert_eq!(store.load(), Ok(Some(custom)));
        assert_eq!(entry_names(root.path()), ["launcher-shortcut"]);
    }

    /// Only the current custom binding expands the six-choice picker and all standard indices remain stable.
    #[test]
    fn picker_appends_current_custom_and_discards_it_after_standard_selection() {
        let custom = ShortcutBinding::parse("control+alt+KeyK").unwrap();
        let choices = custom.picker_choices();
        assert_eq!(&choices[..6], &ShortcutBinding::ALL);
        assert_eq!(choices.get(6), Some(&custom));
        assert_eq!(choices.get(7), None);
        for standard in ShortcutBinding::ALL {
            // A successfully selected standard key cannot leave a stale custom option or duplicate current value.
            assert_eq!(standard.picker_choices(), ShortcutBinding::ALL);
        }
        assert_eq!(custom.display_label(), "⌃⌥K");
    }

    /// An inactive placeholder cannot resolve to the default; each later chord needs an explicit selection.
    #[test]
    fn inactive_picker_placeholder_and_all_choices_share_one_index_model() {
        let model = ShortcutPickerModel::new(None);
        assert_eq!(model.selected(), 0);
        assert_eq!(model.rows().len(), ShortcutBinding::ALL.len() + 1);
        assert_eq!(model.binding_at(0), None);
        for (index, binding) in ShortcutBinding::ALL.into_iter().enumerate() {
            // Verify every standard row after the placeholder so its offset cannot misregister another chord.
            assert_eq!(model.binding_at(index + 1), Some(binding));
        }
        assert_eq!(model.binding_at(model.rows().len()), None);
    }

    /// A displayed custom model keeps its row identities even when a later refresh builds another model.
    #[test]
    fn active_picker_custom_mapping_is_owned_and_not_rebuilt_from_later_state() {
        let custom = ShortcutBinding::parse("control+alt+KeyK").unwrap();
        let old = ShortcutPickerModel::new(Some(ShortcutView { binding: custom }));
        let replacement = ShortcutPickerModel::new(Some(ShortcutView {
            binding: ShortcutBinding::OptionSpace,
        }));
        assert_eq!(old.selected(), 6);
        assert_eq!(old.binding_at(6), Some(custom));
        assert_eq!(replacement.binding_at(6), None);
        assert_eq!(
            replacement.binding_at(replacement.selected()),
            Some(ShortcutBinding::OptionSpace)
        );
    }
}
