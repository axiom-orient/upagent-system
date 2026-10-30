use globset::Glob;
use rax_core::{
    ErrorCode, ErrorPhase, FsEntry, FsEntryKind, FsGlobInput, FsGlobOutput, FsGrepInput,
    FsGrepOutput, FsListInput, FsListOutput, FsReadInput, FsReadOutput, FsStatInput, FsStatOutput,
    GrepMatch, RaxError,
};
use regex::RegexBuilder;

use super::{
    storage::{capture_file, encode_content, hash_file, read_complete_file},
    traversal::{HARD_ENTRY_LIMIT, WalkControl, collect, entry_kind, path_to_protocol, visit},
};
use crate::{
    error::{io_error, operation_error},
    timeutil::unix_millis,
    workspace::Workspace,
};

mod observation;

pub use observation::{current_precondition, snapshot_hash, verify_precondition};

pub fn read(workspace: &Workspace, input: &FsReadInput) -> Result<FsReadOutput, RaxError> {
    workspace.ensure_no_symlink_components(&input.path, false, ErrorPhase::Apply)?;
    let relative = workspace.relative(&input.path, false)?;
    let dir = workspace.dir()?;
    let metadata = dir.symlink_metadata(&relative).map_err(|error| {
        io_error(
            ErrorPhase::Apply,
            "failed to inspect file",
            Some(&relative),
            error,
        )
    })?;
    if !metadata.is_file() {
        return Err(operation_error(
            ErrorCode::NotAFile,
            ErrorPhase::Apply,
            "fs_read requires a regular file",
        )
        .with_detail("path", input.path.clone()));
    }

    let captured = capture_file(&dir, &relative, input.max_bytes)?;
    let (content_kind, encoding, content) = encode_content(&captured.preview, captured.truncated);
    Ok(FsReadOutput {
        path: input.path.clone(),
        content_kind,
        encoding,
        content,
        size: captured.total_bytes,
        sha256: captured.sha256,
        truncated: captured.truncated,
    })
}

pub fn stat(workspace: &Workspace, input: &FsStatInput) -> Result<FsStatOutput, RaxError> {
    if input.follow_symlinks {
        return Err(operation_error(
            ErrorCode::InvalidRequest,
            ErrorPhase::Validate,
            "followSymlinks is not supported; Rax v1 never follows filesystem links",
        ));
    }
    workspace.ensure_no_symlink_components(&input.path, true, ErrorPhase::Apply)?;
    let relative = workspace.relative(&input.path, true)?;
    let dir = workspace.dir()?;
    let metadata = dir.symlink_metadata(&relative).map_err(|error| {
        io_error(
            ErrorPhase::Apply,
            "failed to stat path",
            Some(&relative),
            error,
        )
    })?;

    let kind = entry_kind(&metadata);
    let sha256 = if metadata.is_file() {
        Some(hash_file(&dir, &relative)?)
    } else {
        None
    };
    Ok(FsStatOutput {
        path: input.path.clone(),
        kind,
        size: metadata.len(),
        modified_unix_ms: metadata
            .modified()
            .ok()
            .and_then(|value| unix_millis(value.into_std())),
        sha256,
    })
}

pub fn list(workspace: &Workspace, input: &FsListInput) -> Result<FsListOutput, RaxError> {
    workspace.ensure_no_symlink_components(&input.path, false, ErrorPhase::Apply)?;
    let root = workspace.relative(&input.path, true)?;
    let dir = workspace.dir()?;
    let limit = usize::try_from(input.max_entries).unwrap_or(usize::MAX);
    let walked = collect(
        &dir,
        &root,
        input.recursive,
        usize::from(input.max_depth),
        limit,
        ErrorPhase::Apply,
    )?;
    Ok(FsListOutput {
        entries: walked
            .entries
            .into_iter()
            .map(|entry| FsEntry {
                path: path_to_protocol(&entry.path),
                kind: entry.kind,
                size: entry.size,
            })
            .collect(),
        truncated: walked.truncated,
    })
}

