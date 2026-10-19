//! Fixed-capacity clipboard persistence with operation-scoped descriptors and bounded owned buffers.
//!
//! Private 0700/0600 paths and single-link descriptor checks reject preexisting aliases. A transient
//! exclusive file lock serializes cooperating instances, including same-thread native reentry; it
//! does not stop an external writer that ignores advisory locks. Positional IO never exposes named
//! file pages as Rust references: callbacks borrow owned, CRC/UTF-8-validated bytes, so truncation or
//! mutation through another safe file handle cannot invalidate their strings or cause SIGBUS.
//! File identity/extent checks reject observed replacements; they do not promise an atomic defense
//! against a hostile same-user process changing the private directory between system calls.

use std::fs::{self, File, OpenOptions};
use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::fs::{FileExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use thiserror::Error;

pub(crate) const MAX_ENTRY_COUNT: usize = 20;
pub(crate) const MAX_ENTRY_BYTES: usize = 256 * 1_024;
pub(crate) const MAX_QUERY_RESULTS: usize = MAX_ENTRY_COUNT;

const CLIPBOARD_STORAGE_DIRECTORY_NAME: &str = "Clipboard";
const CLIPBOARD_STORAGE_FILE_NAME: &str = "history-v1.mmap";
const CLIPBOARD_FILE_MAGIC: &[u8; 8] = b"OMNICLP1";
const CLIPBOARD_FILE_VERSION: u32 = 1;
// Preserve the original 16 KiB metadata/slot alignment and exact on-disk format across IO changes.
const METADATA_BYTES: usize = 16 * 1_024;
const SLOT_METADATA_START: usize = 64;
const SLOT_METADATA_BYTES: usize = 32;
const SLOT_ID_OFFSET: usize = 0;
const SLOT_CAPTURED_AT_OFFSET: usize = 8;
const SLOT_RECENCY_OFFSET: usize = 16;
const SLOT_CONTENT_LENGTH_OFFSET: usize = 24;
const SLOT_CHECKSUM_OFFSET: usize = 28;
const PREVIEW_CHARACTERS: usize = 180;
pub(crate) const MAX_QUERY_CHARACTERS: usize = 128;
const CLIPBOARD_STORAGE_FILE_BYTES: u64 =
    (METADATA_BYTES + MAX_ENTRY_COUNT * MAX_ENTRY_BYTES) as u64;

#[derive(Debug, Error)]
/// Bounded storage errors; the native presentation layer hides path-bearing IO details.
pub(crate) enum ClipboardHistoryFileError {
    #[error("剪贴板历史文件不可用")]
    StorageUnavailable(#[source] io::Error),
    #[error("剪贴板历史文件格式不受支持")]
    InvalidFormat,
    #[error("找不到这条剪贴板记录")]
    EntryNotFound,
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// A bounded preview plus scalar identity, timestamp and byte-count metadata.
pub(crate) struct ClipboardFileEntry {
    pub(crate) id: i64,
    pub(crate) preview: String,
    pub(crate) captured_at_ms: i64,
    pub(crate) byte_count: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// The post-write visible item count without returning or retaining the stored body.
pub(crate) struct ClipboardRecordOutcome {
    pub(crate) item_count: usize,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
/// A fixed-width metadata record; invalid payloads remain invisible on reopen.
struct SlotMetadata {
    id: i64,
    captured_at_ms: i64,
    recency: u64,
    content_length: u32,
    checksum: u32,
}

impl SlotMetadata {
    /// Validate scalar metadata before it may define an owned payload read.
    fn is_declared(self) -> bool {
        // An occupied slot needs positive identity/recency and a nonempty in-slot payload range.
        self.id > 0
            && self.recency > 0
            && self.content_length > 0
            && self.content_length as usize <= MAX_ENTRY_BYTES
    }
}

#[derive(Debug, Clone, Copy)]
/// One validated declared metadata record associated with its fixed payload slot.
struct IndexedSlotMetadata {
    index: usize,
    metadata: SlotMetadata,
}

#[derive(Debug)]
/// Owns only application-private paths; each foreground operation opens and closes its file.
pub(crate) struct ClipboardHistoryFile {
    storage_directory: PathBuf,
    storage_path: PathBuf,
}

impl ClipboardHistoryFile {
    /// Resolve stable filenames without reading or creating clipboard storage.
    pub(crate) fn new(app_data_directory: PathBuf) -> Self {
        // Construction retains paths only; each operation owns its descriptor, lock, and bounded buffers.
        let storage_directory = app_data_directory.join(CLIPBOARD_STORAGE_DIRECTORY_NAME);
        let storage_path = storage_directory.join(CLIPBOARD_STORAGE_FILE_NAME);
        Self {
            storage_directory,
            storage_path,
        }
    }

    /// Lazily create or validate the one fixed file for an explicit foreground activation.
    pub(crate) fn prepare(&self) -> Result<(), ClipboardHistoryFileError> {
        // Validation owns a transient descriptor only; dropping it leaves no resident file owner.
        self.open_or_create_file().map(drop)
    }

    /// Count declared slots using only a short-lived metadata buffer in storage tests.
    #[cfg(test)]
    pub(crate) fn item_count(&self) -> Result<usize, ClipboardHistoryFileError> {
        let Some(file) = self.open_existing_file()? else {
            // Empty status must not create or preallocate persistent storage.
            return Ok(0);
        };
        Ok(read_declared_slot_metadata(&file)?.len())
    }

    /// Commit one allowed body payload-first and promote duplicates without consuming a new slot.
    pub(crate) fn record(
        &self,
        content: &str,
        captured_at_ms: i64,
    ) -> Result<Option<ClipboardRecordOutcome>, ClipboardHistoryFileError> {
        if content.is_empty() || content.len() > MAX_ENTRY_BYTES {
            // Rejected captures cannot allocate a new file or overwrite a valid slot.
            return Ok(None);
        }

        // Acquire a scoped descriptor only after the capture passed its byte budget.
        let file = self.open_or_create_file()?;
        let declared_slots = read_declared_slot_metadata(&file)?;
        let checksum = crc32fast::hash(content.as_bytes());
        let mut duplicate_slot = None;
        // Compare only equal-length/checksum candidates; bytes still decide duplicate identity.
        for slot in declared_slots.iter().copied().filter(|slot| {
            // Scalar prefiltering avoids reading bodies that cannot be exact duplicates.
            slot.metadata.content_length as usize == content.len()
                && slot.metadata.checksum == checksum
        }) {
            // Every candidate body is validated in one bounded owned buffer before equality can deduplicate.
            if read_valid_clipboard_slot(&file, slot.metadata, slot.index, |stored_content| {
                // Equal hashes alone are insufficient; compare the validated bytes before reusing identity.
                stored_content.as_bytes() == content.as_bytes()
            })?
            .is_some_and(std::convert::identity)
            {
                // Equal validated bytes retain the existing identity and merely advance recency.
                duplicate_slot = Some(slot);
                break;
            }
        }

        // Derive monotonically increasing scalar identifiers and recency from the fixed metadata.
        let max_id = declared_slots
            .iter()
            .map(|slot| {
                // Every declared scalar identity contributes to the next positive record identifier.
                slot.metadata.id
            })
            .max()
            .unwrap_or(0);
        let max_recency = declared_slots
            .iter()
            .map(|slot| {
                // A new capture or duplicate promotion must sort after the most recent declared record.
                slot.metadata.recency
            })
            .max()
            .unwrap_or(0);
        // Prefer a duplicate, then an unused slot, and only then the least-recently-used record.
        let target_slot = duplicate_slot.unwrap_or_else(|| {
            // New content consumes a vacant fixed slot before any valid item can be evicted.
            first_unused_slot(&declared_slots).unwrap_or_else(|| {
                // At fixed capacity, evict the actual least-recent record with an ID tie breaker.
                declared_slots
                    .iter()
                    .min_by_key(|slot| (slot.metadata.recency, slot.metadata.id))
                    .copied()
                    .expect("a full fixed clipboard file has an LRU slot")
            })
        });
        let next_id = if duplicate_slot.is_some() {
            // Promoting an existing body preserves external row identity.
            target_slot.metadata.id
        } else {
            // A new body receives the next bounded positive identity.
            max_id.saturating_add(1).max(1)
        };
        let next_recency = max_recency.saturating_add(1).max(1);

        // Flush new payload bytes before metadata can declare them visible to another operation.
        write_clipboard_payload(
            &file,
            target_slot.index,
            target_slot.metadata.content_length as usize,
            content.as_bytes(),
        )?;
        // Refuse metadata publication if this private pathname stopped identifying the opened inode.
        validate_clipboard_file_path(&file, &self.storage_path)?;
        let mut header = read_clipboard_header(&file)?;
        write_slot_metadata(
            &mut header,
            target_slot.index,
            SlotMetadata {
                id: next_id,
                captured_at_ms,
                recency: next_recency,
                content_length: content.len() as u32,
                checksum,
            },
        );
        persist_clipboard_slot_metadata(&file, &header, target_slot.index)?;

        let item_count = if duplicate_slot.is_some() {
            // Recency promotion does not create a twenty-first item.
            declared_slots.len()
        } else {
            // New slots increase the count only until fixed capacity is reached.
            declared_slots.len().saturating_add(1).min(MAX_ENTRY_COUNT)
        };
        Ok(Some(ClipboardRecordOutcome { item_count }))
    }

    /// Inspect the fixed slot budget and retain only the requested bounded previews after body buffers drop.
    pub(crate) fn search(
        &self,
        query: &str,
        requested_limit: usize,
    ) -> Result<Vec<ClipboardFileEntry>, ClipboardHistoryFileError> {
        let Some(file) = self.open_existing_file()? else {
            // Searching a fresh installation is an empty success without side effects.
            return Ok(Vec::new());
        };
        // Bound query normalization separately from the fixed history and result budgets.
        let normalized_query = query
            .chars()
            .take(MAX_QUERY_CHARACTERS)
            .collect::<String>()
            .trim()
            .to_lowercase();
        let limit = requested_limit.clamp(1, MAX_QUERY_RESULTS);
        let mut declared_slots = read_declared_slot_metadata(&file)?;
        declared_slots.sort_unstable_by(|left, right| {
            // Newest recency wins; identity gives deterministic ordering for interrupted equal counters.
            right
                .metadata
                .recency
                .cmp(&left.metadata.recency)
                .then_with(|| {
                    // Equal interrupted counters use stable newest-identity ordering for visible results.
                    right.metadata.id.cmp(&left.metadata.id)
                })
        });

        // Inspect recency order and retain only the caller's bounded preview results.
        let mut results = Vec::with_capacity(limit.min(declared_slots.len()));
        for slot in declared_slots {
            // Each iteration advances the fixed slot scan; nonmatches consume no visible-result capacity.
            if results.len() >= limit {
                // Later slots cannot contribute once the explicit visible-result budget is full.
                break;
            }
            let entry =
                read_valid_clipboard_slot(&file, slot.metadata, slot.index, |stored_content| {
                    // Matching inspects the transient body, retaining only bounded presentation metadata.
                    content_matches_clipboard_query(stored_content, &normalized_query).then(|| {
                        // Retain an owned preview and scalar metadata; the full body is released after this callback.
                        ClipboardFileEntry {
                            id: slot.metadata.id,
                            preview: preview_clipboard_content(stored_content),
                            captured_at_ms: slot.metadata.captured_at_ms,
                            byte_count: slot.metadata.content_length as usize,
                        }
                    })
                })?;
            if let Some(Some(entry)) = entry {
                // Only valid matching payloads produce preview data; corrupt slots stay invisible.
                results.push(entry);
            }
        }
        Ok(results)
    }

    /// Borrow one validated owned body for a callback, then update its still-current LRU recency.
    pub(crate) fn with_entry_content<T>(
        &self,
        entry_id: i64,
        consume_content: impl FnOnce(&str) -> T,
    ) -> Result<T, ClipboardHistoryFileError> {
        let file = self
            .open_existing_file()?
            .ok_or(ClipboardHistoryFileError::EntryNotFound)?;
        let declared_slots = read_declared_slot_metadata(&file)?;
        // Resolve the user-selected scalar ID before allocating or reading body bytes.
        let selected_slot = declared_slots
            .iter()
            .find(|slot| slot.metadata.id == entry_id)
            .copied()
            .ok_or(ClipboardHistoryFileError::EntryNotFound)?;
        let result = read_valid_clipboard_slot(
            &file,
            selected_slot.metadata,
            selected_slot.index,
            consume_content,
        )?
        .ok_or(ClipboardHistoryFileError::EntryNotFound)?;

        // Explicit restore promotes recency after its callback and owned-body borrow have finished.
        let next_recency = declared_slots
            .iter()
            .map(|slot| {
                // Restore advances beyond every declared recency observed under the current lease.
                slot.metadata.recency
            })
            .max()
            .unwrap_or(0)
            .saturating_add(1)
            .max(1);
        // A native consumer may reenter; revalidate identity before publishing its recency change.
        validate_clipboard_file_path(&file, &self.storage_path)?;
        let mut header = read_clipboard_header(&file)?;
        if !clipboard_file_header_is_valid(&header) {
            // A noncooperating callback writer may replace the protocol header without changing the inode.
            return Err(ClipboardHistoryFileError::InvalidFormat);
        }
        if read_slot_metadata(&header, selected_slot.index) != selected_slot.metadata
            || read_valid_clipboard_slot(
                &file,
                selected_slot.metadata,
                selected_slot.index,
                |_| {
                    // A noncooperating callback writer must not make stale bytes visible through promotion.
                },
            )?
            .is_none()
        {
            // The callback already consumed its owned snapshot, but a changed entry cannot be republished.
            return Err(ClipboardHistoryFileError::EntryNotFound);
        }
        write_slot_metadata(
            &mut header,
            selected_slot.index,
            SlotMetadata {
                recency: next_recency,
                ..selected_slot.metadata
            },
        );
        persist_clipboard_slot_metadata(&file, &header, selected_slot.index)?;
        Ok(result)
    }

    /// Invalidate every record before overwriting all application-visible payload bytes.
    pub(crate) fn clear(&self) -> Result<(), ClipboardHistoryFileError> {
        let Some(file) = self.open_existing_file()? else {
            // Clearing missing history is a no-op and must not create a file.
            return Ok(());
        };

        // Invalidate metadata first, so an interrupted clear cannot expose a
        // partially overwritten payload as a valid history entry.
        validate_clipboard_file_path(&file, &self.storage_path)?;
        let metadata_end = SLOT_METADATA_START + MAX_ENTRY_COUNT * SLOT_METADATA_BYTES;
        zero_clipboard_range(
            &file,
            SLOT_METADATA_START as u64,
            metadata_end - SLOT_METADATA_START,
        )?;
        file.sync_data()
            .map_err(ClipboardHistoryFileError::StorageUnavailable)?;

        // Only a successful metadata sync permits the bounded payload overwrite phase to start.
        for slot_index in 0..MAX_ENTRY_COUNT {
            // Reuse fixed scratch inside each slot; clear retains no full-history body collection.
            zero_clipboard_range(&file, clipboard_slot_offset(slot_index), MAX_ENTRY_BYTES)?;
        }
        file.sync_data()
            .map_err(ClipboardHistoryFileError::StorageUnavailable)?;
        Ok(())
    }

    #[cfg(test)]
    /// Expose the synthetic test path without changing production ownership.
    fn storage_path(&self) -> &Path {
        // Tests receive only the configured path and never a retained descriptor, lease, or body buffer.
        &self.storage_path
    }

    /// Create a private fixed-size file once, or validate the existing compatible format.
    fn open_or_create_file(&self) -> Result<File, ClipboardHistoryFileError> {
        self.prepare_storage_directory()?;
        match create_new_clipboard_file(&self.storage_path) {
            Ok(file) => {
                // Only exclusive first creation may preallocate and establish the initial format.
                validate_clipboard_file_path(&file, &self.storage_path)?;
                preallocate_clipboard_file(&file)?;
                initialize_clipboard_file_header(&file)?;
                // A successful activation publishes only the still-owned, non-aliased private file.
                validate_clipboard_file_path(&file, &self.storage_path)?;
                Ok(file)
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                // Existing history must retain its original length and validated protocol.
                let file = open_existing_clipboard_file(&self.storage_path)
                    .map_err(ClipboardHistoryFileError::StorageUnavailable)?;
                validate_clipboard_file_path(&file, &self.storage_path)?;
                prepare_existing_clipboard_file(&file, true)?;
                // Recovery must not return a descriptor whose final pathname changed during setup.
                validate_clipboard_file_path(&file, &self.storage_path)?;
                Ok(file)
            }
            // Surface the failed acquisition without interpreting or replacing unrelated contents.
            Err(error) => Err(ClipboardHistoryFileError::StorageUnavailable(error)),
        }
    }

    /// Read existing storage without creating a file for empty status/search/clear paths.
    fn open_existing_file(&self) -> Result<Option<File>, ClipboardHistoryFileError> {
        let file = match open_existing_clipboard_file(&self.storage_path) {
            // A successful existing open is still subject to format validation before any owned read.
            Ok(file) => file,
            // Missing storage is an empty state for read/clear operations.
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            // Permissions, no-follow and other IO failures remain explicit storage failures.
            Err(error) => return Err(ClipboardHistoryFileError::StorageUnavailable(error)),
        };
        // Existing reads and clear also reject directory redirection or an unrelated hard-linked inode.
        validate_clipboard_file_path(&file, &self.storage_path)?;
        prepare_existing_clipboard_file(&file, false)?;
        Ok(Some(file))
    }

    /// Create and validate the application-private directory with restrictive permissions.
    fn prepare_storage_directory(&self) -> Result<(), ClipboardHistoryFileError> {
        // Ensure the root exists, reject final-component redirection, then restrict directory access.
        fs::create_dir_all(&self.storage_directory)
            .map_err(ClipboardHistoryFileError::StorageUnavailable)?;
        let metadata = fs::symlink_metadata(&self.storage_directory)
            .map_err(ClipboardHistoryFileError::StorageUnavailable)?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            // The storage root itself must not redirect history outside the supplied app-data root.
            return Err(ClipboardHistoryFileError::InvalidFormat);
        }
        fs::set_permissions(&self.storage_directory, fs::Permissions::from_mode(0o700))
            .map_err(ClipboardHistoryFileError::StorageUnavailable)
    }
}

/// Acquire one exclusive new regular-path candidate without following its final symlink.
fn create_new_clipboard_file(path: &Path) -> io::Result<File> {
    // Exclusive creation protects existing data; descriptor inheritance and final links are disabled.
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(path)?;
    // The operation keeps this independently opened lease until descriptor drop, including error unwind.
    file.try_lock()?;
    Ok(file)
}

/// Open existing private storage with close-on-exec and final-component no-follow.
fn open_existing_clipboard_file(path: &Path) -> io::Result<File> {
    // No truncate/create/final-link following is permitted; NONBLOCK prevents malformed FIFO paths hanging open.
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)?;
    // Refuse competing operations without blocking AppKit or a reentrant same-thread callback.
    file.try_lock()?;
    Ok(file)
}

/// Reject aliased or nonregular descriptors before recovery, allocation, or any positional IO.
fn validate_clipboard_file_descriptor(
    file: &File,
) -> Result<fs::Metadata, ClipboardHistoryFileError> {
    // Inspect the opened inode itself: O_NOFOLLOW alone cannot detect hard-link aliases.
    let metadata = file
        .metadata()
        .map_err(ClipboardHistoryFileError::StorageUnavailable)?;
    if !metadata.is_file() || metadata.nlink() != 1 {
        // Neither an external alias nor a detached inode is this private history's sole pathname.
        return Err(ClipboardHistoryFileError::InvalidFormat);
    }
    Ok(metadata)
}

/// Confirm that the private directory and final pathname still identify the validated descriptor.
fn validate_clipboard_file_path(file: &File, path: &Path) -> Result<(), ClipboardHistoryFileError> {
    // Validate the descriptor before any path operation can authorize mutation of its inode.
    let descriptor = validate_clipboard_file_descriptor(file)?;
    let directory = path
        .parent()
        .ok_or(ClipboardHistoryFileError::InvalidFormat)?;
    let directory_metadata =
        fs::symlink_metadata(directory).map_err(ClipboardHistoryFileError::StorageUnavailable)?;
    if directory_metadata.file_type().is_symlink() || !directory_metadata.is_dir() {
        // Existing-file operations must reject the same final-directory redirection as preparation.
        return Err(ClipboardHistoryFileError::InvalidFormat);
    }

    // Compare device and inode without following the final component; path replacement is not adoption.
    let path_metadata =
        fs::symlink_metadata(path).map_err(ClipboardHistoryFileError::StorageUnavailable)?;
    if !path_metadata.is_file()
        || path_metadata.nlink() != 1
        || descriptor.dev() != path_metadata.dev()
        || descriptor.ino() != path_metadata.ino()
    {
        // Refuse further mutation/publication through a descriptor whose configured pathname was replaced.
        return Err(ClipboardHistoryFileError::InvalidFormat);
    }
    Ok(())
}

/// Require the complete fixed extent on the still-single-link inode before reading or writing a range.
fn validate_clipboard_extent(file: &File) -> Result<(), ClipboardHistoryFileError> {
    // Recheck after callbacks as well as acquisition so newly introduced aliases cannot receive writes.
    if validate_clipboard_file_descriptor(file)?.len() != CLIPBOARD_STORAGE_FILE_BYTES {
        // An observed truncation is a bounded format error rather than an attempt to recreate missing bytes.
        return Err(ClipboardHistoryFileError::InvalidFormat);
    }
    Ok(())
}

/// Reserve the complete fixed extent before setting the file length once.
fn preallocate_clipboard_file(file: &File) -> Result<(), ClipboardHistoryFileError> {
    // Recovery of a zero-length file must never enlarge or chmod an unrelated hard-linked victim.
    validate_clipboard_file_descriptor(file)?;
    #[cfg(target_os = "macos")]
    {
        // Reserve the complete Darwin file extent before publishing the original fixed-size format.
        let allocation_length = i64::try_from(CLIPBOARD_STORAGE_FILE_BYTES)
            .map_err(|_| ClipboardHistoryFileError::InvalidFormat)?;
        let mut allocation = libc::fstore_t {
            fst_flags: libc::F_ALLOCATECONTIG,
            fst_posmode: libc::F_PEOFPOSMODE,
            fst_offset: 0,
            fst_length: allocation_length,
            fst_bytesalloc: 0,
        };
        // SAFETY: `file` owns a valid descriptor and `allocation` has the exact
        // Darwin `fstore_t` layout required by F_PREALLOCATE.
        let contiguous_result = unsafe {
            libc::fcntl(
                file.as_raw_fd(),
                libc::F_PREALLOCATE,
                std::ptr::addr_of!(allocation),
            )
        };
        if contiguous_result == -1 {
            // Fragmented free space may require Apple's full-allocation fallback without resizing later.
            allocation.fst_flags = libc::F_ALLOCATEALL;
            // SAFETY: Same valid descriptor and initialized `fstore_t`; this is
            // Apple's documented fallback when contiguous allocation is not available.
            let allocated_result = unsafe {
                libc::fcntl(
                    file.as_raw_fd(),
                    libc::F_PREALLOCATE,
                    std::ptr::addr_of!(allocation),
                )
            };
            if allocated_result == -1 {
                // Refuse initialization when the full fixed capacity could not be reserved.
                return Err(ClipboardHistoryFileError::StorageUnavailable(
                    io::Error::last_os_error(),
                ));
            }
        }
    }

    // Only first initialization/recovery establishes length and permissions; ordinary writes do neither.
    file.set_len(CLIPBOARD_STORAGE_FILE_BYTES)
        .map_err(ClipboardHistoryFileError::StorageUnavailable)?;
    file.set_permissions(fs::Permissions::from_mode(0o600))
        .map_err(ClipboardHistoryFileError::StorageUnavailable)
}

/// Validate fixed size/header and recover only the explicitly permitted uninitialized state.
fn prepare_existing_clipboard_file(
    file: &File,
    recover_uninitialized_file: bool,
) -> Result<(), ClipboardHistoryFileError> {
    // Reject incompatible extent before a metadata read can assume its full fixed page exists.
    let file_length = validate_clipboard_file_descriptor(file)?.len();
    if file_length == 0 && recover_uninitialized_file {
        // Recover a first-creation interruption only on the explicit create/prepare path.
        preallocate_clipboard_file(file)?;
        initialize_clipboard_file_header(file)?;
        return Ok(());
    }
    if file_length != CLIPBOARD_STORAGE_FILE_BYTES {
        // Incompatible lengths are never truncated or reinterpreted as a current-format history.
        return Err(ClipboardHistoryFileError::InvalidFormat);
    }

    // Copy validation decisions from an owned metadata page before attempting any recovery write.
    let header = read_clipboard_header(file)?;
    let header_is_valid = clipboard_file_header_is_valid(&header);
    let header_is_uninitialized = header.iter().all(|byte| {
        // Only a completely untouched page is eligible for first-creation recovery.
        *byte == 0
    });
    if header_is_valid {
        // A fully compatible header allows scoped normal operations without mutation.
        return Ok(());
    }
    if recover_uninitialized_file && header_is_uninitialized {
        // An untouched preallocated file can complete its original initialization safely.
        initialize_clipboard_file_header(file)
    } else {
        // Unknown magic/version/capacity remains an explicit error, preserving the original file.
        Err(ClipboardHistoryFileError::InvalidFormat)
    }
}

/// Write the stable version/capacity header before declaring any payload slot.
fn initialize_clipboard_file_header(file: &File) -> Result<(), ClipboardHistoryFileError> {
    // Publish a complete empty versioned metadata page, then flush it before activation returns.
    let mut header = [0u8; METADATA_BYTES];
    header[0..CLIPBOARD_FILE_MAGIC.len()].copy_from_slice(CLIPBOARD_FILE_MAGIC);
    header[8..12].copy_from_slice(&CLIPBOARD_FILE_VERSION.to_le_bytes());
    header[12..16].copy_from_slice(&(MAX_ENTRY_COUNT as u32).to_le_bytes());
    header[16..20].copy_from_slice(&(MAX_ENTRY_BYTES as u32).to_le_bytes());
    write_clipboard_range(file, 0, &header)?;
    file.sync_data()
        .map_err(ClipboardHistoryFileError::StorageUnavailable)
}

/// Reject incompatible protocol, capacity or payload-size declarations.
fn clipboard_file_header_is_valid(header: &[u8]) -> bool {
    // Every encoded capacity agrees exactly with the one supported on-disk protocol.
    header.get(0..8) == Some(CLIPBOARD_FILE_MAGIC.as_slice())
        && header.get(8..12) == Some(CLIPBOARD_FILE_VERSION.to_le_bytes().as_slice())
        && header.get(12..16) == Some((MAX_ENTRY_COUNT as u32).to_le_bytes().as_slice())
        && header.get(16..20) == Some((MAX_ENTRY_BYTES as u32).to_le_bytes().as_slice())
}

/// Decode valid scalar records from one bounded metadata buffer without retaining it.
fn read_declared_slot_metadata(
    file: &File,
) -> Result<Vec<IndexedSlotMetadata>, ClipboardHistoryFileError> {
    let header = read_clipboard_header(file)?;
    let mut declared_slots = Vec::with_capacity(MAX_ENTRY_COUNT);
    for index in 0..MAX_ENTRY_COUNT {
        // The bounded scan decodes scalar metadata without touching any clipboard body.
        let metadata = read_slot_metadata(&header, index);
        if metadata.is_declared() {
            // Only structurally valid scalar records can later request a bounded payload read.
            declared_slots.push(IndexedSlotMetadata { index, metadata });
        }
    }
    Ok(declared_slots)
}

/// Decode one fixed little-endian record from the already-sized metadata region.
fn read_slot_metadata(header: &[u8], slot_index: usize) -> SlotMetadata {
    // Callers enumerate only fixed slot indices within an already-sized complete owned metadata page.
    let offset = slot_metadata_offset(slot_index);
    SlotMetadata {
        id: i64::from_le_bytes(
            header[offset + SLOT_ID_OFFSET..offset + SLOT_ID_OFFSET + 8]
                .try_into()
                .expect("fixed clipboard ID field"),
        ),
        captured_at_ms: i64::from_le_bytes(
            header[offset + SLOT_CAPTURED_AT_OFFSET..offset + SLOT_CAPTURED_AT_OFFSET + 8]
                .try_into()
                .expect("fixed clipboard timestamp field"),
        ),
        recency: u64::from_le_bytes(
            header[offset + SLOT_RECENCY_OFFSET..offset + SLOT_RECENCY_OFFSET + 8]
                .try_into()
                .expect("fixed clipboard recency field"),
        ),
        content_length: u32::from_le_bytes(
            header[offset + SLOT_CONTENT_LENGTH_OFFSET..offset + SLOT_CONTENT_LENGTH_OFFSET + 4]
                .try_into()
                .expect("fixed clipboard length field"),
        ),
        checksum: u32::from_le_bytes(
            header[offset + SLOT_CHECKSUM_OFFSET..offset + SLOT_CHECKSUM_OFFSET + 4]
                .try_into()
                .expect("fixed clipboard checksum field"),
        ),
    }
}

/// Publish one complete scalar record into its known metadata offset.
fn write_slot_metadata(header: &mut [u8], slot_index: usize, metadata: SlotMetadata) {
    // Scalar serialization never changes file shape or publishes any native/process pointer.
    let offset = slot_metadata_offset(slot_index);
    header[offset + SLOT_ID_OFFSET..offset + SLOT_ID_OFFSET + 8]
        .copy_from_slice(&metadata.id.to_le_bytes());
    header[offset + SLOT_CAPTURED_AT_OFFSET..offset + SLOT_CAPTURED_AT_OFFSET + 8]
        .copy_from_slice(&metadata.captured_at_ms.to_le_bytes());
    header[offset + SLOT_RECENCY_OFFSET..offset + SLOT_RECENCY_OFFSET + 8]
        .copy_from_slice(&metadata.recency.to_le_bytes());
    header[offset + SLOT_CONTENT_LENGTH_OFFSET..offset + SLOT_CONTENT_LENGTH_OFFSET + 4]
        .copy_from_slice(&metadata.content_length.to_le_bytes());
    header[offset + SLOT_CHECKSUM_OFFSET..offset + SLOT_CHECKSUM_OFFSET + 4]
        .copy_from_slice(&metadata.checksum.to_le_bytes());
}

/// Calculate a record offset within the fixed metadata page.
fn slot_metadata_offset(slot_index: usize) -> usize {
    // The fixed metadata prefix leaves room for all twenty records within the first page.
    SLOT_METADATA_START + slot_index * SLOT_METADATA_BYTES
}

/// Choose an undeclared fixed slot before eviction is necessary.
fn first_unused_slot(declared_slots: &[IndexedSlotMetadata]) -> Option<IndexedSlotMetadata> {
    // Scan at most twenty positions; undeclared metadata has no payload preservation obligation.
    (0..MAX_ENTRY_COUNT)
        .find(|candidate| {
            // A candidate is vacant only when no declared record owns that physical slot.
            !declared_slots.iter().any(|slot| {
                // The nested scan is also bounded by fixed capacity, independent of body size.
                slot.index == *candidate
            })
        })
        .map(|index| {
            // A vacant slot starts with no previous payload range or identity.
            IndexedSlotMetadata {
                index,
                metadata: SlotMetadata::default(),
            }
        })
}

/// Write/flush a slot prefix and erase stale trailing bytes before metadata publication.
fn write_clipboard_payload(
    file: &File,
    slot_index: usize,
    previous_length: usize,
    content: &[u8],
) -> Result<(), ClipboardHistoryFileError> {
    // Write the borrowed incoming body directly; no second full-body copy is needed for persistence.
    if slot_index >= MAX_ENTRY_COUNT
        || content.is_empty()
        || content.len() > MAX_ENTRY_BYTES
        || previous_length > MAX_ENTRY_BYTES
    {
        // Every offset and obsolete tail remains inside its original fixed-capacity slot.
        return Err(ClipboardHistoryFileError::InvalidFormat);
    }
    let offset = clipboard_slot_offset(slot_index);
    write_clipboard_range(file, offset, content)?;
    if previous_length > content.len() {
        // Erase stale trailing bytes with bounded scratch before the new metadata becomes visible.
        zero_clipboard_range(
            file,
            offset + content.len() as u64,
            previous_length - content.len(),
        )?;
    }
    file.sync_data()
        .map_err(ClipboardHistoryFileError::StorageUnavailable)
}

/// Validate range, CRC and UTF-8 before exposing a transient body to the caller.
fn read_valid_clipboard_slot<T>(
    file: &File,
    metadata: SlotMetadata,
    slot_index: usize,
    read_content: impl FnOnce(&str) -> T,
) -> Result<Option<T>, ClipboardHistoryFileError> {
    if !metadata.is_declared() {
        // Invalid scalar metadata cannot define a body allocation or visible string.
        return Ok(None);
    }
    // The bounded copy owns all bytes before validation; an external file writer cannot alter this borrow.
    let payload = read_clipboard_slot(file, slot_index, metadata.content_length as usize)?;
    if crc32fast::hash(&payload) != metadata.checksum {
        // An interrupted/external write is invisible if its owned snapshot disagrees with the index.
        return Ok(None);
    }
    let Ok(content) = std::str::from_utf8(&payload) else {
        // Even matching checksums cannot authorize invalid UTF-8 at the string/pasteboard boundary.
        return Ok(None);
    };
    // The callback borrows this owned allocation; legal truncation or writes cannot invalidate its string.
    Ok(Some(read_content(content)))
}

/// Match bounded queries without allocating a lowercase body for ASCII or uncased scripts.
fn content_matches_clipboard_query(content: &str, normalized_query: &str) -> bool {
    if normalized_query.is_empty() {
        // An empty foreground query lists the bounded history in recency order.
        return true;
    }
    if normalized_query.is_ascii() {
        // ASCII case matching can inspect byte windows without allocating another full body.
        return content
            .as_bytes()
            .windows(normalized_query.len())
            .any(|window| {
                // Compare only a query-sized borrowed window; no complete body copy is allocated.
                window.eq_ignore_ascii_case(normalized_query.as_bytes())
            });
    }
    if normalized_query.chars().all(|character| {
        // Equal upper/lower expansions identify query characters with no case distinction.
        character.to_lowercase().eq(character.to_uppercase())
    }) {
        // Uncased scripts need no lowercased body copy to preserve matching semantics.
        return content.contains(normalized_query);
    }
    // Cased non-ASCII queries need Unicode folding; this temporary body copy dies within the operation.
    content.to_lowercase().contains(normalized_query)
}

/// Build at most 180 visible characters while collapsing whitespace and excluding controls.
fn preview_clipboard_content(content: &str) -> String {
    let mut preview = String::with_capacity(PREVIEW_CHARACTERS * 4);
    let mut visible_characters = 0usize;
    let mut previous_was_space = false;
    for character in content.chars() {
        // Iterate scalar boundaries so the preview never truncates a UTF-8 codepoint.
        if visible_characters >= PREVIEW_CHARACTERS {
            // Stop before the retained preview exceeds its visible-character budget.
            break;
        }
        if character.is_whitespace() {
            // Collapse runs to one space while avoiding leading whitespace.
            if !previous_was_space && !preview.is_empty() {
                // A separator is needed only between two already-visible text runs.
                preview.push(' ');
                visible_characters += 1;
                previous_was_space = true;
            }
        } else if !character.is_control() {
            // Control characters are omitted, while normal Unicode scalars remain searchable previews.
            preview.push(character);
            visible_characters += 1;
            previous_was_space = false;
        }
    }
    // A collapsed final separator should not consume visible row space.
    preview.truncate(preview.trim_end().len());
    preview
}

/// Read exactly the original metadata page into operation-owned bounded storage.
fn read_clipboard_header(file: &File) -> Result<[u8; METADATA_BYTES], ClipboardHistoryFileError> {
    // An external truncate is reported by extent validation or read_exact_at; no file pages are borrowed.
    validate_clipboard_extent(file)?;
    let mut header = [0u8; METADATA_BYTES];
    file.read_exact_at(&mut header, 0)
        .map_err(ClipboardHistoryFileError::StorageUnavailable)?;
    Ok(header)
}

/// Read one validated payload prefix into an allocation no larger than a single fixed slot.
fn read_clipboard_slot(
    file: &File,
    slot_index: usize,
    length: usize,
) -> Result<Vec<u8>, ClipboardHistoryFileError> {
    if slot_index >= MAX_ENTRY_COUNT || length == 0 || length > MAX_ENTRY_BYTES {
        // Untrusted scalar metadata cannot allocate or read an empty, oversized, or cross-slot body.
        return Err(ClipboardHistoryFileError::InvalidFormat);
    }
    // Positional IO leaves the descriptor cursor unused and makes short reads explicit failures.
    validate_clipboard_extent(file)?;
    let mut payload = vec![0u8; length];
    file.read_exact_at(&mut payload, clipboard_slot_offset(slot_index))
        .map_err(ClipboardHistoryFileError::StorageUnavailable)?;
    Ok(payload)
}

/// Write only a bounded existing-file range; callers own content-before-index sync ordering.
fn write_clipboard_range(
    file: &File,
    offset: u64,
    bytes: &[u8],
) -> Result<(), ClipboardHistoryFileError> {
    if offset.checked_add(bytes.len() as u64).is_none_or(|end| {
        // Even internal callers must stay within the original file's established fixed capacity.
        end > CLIPBOARD_STORAGE_FILE_BYTES
    }) {
        // A malformed offset must never turn a regular update into append or extent growth.
        return Err(ClipboardHistoryFileError::InvalidFormat);
    }
    // A lock serializes cooperating owners; identity/length checks detect observed noncooperating changes.
    validate_clipboard_extent(file)?;
    file.write_all_at(bytes, offset)
        .map_err(ClipboardHistoryFileError::StorageUnavailable)
}

/// Overwrite a bounded range using one fixed 16 KiB zero buffer, without syncing ahead of the caller.
fn zero_clipboard_range(
    file: &File,
    offset: u64,
    length: usize,
) -> Result<(), ClipboardHistoryFileError> {
    // Both clear and stale-tail cleanup reuse this fixed scratch rather than allocating complete histories.
    let zeros = [0u8; METADATA_BYTES];
    let mut written = 0usize;
    while written < length {
        // Each iteration consumes at most one scratch page and advances toward the caller's bounded end.
        let chunk_length = (length - written).min(zeros.len());
        write_clipboard_range(file, offset + written as u64, &zeros[..chunk_length])?;
        written += chunk_length;
    }
    Ok(())
}

/// Publish and sync exactly one scalar record after its payload has already reached the sync boundary.
fn persist_clipboard_slot_metadata(
    file: &File,
    header: &[u8; METADATA_BYTES],
    slot_index: usize,
) -> Result<(), ClipboardHistoryFileError> {
    if slot_index >= MAX_ENTRY_COUNT {
        // No invalid internal slot index can publish beyond the fixed twenty-record metadata table.
        return Err(ClipboardHistoryFileError::InvalidFormat);
    }
    // Preserve every other record and the stable protocol header rather than rewriting an entire page.
    let offset = slot_metadata_offset(slot_index);
    write_clipboard_range(
        file,
        offset as u64,
        &header[offset..offset + SLOT_METADATA_BYTES],
    )?;
    file.sync_data()
        .map_err(ClipboardHistoryFileError::StorageUnavailable)
}

/// Calculate an offset inside the unchanged fixed-slot file format.
fn clipboard_slot_offset(slot_index: usize) -> u64 {
    // Validated slot indices preserve the original 16 KiB alignment and fixed file containment.
    (METADATA_BYTES + slot_index * MAX_ENTRY_BYTES) as u64
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::{FileExt, MetadataExt, OpenOptionsExt, PermissionsExt};

    use tempfile::tempdir;

    use super::{
        CLIPBOARD_STORAGE_FILE_BYTES, ClipboardHistoryFile, ClipboardHistoryFileError,
        MAX_ENTRY_BYTES, MAX_ENTRY_COUNT, MAX_QUERY_RESULTS, METADATA_BYTES, clipboard_slot_offset,
        open_existing_clipboard_file, persist_clipboard_slot_metadata, read_clipboard_header,
        read_clipboard_slot, read_declared_slot_metadata, validate_clipboard_file_path,
        write_clipboard_range, write_slot_metadata,
    };

    #[test]
    /// Verify lazy creation produces exactly one fixed-size application-private file.
    fn creates_one_fixed_preallocated_file_only_under_app_data() {
        // All history operations use disposable synthetic data, never the real application directory.
        let app_data = tempdir().expect("temporary app data");
        let history = ClipboardHistoryFile::new(app_data.path().to_owned());

        history.prepare().expect("prepare clipboard file");

        assert!(history.storage_path().starts_with(app_data.path()));
        assert_eq!(
            fs::metadata(history.storage_path())
                .expect("clipboard file metadata")
                .len(),
            CLIPBOARD_STORAGE_FILE_BYTES
        );
        let files = fs::read_dir(
            history
                .storage_path()
                .parent()
                .expect("clipboard directory"),
        )
        .expect("clipboard directory entries")
        .collect::<Result<Vec<_>, _>>()
        .expect("clipboard directory contents");
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path(), history.storage_path());
    }

    #[test]
    /// Verify real recency, rather than ID age, controls eviction and survives reopening.
    fn persists_twenty_entries_and_evicts_the_least_recently_used_slot() {
        // All history operations use disposable synthetic data, never the real application directory.
        let app_data = tempdir().expect("temporary app data");
        let history = ClipboardHistoryFile::new(app_data.path().to_owned());
        for index in 0..MAX_ENTRY_COUNT {
            // Fill every fixed slot so the next new body must make a genuine eviction decision.
            history
                .record(&format!("entry-{index}"), index as i64)
                .expect("record clipboard entry");
        }
        // Restore the oldest identity, then add a new body to distinguish true LRU from FIFO eviction.
        let oldest_id = history.search("entry-0", 1).expect("find oldest entry")[0].id;
        history
            .with_entry_content(oldest_id, |_| ())
            .expect("touch oldest entry");
        history
            .record("entry-20", 20)
            .expect("record replacement entry");

        assert_eq!(history.item_count().expect("item count"), MAX_ENTRY_COUNT);
        assert!(!history.search("entry-0", 1).expect("search").is_empty());
        let entries = history.search("", MAX_QUERY_RESULTS).expect("search");
        assert!(entries.iter().any(|entry| entry.preview == "entry-0"));
        assert!(!entries.iter().any(|entry| entry.preview == "entry-1"));

        // A new path-only owner must reconstruct the same fixed history from the persisted format.
        drop(history);
        let reopened = ClipboardHistoryFile::new(app_data.path().to_owned());
        assert_eq!(
            reopened
                .search("", MAX_QUERY_RESULTS)
                .expect("reopen clipboard history")
                .len(),
            MAX_ENTRY_COUNT
        );
    }

    #[test]
    /// Keep duplicate bodies in one slot with their original identity.
    fn repeated_content_moves_to_mru_without_consuming_another_slot() {
        // All history operations use disposable synthetic data, never the real application directory.
        let app_data = tempdir().expect("temporary app data");
        let history = ClipboardHistoryFile::new(app_data.path().to_owned());
        history.record("same", 1).expect("first record");
        let original_id = history.search("", 1).expect("first search")[0].id;
        history.record("other", 2).expect("second record");
        history.record("same", 3).expect("repeat record");

        let entries = history.search("", MAX_QUERY_RESULTS).expect("search");
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].id, original_id);
        assert_eq!(entries[0].preview, "same");
    }

    #[test]
    /// Verify clear removes logical visibility and overwrites all application-visible slot bytes.
    fn clear_invalidates_metadata_and_overwrites_every_payload_byte() {
        // All history operations use disposable synthetic data, never the real application directory.
        let app_data = tempdir().expect("temporary app data");
        let history = ClipboardHistoryFile::new(app_data.path().to_owned());
        history
            .record("body that must be cleared", 1)
            .expect("record clipboard entry");

        history.clear().expect("clear clipboard history");

        assert_eq!(history.item_count().expect("item count"), 0);
        // Logical emptiness alone is insufficient: clear must also overwrite application-visible bodies.
        let bytes = fs::read(history.storage_path()).expect("read cleared file");
        assert!(bytes[METADATA_BYTES..].iter().all(|byte| *byte == 0));
    }

    #[test]
    /// Keep rejected oversized captures from creating any history file.
    fn rejects_oversized_content_without_creating_storage() {
        // All history operations use disposable synthetic data, never the real application directory.
        let app_data = tempdir().expect("temporary app data");
        let history = ClipboardHistoryFile::new(app_data.path().to_owned());
        let oversized = "x".repeat(MAX_ENTRY_BYTES + 1);

        assert_eq!(history.record(&oversized, 1).expect("reject entry"), None);
        assert!(!history.storage_path().exists());
    }

    #[test]
    /// Verify ASCII/Unicode search and bounded, whitespace-normalized previews.
    fn search_uses_bounded_previews_and_case_aware_matching() {
        // All history operations use disposable synthetic data, never the real application directory.
        let app_data = tempdir().expect("temporary app data");
        let history = ClipboardHistoryFile::new(app_data.path().to_owned());
        let content = format!("  Build RELEASE\n\n中文\t{}", "界".repeat(300));
        history.record(&content, 1).expect("record content");

        assert_eq!(history.search("release", 1).expect("ASCII search").len(), 1);
        let entry = &history.search("中文", 1).expect("CJK search")[0];
        assert!(entry.preview.starts_with("Build RELEASE 中文 "));
        assert!(entry.preview.chars().count() <= 180);
    }

    #[test]
    /// Treat torn/corrupted payloads as invisible instead of exposing unchecked bytes.
    fn skips_a_slot_when_payload_checksum_no_longer_matches_metadata() {
        // All history operations use disposable synthetic data, never the real application directory.
        let app_data = tempdir().expect("temporary app data");
        let history = ClipboardHistoryFile::new(app_data.path().to_owned());
        history.record("valid", 1).expect("record content");
        // Corrupt only the isolated fixture to verify validation independently of the normal writer.
        let file = open_existing_clipboard_file(history.storage_path()).expect("open history");
        let slot = read_declared_slot_metadata(&file).expect("read metadata")[0];
        let mut payload =
            read_clipboard_slot(&file, slot.index, slot.metadata.content_length as usize)
                .expect("read payload");
        payload[0] ^= 1;
        write_clipboard_range(&file, clipboard_slot_offset(slot.index), &payload)
            .expect("write corrupted payload");
        file.sync_data().expect("sync corrupted payload");
        drop(file);

        assert!(
            history
                .search("", MAX_QUERY_RESULTS)
                .expect("search")
                .is_empty()
        );
    }

    #[test]
    /// CRC validity cannot authorize a malformed UTF-8 body across the native string boundary.
    fn skips_a_slot_when_checksum_matches_invalid_utf8() {
        // All history operations use disposable synthetic data, never the real application directory.
        let app_data = tempdir().expect("temporary app data");
        let history = ClipboardHistoryFile::new(app_data.path().to_owned());
        history.record("x", 1).expect("record content");
        // Corrupt only the isolated fixture to verify validation independently of the normal writer.
        let file = open_existing_clipboard_file(history.storage_path()).expect("open history");
        let slot = read_declared_slot_metadata(&file).expect("read metadata")[0];
        let invalid_utf8 = [0xff];
        write_clipboard_range(&file, clipboard_slot_offset(slot.index), &invalid_utf8)
            .expect("write invalid UTF-8");
        file.sync_data().expect("sync invalid UTF-8");

        // Publish deliberately adversarial metadata while keeping the overall fixed file shape valid.
        let mut header = read_clipboard_header(&file).expect("read metadata");
        write_slot_metadata(
            &mut header,
            slot.index,
            super::SlotMetadata {
                checksum: crc32fast::hash(&invalid_utf8),
                ..slot.metadata
            },
        );
        persist_clipboard_slot_metadata(&file, &header, slot.index).expect("sync metadata");
        drop(file);

        assert!(
            history
                .search("", MAX_QUERY_RESULTS)
                .expect("search")
                .is_empty()
        );
    }

    #[test]
    /// Reject a malicious declared range before allocating or reading any payload.
    fn skips_a_slot_with_an_out_of_bounds_declared_length() {
        // All history operations use disposable synthetic data, never the real application directory.
        let app_data = tempdir().expect("temporary app data");
        let history = ClipboardHistoryFile::new(app_data.path().to_owned());
        history.record("x", 1).expect("record content");
        // Corrupt only the isolated fixture to verify validation independently of the normal writer.
        let file = open_existing_clipboard_file(history.storage_path()).expect("open history");
        let slot = read_declared_slot_metadata(&file).expect("read metadata")[0];
        // Publish deliberately adversarial metadata while keeping the overall fixed file shape valid.
        let mut header = read_clipboard_header(&file).expect("read metadata");
        write_slot_metadata(
            &mut header,
            slot.index,
            super::SlotMetadata {
                content_length: (MAX_ENTRY_BYTES + 1) as u32,
                ..slot.metadata
            },
        );
        persist_clipboard_slot_metadata(&file, &header, slot.index).expect("sync metadata");
        drop(file);

        assert!(
            history
                .search("", MAX_QUERY_RESULTS)
                .expect("search")
                .is_empty()
        );
    }

    /// Empty reads/clear and rejected captures must not create the fixed backing file.
    #[test]
    fn empty_operations_are_lazy_and_empty_bodies_are_rejected() {
        // A missing nested app-data path makes any accidental directory creation observable.
        let root = tempdir().unwrap();
        let app_data = root.path().join("not-created");
        let history = ClipboardHistoryFile::new(app_data.clone());
        assert_eq!(history.item_count().unwrap(), 0);
        assert!(history.search("", MAX_QUERY_RESULTS).unwrap().is_empty());
        history.clear().unwrap();
        assert_eq!(history.record("", 1).unwrap(), None);
        assert!(!app_data.exists());
    }

    /// Unknown extent/version data must be rejected without truncation or header replacement.
    #[test]
    fn rejects_incompatible_file_shape_and_header_without_rewriting_data() {
        // First create an incompatible nonempty shape, which is not a recoverable empty creation.
        let root = tempdir().unwrap();
        let history = ClipboardHistoryFile::new(root.path().to_owned());
        fs::create_dir_all(history.storage_path().parent().unwrap()).unwrap();
        fs::write(history.storage_path(), b"unrelated data").unwrap();
        let before = fs::read(history.storage_path()).unwrap();
        assert!(matches!(
            history.prepare(),
            Err(super::ClipboardHistoryFileError::InvalidFormat)
        ));
        assert_eq!(fs::read(history.storage_path()).unwrap(), before);

        // A fixed-sized file with incompatible version is equally unsafe to reinterpret.
        fs::remove_file(history.storage_path()).unwrap();
        history.prepare().unwrap();
        let mut bytes = fs::read(history.storage_path()).unwrap();
        bytes[8..12].copy_from_slice(&99u32.to_le_bytes());
        fs::write(history.storage_path(), &bytes).unwrap();
        assert!(matches!(
            history.prepare(),
            Err(super::ClipboardHistoryFileError::InvalidFormat)
        ));
        assert_eq!(fs::read(history.storage_path()).unwrap(), bytes);
    }

    /// Metadata invalidation alone makes a clear interrupted before payload overwrite invisible.
    #[test]
    fn interrupted_clear_does_not_republish_still_present_payload() {
        // Simulate the exact clear commit point using only a synthetic fixed history file.
        let root = tempdir().unwrap();
        let history = ClipboardHistoryFile::new(root.path().to_owned());
        history
            .record("payload left after simulated interruption", 1)
            .unwrap();
        let file = open_existing_clipboard_file(history.storage_path()).unwrap();
        let mut header = read_clipboard_header(&file).unwrap();
        header[super::SLOT_METADATA_START
            ..super::SLOT_METADATA_START + MAX_ENTRY_COUNT * super::SLOT_METADATA_BYTES]
            .fill(0);
        write_clipboard_range(&file, 0, &header).unwrap();
        file.sync_data().unwrap();
        drop(file);
        drop(history);

        // Reopening must consult invalidated metadata rather than scanning old payload bytes.
        let reopened = ClipboardHistoryFile::new(root.path().to_owned());
        assert!(reopened.search("", MAX_QUERY_RESULTS).unwrap().is_empty());
        assert!(
            fs::read(reopened.storage_path()).unwrap()[METADATA_BYTES..]
                .iter()
                .any(|byte| {
                    // This proves the fixture models interrupted clear rather than a completed overwrite.
                    *byte != 0
                })
        );
    }

    /// Final-component links cannot redirect clipboard writes to unrelated user data.
    #[test]
    fn rejects_history_file_and_directory_symlinks() {
        use std::os::unix::fs::symlink;

        // The file-link target contains sentinel data that must survive every rejected operation.
        let root = tempdir().unwrap();
        let outside = root.path().join("outside");
        fs::write(&outside, b"preserved sentinel").unwrap();
        let app_data = root.path().join("application");
        let history = ClipboardHistoryFile::new(app_data.clone());
        fs::create_dir_all(history.storage_path().parent().unwrap()).unwrap();
        symlink(&outside, history.storage_path()).unwrap();
        assert!(history.prepare().is_err());
        assert!(history.search("", MAX_QUERY_RESULTS).is_err());
        assert_eq!(fs::read(&outside).unwrap(), b"preserved sentinel");

        // A linked Clipboard directory is also refused before any history payload is created.
        fs::remove_file(history.storage_path()).unwrap();
        fs::remove_dir(history.storage_path().parent().unwrap()).unwrap();
        let outside_directory = root.path().join("outside-directory");
        fs::create_dir(&outside_directory).unwrap();
        symlink(&outside_directory, app_data.join("Clipboard")).unwrap();
        assert!(matches!(
            history.prepare(),
            Err(super::ClipboardHistoryFileError::InvalidFormat)
        ));
        assert_eq!(fs::read_dir(outside_directory).unwrap().count(), 0);
    }

    /// Every entry point must reject an aliased inode before reading bodies or mutating its format.
    fn assert_aliased_history_is_rejected(history: &ClipboardHistoryFile) {
        // Exercise recovery, capture, clear, search, and restore independently against the same alias.
        assert!(matches!(
            history.prepare(),
            Err(ClipboardHistoryFileError::InvalidFormat)
        ));
        assert!(matches!(
            history.record("must not append", 99),
            Err(ClipboardHistoryFileError::InvalidFormat)
        ));
        assert!(matches!(
            history.clear(),
            Err(ClipboardHistoryFileError::InvalidFormat)
        ));
        assert!(matches!(
            history.search("", MAX_QUERY_RESULTS),
            Err(ClipboardHistoryFileError::InvalidFormat)
        ));
        let consumed = std::cell::Cell::new(false);
        assert!(matches!(
            history.with_entry_content(1, |_| {
                // An unrelated file must never cross the callback boundary, even if its format is compatible.
                consumed.set(true);
            }),
            Err(ClipboardHistoryFileError::InvalidFormat)
        ));
        assert!(!consumed.get());
    }

    /// A zero-length hard-link target cannot be mistaken for an interrupted first creation.
    #[test]
    fn rejects_empty_hardlink_without_preallocation_header_write_or_permission_change() {
        // The target and alias are actual same-volume hard links entirely under an isolated root.
        let root = tempdir().unwrap();
        let victim = root.path().join("empty-victim");
        fs::write(&victim, []).unwrap();
        fs::set_permissions(&victim, fs::Permissions::from_mode(0o640)).unwrap();
        let history = ClipboardHistoryFile::new(root.path().join("application"));
        fs::create_dir_all(history.storage_path().parent().unwrap()).unwrap();
        fs::hard_link(&victim, history.storage_path()).unwrap();
        let before = fs::metadata(&victim).unwrap();

        // Recovery used to enlarge this external inode and overwrite its header despite O_NOFOLLOW.
        assert_aliased_history_is_rejected(&history);
        let after = fs::metadata(&victim).unwrap();
        assert!(fs::read(&victim).unwrap().is_empty());
        assert_eq!(after.len(), 0);
        assert_eq!(after.blocks(), before.blocks());
        assert_eq!(after.mode(), before.mode());
        assert_eq!(
            (after.dev(), after.ino(), after.nlink()),
            (before.dev(), before.ino(), 2)
        );
        assert_eq!(
            fs::metadata(history.storage_path()).unwrap().ino(),
            before.ino()
        );
    }

    /// Compatible history is still another file's data when more than one directory entry names it.
    #[test]
    fn rejects_compatible_history_hardlink_without_append_clear_or_recency_mutation() {
        // A valid source exercises paths that would otherwise pass length, magic, version, and CRC checks.
        let root = tempdir().unwrap();
        let original = ClipboardHistoryFile::new(root.path().join("original"));
        original.record("preserved synthetic history", 7).unwrap();
        let alias = ClipboardHistoryFile::new(root.path().join("alias"));
        fs::create_dir_all(alias.storage_path().parent().unwrap()).unwrap();
        fs::hard_link(original.storage_path(), alias.storage_path()).unwrap();
        let before = fs::read(original.storage_path()).unwrap();
        let metadata_before = fs::metadata(original.storage_path()).unwrap();

        // Both names lose the sole-link invariant; neither can authorize a mutation of the shared inode.
        assert_aliased_history_is_rejected(&alias);
        assert_aliased_history_is_rejected(&original);
        assert_eq!(fs::read(original.storage_path()).unwrap(), before);
        assert_eq!(fs::read(alias.storage_path()).unwrap(), before);
        let after = fs::metadata(original.storage_path()).unwrap();
        assert_eq!(
            (after.len(), after.mode(), after.nlink()),
            (metadata_before.len(), metadata_before.mode(), 2)
        );

        // Removing only the test-created alias restores normal access to the unchanged original history.
        fs::remove_file(alias.storage_path()).unwrap();
        assert_eq!(
            original.search("", MAX_QUERY_RESULTS).unwrap()[0].preview,
            "preserved synthetic history"
        );
    }

    /// The original sole-link zero-length crash-recovery case remains supported without a format migration.
    #[test]
    fn recovers_owned_zero_length_creation_and_preserves_fixed_private_format() {
        // Simulate a process ending after exclusive file creation and before the initial allocation.
        let root = tempdir().unwrap();
        let history = ClipboardHistoryFile::new(root.path().to_owned());
        fs::create_dir_all(history.storage_path().parent().unwrap()).unwrap();
        drop(
            fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(history.storage_path())
                .unwrap(),
        );
        assert_eq!(fs::metadata(history.storage_path()).unwrap().len(), 0);

        // Explicit activation repairs only this valid uninitialized inode, then ordinary writes use it.
        history.prepare().unwrap();
        history.record("recovered", 1).unwrap();
        assert_eq!(
            history.search("", MAX_QUERY_RESULTS).unwrap()[0].preview,
            "recovered"
        );
        let file = fs::metadata(history.storage_path()).unwrap();
        let directory = fs::metadata(history.storage_path().parent().unwrap()).unwrap();
        assert_eq!(
            (file.len(), file.mode() & 0o777, file.nlink()),
            (CLIPBOARD_STORAGE_FILE_BYTES, 0o600, 1)
        );
        assert_eq!(directory.mode() & 0o777, 0o700);
        let bytes = fs::read(history.storage_path()).unwrap();
        assert_eq!(&bytes[..8], b"OMNICLP1");
        assert_eq!(&bytes[8..20], &[1, 0, 0, 0, 20, 0, 0, 0, 0, 0, 4, 0]);
    }

    /// Different independently opened handles must fail promptly while an operation owns the lease.
    fn assert_operation_is_busy<T>(result: Result<T, ClipboardHistoryFileError>) {
        match result {
            Err(ClipboardHistoryFileError::StorageUnavailable(error)) => {
                // Contention is a recoverable nonblocking IO failure, never a waiting native callback.
                assert_eq!(error.kind(), std::io::ErrorKind::WouldBlock);
            }
            Err(error) => {
                // Format/not-found errors would conceal a missing or incorrectly scoped lease.
                panic!("expected clipboard operation contention, received {error}");
            }
            Ok(_) => {
                // A successful competing operation would invalidate serialization of the outer callback.
                panic!("competing clipboard operation unexpectedly acquired the lease");
            }
        }
    }

    /// Reentrant operations cannot clear or replace bytes while the outer restore callback is executing.
    #[test]
    fn rejects_same_owner_and_cross_instance_reentry_then_releases_the_lease() {
        // Two safe owners resolve to the same inode; no process-global path cache is needed for exclusion.
        let root = tempdir().unwrap();
        let history = ClipboardHistoryFile::new(root.path().to_owned());
        let second = ClipboardHistoryFile::new(root.path().to_owned());
        history.record("stable callback body", 1).unwrap();
        let id = history.search("", 1).unwrap()[0].id;
        history
            .with_entry_content(id, |content| {
                // Native callbacks may reenter either owner, including read paths and explicit activation.
                assert_operation_is_busy(history.clear());
                assert_operation_is_busy(history.record("nested", 2));
                assert_operation_is_busy(second.prepare());
                assert_operation_is_busy(second.search("", 1));
                assert_operation_is_busy(second.with_entry_content(id, |_| {
                    // A nested restore cannot invoke its consumer while another callback owns the lease.
                    panic!("nested clipboard consumer must not execute");
                }));
                assert_eq!(content, "stable callback body");
            })
            .unwrap();

        // Closing the operation descriptor releases the lease for the next independent operation.
        second.record("next operation", 3).unwrap();
        assert_eq!(history.search("", MAX_QUERY_RESULTS).unwrap().len(), 2);
    }

    /// Operation exclusion applies to safe cross-thread users as well as same-thread native reentry.
    #[test]
    fn rejects_cross_thread_competing_writer_without_retaining_a_background_owner() {
        // The thread exists only inside this synthetic callback and joins before the callback returns.
        let root = tempdir().unwrap();
        let history = ClipboardHistoryFile::new(root.path().to_owned());
        history.record("thread-safe owned snapshot", 1).unwrap();
        let id = history.search("", 1).unwrap()[0].id;
        history
            .with_entry_content(id, |content| {
                // A separate owner and descriptor cannot bypass the operation-scoped exclusive lease.
                let competing = ClipboardHistoryFile::new(root.path().to_owned());
                let result = std::thread::spawn(move || {
                    // The competing thread performs one bounded attempt and exits without waiting for release.
                    competing.record("competing body", 2)
                })
                .join()
                .unwrap();
                assert_operation_is_busy(result);
                assert_eq!(content, "thread-safe owned snapshot");
            })
            .unwrap();
        assert_eq!(history.search("", MAX_QUERY_RESULTS).unwrap().len(), 1);
    }

    /// Unwinding a callback releases its transient operation lease without retaining a poisoned owner.
    #[test]
    fn callback_unwind_releases_descriptor_and_allows_a_later_operation() {
        // A pure Rust consumer panic is caught outside the operation, without crossing a native ABI.
        let root = tempdir().unwrap();
        let history = ClipboardHistoryFile::new(root.path().to_owned());
        history.record("before unwind", 1).unwrap();
        let id = history.search("", 1).unwrap()[0].id;
        let result = std::panic::catch_unwind(|| {
            // Descriptor and owned body both unwind before the caller attempts another file operation.
            let _ = history.with_entry_content(id, |_| {
                // Simulate an arbitrary safe consumer ending abruptly while its owned snapshot is live.
                panic!("synthetic consumer unwind");
            });
        });
        assert!(result.is_err());
        history.record("after unwind", 2).unwrap();
        assert_eq!(history.search("", MAX_QUERY_RESULTS).unwrap().len(), 2);
    }

    /// Advisory locks cannot prevent external truncation, so the callback must borrow an owned snapshot.
    #[test]
    fn external_truncate_preserves_callback_string_and_fails_promotion_without_regrowing_file() {
        // A real safe File handle deliberately ignores the cooperating storage lock inside the consumer.
        let root = tempdir().unwrap();
        let history = ClipboardHistoryFile::new(root.path().to_owned());
        let body = "synthetic UTF-8 snapshot: 蓝色 🧪";
        history.record(body, 1).unwrap();
        let id = history.search("", 1).unwrap()[0].id;
        let consumed = std::cell::Cell::new(false);
        let result = history.with_entry_content(id, |content| {
            // Truncation would invalidate a named mmap; it cannot affect this already-owned UTF-8 buffer.
            fs::OpenOptions::new()
                .write(true)
                .open(history.storage_path())
                .unwrap()
                .set_len(0)
                .unwrap();
            assert_eq!(content, body);
            assert!(std::str::from_utf8(content.as_bytes()).is_ok());
            consumed.set(true);
        });
        assert!(consumed.get());
        assert!(matches!(
            result,
            Err(ClipboardHistoryFileError::InvalidFormat)
        ));
        assert_eq!(fs::metadata(history.storage_path()).unwrap().len(), 0);
    }

    /// An external writer can forge CRC metadata but cannot mutate the callback's valid owned string.
    #[test]
    fn external_matching_crc_invalid_utf8_never_invalidates_callback_or_republishes_stale_entry() {
        // The fixture has one known slot; its new invalid UTF-8 and matching CRC are written with safe IO.
        let root = tempdir().unwrap();
        let history = ClipboardHistoryFile::new(root.path().to_owned());
        history.record("x", 1).unwrap();
        let id = history.search("", 1).unwrap()[0].id;
        let result = history.with_entry_content(id, |content| {
            // A noncooperating safe writer bypasses the advisory lease, as an ordinary same-user process can.
            let external = fs::OpenOptions::new()
                .write(true)
                .open(history.storage_path())
                .unwrap();
            external
                .write_all_at(&[0xff], clipboard_slot_offset(0))
                .unwrap();
            external
                .write_all_at(
                    &crc32fast::hash(&[0xff]).to_le_bytes(),
                    (super::SLOT_METADATA_START + super::SLOT_CHECKSUM_OFFSET) as u64,
                )
                .unwrap();
            external.sync_data().unwrap();
            assert_eq!(content, "x");
            assert_eq!(content.as_bytes(), b"x");
        });

        // The changed metadata rejects promotion, and later reads independently reject the forged UTF-8.
        assert!(matches!(
            result,
            Err(ClipboardHistoryFileError::EntryNotFound)
        ));
        assert!(history.search("", MAX_QUERY_RESULTS).unwrap().is_empty());
        assert_eq!(
            fs::metadata(history.storage_path()).unwrap().len(),
            CLIPBOARD_STORAGE_FILE_BYTES
        );
    }

    /// A callback-created alias must be rejected again at the post-consumption metadata boundary.
    #[test]
    fn late_hardlink_during_callback_prevents_recency_publication() {
        // Only the callback adds the alias, after all initial open/format checks have already passed.
        let root = tempdir().unwrap();
        let history = ClipboardHistoryFile::new(root.path().to_owned());
        history.record("late alias snapshot", 1).unwrap();
        let id = history.search("", 1).unwrap()[0].id;
        let before = fs::read(history.storage_path()).unwrap();
        let alias = root.path().join("late-alias");
        let result = history.with_entry_content(id, |content| {
            // Link creation does not change the owned buffer; the subsequent mutation gate sees nlink=2.
            fs::hard_link(history.storage_path(), &alias).unwrap();
            assert_eq!(content, "late alias snapshot");
        });
        assert!(matches!(
            result,
            Err(ClipboardHistoryFileError::InvalidFormat)
        ));
        assert_eq!(fs::read(&alias).unwrap(), before);
        assert_eq!(fs::read(history.storage_path()).unwrap(), before);
    }

    /// A private pathname replaced during a callback must never adopt or overwrite the replacement inode.
    #[test]
    fn callback_path_replacement_preserves_both_inodes_and_rejects_publication() {
        // Rename keeps the original inode single-linked, so an inode/path comparison is necessary.
        let root = tempdir().unwrap();
        let history = ClipboardHistoryFile::new(root.path().to_owned());
        history.record("original inode", 1).unwrap();
        let id = history.search("", 1).unwrap()[0].id;
        let before = fs::read(history.storage_path()).unwrap();
        let moved = root.path().join("moved-original");
        let result = history.with_entry_content(id, |content| {
            // The consumer may use safe filesystem APIs without compromising the validity of its borrow.
            fs::rename(history.storage_path(), &moved).unwrap();
            fs::write(history.storage_path(), b"replacement sentinel").unwrap();
            assert_eq!(content, "original inode");
        });
        assert!(matches!(
            result,
            Err(ClipboardHistoryFileError::InvalidFormat)
        ));
        assert_eq!(fs::read(&moved).unwrap(), before);
        assert_eq!(
            fs::read(history.storage_path()).unwrap(),
            b"replacement sentinel"
        );
    }

    /// Existing-file operations must reject directory symlinks even when they expose compatible history.
    #[test]
    fn existing_read_and_clear_reject_directory_symlink_without_mutating_its_target() {
        // The target is a real compatible sole-link history, so final-file checks alone would accept it.
        let root = tempdir().unwrap();
        let target = ClipboardHistoryFile::new(root.path().join("outside"));
        target.record("private target sentinel", 1).unwrap();
        let before = fs::read(target.storage_path()).unwrap();
        let app_data = root.path().join("redirected");
        fs::create_dir(&app_data).unwrap();
        std::os::unix::fs::symlink(
            target.storage_path().parent().unwrap(),
            app_data.join("Clipboard"),
        )
        .unwrap();
        let redirected = ClipboardHistoryFile::new(app_data);

        // Read and clear cannot rely on an earlier prepare call to have validated the private directory.
        assert!(matches!(
            redirected.search("", 1),
            Err(ClipboardHistoryFileError::InvalidFormat)
        ));
        assert!(matches!(
            redirected.clear(),
            Err(ClipboardHistoryFileError::InvalidFormat)
        ));
        assert!(matches!(
            redirected.prepare(),
            Err(ClipboardHistoryFileError::InvalidFormat)
        ));
        assert_eq!(fs::read(target.storage_path()).unwrap(), before);
    }

    /// Owned reads and writes retain the same explicit single-slot and fixed-file bounds as the format.
    #[test]
    fn rejects_oversized_reads_and_out_of_range_writes_without_changing_fixed_extent() {
        // The exact maximum body succeeds; malformed internal ranges remain rejected before allocation/IO.
        let root = tempdir().unwrap();
        let history = ClipboardHistoryFile::new(root.path().to_owned());
        let maximum = "x".repeat(MAX_ENTRY_BYTES);
        history.record(&maximum, 1).unwrap();
        let id = history.search("", 1).unwrap()[0].id;
        assert_eq!(
            history.with_entry_content(id, str::len).unwrap(),
            MAX_ENTRY_BYTES
        );
        let file = open_existing_clipboard_file(history.storage_path()).unwrap();
        assert!(matches!(
            read_clipboard_slot(&file, 0, MAX_ENTRY_BYTES + 1),
            Err(ClipboardHistoryFileError::InvalidFormat)
        ));
        assert!(matches!(
            read_clipboard_slot(&file, MAX_ENTRY_COUNT, 1),
            Err(ClipboardHistoryFileError::InvalidFormat)
        ));
        assert!(matches!(
            write_clipboard_range(&file, CLIPBOARD_STORAGE_FILE_BYTES, &[1]),
            Err(ClipboardHistoryFileError::InvalidFormat)
        ));
        assert!(matches!(
            write_clipboard_range(&file, u64::MAX, &[1]),
            Err(ClipboardHistoryFileError::InvalidFormat)
        ));
        validate_clipboard_file_path(&file, history.storage_path()).unwrap();
        assert_eq!(file.metadata().unwrap().len(), CLIPBOARD_STORAGE_FILE_BYTES);
    }
}
