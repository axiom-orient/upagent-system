use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use rax_core::{
    ContentEncoding, ErrorCode, ErrorPhase, FsMkdirInput, FsMoveInput, FsMutationOutput,
    FsPatchInput, FsRemoveInput, FsWriteInput, RaxError,
};

use super::{
    edit::apply_text_edits,
    storage::{
        atomic_write, create_new_file, current_file_hash, hash_file, read_complete_file,
        sha256_bytes,
    },
    support::{ensure_target_absent, mark_partial, mutation_output, precondition_error},
    traversal::entry_kind,
};
use crate::{
    error::{io_error, operation_error},
    workspace::Workspace,
};

pub fn write(workspace: &Workspace, input: &FsWriteInput) -> Result<FsMutationOutput, RaxError> {
    workspace.ensure_no_symlink_components(&input.path, false, ErrorPhase::Apply)?;
    let relative = workspace.relative(&input.path, false)?;
    let dir = workspace.dir()?;
    let bytes = match input.encoding {
        ContentEncoding::Utf8 => input.content.as_bytes().to_vec(),
        ContentEncoding::Base64 => BASE64.decode(&input.content).map_err(|error| {
            RaxError::new(
                ErrorCode::InvalidRequest,
                ErrorPhase::Validate,
                false,
                format!("invalid base64 content: {error}"),
            )
        })?,
    };

    let before = current_file_hash(&dir, &relative)?;
    if input.create_only && before.is_some() {
        return Err(operation_error(
            ErrorCode::AlreadyExists,
            ErrorPhase::Apply,
            "create-only write target already exists",
        )
        .with_detail("path", input.path.clone()));
    }
    if let Some(expected) = &input.expected_sha256
        && before.as_deref() != Some(expected.as_str())
    {
        return Err(precondition_error(
            &input.path,
            Some(expected),
            before.as_deref(),
        ));
    }

    if input.create_only {
        create_new_file(&dir, &relative, &bytes)?;
    } else {
        atomic_write(&dir, &relative, &bytes)?;
    }
    let after = hash_file(&dir, &relative)
        .map_err(|error| mark_partial(error, "file was replaced but post-write hashing failed"))?;
    if after != sha256_bytes(&bytes) {
        return Err(mark_partial(
            operation_error(
                ErrorCode::OperationFailed,
                ErrorPhase::Verify,
                "written file hash did not match the requested content",
            )
            .with_detail("path", input.path.clone()),
            "file was replaced but content verification failed",
        ));
    }

    Ok(mutation_output(
        vec![input.path.clone()],
        before.map(|hash| (input.path.clone(), hash)),
        Some((input.path.clone(), after)),
    ))
}

pub fn patch(workspace: &Workspace, input: &FsPatchInput) -> Result<FsMutationOutput, RaxError> {
    workspace.ensure_no_symlink_components(&input.path, false, ErrorPhase::Apply)?;
    let relative = workspace.relative(&input.path, false)?;
    let dir = workspace.dir()?;
    let bytes = read_complete_file(&dir, &relative, input.max_bytes)?;
    let before = sha256_bytes(&bytes);
    if before != input.expected_sha256 {
        return Err(precondition_error(
            &input.path,
            Some(&input.expected_sha256),
            Some(&before),
        ));
    }
    let text = std::str::from_utf8(&bytes).map_err(|_| {
        operation_error(
            ErrorCode::BinaryFile,
            ErrorPhase::Apply,
            "fs_patch requires UTF-8 text",
        )
        .with_detail("path", input.path.clone())
    })?;
    let output = apply_text_edits(text, &input.edits)?;
    atomic_write(&dir, &relative, output.as_bytes())?;
    let after = hash_file(&dir, &relative)
        .map_err(|error| mark_partial(error, "file was patched but post-patch hashing failed"))?;
    if after != sha256_bytes(output.as_bytes()) {
        return Err(mark_partial(
            operation_error(
                ErrorCode::OperationFailed,
                ErrorPhase::Verify,
                "patched file verification failed",
            )
            .with_detail("path", input.path.clone()),
            "file was patched but content verification failed",
        ));
    }
    Ok(mutation_output(
        vec![input.path.clone()],
        Some((input.path.clone(), before)),
        Some((input.path.clone(), after)),
    ))
}

