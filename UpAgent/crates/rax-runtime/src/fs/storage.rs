use std::{
    io::{Read, Write},
    path::Path,
};

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use cap_std::fs::{Dir, OpenOptions};
use rax_core::{ErrorCode, ErrorPhase, FileContentKind, MAX_READ_LIMIT, RaxError};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::{support::mark_partial, traversal::path_to_protocol};
use crate::error::{invalid_path, io_error, operation_error};

#[derive(Debug)]
pub(super) struct CapturedFile {
    pub(super) preview: Vec<u8>,
    pub(super) total_bytes: u64,
    pub(super) sha256: String,
    pub(super) truncated: bool,
}

pub(super) fn capture_file(
    dir: &Dir,
    path: &Path,
    max_bytes: u64,
) -> Result<CapturedFile, RaxError> {
    let metadata = dir.symlink_metadata(path).map_err(|error| {
        io_error(
            ErrorPhase::Apply,
            "failed to inspect file",
            Some(path),
            error,
        )
    })?;
    if !metadata.is_file() {
        return Err(operation_error(
            ErrorCode::NotAFile,
            ErrorPhase::Apply,
            "expected a regular file",
        )
        .with_detail("path", path_to_protocol(path)));
    }
    ensure_hashable_size(path, metadata.len(), ErrorPhase::Apply)?;
    let mut file = dir
        .open(path)
        .map_err(|error| io_error(ErrorPhase::Apply, "failed to open file", Some(path), error))?;
    let preview_limit = usize::try_from(max_bytes).unwrap_or(usize::MAX);
    let mut preview = Vec::with_capacity(preview_limit.min(64 * 1024));
    let mut hasher = Sha256::new();
    let mut total_bytes = 0_u64;
    let mut buffer = vec![0_u8; 64 * 1024];
    loop {
        let count = file.read(&mut buffer).map_err(|error| {
            io_error(ErrorPhase::Apply, "failed to read file", Some(path), error)
        })?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
        total_bytes = checked_byte_count(
            total_bytes,
            count,
            path,
            ErrorPhase::Apply,
            "file byte count overflowed while it was read",
        )?;
        if total_bytes > MAX_READ_LIMIT {
            return Err(operation_error(
                ErrorCode::LimitExceeded,
                ErrorPhase::Apply,
                "file grew beyond the hard hashing limit while it was read",
            )
            .with_detail("path", path_to_protocol(path))
            .with_detail("limit", MAX_READ_LIMIT));
        }
        if preview.len() < preview_limit {
            let remaining = preview_limit - preview.len();
            preview.extend_from_slice(&buffer[..count.min(remaining)]);
        }
    }
    Ok(CapturedFile {
        preview,
        total_bytes,
        sha256: format!("{:x}", hasher.finalize()),
        truncated: total_bytes > max_bytes,
    })
}

pub(super) fn read_complete_file(
    dir: &Dir,
    path: &Path,
    max_bytes: u64,
) -> Result<Vec<u8>, RaxError> {
    let metadata = dir.symlink_metadata(path).map_err(|error| {
        io_error(
            ErrorPhase::Apply,
            "failed to inspect file",
            Some(path),
            error,
        )
    })?;
    if !metadata.is_file() {
        return Err(operation_error(
            ErrorCode::NotAFile,
            ErrorPhase::Apply,
            "expected a regular file",
        )
        .with_detail("path", path_to_protocol(path)));
    }
    if metadata.len() > max_bytes {
        return Err(operation_error(
            ErrorCode::LimitExceeded,
            ErrorPhase::Apply,
            "file exceeds the configured byte limit",
        )
        .with_detail("path", path_to_protocol(path))
        .with_detail("size", metadata.len())
        .with_detail("limit", max_bytes));
    }
    let capacity = usize::try_from(metadata.len()).map_err(|_| {
        operation_error(
            ErrorCode::LimitExceeded,
            ErrorPhase::Apply,
            "file is too large for this process address space",
        )
    })?;
    let file = dir
        .open(path)
        .map_err(|error| io_error(ErrorPhase::Apply, "failed to open file", Some(path), error))?;
    let read_limit = max_bytes.checked_add(1).ok_or_else(|| {
        operation_error(
            ErrorCode::LimitExceeded,
            ErrorPhase::Apply,
            "file read limit overflowed",
        )
    })?;
    let mut limited = file.take(read_limit);
    let mut bytes = Vec::with_capacity(capacity);
    limited
        .read_to_end(&mut bytes)
        .map_err(|error| io_error(ErrorPhase::Apply, "failed to read file", Some(path), error))?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > max_bytes {
        return Err(operation_error(
            ErrorCode::LimitExceeded,
            ErrorPhase::Apply,
            "file grew beyond the configured byte limit while it was read",
        )
        .with_detail("path", path_to_protocol(path))
        .with_detail("limit", max_bytes));
    }
    Ok(bytes)
}

