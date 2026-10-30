use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::error::{ErrorCode, ErrorPhase, RaxError};

pub const DEFAULT_READ_LIMIT: u64 = 10 * 1024 * 1024;
pub const DEFAULT_OUTPUT_LIMIT: u64 = 4 * 1024 * 1024;
pub const DEFAULT_ENTRY_LIMIT: u32 = 10_000;
pub const DEFAULT_MATCH_LIMIT: u32 = 10_000;
pub const DEFAULT_MAX_DEPTH: u16 = 64;
pub const DEFAULT_TIMEOUT_MS: u64 = 120_000;

pub const MAX_READ_LIMIT: u64 = 64 * 1024 * 1024;
pub const MAX_OUTPUT_LIMIT: u64 = 64 * 1024 * 1024;
pub const MAX_ENTRY_LIMIT: u32 = 100_000;
pub const MAX_MATCH_LIMIT: u32 = 100_000;
pub const MAX_MAX_DEPTH: u16 = 256;
pub const MAX_TIMEOUT_MS: u64 = 24 * 60 * 60 * 1000;
pub const MAX_EDIT_COUNT: usize = 100_000;
pub const MAX_CONTROLLED_TEXT_BYTES: usize = 64 * 1024 * 1024;
pub const MAX_PROCESS_ARGUMENTS: usize = 4096;
pub const MAX_ENVIRONMENT_ENTRIES: usize = 1024;
pub const MAX_PATH_BYTES: usize = 4096;
pub const MAX_PATTERN_BYTES: usize = 64 * 1024;
pub const MAX_PROGRAM_BYTES: usize = 4096;
pub const MAX_REVISION_BYTES: usize = 4096;
pub const MAX_ENVIRONMENT_KEY_BYTES: usize = 4096;

pub const OPERATION_NAMES: &[&str] = &[
    "fs_read",
    "fs_stat",
    "fs_list",
    "fs_glob",
    "fs_grep",
    "fs_write",
    "fs_patch",
    "fs_mkdir",
    "fs_move",
    "fs_copy",
    "fs_remove",
    "process_run",
    "process_shell",
    "git_status",
    "git_diff",
    "git_log",
    "git_show",
    "git_remote",
    "git_branches",
    "git_stage",
    "git_unstage",
    "git_discard",
    "git_commit",
    "git_checkout",
    "git_fetch",
    "git_pull_ff",
    "git_push",
];

fn default_read_limit() -> u64 {
    DEFAULT_READ_LIMIT
}
fn default_output_limit() -> u64 {
    DEFAULT_OUTPUT_LIMIT
}
fn default_entry_limit() -> u32 {
    DEFAULT_ENTRY_LIMIT
}
fn default_match_limit() -> u32 {
    DEFAULT_MATCH_LIMIT
}
fn default_max_depth() -> u16 {
    DEFAULT_MAX_DEPTH
}
fn default_timeout_ms() -> u64 {
    DEFAULT_TIMEOUT_MS
}
fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", content = "input", rename_all = "snake_case")]
pub enum Operation {
    FsRead(FsReadInput),
    FsStat(FsStatInput),
    FsList(FsListInput),
    FsGlob(FsGlobInput),
    FsGrep(FsGrepInput),
    FsWrite(FsWriteInput),
    FsPatch(FsPatchInput),
    FsMkdir(FsMkdirInput),
    FsMove(FsMoveInput),
    FsCopy(FsCopyInput),
    FsRemove(FsRemoveInput),
    ProcessRun(ProcessRunInput),
    ProcessShell(ProcessShellInput),
    GitStatus(GitRepositoryInput),
    GitDiff(GitDiffInput),
    GitLog(GitLogInput),
    GitShow(GitShowInput),
    GitRemote(GitRepositoryInput),
    GitBranches(GitRepositoryInput),
    GitStage(GitPathsInput),
    GitUnstage(GitPathsInput),
    GitDiscard(GitDiscardInput),
    GitCommit(GitCommitInput),
    GitCheckout(GitCheckoutInput),
    GitFetch(GitRemoteInput),
    GitPullFf(GitRemoteInput),
    GitPush(GitPushInput),
}

