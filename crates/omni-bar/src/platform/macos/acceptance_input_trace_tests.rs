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
    // Wide values are external JSON-format inputs here; this fixture does not establish a real native input callback.
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

/// Invalid recordings remain inspectable without claiming valid input or latency evidence.
#[test]
fn invalid_recordings_remain_invalid_in_exported_footer() {
    // Invalid recordings stay exportable for inspection, but neither header nor footer may label them valid.
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
    // Exercise maximum-width scalar spellings together, rather than inferring total budget from a small average record.
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
    // Too many populated records or an unused Empty entry fail as malformed evidence without silent truncation.
    assert!(matches!(
        encode(evidence(&vec![record; CAPACITY + 1])),
        Err(ExportError::InvalidShape)
    ));
    assert!(matches!(
        encode(evidence(&[Record::default()])),
        Err(ExportError::InvalidShape)
    ));
    // Oversized single-line serialization must reject before extending the retained output bytes.
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

/// Exclusive fixed-name creation preserves preexisting file/hardlink/symlink nodes and their victim bytes.
#[test]
fn existing_aliases_are_never_overwritten_or_removed() {
    // Both temporary roots are test-owned; the victim deliberately lives outside export authority, never in the user vault.
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
    // Three actual existing aliases force exclusive creation to choose the fourth fixed name while preserving victims.
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
    // Retain the real directory descriptor, then replace only this test's owned pathname to exercise publication identity.
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
    // A symlink to the retained original directory is still not the promised real final-component authority.
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
    // The open output fd remains real and live while its owned pathname changes to an external synthetic victim link.
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
    // Newly introducing a hardlink to another exclusively created output also prevents completion certification.
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
