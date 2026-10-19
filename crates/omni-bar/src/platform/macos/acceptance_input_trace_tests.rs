//! Pure scalar-format and synthetic-file regressions; no native application, input, capture or TCC.

use super::*;
use std::os::unix::fs::{PermissionsExt, symlink};

/// The export contract uses an explicitly private directory, independent of the process umask.
fn private_directory() -> tempfile::TempDir {
    tempfile::Builder::new()
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir()
        .unwrap()
}

/// Public Copy records are sufficient test inputs; no fake plugin Snapshot constructor is exposed.
fn evidence(records: &[Record]) -> Evidence<'_> {
    Evidence {
        records,
        overflowed: false,
        invalid_scope: false,
        aborted_scope: false,
        armed_at: u64::MAX,
        numer: 125,
        denom: 3,
        initial: Document {
            generation: u64::MAX,
            utf8_bytes: 1_048_576,
            utf16_units: 1_048_576,
        },
    }
}

/// Parse only an entirely newline-terminated artifact; a partial final JSON object is not a complete footer.
fn completed(bytes: &[u8]) -> bool {
    if !bytes.ends_with(b"\n") {
        // A footer write failing at its final byte must remain incomplete even if its JSON object is otherwise valid.
        return false;
    }
    let Some(line) = bytes.split(|byte| *byte == b'\n').rev().nth(1) else {
        // Empty files contain neither a schema header nor completion authority.
        return false;
    };
    let Ok(value) = serde_json::from_slice::<Value>(line) else {
        // Truncated/corrupt JSON never supplies a successful complete record.
        return false;
    };
    value["kind"] == "complete" && value["export_complete"] == true
}

