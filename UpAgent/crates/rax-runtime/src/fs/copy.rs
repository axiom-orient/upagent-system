use std::{collections::BTreeMap, path::Path};

use cap_std::fs::{Dir, OpenOptions};
use rax_core::{ErrorCode, ErrorPhase, FsCopyInput, FsEntryKind, FsMutationOutput, RaxError};
use uuid::Uuid;

use super::{
    storage::hash_file,
    support::{ensure_target_absent, mark_partial, mark_rollback_complete},
    traversal::{HARD_ENTRY_LIMIT, collect, path_to_protocol},
};
use crate::{
    error::{io_error, operation_error},
    workspace::Workspace,
};

pub fn copy_path(workspace: &Workspace, input: &FsCopyInput) -> Result<FsMutationOutput, RaxError> {
    copy_path_after_check(workspace, input, || {})
}

fn copy_path_after_check(
    workspace: &Workspace,
    input: &FsCopyInput,
    after_check: impl FnOnce(),
) -> Result<FsMutationOutput, RaxError> {
    workspace.ensure_no_symlink_components(&input.from, false, ErrorPhase::Apply)?;
    workspace.ensure_no_symlink_components(&input.to, false, ErrorPhase::Apply)?;
    let from = workspace.relative(&input.from, false)?;
    let to = workspace.relative(&input.to, false)?;
    let dir = workspace.dir()?;
    ensure_target_absent(&dir, &to, &input.to)?;
    after_check();
    let source = dir.symlink_metadata(&from).map_err(|error| {
        io_error(
            ErrorPhase::Apply,
            "failed to inspect copy source",
            Some(&from),
            error,
        )
    })?;

    let entries = if source.is_dir() {
        if !input.recursive {
            return Err(operation_error(
                ErrorCode::NotAFile,
                ErrorPhase::Apply,
                "copy source is a directory; recursive must be true",
            ));
        }
        let walked = collect(
            &dir,
            &from,
            true,
            usize::MAX,
            HARD_ENTRY_LIMIT,
            ErrorPhase::Apply,
        )?;
        if walked.truncated {
            return Err(operation_error(
                ErrorCode::LimitExceeded,
                ErrorPhase::Apply,
                "recursive copy exceeded the hard safety limit",
            )
            .with_detail("limit", u64::try_from(HARD_ENTRY_LIMIT).unwrap_or(u64::MAX)));
        }
        for entry in &walked.entries {
            match &entry.kind {
                FsEntryKind::File | FsEntryKind::Directory => {}
                FsEntryKind::Symlink => {
                    return Err(operation_error(
                        ErrorCode::SymlinkNotAllowed,
                        ErrorPhase::Apply,
                        "recursive copy does not copy symbolic links",
                    )
                    .with_detail("path", path_to_protocol(&entry.path)));
                }
                FsEntryKind::Other => {
                    return Err(operation_error(
                        ErrorCode::Unsupported,
                        ErrorPhase::Apply,
                        "recursive copy supports only regular files and directories",
                    )
                    .with_detail("path", path_to_protocol(&entry.path)));
                }
            }
        }
        Some(walked.entries)
    } else if source.is_file() {
        None
    } else {
        return Err(operation_error(
            ErrorCode::Unsupported,
            ErrorPhase::Apply,
            "copy source must be a regular file or directory",
        ));
    };

    // Keep incomplete copies off the public target. The held parent capability
    // also binds installation and cleanup to the same directory.
    let parent_path = to
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let parent = dir.open_dir(parent_path).map_err(|error| {
        io_error(
            ErrorPhase::Apply,
            "failed to open copy target parent",
            Some(&to),
            error,
        )
    })?;
    let temporary = format!(".rax-copy-{}", Uuid::new_v4());
    let temporary = Path::new(&temporary);
    if source.is_dir() {
        parent.create_dir(temporary)
    } else {
        parent
            .open_with(temporary, OpenOptions::new().write(true).create_new(true))
            .map(drop)
    }
    .map_err(|error| {
        io_error(
            ErrorPhase::Apply,
            "failed to create temporary copy target",
            Some(&to),
            error,
        )
    })?;

    let copy_result = (|| {
        let mut changed = Vec::new();
        let mut after = BTreeMap::new();
        if source.is_file() {
            let source_hash = hash_file(&dir, &from)?;
            dir.copy(&from, &parent, temporary).map_err(|error| {
                io_error(ErrorPhase::Apply, "failed to copy file", Some(&from), error)
            })?;
            let target_hash = hash_file(&parent, temporary).map_err(|error| {
                mark_partial(error, "file was copied but target hashing failed")
            })?;
            if source_hash != target_hash {
                return Err(mark_partial(
                    operation_error(
                        ErrorCode::OperationFailed,
                        ErrorPhase::Verify,
                        "copied file did not match its source",
                    )
                    .with_detail("source", input.from.clone())
                    .with_detail("target", input.to.clone()),
                    "copy completed but content verification failed",
                ));
            }
            changed.push(input.to.clone());
            after.insert(input.to.clone(), target_hash);
        } else {
            changed.push(input.to.clone());
            for entry in entries.as_deref().unwrap_or_default() {
                let suffix = entry.path.strip_prefix(&from).map_err(|_| {
                    operation_error(
                        ErrorCode::InvariantViolation,
                        ErrorPhase::Apply,
                        "copy traversal produced a path outside the source",
                    )
                })?;
                let destination = temporary.join(suffix);
                let destination_text = path_to_protocol(&to.join(suffix));
                match &entry.kind {
                    FsEntryKind::Directory => parent.create_dir(&destination).map_err(|error| {
                        io_error(
                            ErrorPhase::Apply,
                            "failed to create copied directory",
                            Some(&destination),
                            error,
                        )
                    })?,
                    FsEntryKind::File => {
                        let source_hash = hash_file(&dir, &entry.path)?;
                        dir.copy(&entry.path, &parent, &destination)
                            .map_err(|error| {
                                io_error(
                                    ErrorPhase::Apply,
                                    "failed to copy file",
                                    Some(&entry.path),
                                    error,
                                )
                            })?;
                        let target_hash = hash_file(&parent, &destination).map_err(|error| {
                            mark_partial(error, "file was copied but target hashing failed")
                        })?;
                        if source_hash != target_hash {
                            return Err(mark_partial(
                                operation_error(
                                    ErrorCode::OperationFailed,
                                    ErrorPhase::Verify,
                                    "copied file did not match its source",
                                )
                                .with_detail("source", path_to_protocol(&entry.path))
                                .with_detail("target", destination_text.clone()),
                                "copy completed but content verification failed",
                            ));
                        }
                        after.insert(destination_text.clone(), target_hash);
                    }
                    FsEntryKind::Symlink | FsEntryKind::Other => {
                        return Err(operation_error(
                            ErrorCode::InvariantViolation,
                            ErrorPhase::Apply,
                            "validated copy traversal changed before application",
                        )
                        .with_detail("path", path_to_protocol(&entry.path)));
                    }
                }
                changed.push(destination_text);
            }
        }
        install_copy(
            &parent,
            temporary,
            Path::new(to.file_name().ok_or_else(|| {
                operation_error(
                    ErrorCode::InvalidPath,
                    ErrorPhase::Apply,
                    "copy target has no file name",
                )
            })?),
        )
        .map_err(|error| {
            io_error(
                ErrorPhase::Apply,
                "failed to install copy target",
                Some(&to),
                error,
            )
        })?;
        Ok(FsMutationOutput {
            changed_paths: changed,
            before_sha256: BTreeMap::new(),
            after_sha256: after,
            verified: true,
        })
    })();

    match copy_result {
        Ok(output) => Ok(output),
        Err(mut error) => match rollback_copy_target(&parent, temporary) {
            Ok(removed) => Err(mark_rollback_complete(error, removed)),
            Err(cleanup_error) => {
                error
                    .details
                    .insert("cleanupError".to_string(), cleanup_error.message.into());
                Err(mark_partial(
                    error,
                    "copy failed and the newly-created target could not be removed",
                ))
            }
        },
    }
}