pub(super) fn hash_file(dir: &Dir, path: &Path) -> Result<String, RaxError> {
    hash_file_at_phase(dir, path, ErrorPhase::Apply)
}

pub(super) fn hash_file_at_phase(
    dir: &Dir,
    path: &Path,
    phase: ErrorPhase,
) -> Result<String, RaxError> {
    let metadata = dir.symlink_metadata(path).map_err(|error| {
        io_error(
            phase,
            "failed to inspect file for hashing",
            Some(path),
            error,
        )
    })?;
    if !metadata.is_file() {
        return Err(operation_error(
            ErrorCode::NotAFile,
            phase,
            "hash target must be a regular file",
        )
        .with_detail("path", path_to_protocol(path)));
    }
    ensure_hashable_size(path, metadata.len(), phase)?;
    let mut file = dir
        .open(path)
        .map_err(|error| io_error(phase, "failed to open file for hashing", Some(path), error))?;
    let mut hasher = Sha256::new();
    let mut total_bytes = 0_u64;
    let mut buffer = vec![0_u8; 64 * 1024];
    loop {
        let count = file
            .read(&mut buffer)
            .map_err(|error| io_error(phase, "failed to hash file", Some(path), error))?;
        if count == 0 {
            break;
        }
        total_bytes = checked_byte_count(
            total_bytes,
            count,
            path,
            phase,
            "file byte count overflowed while hashing",
        )?;
        if total_bytes > MAX_READ_LIMIT {
            return Err(operation_error(
                ErrorCode::LimitExceeded,
                phase,
                "file grew beyond the hard hashing limit while it was read",
            )
            .with_detail("path", path_to_protocol(path))
            .with_detail("limit", MAX_READ_LIMIT));
        }
        hasher.update(&buffer[..count]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn ensure_hashable_size(path: &Path, size: u64, phase: ErrorPhase) -> Result<(), RaxError> {
    if size > MAX_READ_LIMIT {
        return Err(operation_error(
            ErrorCode::LimitExceeded,
            phase,
            "file exceeds the hard hashing limit",
        )
        .with_detail("path", path_to_protocol(path))
        .with_detail("size", size)
        .with_detail("limit", MAX_READ_LIMIT));
    }
    Ok(())
}

fn checked_byte_count(
    total: u64,
    count: usize,
    path: &Path,
    phase: ErrorPhase,
    message: &str,
) -> Result<u64, RaxError> {
    total
        .checked_add(u64::try_from(count).unwrap_or(u64::MAX))
        .ok_or_else(|| {
            operation_error(ErrorCode::LimitExceeded, phase, message)
                .with_detail("path", path_to_protocol(path))
        })
}

#[must_use]
pub(super) fn sha256_bytes(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

pub(super) fn current_file_hash(dir: &Dir, path: &Path) -> Result<Option<String>, RaxError> {
    match dir.symlink_metadata(path) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() {
                return Err(operation_error(
                    ErrorCode::SymlinkNotAllowed,
                    ErrorPhase::Apply,
                    "target is a symbolic link",
                )
                .with_detail("path", path_to_protocol(path)));
            }
            if !metadata.is_file() {
                return Err(operation_error(
                    ErrorCode::NotAFile,
                    ErrorPhase::Apply,
                    "target exists but is not a regular file",
                )
                .with_detail("path", path_to_protocol(path)));
            }
            Ok(Some(hash_file(dir, path)?))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(io_error(
            ErrorPhase::Apply,
            "failed to inspect target",
            Some(path),
            error,
        )),
    }
}

pub(super) fn create_new_file(dir: &Dir, path: &Path, bytes: &[u8]) -> Result<(), RaxError> {
    let (parent, leaf) = open_parent(dir, path)?;
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    let mut file = parent.open_with(&leaf, &options).map_err(|error| {
        io_error(
            ErrorPhase::Apply,
            "failed to create create-only target",
            Some(path),
            error,
        )
    })?;
    if let Err(error) = file.write_all(bytes).and_then(|()| file.sync_all()) {
        drop(file);
        let cleanup = parent.remove_file(&leaf);
        let base = io_error(
            ErrorPhase::Apply,
            "failed to write create-only target",
            Some(path),
            error,
        );
        return match cleanup {
            Ok(()) => Err(base),
            Err(cleanup_error) => Err(mark_partial(
                base.with_detail("cleanupError", cleanup_error.to_string()),
                "create-only target may remain after a failed write",
            )),
        };
    }
    sync_directory(&parent).map_err(|error| {
        mark_partial(
            error,
            "create-only target was written but its parent directory could not be synced",
        )
    })
}

pub(super) fn atomic_write(dir: &Dir, path: &Path, bytes: &[u8]) -> Result<(), RaxError> {
    let (parent, leaf) = open_parent(dir, path)?;
    let leaf_text = leaf.to_str().ok_or_else(|| {
        invalid_path(
            "non-UTF-8 file names cannot be represented by the protocol",
            leaf.to_string_lossy().to_string(),
        )
    })?;
    let temporary = format!(".{leaf_text}.rax-tmp-{}", Uuid::now_v7().simple());
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    let mut file = parent.open_with(&temporary, &options).map_err(|error| {
        io_error(
            ErrorPhase::Apply,
            "failed to create atomic write temporary file",
            Some(Path::new(&temporary)),
            error,
        )
    })?;

    if let Ok(existing) = parent.symlink_metadata(&leaf)
        && existing.is_file()
        && let Err(error) = file.set_permissions(existing.permissions())
    {
        drop(file);
        return Err(cleanup_temporary_error(
            &parent,
            &temporary,
            io_error(
                ErrorPhase::Apply,
                "failed to preserve target permissions",
                Some(path),
                error,
            ),
        ));
    }
    if let Err(error) = file.write_all(bytes).and_then(|()| file.sync_all()) {
        drop(file);
        return Err(cleanup_temporary_error(
            &parent,
            &temporary,
            io_error(
                ErrorPhase::Apply,
                "failed to write atomic temporary file",
                Some(path),
                error,
            ),
        ));
    }
    drop(file);
    parent.rename(&temporary, &parent, &leaf).map_err(|error| {
        cleanup_temporary_error(
            &parent,
            &temporary,
            io_error(
                ErrorPhase::Apply,
                "failed to atomically replace target",
                Some(path),
                error,
            ),
        )
    })?;
    sync_directory(&parent).map_err(|error| {
        mark_partial(
            error,
            "file was replaced but its parent directory could not be synced",
        )
    })
}

fn open_parent(dir: &Dir, path: &Path) -> Result<(Dir, std::ffi::OsString), RaxError> {
    let parent_path = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let leaf = path.file_name().ok_or_else(|| {
        invalid_path(
            "write target has no file name",
            path.to_string_lossy().to_string(),
        )
    })?;
    let parent = dir.open_dir(parent_path).map_err(|error| {
        io_error(
            ErrorPhase::Apply,
            "failed to open target parent directory",
            Some(parent_path),
            error,
        )
    })?;
    Ok((parent, leaf.to_os_string()))
}

fn cleanup_temporary_error(dir: &Dir, temporary: &str, mut error: RaxError) -> RaxError {
    if let Err(cleanup) = dir.remove_file(temporary) {
        error = mark_partial(
            error.with_detail("cleanupError", cleanup.to_string()),
            "atomic write temporary file may remain after a failure",
        );
    }
    error
}

#[cfg(unix)]
fn sync_directory(dir: &Dir) -> Result<(), RaxError> {
    dir.open(".")
        .and_then(|directory| directory.sync_all())
        .map_err(|error| {
            io_error(
                ErrorPhase::Apply,
                "failed to sync target parent directory",
                None,
                error,
            )
        })
}

#[cfg(not(unix))]
fn sync_directory(_dir: &Dir) -> Result<(), RaxError> {
    Ok(())
}

pub(super) fn encode_content(
    bytes: &[u8],
    preview_truncated: bool,
) -> (FileContentKind, String, String) {
    if !bytes.contains(&0) {
        if let Ok(text) = std::str::from_utf8(bytes) {
            return (FileContentKind::Text, "utf8".to_string(), text.to_string());
        }
        if preview_truncated && let Some(valid) = valid_utf8_prefix(bytes) {
            return (FileContentKind::Text, "utf8".to_string(), valid.to_string());
        }
    }
    (
        FileContentKind::Binary,
        "base64".to_string(),
        BASE64.encode(bytes),
    )
}

fn valid_utf8_prefix(bytes: &[u8]) -> Option<&str> {
    let error = std::str::from_utf8(bytes).err()?;
    if error.error_len().is_some() {
        return None;
    }
    std::str::from_utf8(&bytes[..error.valid_up_to()]).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn complete_invalid_utf8_is_returned_losslessly_as_base64() {
        let bytes = [b'a', 0xe2, 0x82];
        let (kind, encoding, content) = encode_content(&bytes, false);
        assert_eq!(kind, FileContentKind::Binary);
        assert_eq!(encoding, "base64");
        assert_eq!(BASE64.decode(content).expect("base64"), bytes);
    }

    #[test]
    fn truncated_preview_may_drop_only_an_incomplete_final_code_point() {
        let bytes = [b'a', 0xe2, 0x82];
        let (kind, encoding, content) = encode_content(&bytes, true);
        assert_eq!(kind, FileContentKind::Text);
        assert_eq!(encoding, "utf8");
        assert_eq!(content, "a");
    }

    #[test]
    fn interior_invalid_utf8_remains_binary_even_when_preview_is_truncated() {
        let bytes = [b'a', 0xff, b'b'];
        let (kind, encoding, content) = encode_content(&bytes, true);
        assert_eq!(kind, FileContentKind::Binary);
        assert_eq!(encoding, "base64");
        assert_eq!(BASE64.decode(content).expect("base64"), bytes);
    }
}
