use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{error::RaxError, operation::ContentEncoding, plan::Plan, policy::EffectDecision};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", content = "data", rename_all = "snake_case")]
pub enum OperationOutput {
    FsRead(FsReadOutput),
    FsStat(FsStatOutput),
    FsList(FsListOutput),
    FsGlob(FsGlobOutput),
    FsGrep(FsGrepOutput),
    FsMutation(FsMutationOutput),
    Process(ProcessOutput),
    GitStatus(GitStatusOutput),
    GitDiff(GitTextOutput),
    GitLog(GitLogOutput),
    GitShow(GitTextOutput),
    GitRemote(GitRemoteOutput),
    GitBranches(GitBranchesOutput),
    GitMutation(GitMutationOutput),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum FileContentKind {
    Text,
    Binary,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct FsReadOutput {
    pub path: String,
    pub content_kind: FileContentKind,
    pub encoding: String,
    pub content: String,
    pub size: u64,
    pub sha256: String,
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum FsEntryKind {
    File,
    Directory,
    Symlink,
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct FsStatOutput {
    pub path: String,
    pub kind: FsEntryKind,
    pub size: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub modified_unix_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct FsEntry {
    pub path: String,
    pub kind: FsEntryKind,
    pub size: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct FsListOutput {
    pub entries: Vec<FsEntry>,
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct FsGlobOutput {
    pub paths: Vec<String>,
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct GrepMatch {
    pub path: String,
    pub line: u64,
    pub column: u64,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct FsGrepOutput {
    pub matches: Vec<GrepMatch>,
    pub files_scanned: u64,
    pub skipped_binary: u64,
    pub skipped_too_large: u64,
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct FsMutationOutput {
    pub changed_paths: Vec<String>,
    #[serde(default)]
    pub before_sha256: BTreeMap<String, String>,
    #[serde(default)]
    pub after_sha256: BTreeMap<String, String>,
    pub verified: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ProcessOutput {
    pub program: String,
    pub args: Vec<String>,
    pub exit_code: Option<i32>,
    pub success: bool,
    pub timed_out: bool,
    pub cancelled: bool,
    pub duration_ms: u64,
    pub stdout_encoding: ContentEncoding,
    pub stdout: String,
    pub stderr_encoding: ContentEncoding,
    pub stderr: String,
    pub stdout_bytes: u64,
    pub stderr_bytes: u64,
    pub stdout_truncated: bool,
    pub stderr_truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct GitChangedFile {
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub original_path: Option<String>,
    pub index_status: String,
    pub worktree_status: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct GitStatusOutput {
    pub repository: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upstream: Option<String>,
    pub ahead: u64,
    pub behind: u64,
    pub detached: bool,
    pub files: Vec<GitChangedFile>,
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct GitTextOutput {
    pub encoding: ContentEncoding,
    pub text: String,
    pub bytes: u64,
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct GitLogEntry {
    pub sha: String,
    pub parents: Vec<String>,
    pub author_name: String,
    pub author_email: String,
    pub timestamp: i64,
    pub subject: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct GitLogOutput {
    pub entries: Vec<GitLogEntry>,
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct GitRemoteEntry {
    pub name: String,
    pub url: String,
    pub direction: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct GitRemoteOutput {
    pub remotes: Vec<GitRemoteEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct GitBranchEntry {
    pub name: String,
    pub current: bool,
    pub remote: bool,
    pub object_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct GitBranchesOutput {
    pub branches: Vec<GitBranchEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct GitMutationOutput {
    pub repository: String,
    pub action: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head_before: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head_after: Option<String>,
    pub verified: bool,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub details: BTreeMap<String, Value>,
}

#[allow(
    clippy::large_enum_variant,
    reason = "Boxing Plan would change the public result contract without reducing wire size"
)]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum Outcome {
    Succeeded {
        output: OperationOutput,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        receipt_path: Option<String>,
    },
    ApprovalRequired {
        plan: Plan,
    },
    Failed {
        error: RaxError,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        receipt_path: Option<String>,
    },
    Cancelled {
        error: RaxError,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        receipt_path: Option<String>,
    },
    Denied {
        error: RaxError,
    },
    Expired {
        error: RaxError,
    },
    PartiallyApplied {
        error: RaxError,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        receipt_path: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ResultEnvelope {
    pub schema_version: String,
    pub request_id: String,
    pub run_id: String,
    pub operation: String,
    pub started_at: String,
    pub finished_at: String,
    pub duration_ms: u64,
    pub outcome: Outcome,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct EventEnvelope {
    pub schema_version: String,
    pub run_id: String,
    pub sequence: u64,
    pub timestamp: String,
    pub state: crate::lifecycle::RunState,
    pub event: crate::lifecycle::EventKind,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub data: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct DescribeOutput {
    pub product: String,
    pub version: String,
    pub schema_version: String,
    pub operations: Vec<String>,
    pub effects: Vec<String>,
    pub adapters: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ApprovalSummary {
    pub plan_id: String,
    pub fingerprint: String,
    pub expires_at: String,
    pub effects: Vec<EffectDecision>,
}