fn install_copy(parent: &Dir, temporary: &Path, target: &Path) -> std::io::Result<()> {
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    {
        rustix::fs::renameat_with(
            parent,
            temporary,
            parent,
            target,
            rustix::fs::RenameFlags::NOREPLACE,
        )
        .map_err(Into::into)
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = (parent, temporary, target);
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "atomic no-replace copy installation is unsupported on this platform",
        ))
    }
}

fn rollback_copy_target(dir: &Dir, target: &Path) -> Result<bool, RaxError> {
    let metadata = match dir.symlink_metadata(target) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => {
            return Err(io_error(
                ErrorPhase::Recover,
                "failed to inspect incomplete copy target",
                Some(target),
                error,
            ));
        }
    };
    if metadata.is_dir() {
        dir.remove_dir_all(target).map_err(|error| {
            io_error(
                ErrorPhase::Recover,
                "failed to remove incomplete copied directory",
                Some(target),
                error,
            )
        })?;
    } else {
        dir.remove_file(target).map_err(|error| {
            io_error(
                ErrorPhase::Recover,
                "failed to remove incomplete copied file",
                Some(target),
                error,
            )
        })?;
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn competing_file_is_never_overwritten_by_copy() {
        competing_target_survives(false);
    }

    #[test]
    fn competing_directory_is_never_removed_by_rollback() {
        competing_target_survives(true);
    }

    fn competing_target_survives(directory: bool) {
        let root = tempfile::tempdir().unwrap();
        if directory {
            fs::create_dir(root.path().join("source")).unwrap();
            fs::write(root.path().join("source/content"), "source").unwrap();
        } else {
            fs::write(root.path().join("source"), "source").unwrap();
        }
        let workspace = Workspace::open(root.path()).unwrap();
        let input = FsCopyInput {
            from: "source".into(),
            to: "target".into(),
            recursive: directory,
        };
        let result = copy_path_after_check(&workspace, &input, || {
            if directory {
                fs::create_dir(root.path().join("target")).unwrap();
                fs::write(root.path().join("target/content"), "other writer").unwrap();
            } else {
                fs::write(root.path().join("target"), "other writer").unwrap();
            }
        });
        let error = result.expect_err("a competing target must block the copy");
        assert_eq!(error.code, ErrorCode::AlreadyExists);
        let target = if directory {
            "target/content"
        } else {
            "target"
        };
        assert_eq!(fs::read(root.path().join(target)).unwrap(), b"other writer");
        assert_no_temporary_copies(root.path());
    }

    #[test]
    fn staged_copy_preserves_contents_permissions_and_reported_paths() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path().join("source/nested")).unwrap();
        fs::write(root.path().join("source/nested/file"), "contents").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(
                root.path().join("source/nested/file"),
                fs::Permissions::from_mode(0o640),
            )
            .unwrap();
        }
        let workspace = Workspace::open(root.path()).unwrap();
        for (from, to, recursive) in [
            ("source", "tree", true),
            ("source/nested/file", "file", false),
        ] {
            let output = copy_path(
                &workspace,
                &FsCopyInput {
                    from: from.into(),
                    to: to.into(),
                    recursive,
                },
            )
            .unwrap();
            let target = if recursive {
                "tree/nested/file"
            } else {
                "file"
            };
            assert_eq!(fs::read(root.path().join(target)).unwrap(), b"contents");
            assert!(output.verified);
            assert!(output.changed_paths.contains(&target.to_string()));
            assert!(output.after_sha256.contains_key(target));
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                assert_eq!(
                    fs::metadata(root.path().join(target))
                        .unwrap()
                        .permissions()
                        .mode()
                        & 0o777,
                    0o640
                );
            }
        }
        assert_no_temporary_copies(root.path());
    }

    #[test]
    fn failed_copy_removes_only_its_temporary_target() {
        let root = tempfile::tempdir().unwrap();
        fs::File::create(root.path().join("source"))
            .unwrap()
            .set_len(rax_core::MAX_READ_LIMIT + 1)
            .unwrap();
        let workspace = Workspace::open(root.path()).unwrap();
        let error = copy_path(
            &workspace,
            &FsCopyInput {
                from: "source".into(),
                to: "target".into(),
                recursive: false,
            },
        )
        .unwrap_err();
        assert_eq!(error.code, ErrorCode::LimitExceeded);
        assert_eq!(error.details["rollbackComplete"], true);
        assert!(!root.path().join("target").exists());
        assert_no_temporary_copies(root.path());
    }

    fn assert_no_temporary_copies(root: &Path) {
        assert!(fs::read_dir(root).unwrap().all(|entry| {
            !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".rax-copy-")
        }));
    }
}
