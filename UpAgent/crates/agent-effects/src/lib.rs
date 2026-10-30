#![forbid(unsafe_code)]

mod catalog;
mod projection;
mod replace_text;

use projection::{approval_request, denied_tool_result, execution_from_result};
use replace_text::replace_text_operation;
#[cfg(test)]
use replace_text::validate_replacement_budget;

use std::{
    collections::{BTreeMap, BTreeSet},
    env,
    path::Path,
    sync::Arc,
    time::Duration,
};

use agent_core::{ApprovalRequest, EffectApprovalMode, EffectReceiptRef, ToolCall, ToolResult};
use rax_core::{
    ContentEncoding, FsGrepInput, FsListInput, FsReadInput, FsWriteInput, GitDiffInput,
    GitRepositoryInput, Operation, Outcome, ProcessRunInput, RequestEnvelope, RunState,
};
#[cfg(test)]
use rax_core::{MAX_CONTROLLED_TEXT_BYTES, MAX_EDIT_COUNT};
pub use rax_runtime::CancellationToken as EffectCancellationToken;
use rax_runtime::{Engine, EngineOptions, RecoveryRecord, RecoveryResolution, RunSummary};
use serde::Deserialize;
#[cfg(test)]
use serde_json::Value;
use thiserror::Error;

pub use catalog::tool_definitions;

