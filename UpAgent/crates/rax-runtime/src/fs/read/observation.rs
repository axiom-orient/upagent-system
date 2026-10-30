use sha2::{Digest, Sha256};

use rax_core::{ErrorCode, ErrorPhase, FsEntryKind, PathKind, PathPrecondition, RaxError};

use super::super::{
    storage::hash_file_at_phase,
    traversal::{HARD_ENTRY_LIMIT, WalkControl, entry_kind, path_to_protocol, visit as visit_walk},
};
use crate::{
    error::{io_error, operation_error},
    workspace::Workspace,
};

const TREE_BYTE_HARD_LIMIT: u64 = 8 * 1024 * 1024 * 1024;

pub fn current_precondition(
    workspace: &Workspace,
    path: &str,
) -> Result<PathPrecondition, RaxError> {
    workspace.ensure_no_symlink_components(path, true, ErrorPhase::Plan)?;
    let relative = workspace.relative(path, true)?;
    let dir = workspace.dir()?;
    match dir.symlink_metadata(&relative) {
        Ok(metadata) => {
            let kind = match entry_kind(&metadata) {
                FsEntryKind::File => PathKind::File,
                FsEntryKind::Directory => PathKind::Directory,
                FsEntryKind::Symlink => PathKind::Symlink,
                FsEntryKind::Other => PathKind::Other,
            };
            let (sha256, size) = if metadata.is_file() {
                (
                    Some(hash_file_at_phase(&dir, &relative, ErrorPhase::Plan)?),
                    Some(metadata.len()),
                )
            } else {
                (None, None)
            };
            Ok(PathPrecondition {
                path: path.to_string(),
                exists: true,
                kind: Some(kind),
                sha256,
                size,
            })
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(PathPrecondition {
            path: path.to_string(),
            exists: false,
            kind: None,
            sha256: None,
            size: None,
        }),
        Err(error) => Err(io_error(
            ErrorPhase::Plan,
            "failed to capture path precondition",
            Some(&relative),
            error,
        )),
    }
}

pub fn verify_precondition(
    workspace: &Workspace,
    expected: &PathPrecondition,
) -> Result<(), RaxError> {
    let actual = current_precondition(workspace, &expected.path)?;
    if &actual != expected {
        return Err(RaxError::new(
            ErrorCode::PreconditionFailed,
            ErrorPhase::Authorize,
            false,
            "path changed after the plan was created",
        )
        .with_detail("path", expected.path.clone())
        .with_detail("expected", serialize_precondition(expected)?)
        .with_detail("actual", serialize_precondition(&actual)?));
    }
    Ok(())
}

pub fn snapshot_hash(workspace: &Workspace, path: &str) -> Result<String, RaxError> {
    workspace.ensure_no_symlink_components(path, false, ErrorPhase::Plan)?;
    let relative = workspace.relative(path, true)?;
    let dir = workspace.dir()?;
    let metadata = dir.symlink_metadata(&relative).map_err(|error| {
        io_error(
            ErrorPhase::Plan,
            "failed to inspect snapshot root",
            Some(&relative),
            error,
        )
    })?;
    let mut hasher = Sha256::new();
    if metadata.is_file() {
        hasher.update(b"file\0");
        hasher.update(hash_file_at_phase(&dir, &relative, ErrorPhase::Plan)?.as_bytes());
        return Ok(format!("{:x}", hasher.finalize()));
    }
    if !metadata.is_dir() {
        return Err(operation_error(
            ErrorCode::Unsupported,
            ErrorPhase::Plan,
            "snapshot root must be a regular file or directory",
        )
        .with_detail("path", path.to_string()));
    }

    hasher.update(b"directory\0");
    let mut total_file_bytes = 0_u64;
    let summary = visit_walk(
        &dir,
        &relative,
        true,
        usize::MAX,
        HARD_ENTRY_LIMIT,
        ErrorPhase::Plan,
        |entry| {
            let suffix = entry.path.strip_prefix(&relative).map_err(|_| {
                operation_error(
                    ErrorCode::InvariantViolation,
                    ErrorPhase::Plan,
                    "snapshot traversal produced a path outside its root",
                )
                .with_detail("path", path_to_protocol(&entry.path))
            })?;
            let name = path_to_protocol(suffix);
            hasher.update(name.as_bytes());
            hasher.update([0]);
            match entry.kind {
                FsEntryKind::File => {
                    total_file_bytes =
                        total_file_bytes.checked_add(entry.size).ok_or_else(|| {
                            operation_error(
                                ErrorCode::LimitExceeded,
                                ErrorPhase::Plan,
                                "directory snapshot byte count overflowed",
                            )
                        })?;
                    if total_file_bytes > TREE_BYTE_HARD_LIMIT {
                        return Err(operation_error(
                            ErrorCode::LimitExceeded,
                            ErrorPhase::Plan,
                            "directory snapshot exceeds the hard byte limit",
                        )
                        .with_detail("size", total_file_bytes)
                        .with_detail("limit", TREE_BYTE_HARD_LIMIT));
                    }
                    hasher.update(b"file\0");
                    hasher.update(
                        hash_file_at_phase(&dir, &entry.path, ErrorPhase::Plan)?.as_bytes(),
                    );
                }
                FsEntryKind::Directory => hasher.update(b"directory\0"),
                FsEntryKind::Symlink => {
                    return Err(operation_error(
                        ErrorCode::SymlinkNotAllowed,
                        ErrorPhase::Plan,
                        "directory snapshot does not follow symbolic links",
                    )
                    .with_detail("path", path_to_protocol(&entry.path)));
                }
                FsEntryKind::Other => {
                    return Err(operation_error(
                        ErrorCode::Unsupported,
                        ErrorPhase::Plan,
                        "directory snapshot encountered an unsupported filesystem object",
                    )
                    .with_detail("path", path_to_protocol(&entry.path)));
                }
            }
            hasher.update([0xff]);
            Ok(WalkControl::Continue)
        },
    )?;
    if summary.hard_limit_reached {
        return Err(operation_error(
            ErrorCode::LimitExceeded,
            ErrorPhase::Plan,
            "directory snapshot exceeded the hard safety limit",
        )
        .with_detail("limit", u64::try_from(HARD_ENTRY_LIMIT).unwrap_or(u64::MAX)));
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn serialize_precondition(value: &PathPrecondition) -> Result<serde_json::Value, RaxError> {
    serde_json::to_value(value).map_err(|error| {
        operation_error(
            ErrorCode::InvariantViolation,
            ErrorPhase::Authorize,
            format!("failed to serialize path precondition: {error}"),
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace::Workspace;

    #[test]
    fn directory_preconditions_do_not_depend_on_platform_metadata_size() {
        let root = tempfile::tempdir().unwrap();
        let workspace = Workspace::open(root.path()).unwrap();
        let value = current_precondition(&workspace, "").unwrap();
        assert_eq!(value.kind, Some(PathKind::Directory));
        assert_eq!(value.size, None);
        assert_eq!(value.sha256, None);
    }
}