impl Operation {
    pub fn validate(&self) -> Result<(), RaxError> {
        match self {
            Self::FsRead(v) => {
                validate_relative_path(&v.path, false)?;
                validate_bounded_limit("maxBytes", v.max_bytes, MAX_READ_LIMIT)?;
            }
            Self::FsStat(v) => {
                validate_relative_path(&v.path, true)?;
                if v.follow_symlinks {
                    return Err(invalid(
                        "followSymlinks is not supported; Rax v1 never follows filesystem links",
                    ));
                }
            }
            Self::FsList(v) => {
                validate_relative_path(&v.path, true)?;
                validate_bounded_limit(
                    "maxEntries",
                    u64::from(v.max_entries),
                    u64::from(MAX_ENTRY_LIMIT),
                )?;
                validate_bounded_limit(
                    "maxDepth",
                    u64::from(v.max_depth),
                    u64::from(MAX_MAX_DEPTH),
                )?;
            }
            Self::FsGlob(v) => {
                validate_relative_path(&v.path, true)?;
                validate_pattern(&v.pattern)?;
                validate_bounded_limit(
                    "maxEntries",
                    u64::from(v.max_entries),
                    u64::from(MAX_ENTRY_LIMIT),
                )?;
                validate_bounded_limit(
                    "maxDepth",
                    u64::from(v.max_depth),
                    u64::from(MAX_MAX_DEPTH),
                )?;
            }
            Self::FsGrep(v) => {
                validate_relative_path(&v.path, true)?;
                validate_pattern(&v.pattern)?;
                validate_bounded_limit(
                    "maxMatches",
                    u64::from(v.max_matches),
                    u64::from(MAX_MATCH_LIMIT),
                )?;
                validate_bounded_limit("maxFileBytes", v.max_file_bytes, MAX_READ_LIMIT)?;
                validate_bounded_limit(
                    "maxDepth",
                    u64::from(v.max_depth),
                    u64::from(MAX_MAX_DEPTH),
                )?;
            }
            Self::FsWrite(v) => {
                validate_relative_path(&v.path, false)?;
                validate_hash(v.expected_sha256.as_deref())?;
                if v.create_only && v.expected_sha256.is_some() {
                    return Err(invalid(
                        "createOnly and expectedSha256 are mutually exclusive",
                    ));
                }
                validate_controlled_text("content", &v.content)?;
            }
            Self::FsPatch(v) => {
                validate_relative_path(&v.path, false)?;
                validate_hash(Some(&v.expected_sha256))?;
                validate_bounded_limit("maxBytes", v.max_bytes, MAX_READ_LIMIT)?;
                if v.edits.is_empty() {
                    return Err(invalid("edits must not be empty"));
                }
                if v.edits.len() > MAX_EDIT_COUNT {
                    return Err(invalid(format!(
                        "edits must contain at most {MAX_EDIT_COUNT} entries"
                    )));
                }
                let mut replacement_bytes = 0_usize;
                for edit in &v.edits {
                    if edit.start > edit.end {
                        return Err(invalid("text edit start must be <= end"));
                    }
                    replacement_bytes = replacement_bytes
                        .checked_add(edit.replacement.len())
                        .ok_or_else(|| invalid("replacement content size overflowed"))?;
                    if replacement_bytes > MAX_CONTROLLED_TEXT_BYTES {
                        return Err(invalid(format!(
                            "replacement content must be at most {MAX_CONTROLLED_TEXT_BYTES} bytes"
                        )));
                    }
                }
            }
            Self::FsMkdir(v) => validate_relative_path(&v.path, false)?,
            Self::FsMove(v) => {
                validate_relative_path(&v.from, false)?;
                validate_relative_path(&v.to, false)?;
                if v.from == v.to {
                    return Err(invalid("move source and target must differ"));
                }
            }
            Self::FsCopy(v) => {
                validate_relative_path(&v.from, false)?;
                validate_relative_path(&v.to, false)?;
                if v.from == v.to {
                    return Err(invalid("copy source and target must differ"));
                }
            }
            Self::FsRemove(v) => {
                validate_relative_path(&v.path, false)?;
                validate_hash(v.expected_sha256.as_deref())?;
            }
            Self::ProcessRun(v) => validate_process(
                &v.program,
                &v.args,
                v.cwd.as_deref(),
                &v.env,
                v.timeout_ms,
                v.max_output_bytes,
            )?,
            Self::ProcessShell(v) => {
                if v.command.trim().is_empty() || v.command.contains('\0') {
                    return Err(invalid(
                        "shell command must be non-empty and contain no NUL",
                    ));
                }
                validate_controlled_text("shell command", &v.command)?;
                validate_process(
                    "shell",
                    &[],
                    v.cwd.as_deref(),
                    &v.env,
                    v.timeout_ms,
                    v.max_output_bytes,
                )?;
            }
            Self::GitStatus(v) | Self::GitRemote(v) | Self::GitBranches(v) => {
                validate_relative_path(&v.repository, true)?;
            }
            Self::GitDiff(v) => {
                validate_relative_path(&v.repository, true)?;
                for path in &v.paths {
                    validate_git_path(path)?;
                }
                validate_bounded_limit("maxBytes", v.max_bytes, MAX_OUTPUT_LIMIT)?;
            }
            Self::GitLog(v) => {
                validate_relative_path(&v.repository, true)?;
                validate_bounded_limit(
                    "maxEntries",
                    u64::from(v.max_entries),
                    u64::from(MAX_ENTRY_LIMIT),
                )?;
            }
            Self::GitShow(v) => {
                validate_relative_path(&v.repository, true)?;
                validate_revision(&v.revision)?;
                validate_bounded_limit("maxBytes", v.max_bytes, MAX_OUTPUT_LIMIT)?;
            }
            Self::GitStage(v) | Self::GitUnstage(v) => {
                validate_git_paths(&v.repository, &v.paths)?;
            }
            Self::GitDiscard(v) => validate_git_paths(&v.repository, &v.paths)?,
            Self::GitCommit(v) => {
                validate_relative_path(&v.repository, true)?;
                if v.message.trim().is_empty() || v.message.contains('\0') {
                    return Err(invalid(
                        "commit message must be non-empty and contain no NUL",
                    ));
                }
                validate_controlled_text("commit message", &v.message)?;
            }
            Self::GitCheckout(v) => {
                validate_relative_path(&v.repository, true)?;
                validate_revision(&v.target)?;
            }
            Self::GitFetch(v) | Self::GitPullFf(v) => {
                validate_relative_path(&v.repository, true)?;
                if let Some(remote) = &v.remote {
                    validate_revision(remote)?;
                }
                validate_bounded_limit("timeoutMs", v.timeout_ms, MAX_TIMEOUT_MS)?;
            }
            Self::GitPush(v) => {
                validate_relative_path(&v.repository, true)?;
                if let Some(remote) = &v.remote {
                    validate_revision(remote)?;
                }
                if let Some(branch) = &v.branch {
                    validate_revision(branch)?;
                }
                validate_bounded_limit("timeoutMs", v.timeout_ms, MAX_TIMEOUT_MS)?;
            }
        }
        Ok(())
    }