pub fn glob(workspace: &Workspace, input: &FsGlobInput) -> Result<FsGlobOutput, RaxError> {
    workspace.ensure_no_symlink_components(&input.path, false, ErrorPhase::Apply)?;
    let matcher = Glob::new(&input.pattern)
        .map_err(|error| {
            RaxError::new(
                ErrorCode::InvalidRequest,
                ErrorPhase::Validate,
                false,
                format!("invalid glob pattern: {error}"),
            )
        })?
        .compile_matcher();
    let root = workspace.relative(&input.path, true)?;
    let dir = workspace.dir()?;
    let output_limit = usize::try_from(input.max_entries).unwrap_or(usize::MAX);
    let mut paths = Vec::with_capacity(output_limit.min(1024));
    let mut output_truncated = false;
    let summary = visit(
        &dir,
        &root,
        true,
        usize::from(input.max_depth),
        HARD_ENTRY_LIMIT,
        ErrorPhase::Apply,
        |entry| {
            let relative_to_root = entry.path.strip_prefix(&root).map_err(|_| {
                operation_error(
                    ErrorCode::InvariantViolation,
                    ErrorPhase::Apply,
                    "glob traversal produced a path outside its root",
                )
                .with_detail("path", path_to_protocol(&entry.path))
            })?;
            if matcher.is_match(relative_to_root) {
                if paths.len() == output_limit {
                    output_truncated = true;
                    return Ok(WalkControl::Stop);
                }
                paths.push(path_to_protocol(&entry.path));
            }
            Ok(WalkControl::Continue)
        },
    )?;
    Ok(FsGlobOutput {
        paths,
        truncated: output_truncated || summary.hard_limit_reached,
    })
}

pub fn grep(workspace: &Workspace, input: &FsGrepInput) -> Result<FsGrepOutput, RaxError> {
    workspace.ensure_no_symlink_components(&input.path, false, ErrorPhase::Apply)?;
    let pattern = if input.regex {
        input.pattern.clone()
    } else {
        regex::escape(&input.pattern)
    };
    let regex = RegexBuilder::new(&pattern)
        .case_insensitive(!input.case_sensitive)
        .build()
        .map_err(|error| {
            RaxError::new(
                ErrorCode::InvalidRequest,
                ErrorPhase::Validate,
                false,
                format!("invalid grep pattern: {error}"),
            )
        })?;

    let root = workspace.relative(&input.path, true)?;
    let dir = workspace.dir()?;
    let match_limit = usize::try_from(input.max_matches).unwrap_or(usize::MAX);
    let mut matches = Vec::with_capacity(match_limit.min(1024));
    let mut files_scanned = 0_u64;
    let mut skipped_binary = 0_u64;
    let mut skipped_too_large = 0_u64;
    let mut output_truncated = false;

    let summary = visit(
        &dir,
        &root,
        true,
        usize::from(input.max_depth),
        HARD_ENTRY_LIMIT,
        ErrorPhase::Apply,
        |entry| {
            if entry.kind != FsEntryKind::File {
                return Ok(WalkControl::Continue);
            }
            if entry.size > input.max_file_bytes {
                skipped_too_large = skipped_too_large.saturating_add(1);
                return Ok(WalkControl::Continue);
            }
            let bytes = read_complete_file(&dir, &entry.path, input.max_file_bytes)?;
            if bytes.contains(&0) {
                skipped_binary = skipped_binary.saturating_add(1);
                return Ok(WalkControl::Continue);
            }
            let Ok(text) = std::str::from_utf8(&bytes) else {
                skipped_binary = skipped_binary.saturating_add(1);
                return Ok(WalkControl::Continue);
            };
            files_scanned = files_scanned.saturating_add(1);
            let display_path = path_to_protocol(&entry.path);
            for (line_index, line) in text.lines().enumerate() {
                for found in regex.find_iter(line) {
                    if matches.len() == match_limit {
                        output_truncated = true;
                        return Ok(WalkControl::Stop);
                    }
                    matches.push(GrepMatch {
                        path: display_path.clone(),
                        line: u64::try_from(line_index + 1).unwrap_or(u64::MAX),
                        column: u64::try_from(found.start() + 1).unwrap_or(u64::MAX),
                        text: line.to_string(),
                    });
                }
            }
            Ok(WalkControl::Continue)
        },
    )?;
    if summary.hard_limit_reached {
        return Err(operation_error(
            ErrorCode::LimitExceeded,
            ErrorPhase::Apply,
            "grep filesystem traversal exceeded the hard safety limit",
        )
        .with_detail("limit", u64::try_from(HARD_ENTRY_LIMIT).unwrap_or(u64::MAX)));
    }

    Ok(FsGrepOutput {
        matches,
        files_scanned,
        skipped_binary,
        skipped_too_large,
        truncated: output_truncated,
    })
}
