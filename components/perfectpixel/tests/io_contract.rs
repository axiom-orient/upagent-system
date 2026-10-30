use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use perfectpixel::{AtomicDirectoryEntry, AtomicDirectoryWriter, AtomicFileWriter, PpError};

use std::os::unix::fs::symlink;

const HELLO_SHA256: [u8; 32] = [
    0x2c, 0xf2, 0x4d, 0xba, 0x5f, 0xb0, 0xa3, 0x0e, 0x26, 0xe8, 0x3b, 0x2a, 0xc5, 0xb9, 0xe2, 0x9e,
    0x1b, 0x16, 0x1e, 0x5c, 0x1f, 0xa7, 0x42, 0x5e, 0x73, 0x04, 0x33, 0x62, 0x93, 0x8b, 0x98, 0x24,
];

#[test]
fn atomic_writer_does_not_overwrite_neighbor_temp_files() {
    let dir = unique_temp_dir("perfectpixel-atomic");
    fs::create_dir_all(&dir).expect("temp dir");
    let target = dir.join("asset.bin");
    let existing_neighbor = dir.join("asset.bin.tmp");
    let collision = dir.join(format!(".asset.bin.tmp.{}.0", std::process::id()));

    fs::write(&existing_neighbor, b"existing").expect("existing temp");
    fs::write(&collision, b"collision").expect("collision temp");

    AtomicFileWriter::write_bytes(&target, b"new bytes").expect("atomic write");

    assert_eq!(fs::read(&target).expect("target"), b"new bytes");
    assert_eq!(
        fs::read(&existing_neighbor).expect("existing temp"),
        b"existing"
    );
    assert_eq!(fs::read(&collision).expect("collision temp"), b"collision");

    let _ = fs::remove_dir_all(dir);
}