    #[must_use]
    pub const fn is_read_only(&self) -> bool {
        matches!(
            self,
            Self::FsRead(_)
                | Self::FsStat(_)
                | Self::FsList(_)
                | Self::FsGlob(_)
                | Self::FsGrep(_)
                | Self::GitStatus(_)
                | Self::GitDiff(_)
                | Self::GitLog(_)
                | Self::GitShow(_)
                | Self::GitRemote(_)
                | Self::GitBranches(_)
        )
    }

    #[must_use]
    pub const fn operation_name(&self) -> &'static str {
        match self {
            Self::FsRead(_) => "fs_read",
            Self::FsStat(_) => "fs_stat",
            Self::FsList(_) => "fs_list",
            Self::FsGlob(_) => "fs_glob",
            Self::FsGrep(_) => "fs_grep",
            Self::FsWrite(_) => "fs_write",
            Self::FsPatch(_) => "fs_patch",
            Self::FsMkdir(_) => "fs_mkdir",
            Self::FsMove(_) => "fs_move",
            Self::FsCopy(_) => "fs_copy",
            Self::FsRemove(_) => "fs_remove",
            Self::ProcessRun(_) => "process_run",
            Self::ProcessShell(_) => "process_shell",
            Self::GitStatus(_) => "git_status",
            Self::GitDiff(_) => "git_diff",
            Self::GitLog(_) => "git_log",
            Self::GitShow(_) => "git_show",
            Self::GitRemote(_) => "git_remote",
            Self::GitBranches(_) => "git_branches",
            Self::GitStage(_) => "git_stage",
            Self::GitUnstage(_) => "git_unstage",
            Self::GitDiscard(_) => "git_discard",
            Self::GitCommit(_) => "git_commit",
            Self::GitCheckout(_) => "git_checkout",
            Self::GitFetch(_) => "git_fetch",
            Self::GitPullFf(_) => "git_pull_ff",
            Self::GitPush(_) => "git_push",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct FsReadInput {
    pub path: String,
    #[serde(default = "default_read_limit")]
    pub max_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct FsStatInput {
    pub path: String,
    #[serde(default)]
    pub follow_symlinks: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct FsListInput {
    #[serde(default)]
    pub path: String,
    #[serde(default)]
    pub recursive: bool,
    #[serde(default = "default_max_depth")]
    pub max_depth: u16,
    #[serde(default = "default_entry_limit")]
    pub max_entries: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct FsGlobInput {
    #[serde(default)]
    pub path: String,
    pub pattern: String,
    #[serde(default = "default_max_depth")]
    pub max_depth: u16,
    #[serde(default = "default_entry_limit")]
    pub max_entries: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct FsGrepInput {
    #[serde(default)]
    pub path: String,
    pub pattern: String,
    #[serde(default = "default_true")]
    pub regex: bool,
    #[serde(default = "default_true")]
    pub case_sensitive: bool,
    #[serde(default = "default_max_depth")]
    pub max_depth: u16,
    #[serde(default = "default_match_limit")]
    pub max_matches: u32,
    #[serde(default = "default_read_limit")]
    pub max_file_bytes: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ContentEncoding {
    Utf8,
    Base64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct FsWriteInput {
    pub path: String,
    pub content: String,
    #[serde(default = "default_content_encoding")]
    pub encoding: ContentEncoding,
    #[serde(default)]
    pub create_only: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_sha256: Option<String>,
}

const fn default_content_encoding() -> ContentEncoding {
    ContentEncoding::Utf8
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct TextEdit {
    pub start: u64,
    pub end: u64,
    pub replacement: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct FsPatchInput {
    pub path: String,
    pub expected_sha256: String,
    #[serde(default = "default_read_limit")]
    pub max_bytes: u64,
    pub edits: Vec<TextEdit>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct FsMkdirInput {
    pub path: String,
    #[serde(default = "default_true")]
    pub recursive: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct FsMoveInput {
    pub from: String,
    pub to: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct FsCopyInput {
    pub from: String,
    pub to: String,
    #[serde(default)]
    pub recursive: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct FsRemoveInput {
    pub path: String,
    #[serde(default)]
    pub recursive: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_sha256: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ProcessRunInput {
    pub program: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default)]
    pub clear_env: bool,
    #[serde(default = "default_timeout_ms")]
    pub timeout_ms: u64,
    #[serde(default = "default_output_limit")]
    pub max_output_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ProcessShellInput {
    pub command: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default)]
    pub clear_env: bool,
    #[serde(default = "default_timeout_ms")]
    pub timeout_ms: u64,
    #[serde(default = "default_output_limit")]
    pub max_output_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct GitRepositoryInput {
    #[serde(default)]
    pub repository: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct GitDiffInput {
    #[serde(default)]
    pub repository: String,
    #[serde(default)]
    pub staged: bool,
    #[serde(default)]
    pub paths: Vec<String>,
    #[serde(default = "default_output_limit")]
    pub max_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct GitLogInput {
    #[serde(default)]
    pub repository: String,
    #[serde(default = "default_log_entries")]
    pub max_entries: u32,
}

const fn default_log_entries() -> u32 {
    100
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct GitShowInput {
    #[serde(default)]
    pub repository: String,
    pub revision: String,
    #[serde(default = "default_output_limit")]
    pub max_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct GitPathsInput {
    #[serde(default)]
    pub repository: String,
    pub paths: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct GitDiscardInput {
    #[serde(default)]
    pub repository: String,
    pub paths: Vec<String>,
    #[serde(default)]
    pub include_untracked: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct GitCommitInput {
    #[serde(default)]
    pub repository: String,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct GitCheckoutInput {
    #[serde(default)]
    pub repository: String,
    pub target: String,
    #[serde(default)]
    pub create: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct GitRemoteInput {
    #[serde(default)]
    pub repository: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote: Option<String>,
    #[serde(default = "default_timeout_ms")]
    pub timeout_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct GitPushInput {
    #[serde(default)]
    pub repository: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    #[serde(default)]
    pub set_upstream: bool,
    #[serde(default = "default_timeout_ms")]
    pub timeout_ms: u64,
}

fn validate_process(
    program: &str,
    args: &[String],
    cwd: Option<&str>,
    env: &BTreeMap<String, String>,
    timeout_ms: u64,
    max_output_bytes: u64,
) -> Result<(), RaxError> {
    if program.trim().is_empty() || program.contains('\0') {
        return Err(invalid("program must be non-empty and contain no NUL"));
    }
    if program.len() > MAX_PROGRAM_BYTES {
        return Err(invalid(format!(
            "program must be at most {MAX_PROGRAM_BYTES} bytes"
        )));
    }
    if args.len() > MAX_PROCESS_ARGUMENTS {
        return Err(invalid(format!(
            "process args must contain at most {MAX_PROCESS_ARGUMENTS} entries"
        )));
    }
    let mut argument_bytes = 0_usize;
    for arg in args {
        if arg.contains('\0') {
            return Err(invalid("process arguments must contain no NUL"));
        }
        argument_bytes = argument_bytes
            .checked_add(arg.len())
            .ok_or_else(|| invalid("process argument size overflowed"))?;
        if argument_bytes > MAX_CONTROLLED_TEXT_BYTES {
            return Err(invalid(format!(
                "process arguments must total at most {MAX_CONTROLLED_TEXT_BYTES} bytes"
            )));
        }
    }
    if let Some(cwd) = cwd {
        validate_relative_path(cwd, true)?;
    }
    if env.len() > MAX_ENVIRONMENT_ENTRIES {
        return Err(invalid(format!(
            "environment must contain at most {MAX_ENVIRONMENT_ENTRIES} entries"
        )));
    }
    let mut environment_bytes = 0_usize;
    for (key, value) in env {
        if key.is_empty() || key.contains('=') || key.contains('\0') || value.contains('\0') {
            return Err(invalid("environment keys/values are invalid"));
        }
        if key.len() > MAX_ENVIRONMENT_KEY_BYTES {
            return Err(invalid(format!(
                "environment keys must be at most {MAX_ENVIRONMENT_KEY_BYTES} bytes"
            )));
        }
        environment_bytes = environment_bytes
            .checked_add(key.len())
            .and_then(|bytes| bytes.checked_add(value.len()))
            .ok_or_else(|| invalid("environment size overflowed"))?;
        if environment_bytes > MAX_CONTROLLED_TEXT_BYTES {
            return Err(invalid(format!(
                "environment must total at most {MAX_CONTROLLED_TEXT_BYTES} bytes"
            )));
        }
    }
    validate_bounded_limit("timeoutMs", timeout_ms, MAX_TIMEOUT_MS)?;
    validate_bounded_limit("maxOutputBytes", max_output_bytes, MAX_OUTPUT_LIMIT)
}

fn validate_git_paths(repository: &str, paths: &[String]) -> Result<(), RaxError> {
    validate_relative_path(repository, true)?;
    if paths.is_empty() {
        return Err(invalid("git paths must not be empty"));
    }
    for path in paths {
        validate_git_path(path)?;
    }
    Ok(())
}

fn validate_git_path(path: &str) -> Result<(), RaxError> {
    validate_relative_path(path, false)?;
    if path.contains(':') || path.chars().any(char::is_control) {
        return Err(invalid("git path contains a prohibited character"));
    }
    Ok(())
}

fn validate_revision(value: &str) -> Result<(), RaxError> {
    if value.len() > MAX_REVISION_BYTES {
        return Err(invalid(format!(
            "Git revision/name must be at most {MAX_REVISION_BYTES} bytes"
        )));
    }
    if value.is_empty()
        || value.starts_with('-')
        || value.contains('\0')
        || value.chars().any(char::is_control)
    {
        return Err(invalid("Git revision/name is invalid"));
    }
    Ok(())
}

fn validate_pattern(pattern: &str) -> Result<(), RaxError> {
    if pattern.len() > MAX_PATTERN_BYTES {
        return Err(invalid(format!(
            "pattern must be at most {MAX_PATTERN_BYTES} bytes"
        )));
    }
    if pattern.is_empty() || pattern.contains('\0') {
        return Err(invalid("pattern must be non-empty and contain no NUL"));
    }
    Ok(())
}

fn validate_hash(value: Option<&str>) -> Result<(), RaxError> {
    if let Some(value) = value
        && (value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()))
    {
        return Err(invalid("SHA-256 must be exactly 64 hexadecimal characters"));
    }
    Ok(())
}

fn validate_bounded_limit(name: &str, value: u64, maximum: u64) -> Result<(), RaxError> {
    if value == 0 || value > maximum {
        return Err(invalid(format!("{name} must be between 1 and {maximum}")));
    }
    Ok(())
}

fn validate_controlled_text(name: &str, value: &str) -> Result<(), RaxError> {
    if value.len() > MAX_CONTROLLED_TEXT_BYTES {
        return Err(invalid(format!(
            "{name} must be at most {MAX_CONTROLLED_TEXT_BYTES} bytes"
        )));
    }
    Ok(())
}

pub fn validate_relative_path(path: &str, allow_empty: bool) -> Result<(), RaxError> {
    if path.len() > MAX_PATH_BYTES {
        return Err(invalid(format!(
            "path must be at most {MAX_PATH_BYTES} bytes"
        )));
    }
    if path.contains('\0') {
        return Err(invalid("path contains NUL"));
    }
    if path.is_empty() {
        return if allow_empty {
            Ok(())
        } else {
            Err(invalid("path must not be empty"))
        };
    }
    let parsed = std::path::Path::new(path);
    if parsed.is_absolute() {
        return Err(invalid("absolute paths are not allowed"));
    }
    for component in parsed.components() {
        if !matches!(component, std::path::Component::Normal(_)) {
            return Err(invalid("path may contain only normal relative components"));
        }
    }
    Ok(())
}

fn invalid(message: impl Into<String>) -> RaxError {
    RaxError::new(
        ErrorCode::InvalidRequest,
        ErrorPhase::Validate,
        false,
        message,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_validation_rejects_escape_and_absolute_paths() {
        assert!(validate_relative_path("../secret", false).is_err());
        assert!(validate_relative_path("a/../secret", false).is_err());
        assert!(validate_relative_path("/etc/passwd", false).is_err());
        assert!(validate_relative_path("src/main.rs", false).is_ok());
        assert!(validate_relative_path(&"a".repeat(MAX_PATH_BYTES + 1), false).is_err());
    }

    #[test]
    fn stat_rejects_link_following() {
        let operation = Operation::FsStat(FsStatInput {
            path: "link".to_string(),
            follow_symlinks: true,
        });
        assert!(operation.validate().is_err());
    }

    #[test]
    fn unknown_fields_are_rejected() {
        let json = r#"{
          "kind":"fs_read",
          "input":{"path":"a.txt","unexpected":true}
        }"#;
        assert!(serde_json::from_str::<Operation>(json).is_err());
    }
}