#[derive(Debug, Error)]
pub enum EffectError {
    #[error("effect contract failed: {0}")]
    Contract(String),
    #[error("RAX failed: {0}")]
    Rax(#[from] rax_core::RaxError),
    #[error("effect serialization failed: {0}")]
    Json(#[from] serde_json::Error),
}

#[derive(Debug, Clone, PartialEq)]
pub enum EffectExecution {
    Completed {
        result: ToolResult,
        receipt: Option<EffectReceiptRef>,
    },
    ApprovalRequired(ApprovalRequest),
    ResolutionRequired {
        result: ToolResult,
        receipt: Option<EffectReceiptRef>,
    },
}

/// The effect recovery projection is intentionally separate from task/session
/// journal recovery. It describes RAX runs and accepts only RAX's verified
/// outcome vocabulary.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct EffectRecoveryEntry {
    pub kind: &'static str,
    pub run_id: String,
    pub request_id: String,
    pub operation: String,
    pub state: RunState,
    pub result_present: bool,
    pub recovery_required: bool,
    pub resolved: bool,
    pub corrupt: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub diagnostic: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EffectRecoveryResolution {
    VerifiedSucceeded,
    VerifiedFailed,
    ManualReviewRequired,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct EffectRecoveryRecord {
    pub kind: &'static str,
    pub run_id: String,
    pub resolution: EffectRecoveryResolution,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    pub resolved_at: String,
}

const EFFECT_RECOVERY_KIND: &str = "effect";

#[derive(Debug, Clone)]
pub struct EffectPlane {
    engine: Arc<Engine>,
}

impl EffectPlane {
    pub fn open(
        workspace: impl AsRef<Path>,
        state_dir: impl AsRef<Path>,
    ) -> Result<Self, EffectError> {
        let engine = Engine::open(
            workspace,
            EngineOptions {
                state_dir: Some(state_dir.as_ref().to_path_buf()),
                plan_ttl: Duration::from_mins(15),
                execution_lock_wait: rax_runtime::DEFAULT_EXECUTION_LOCK_WAIT,
            },
        )?;
        Ok(Self {
            engine: Arc::new(engine),
        })
    }

    pub fn execute(
        &self,
        call: ToolCall,
        approval: EffectApprovalMode,
    ) -> Result<EffectExecution, EffectError> {
        self.execute_with_timeout_limit(call, approval, u64::MAX)
    }

    pub fn execute_with_timeout_limit(
        &self,
        call: ToolCall,
        approval: EffectApprovalMode,
        maximum_process_timeout_ms: u64,
    ) -> Result<EffectExecution, EffectError> {
        let cancellation = self.engine.cancellation_token();
        self.execute_with_timeout_limit_and_cancellation(
            call,
            approval,
            maximum_process_timeout_ms,
            &cancellation,
        )
    }

    pub fn execute_with_timeout_limit_and_cancellation(
        &self,
        call: ToolCall,
        approval: EffectApprovalMode,
        maximum_process_timeout_ms: u64,
        cancellation: &EffectCancellationToken,
    ) -> Result<EffectExecution, EffectError> {
        self.execute_with_grants(
            call,
            approval,
            maximum_process_timeout_ms,
            cancellation,
            &BTreeSet::new(),
        )
    }

    /// Execute one semantic effect, redeeming any operator decision that already
    /// covers it.
    ///
    /// `approved_subjects` holds decisions the caller has already consumed for
    /// this run. A plan whose approval subject is present is applied under the
    /// same one-time RAX grant that `TaskGrant` uses, so the operator's decision
    /// authorises exactly the workspace, operation and preconditions they saw.
    pub fn execute_with_grants(
        &self,
        call: ToolCall,
        approval: EffectApprovalMode,
        maximum_process_timeout_ms: u64,
        cancellation: &EffectCancellationToken,
        approved_subjects: &BTreeSet<String>,
    ) -> Result<EffectExecution, EffectError> {
        if approval == EffectApprovalMode::Deny && !approved_subjects.is_empty() {
            return Err(EffectError::Contract(
                "deny effect approval cannot use redeemed approvals".to_owned(),
            ));
        }
        if maximum_process_timeout_ms == 0 {
            return Err(EffectError::Contract(
                "effect timeout limit must be greater than zero".to_owned(),
            ));
        }
        let operation = operation_for(
            &self.engine,
            &call,
            maximum_process_timeout_ms,
            cancellation,
        )?;
        let request = RequestEnvelope {
            schema_version: rax_core::SCHEMA_VERSION.to_owned(),
            request_id: None,
            operation,
            record: false, // RAX still forces durable records for non-read-only operations.
        };
        let result = self.engine.run_with_cancellation(request, cancellation)?;
        match result.outcome.clone() {
            Outcome::ApprovalRequired { plan } => {
                let subject = plan.approval_subject().map_err(|error| {
                    EffectError::Contract(format!(
                        "plan approval subject could not be computed: {error}"
                    ))
                })?;
                if approved_subjects.contains(&subject) || approval == EffectApprovalMode::TaskGrant
                {
                    let grant = self.engine.approve(&plan.plan_id)?;
                    let applied = self.engine.apply_with_cancellation(
                        &plan.plan_id,
                        &grant.token,
                        cancellation,
                    )?;
                    return execution_from_result(call, applied);
                }
                match approval {
                    EffectApprovalMode::PlanOnly => Ok(EffectExecution::ApprovalRequired(
                        approval_request(&plan, subject),
                    )),
                    EffectApprovalMode::Deny | EffectApprovalMode::TaskGrant => {
                        Ok(EffectExecution::Completed {
                            result: denied_tool_result(&call, &plan),
                            receipt: None,
                        })
                    }
                }
            }
            _ => execution_from_result(call, result),
        }
    }

    pub fn list_recovery(&self) -> Result<Vec<EffectRecoveryEntry>, EffectError> {
        self.engine
            .list_recovery()
            .map(|entries| entries.into_iter().map(effect_recovery_entry).collect())
            .map_err(EffectError::Rax)
    }

    pub fn resolve_recovery(
        &self,
        run_id: &str,
        resolution: EffectRecoveryResolution,
        note: Option<String>,
    ) -> Result<EffectRecoveryRecord, EffectError> {
        let resolution = match resolution {
            EffectRecoveryResolution::VerifiedSucceeded => RecoveryResolution::VerifiedSucceeded,
            EffectRecoveryResolution::VerifiedFailed => RecoveryResolution::VerifiedFailed,
            EffectRecoveryResolution::ManualReviewRequired => {
                RecoveryResolution::ManualReviewRequired
            }
        };
        self.engine
            .resolve_recovery(run_id, resolution, note)
            .map(effect_recovery_record)
            .map_err(EffectError::Rax)
    }
}

fn effect_recovery_entry(summary: RunSummary) -> EffectRecoveryEntry {
    EffectRecoveryEntry {
        kind: EFFECT_RECOVERY_KIND,
        run_id: summary.run_id,
        request_id: summary.request_id,
        operation: summary.operation,
        state: summary.state,
        result_present: summary.result_present,
        recovery_required: summary.recovery_required,
        resolved: summary.resolved,
        corrupt: summary.corrupt,
        diagnostic: summary.diagnostic,
    }
}

fn effect_recovery_record(record: RecoveryRecord) -> EffectRecoveryRecord {
    let resolution = match record.resolution {
        RecoveryResolution::VerifiedSucceeded => EffectRecoveryResolution::VerifiedSucceeded,
        RecoveryResolution::VerifiedFailed => EffectRecoveryResolution::VerifiedFailed,
        RecoveryResolution::ManualReviewRequired => EffectRecoveryResolution::ManualReviewRequired,
    };
    EffectRecoveryRecord {
        kind: EFFECT_RECOVERY_KIND,
        run_id: record.run_id,
        resolution,
        note: record.note,
        resolved_at: record.resolved_at,
    }
}

fn operation_for(
    engine: &Engine,
    call: &ToolCall,
    maximum_process_timeout_ms: u64,
    cancellation: &EffectCancellationToken,
) -> Result<Operation, EffectError> {
    match call.name.as_str() {
        "repo_map" => {
            let input: RepoMapArgs = decode(call)?;
            Ok(Operation::FsList(FsListInput {
                path: input.path,
                recursive: true,
                max_depth: input.max_depth,
                max_entries: input.max_entries,
            }))
        }
        "search_code" => {
            let input: SearchCodeArgs = decode(call)?;
            Ok(Operation::FsGrep(FsGrepInput {
                path: input.path,
                pattern: input.pattern,
                regex: input.regex,
                case_sensitive: input.case_sensitive,
                max_depth: 64,
                max_matches: input.max_matches,
                max_file_bytes: 16 * 1024 * 1024,
            }))
        }
        "read_file" => {
            let input: ReadFileArgs = decode(call)?;
            Ok(Operation::FsRead(FsReadInput {
                path: input.path,
                max_bytes: input.max_bytes,
            }))
        }
        "write_file" => {
            let input: WriteFileArgs = decode(call)?;
            if !input.create_only && input.expected_sha256.is_none() {
                return Err(EffectError::Contract(
                    "write_file replacement requires expectedSha256".to_owned(),
                ));
            }
            Ok(Operation::FsWrite(FsWriteInput {
                path: input.path,
                content: input.content,
                encoding: ContentEncoding::Utf8,
                create_only: input.create_only,
                expected_sha256: input.expected_sha256,
            }))
        }
        "replace_text" => replace_text_operation(engine, call, cancellation),
        "run_check" => {
            let input: RunCheckArgs = decode(call)?;
            Ok(Operation::ProcessRun(ProcessRunInput {
                program: input.program,
                args: input.args,
                cwd: input.cwd,
                env: sanitized_process_environment(),
                clear_env: true,
                timeout_ms: input.timeout_ms.min(maximum_process_timeout_ms),
                max_output_bytes: input.max_output_bytes,
            }))
        }
        "git_status" => {
            let input: GitRepositoryArgs = decode(call)?;
            Ok(Operation::GitStatus(GitRepositoryInput {
                repository: input.repository,
            }))
        }
        "inspect_diff" => {
            let input: InspectDiffArgs = decode(call)?;
            Ok(Operation::GitDiff(GitDiffInput {
                repository: input.repository,
                staged: input.staged,
                paths: input.paths,
                max_bytes: input.max_bytes,
            }))
        }
        other => Err(EffectError::Contract(format!(
            "unknown semantic effect tool {other}"
        ))),
    }
}

fn decode<T: for<'de> Deserialize<'de>>(call: &ToolCall) -> Result<T, EffectError> {
    serde_json::from_value(call.arguments.clone()).map_err(EffectError::Json)
}

fn sanitized_process_environment() -> BTreeMap<String, String> {
    collect_process_environment(|name| env::var(name).ok())
}

fn collect_process_environment(
    mut value_for: impl FnMut(&str) -> Option<String>,
) -> BTreeMap<String, String> {
    const FORWARDED: &[&str] = &[
        "PATH",
        "HOME",
        "USERPROFILE",
        "SYSTEMROOT",
        "WINDIR",
        "COMSPEC",
        "PATHEXT",
        "TMPDIR",
        "TMP",
        "TEMP",
        "LANG",
        "LC_ALL",
        "LC_CTYPE",
        "TERM",
        "NO_COLOR",
        "CI",
        "SOURCE_DATE_EPOCH",
        "XDG_CACHE_HOME",
        "XDG_CONFIG_HOME",
        "XDG_DATA_HOME",
        "CARGO_HOME",
        "RUSTUP_HOME",
        "RUSTC_WRAPPER",
        "SDKROOT",
        "DEVELOPER_DIR",
        "JAVA_HOME",
        "ANDROID_HOME",
        "ANDROID_SDK_ROOT",
        "GRADLE_USER_HOME",
        "GOPATH",
        "GOMODCACHE",
        "GOCACHE",
    ];
    FORWARDED
        .iter()
        .filter_map(|name| value_for(name).map(|value| ((*name).to_owned(), value)))
        .collect()
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct RepoMapArgs {
    #[serde(default)]
    path: String,
    #[serde(default = "default_depth")]
    max_depth: u16,
    #[serde(default = "default_entries")]
    max_entries: u32,
}

const fn default_depth() -> u16 {
    8
}

const fn default_entries() -> u32 {
    10_000
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct SearchCodeArgs {
    #[serde(default)]
    path: String,
    pattern: String,
    #[serde(default = "default_true")]
    regex: bool,
    #[serde(default = "default_true")]
    case_sensitive: bool,
    #[serde(default = "default_matches")]
    max_matches: u32,
}

const fn default_true() -> bool {
    true
}

const fn default_matches() -> u32 {
    10_000
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct ReadFileArgs {
    path: String,
    #[serde(default = "default_read_bytes")]
    max_bytes: u64,
}

const fn default_read_bytes() -> u64 {
    10 * 1024 * 1024
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct WriteFileArgs {
    path: String,
    content: String,
    #[serde(default)]
    create_only: bool,
    #[serde(default)]
    expected_sha256: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct RunCheckArgs {
    /// AXD verification requirement satisfied by this command when it exits
    /// successfully. The runtime records this provenance from the original
    /// tool call; RAX only needs the executable operation.
    #[serde(default, rename = "requirementId")]
    _requirement_id: Option<String>,
    program: String,
    #[serde(default)]
    args: Vec<String>,
    #[serde(default)]
    cwd: Option<String>,
    #[serde(default = "default_timeout")]
    timeout_ms: u64,
    #[serde(default = "default_output_bytes")]
    max_output_bytes: u64,
}

const fn default_timeout() -> u64 {
    120_000
}

const fn default_output_bytes() -> u64 {
    4 * 1024 * 1024
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct GitRepositoryArgs {
    #[serde(default)]
    repository: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct InspectDiffArgs {
    #[serde(default)]
    repository: String,
    #[serde(default)]
    staged: bool,
    #[serde(default)]
    paths: Vec<String>,
    #[serde(default = "default_output_bytes")]
    max_bytes: u64,
}

#[cfg(test)]
mod tests {
    use std::{
        collections::{BTreeMap, BTreeSet},
        fs,
    };

    use agent_core::ToolCallId;
    use serde_json::json;

    use super::*;

    #[test]
    fn strict_effect_schemas_require_every_declared_property() {
        for tool in tool_definitions() {
            let properties = tool
                .input_schema
                .get("properties")
                .and_then(Value::as_object)
                .expect("tool schema properties");
            let required = tool
                .input_schema
                .get("required")
                .and_then(Value::as_array)
                .expect("strict tool schema required list")
                .iter()
                .map(|value| value.as_str().expect("required property name"))
                .collect::<BTreeSet<_>>();
            assert_eq!(
                required,
                properties
                    .keys()
                    .map(String::as_str)
                    .collect::<BTreeSet<_>>(),
                "strict schema for {} must require every property",
                tool.name
            );
        }
    }

    #[test]
    fn read_and_approved_replace_use_the_same_effect_authority()
    -> Result<(), Box<dyn std::error::Error>> {
        let workspace = tempfile::tempdir()?;
        let state = tempfile::tempdir()?;
        fs::write(workspace.path().join("value.txt"), "before")?;
        let plane = EffectPlane::open(workspace.path(), state.path())?;
        let call = ToolCall {
            id: ToolCallId::parse("call-1")?,
            name: "replace_text".to_owned(),
            arguments: json!({
                "path":"value.txt",
                "oldString":"before",
                "newString":"after"
            }),
        };
        let execution = plane.execute(call, EffectApprovalMode::TaskGrant)?;
        assert!(matches!(execution, EffectExecution::Completed { .. }));
        assert_eq!(
            fs::read_to_string(workspace.path().join("value.txt"))?,
            "after"
        );
        Ok(())
    }

    #[test]
    fn deny_dominates_redeemed_subjects_without_mutating() -> Result<(), Box<dyn std::error::Error>>
    {
        let workspace = tempfile::tempdir()?;
        let state = tempfile::tempdir()?;
        let plane = EffectPlane::open(workspace.path(), state.path())?;
        let call = ToolCall {
            id: ToolCallId::parse("call-deny")?,
            name: "write_file".to_owned(),
            arguments: json!({
                "path":"denied.txt",
                "content":"must not be written",
                "createOnly":true,
                "expectedSha256":Value::Null
            }),
        };
        let approved = BTreeSet::from(["exact-redeemed-subject".to_owned()]);
        let error = plane
            .execute_with_grants(
                call,
                EffectApprovalMode::Deny,
                5_000,
                &EffectCancellationToken::default(),
                &approved,
            )
            .expect_err("deny must dominate exact redeemed subjects");
        assert!(matches!(error, EffectError::Contract(message) if message.contains("deny")));
        assert!(!workspace.path().join("denied.txt").exists());
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn effect_recovery_is_listed_and_resolved_through_its_own_projection()
    -> Result<(), Box<dyn std::error::Error>> {
        let workspace = tempfile::tempdir()?;
        let state = tempfile::tempdir()?;
        let plane = EffectPlane::open(workspace.path(), state.path())?;
        let call = ToolCall {
            id: ToolCallId::parse("call-recovery")?,
            name: "run_check".to_owned(),
            arguments: json!({
                "program":"/bin/sh",
                "args":["-c", "exit 7"],
                "cwd":null,
                "timeoutMs":5000,
                "maxOutputBytes":1024
            }),
        };
        let execution = plane.execute(call, EffectApprovalMode::TaskGrant)?;
        let EffectExecution::ResolutionRequired {
            receipt: Some(receipt),
            ..
        } = execution
        else {
            return Err(
                std::io::Error::other("failed process must require effect recovery").into(),
            );
        };
        let listed = plane.list_recovery()?;
        let entry = listed
            .iter()
            .find(|entry| entry.run_id == receipt.id.as_str())
            .ok_or("missing effect recovery entry")?;
        assert_eq!(entry.kind, "effect");
        assert!(entry.recovery_required);
        let record = plane.resolve_recovery(
            &entry.run_id,
            EffectRecoveryResolution::VerifiedFailed,
            Some("expected process failure".to_owned()),
        )?;
        assert_eq!(record.kind, "effect");
        assert_eq!(record.resolution, EffectRecoveryResolution::VerifiedFailed);
        assert!(
            plane
                .list_recovery()?
                .iter()
                .all(|value| value.run_id != entry.run_id)
        );
        Ok(())
    }

    #[test]
    fn replace_text_rejects_replacement_amplification_before_cloning() {
        let count = MAX_EDIT_COUNT;
        let replacement_bytes = (MAX_CONTROLLED_TEXT_BYTES / count) + 1;
        let error = validate_replacement_budget(count, 1, replacement_bytes, count)
            .expect_err("replacement amplification must be bounded");
        assert!(error.to_string().contains("replacement content exceeds"));
    }

    #[test]
    fn replace_text_budget_accepts_a_bounded_size_preserving_edit() {
        validate_replacement_budget(1024, 4, 4, 128)
            .expect("size-preserving edits within the limit must be accepted");
    }

    #[test]
    fn run_check_timeout_is_clamped_to_the_kernel_deadline()
    -> Result<(), Box<dyn std::error::Error>> {
        let workspace = tempfile::tempdir()?;
        let state = tempfile::tempdir()?;
        let plane = EffectPlane::open(workspace.path(), state.path())?;
        let call = ToolCall {
            id: ToolCallId::parse("call-timeout")?,
            name: "run_check".to_owned(),
            arguments: json!({
                "program":"example",
                "timeoutMs":120_000
            }),
        };

        let operation = operation_for(
            &plane.engine,
            &call,
            250,
            &EffectCancellationToken::default(),
        )?;
        let Operation::ProcessRun(input) = operation else {
            return Err(
                std::io::Error::other("run_check did not produce a process operation").into(),
            );
        };
        assert_eq!(input.timeout_ms, 250);
        Ok(())
    }

    #[test]
    fn process_environment_uses_an_explicit_non_secret_allowlist() {
        let values = BTreeMap::from([
            ("PATH".to_owned(), "/bin".to_owned()),
            ("HOME".to_owned(), "/home/test".to_owned()),
            ("PRIVATE_PROVIDER_KEY".to_owned(), "secret".to_owned()),
        ]);
        let environment = collect_process_environment(|name| values.get(name).cloned());
        assert_eq!(environment.get("PATH").map(String::as_str), Some("/bin"));
        assert_eq!(
            environment.get("HOME").map(String::as_str),
            Some("/home/test")
        );
        assert!(!environment.contains_key("PRIVATE_PROVIDER_KEY"));
    }
}