pub fn mkdir(workspace: &Workspace, input: &FsMkdirInput) -> Result<FsMutationOutput, RaxError> {
    workspace.ensure_no_symlink_components(&input.path, false, ErrorPhase::Apply)?;
    let relative = workspace.relative(&input.path, false)?;
    let dir = workspace.dir()?;
    let existed = dir.try_exists(&relative).map_err(|error| {
        io_error(
            ErrorPhase::Apply,
            "failed to inspect mkdir target",
            Some(&relative),
            error,
        )
    })?;
    if existed {
        let metadata = dir.symlink_metadata(&relative).map_err(|error| {
            io_error(
                ErrorPhase::Apply,
                "failed to inspect mkdir target",
                Some(&relative),
                error,
            )
        })?;
        if metadata.is_dir() && input.recursive {
            return Ok(mutation_output(Vec::new(), None, None));
        }
        return Err(operation_error(
            ErrorCode::AlreadyExists,
            ErrorPhase::Apply,
            "mkdir target already exists",
        )
        .with_detail("path", input.path.clone()));
    }
    let result = if input.recursive {
        dir.create_dir_all(&relative)
    } else {
        dir.create_dir(&relative)
    };
    result.map_err(|error| {
        let mapped = io_error(
            ErrorPhase::Apply,
            "failed to create directory",
            Some(&relative),
            error,
        );
        if input.recursive {
            mark_partial(
                mapped,
                "recursive directory creation may have created parent directories",
            )
        } else {
            mapped
        }
    })?;
    let metadata = dir.symlink_metadata(&relative).map_err(|error| {
        mark_partial(
            io_error(
                ErrorPhase::Verify,
                "failed to verify created directory",
                Some(&relative),
                error,
            ),
            "directory creation completed but verification failed",
        )
    })?;
    if !metadata.is_dir() {
        return Err(mark_partial(
            operation_error(
                ErrorCode::OperationFailed,
                ErrorPhase::Verify,
                "mkdir target was not a directory after creation",
            ),
            "directory creation completed but the resulting object was unexpected",
        ));
    }
    Ok(mutation_output(vec![input.path.clone()], None, None))
}

pub fn move_path(workspace: &Workspace, input: &FsMoveInput) -> Result<FsMutationOutput, RaxError> {
    workspace.ensure_no_symlink_components(&input.from, false, ErrorPhase::Apply)?;
    workspace.ensure_no_symlink_components(&input.to, false, ErrorPhase::Apply)?;
    let from = workspace.relative(&input.from, false)?;
    let to = workspace.relative(&input.to, false)?;
    let dir = workspace.dir()?;
    ensure_target_absent(&dir, &to, &input.to)?;

    let source_metadata = dir.symlink_metadata(&from).map_err(|error| {
        io_error(
            ErrorPhase::Apply,
            "failed to inspect move source",
            Some(&from),
            error,
        )
    })?;
    let source_kind = entry_kind(&source_metadata);
    let before = if source_metadata.is_file() {
        Some(hash_file(&dir, &from)?)
    } else if source_metadata.is_dir() {
        None
    } else {
        return Err(operation_error(
            ErrorCode::Unsupported,
            ErrorPhase::Apply,
            "move source must be a regular file or directory",
        )
        .with_detail("path", input.from.clone()));
    };

    dir.rename(&from, &dir, &to)
        .map_err(|error| io_error(ErrorPhase::Apply, "failed to move path", Some(&from), error))?;
    let source_exists = dir.try_exists(&from).map_err(|error| {
        mark_partial(
            io_error(
                ErrorPhase::Verify,
                "failed to verify moved source",
                Some(&from),
                error,
            ),
            "rename completed but source verification failed",
        )
    })?;
    let target_metadata = dir.symlink_metadata(&to).map_err(|error| {
        mark_partial(
            io_error(
                ErrorPhase::Verify,
                "failed to verify moved target",
                Some(&to),
                error,
            ),
            "rename completed but target verification failed",
        )
    })?;
    if source_exists || entry_kind(&target_metadata) != source_kind {
        return Err(mark_partial(
            operation_error(
                ErrorCode::OperationFailed,
                ErrorPhase::Verify,
                "move verification failed",
            ),
            "rename completed but the resulting source/target state was unexpected",
        ));
    }

    let after = if target_metadata.is_file() {
        let hash = hash_file(&dir, &to)
            .map_err(|error| mark_partial(error, "rename completed but target hashing failed"))?;
        if before.as_deref() != Some(hash.as_str()) {
            return Err(mark_partial(
                operation_error(
                    ErrorCode::OperationFailed,
                    ErrorPhase::Verify,
                    "moved file content changed during rename",
                )
                .with_detail("path", input.to.clone()),
                "rename completed but content verification failed",
            ));
        }
        Some(hash)
    } else {
        None
    };

    Ok(mutation_output(
        vec![input.from.clone(), input.to.clone()],
        before.map(|hash| (input.from.clone(), hash)),
        after.map(|hash| (input.to.clone(), hash)),
    ))
}