#[test]
fn atomic_file_writer_rejects_a_symlinked_destination_parent() {
    let dir = unique_temp_dir("perfectpixel-atomic-symlink-parent");
    let real_parent = dir.join("real-parent");
    let linked_parent = dir.join("linked-parent");
    fs::create_dir_all(&real_parent).expect("real parent");
    symlink(&real_parent, &linked_parent).expect("parent symlink");

    let error = AtomicFileWriter::write_bytes(linked_parent.join("asset.bin"), b"new bytes")
        .expect_err("symlinked parent must be rejected");

    assert!(matches!(error, PpError::InvalidRequest(_)));
    assert!(!real_parent.join("asset.bin").exists());
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn atomic_directory_replaces_a_verified_complete_set() {
    let dir = unique_temp_dir("perfectpixel-directory-success");
    let target = dir.join("diagnostics");
    fs::create_dir_all(&target).expect("target");
    fs::write(target.join("previous.txt"), b"previous").expect("previous output");
    fs::create_dir_all(target.join("stale/nested")).expect("stale parent");
    fs::write(target.join("stale/nested/old.txt"), b"previous").expect("stale output");
    let entries = [
        verified_entry(Path::new("candidate.svg"), b"hello"),
        verified_entry(Path::new("renders/render-back.png"), b"hello"),
    ];

    AtomicDirectoryWriter::replace(&target, &entries).expect("directory replacement");

    assert_eq!(
        fs::read(target.join("candidate.svg")).expect("candidate"),
        b"hello"
    );
    assert_eq!(
        fs::read(target.join("renders/render-back.png")).expect("render"),
        b"hello"
    );
    assert!(!target.join("previous.txt").exists());
    assert!(!target.join("stale").exists());
    assert_no_transaction_directories(&dir, "diagnostics");
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn atomic_directory_zero_file_set_creates_an_empty_target() {
    let dir = unique_temp_dir("perfectpixel-directory-empty");
    let target = dir.join("diagnostics");

    AtomicDirectoryWriter::replace(&target, &[]).expect("empty directory publication");

    assert!(target.is_dir());
    assert!(fs::read_dir(&target)
        .expect("empty target")
        .next()
        .is_none());
    assert_no_transaction_directories(&dir, "diagnostics");
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn atomic_directory_rejects_a_symlinked_parent_before_rename() {
    let dir = unique_temp_dir("perfectpixel-directory-symlink-parent");
    let real_parent = dir.join("real-parent");
    let linked_parent = dir.join("linked-parent");
    let real_target = previous_diagnostics(&real_parent);
    symlink(&real_parent, &linked_parent).expect("parent symlink");
    let entries = [verified_entry(Path::new("candidate.svg"), b"hello")];

    let error = AtomicDirectoryWriter::replace(linked_parent.join("diagnostics"), &entries)
        .expect_err("symlinked parent must be rejected");

    assert!(matches!(error, PpError::InvalidRequest(_)));
    assert_previous_output(&real_target);
    assert_no_transaction_directories(&real_parent, "diagnostics");
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn atomic_directory_rejects_traversal_and_preserves_previous_output() {
    let dir = unique_temp_dir("perfectpixel-directory-traversal");
    let target = previous_diagnostics(&dir);
    let entries = [verified_entry(Path::new("../outside"), b"hello")];

    assert!(AtomicDirectoryWriter::replace(&target, &entries).is_err());

    assert_previous_output(&target);
    assert_no_transaction_directories(&dir, "diagnostics");
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn atomic_directory_rejects_duplicate_destinations_and_digest_mismatches() {
    let dir = unique_temp_dir("perfectpixel-directory-invalid");
    let target = previous_diagnostics(&dir);
    let duplicate = [
        verified_entry(Path::new("candidate.svg"), b"hello"),
        verified_entry(Path::new("candidate.svg"), b"hello"),
    ];
    assert!(AtomicDirectoryWriter::replace(&target, &duplicate).is_err());
    assert_previous_output(&target);

    let mismatch = [AtomicDirectoryEntry {
        relative_path: Path::new("candidate.svg"),
        bytes: b"hello",
        sha256: [0; 32],
    }];
    assert!(AtomicDirectoryWriter::replace(&target, &mismatch).is_err());
    assert_previous_output(&target);
    assert_no_transaction_directories(&dir, "diagnostics");
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn atomic_directory_rejects_an_invalid_path_before_mutation() {
    let dir = unique_temp_dir("perfectpixel-directory-stage-failure");
    let target = previous_diagnostics(&dir);
    let invalid_name = PathBuf::from("broken\0name");
    let entries = [
        verified_entry(Path::new("candidate.svg"), b"hello"),
        verified_entry(&invalid_name, b"hello"),
    ];

    assert!(AtomicDirectoryWriter::replace(&target, &entries).is_err());

    assert_previous_output(&target);
    assert_no_transaction_directories(&dir, "diagnostics");
    let _ = fs::remove_dir_all(dir);
}

fn verified_entry<'a>(relative_path: &'a Path, bytes: &'a [u8]) -> AtomicDirectoryEntry<'a> {
    assert_eq!(
        bytes, b"hello",
        "test helper only supplies the known digest"
    );
    AtomicDirectoryEntry {
        relative_path,
        bytes,
        sha256: HELLO_SHA256,
    }
}

fn previous_diagnostics(dir: &Path) -> PathBuf {
    let target = dir.join("diagnostics");
    fs::create_dir_all(&target).expect("target");
    fs::write(target.join("previous.txt"), b"previous").expect("previous output");
    target
}

fn assert_previous_output(target: &Path) {
    assert_eq!(
        fs::read(target.join("previous.txt")).expect("previous output"),
        b"previous"
    );
}

fn assert_no_transaction_directories(parent: &Path, target_name: &str) {
    let prefix = format!(".{target_name}.artifact-set.");
    for entry in fs::read_dir(parent).expect("parent") {
        let name = entry.expect("entry").file_name();
        let name = name.to_string_lossy();
        let Some(suffix) = name.strip_prefix(&prefix) else {
            continue;
        };
        let mut components = suffix.split('.');
        let transaction_shaped = matches!(
            (components.next(), components.next(), components.next()),
            (Some(pid), Some(attempt), None)
                if !pid.is_empty()
                    && pid.bytes().all(|byte| byte.is_ascii_digit())
                    && !attempt.is_empty()
                    && attempt.bytes().all(|byte| byte.is_ascii_digit())
        );
        assert!(!transaction_shaped, "transaction directory left behind");
    }
}

fn unique_temp_dir(prefix: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time")
        .as_nanos();
    std::env::temp_dir().join(format!("{prefix}-{}-{nanos}", std::process::id()))
}