/// Wide clocks/identities survive exactly, optional fields stay null, and output contains no body/path/error.
#[test]
fn stable_scalar_schema_preserves_wide_values_and_null_fields() {
    let record = Record {
        serial: u64::MAX,
        kind: Kind::CanonicalAccepted,
        mach_ticks: u64::MAX,
        nsevent_timestamp_bits: Some(u64::MAX),
        document: Some(evidence(&[]).initial),
        outcome: Outcome::NotApplicable,
        ..Record::default()
    };
    let encoded = encode(evidence(&[record])).unwrap();
    let lines = encoded
        .prefix
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .map(|line| serde_json::from_slice::<Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(lines[0]["schema"], SCHEMA);
    assert_eq!(lines[0]["schema_version"], 1);
    assert_eq!(lines[0]["armed_at_mach_ticks"], u64::MAX.to_string());
    assert_eq!(lines[0]["clock_domain"], "mach_absolute_ticks");
    assert_eq!(lines[1]["mach_ticks"], u64::MAX.to_string());
    assert_eq!(lines[1]["nsevent_timestamp_bits"], u64::MAX.to_string());
    assert_eq!(lines[1]["document"]["generation"], u64::MAX.to_string());
    assert_eq!(lines[1]["document"]["utf8_bytes"], "1048576");
    for field in [
        "event_serial",
        "selector_serial",
        "composition_serial",
        "selector",
        "rejection",
        "native_marked",
    ] {
        // Absence remains distinguishable from a zero serial, false marking or an invented selector label.
        assert!(lines[1][field].is_null(), "{field}");
    }
    let footer: Value = serde_json::from_slice(&encoded.footer).unwrap();
    assert_eq!(footer["trace_valid"], true);
    assert_eq!(footer["latency_acceptance"], false);
    assert_eq!(footer["footer_durability_certified"], false);
}

/// Every current enum spelling is an explicit protocol contract, not an unstable Debug representation.
#[test]
fn exhaustive_labels_and_invalidity_remain_explicit() {
    assert_eq!(
        [
            Kind::Empty,
            Kind::KeyDownBegin,
            Kind::KeyDownEnd,
            Kind::SelectorBegin,
            Kind::SelectorEnd,
            Kind::CanonicalAccepted,
            Kind::CanonicalRejected
        ]
        .map(kind_label),
        [
            "empty",
            "key_down_begin",
            "key_down_end",
            "selector_begin",
            "selector_end",
            "canonical_accepted",
            "canonical_rejected"
        ]
    );
    assert_eq!(
        [
            Selector::InsertText,
            Selector::SetMarkedText,
            Selector::UnmarkText
        ]
        .map(selector_label),
        ["insert_text", "set_marked_text", "unmark_text"]
    );
    assert_eq!(
        [
            Outcome::NotApplicable,
            Outcome::Nested,
            Outcome::Provisional,
            Outcome::CommittedChanged,
            Outcome::CommittedExisting,
            Outcome::NoCommit,
            Outcome::Rejected
        ]
        .map(outcome_label),
        [
            "not_applicable",
            "nested",
            "provisional",
            "committed_changed",
            "committed_existing",
            "no_commit",
            "rejected"
        ]
    );
    assert_eq!(
        [
            Rejection::Closing,
            Rejection::CoordinateOverflow,
            Rejection::DocumentUnavailable,
            Rejection::CanonicalError
        ]
        .map(rejection_label),
        [
            "closing",
            "coordinate_overflow",
            "document_unavailable",
            "canonical_error"
        ]
    );
    let record = Record {
        kind: Kind::CanonicalRejected,
        ..Record::default()
    };
    let records = [record];
    let mut invalid = evidence(&records);
    invalid.overflowed = true;
    invalid.invalid_scope = true;
    invalid.aborted_scope = true;
    let encoded = encode(invalid).unwrap();
    assert!(!encoded.valid);
    let header: Value =
        serde_json::from_slice(encoded.prefix.split(|byte| *byte == b'\n').next().unwrap())
            .unwrap();
    assert_eq!(header["overflowed"], true);
    assert_eq!(header["invalid_scope"], true);
    assert_eq!(header["aborted_scope"], true);
    assert_eq!(
        serde_json::from_slice::<Value>(&encoded.footer).unwrap()["trace_valid"],
        false
    );
}

/// The largest legal run fits the byte ceiling; malformed populated slices fail rather than truncate.
#[test]
fn capacity_and_byte_budgets_bound_the_entire_artifact() {
    let record = Record {
        serial: u64::MAX,
        kind: Kind::SelectorEnd,
        mach_ticks: u64::MAX,
        event_serial: Some(u64::MAX),
        selector_serial: Some(u64::MAX),
        composition_serial: Some(u64::MAX),
        selector: Some(Selector::InsertText),
        nsevent_timestamp_bits: Some(u64::MAX),
        document: Some(evidence(&[]).initial),
        removed_utf16_units: usize::MAX,
        inserted_utf16_units: usize::MAX,
        native_marked: Some(false),
        outcome: Outcome::Rejected,
        rejection: Some(Rejection::CanonicalError),
        accepted_count: u32::MAX,
        rejected_count: u32::MAX,
    };
    let records = vec![record; CAPACITY];
    let encoded = encode(evidence(&records)).unwrap();
    assert!(encoded.prefix.len() + encoded.footer.len() <= MAX_ARTIFACT_BYTES);
    assert_eq!(
        encoded.prefix.iter().filter(|byte| **byte == b'\n').count(),
        CAPACITY + 1
    );
    assert!(matches!(
        encode(evidence(&vec![record; CAPACITY + 1])),
        Err(ExportError::InvalidShape)
    ));
    assert!(matches!(
        encode(evidence(&[Record::default()])),
        Err(ExportError::InvalidShape)
    ));
    let mut bytes = Vec::new();
    assert!(
        append_line(
            &mut bytes,
            &json!("x".repeat(MAX_LINE_BYTES)),
            MAX_ARTIFACT_BYTES
        )
        .is_err()
    );
    assert!(bytes.is_empty());
}

/// A finite failing writer models prefix, pre-footer and final-newline failures without native/filesystem tricks.
struct FailingWriter {
    bytes: Vec<u8>,
    limit: usize,
}
impl Write for FailingWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self.bytes.len() == self.limit {
            // A precise failure boundary tests that the footer is never announced through a partial last write.
            return Err(io::Error::other("synthetic output failure"));
        }
        let count = bytes.len().min(self.limit - self.bytes.len());
        self.bytes.extend_from_slice(&bytes[..count]);
        Ok(count)
    }
    fn flush(&mut self) -> io::Result<()> {
        // This in-memory writer has no delayed IO; failure injection belongs to explicit write boundaries.
        Ok(())
    }
}

/// No IO/identity failure can leave an accepted newline-terminated completion footer.
#[test]
fn incomplete_writes_and_pre_footer_failure_never_complete() {
    let encoded = encode(evidence(&[])).unwrap();
    for limit in [
        0,
        encoded.prefix.len() / 2,
        encoded.prefix.len(),
        encoded.prefix.len() + encoded.footer.len() - 1,
    ] {
        // Each boundary is finite and includes the dangerous case of a complete JSON footer missing only newline.
        let mut writer = FailingWriter {
            bytes: Vec::new(),
            limit,
        };
        assert_eq!(
            write_artifact(&mut writer, &encoded, |_| Ok(())),
            Err(ExportError::Storage)
        );
        assert!(!completed(&writer.bytes));
    }
    let mut writer = Vec::new();
    assert_eq!(
        write_artifact(&mut writer, &encoded, |_| Err(ExportError::RootChanged)),
        Err(ExportError::RootChanged)
    );
    assert_eq!(writer, encoded.prefix);
    assert!(!completed(&writer));
    write_artifact(&mut Vec::new(), &encoded, |_| Ok(())).unwrap();
}