pub fn remove(workspace: &Workspace, input: &FsRemoveInput) -> Result<FsMutationOutput, RaxError> {
    workspace.ensure_no_symlink_components(&input.path, false, ErrorPhase::Apply)?;
    let relative = workspace.relative(&input.path, false)?;
    let dir = workspace.dir()?;
    let metadata = dir.symlink_metadata(&relative).map_err(|error| {
        io_error(
            ErrorPhase::Apply,
            "failed to inspect removal target",
            Some(&relative),
            error,
        )
    })?;
    let before = if metadata.is_file() {
        Some(hash_file(&dir, &relative)?)
    } else {
        None
    };
    if let Some(expected) = &input.expected_sha256
        && before.as_deref() != Some(expected.as_str())
    {
        return Err(precondition_error(
            &input.path,
            Some(expected),
            before.as_deref(),
        ));
    }

    if metadata.is_file() {
        dir.remove_file(&relative).map_err(|error| {
            io_error(
                ErrorPhase::Apply,
                "failed to remove file",
                Some(&relative),
                error,
            )
        })?;
    } else if metadata.is_dir() {
        if input.recursive {
            dir.remove_dir_all(&relative).map_err(|error| {
                mark_partial(
                    io_error(
                        ErrorPhase::Apply,
                        "failed to remove directory tree",
                        Some(&relative),
                        error,
                    ),
                    "recursive removal may have removed a subset of entries",
                )
            })?;
        } else {
            dir.remove_dir(&relative).map_err(|error| {
                io_error(
                    ErrorPhase::Apply,
                    "failed to remove directory",
                    Some(&relative),
                    error,
                )
            })?;
        }
    } else {
        return Err(operation_error(
            ErrorCode::Unsupported,
            ErrorPhase::Apply,
            "removal target must be a regular file or directory",
        ));
    }

    let target_exists = dir.try_exists(&relative).map_err(|error| {
        mark_partial(
            io_error(
                ErrorPhase::Verify,
                "failed to verify removal target",
                Some(&relative),
                error,
            ),
            "removal was attempted but post-removal verification failed",
        )
    })?;
    if target_exists {
        return Err(mark_partial(
            operation_error(
                ErrorCode::OperationFailed,
                ErrorPhase::Verify,
                "removal target still exists after removal",
            ),
            "removal was attempted but the target still exists",
        ));
    }
    Ok(mutation_output(
        vec![input.path.clone()],
        before.map(|hash| (input.path.clone(), hash)),
        None,
    ))
}