/// Exclusive fixed-name creation preserves preexisting file/hardlink/symlink nodes and their victim bytes.
#[test]
fn existing_aliases_are_never_overwritten_or_removed() {
    let root_directory = private_directory();
    let external = tempfile::tempdir().unwrap();
    let victim = external.path().join("victim");
    std::fs::write(&victim, b"unchanged synthetic victim").unwrap();
    std::fs::hard_link(
        &victim,
        root_directory.path().join("source-input-trace-0001.jsonl"),
    )
    .unwrap();
    symlink(
        &victim,
        root_directory.path().join("source-input-trace-0002.jsonl"),
    )
    .unwrap();
    std::fs::write(
        root_directory.path().join("source-input-trace-0003.jsonl"),
        b"existing artifact",
    )
    .unwrap();
    let root = ExportRoot::open(root_directory.path()).unwrap();
    root.publish(&encode(evidence(&[])).unwrap()).unwrap();
    assert_eq!(
        std::fs::read(&victim).unwrap(),
        b"unchanged synthetic victim"
    );
    assert!(
        root_directory
            .path()
            .join("source-input-trace-0001.jsonl")
            .exists()
    );
    assert!(
        std::fs::symlink_metadata(root_directory.path().join("source-input-trace-0002.jsonl"))
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(
        std::fs::read(root_directory.path().join("source-input-trace-0003.jsonl")).unwrap(),
        b"existing artifact"
    );
    assert!(completed(
        &std::fs::read(root_directory.path().join("source-input-trace-0004.jsonl")).unwrap()
    ));
}

/// A retained descriptor does not authorize publication after its declared root pathname is replaced.
#[test]
fn replaced_or_symlink_roots_cannot_receive_exports() {
    let parent = tempfile::tempdir().unwrap();
    let path = parent.path().join("root");
    std::fs::create_dir(&path).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
    let root = ExportRoot::open(&path).unwrap();
    let moved = parent.path().join("moved");
    std::fs::rename(&path, &moved).unwrap();
    std::fs::create_dir(&path).unwrap();
    assert_eq!(
        root.publish(&encode(evidence(&[])).unwrap()),
        Err(ExportError::RootChanged)
    );
    assert_eq!(std::fs::read_dir(&path).unwrap().count(), 0);
    assert_eq!(std::fs::read_dir(&moved).unwrap().count(), 0);
    let link = parent.path().join("link");
    symlink(&moved, &link).unwrap();
    assert!(matches!(
        ExportRoot::open(&link),
        Err(InstallError::RootUnavailable)
    ));
}

/// Publication cannot certify a replaced output pathname or a newly aliased owned output inode.
#[test]
fn output_identity_and_link_count_are_checked_before_completion() {
    let directory = private_directory();
    let external = tempfile::tempdir().unwrap();
    let victim = external.path().join("victim");
    std::fs::write(&victim, b"unchanged synthetic victim").unwrap();
    let root = ExportRoot::open(directory.path()).unwrap();
    let (file, name) = root.create_exclusive().unwrap();
    let original = directory.path().join(name.to_str().unwrap());
    std::fs::remove_file(&original).unwrap();
    symlink(&victim, &original).unwrap();
    assert_eq!(
        root.check_output(&file, &name),
        Err(ExportError::RootChanged)
    );
    assert_eq!(
        std::fs::read(&victim).unwrap(),
        b"unchanged synthetic victim"
    );
    let (file, name) = root.create_exclusive().unwrap();
    let original = directory.path().join(name.to_str().unwrap());
    std::fs::hard_link(&original, external.path().join("new-alias")).unwrap();
    assert_eq!(
        root.check_output(&file, &name),
        Err(ExportError::RootChanged)
    );
}

/// Exhaustion preserves every existing candidate and never wraps to truncate an earlier artifact.
#[test]
fn fixed_filename_capacity_fails_without_overwriting_old_artifacts() {
    let directory = private_directory();
    for index in 1..=MAX_EXPORTS {
        // The bounded synthetic collision cohort exercises the actual exclusive-open loop.
        std::fs::write(
            directory
                .path()
                .join(format!("source-input-trace-{index:04}.jsonl")),
            b"preserved",
        )
        .unwrap();
    }
    let root = ExportRoot::open(directory.path()).unwrap();
    assert_eq!(
        root.publish(&encode(evidence(&[])).unwrap()),
        Err(ExportError::Exhausted)
    );
    assert_eq!(
        std::fs::read_dir(directory.path()).unwrap().count(),
        usize::from(MAX_EXPORTS)
    );
    for entry in std::fs::read_dir(directory.path()).unwrap() {
        // Every preexisting file remains byte-identical after all names are exhausted.
        assert_eq!(std::fs::read(entry.unwrap().path()).unwrap(), b"preserved");
    }
}
