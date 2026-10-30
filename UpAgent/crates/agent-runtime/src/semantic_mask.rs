//! Effect boundary for semantic natural-image masks.
//!
//! `UpAgent` owns the request identity and state transition.  This module only
//! invokes an explicitly supplied adapter; the default adapter reports an
//! unavailable capability and never fabricates a mask.

use agent_core::{
    AgentFailure, FailureCode, SEMANTIC_MASK_CONTRACT_SCHEMA, SEMANTIC_MASK_HELPER_MAX_TIMEOUT_MS,
    SEMANTIC_MASK_HELPER_MIN_TIMEOUT_MS, SemanticMaskOperation, SemanticMaskResult,
    SemanticMaskResultStatus, SemanticMaskSpec,
};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File},
    io::{Read, Write},
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};
use tokio::{
    io::{AsyncRead, AsyncReadExt as _},
    process::{Child, Command},
    task::JoinHandle,
};
use tokio_util::sync::CancellationToken;

use crate::artifact::ArtifactStore;

pub const SEMANTIC_MASK_CAPABILITY: &str = "extract.semantic_mask";
pub const SEMANTIC_MASK_PROCESS_REQUEST_SCHEMA: &str = "upagent.semantic-mask/process-request/1";
pub const SEMANTIC_MASK_PROCESS_RESULT_SCHEMA: &str = "upagent.semantic-mask/process-result/1";
const MAX_PROCESS_REQUEST_BYTES: usize = 512 * 1024;
const MAX_PROCESS_RESULT_BYTES: usize = 256 * 1024;
const MAX_PROCESS_STDERR_BYTES: usize = 64 * 1024;
const MAX_MASK_BYTES: usize = 64 * 1024 * 1024;
const PROCESS_DRAIN_TIMEOUT: Duration = Duration::from_secs(2);
const PROCESS_CLEANUP_TIMEOUT: Duration = Duration::from_secs(2);
const PROCESS_CLEANUP_POLL: Duration = Duration::from_millis(10);

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum SemanticMaskTarget {
    Point {
        x_millionths: u32,
        y_millionths: u32,
    },
    Box {
        x_millionths: u32,
        y_millionths: u32,
        width_millionths: u32,
        height_millionths: u32,
    },
    Scribble {
        points: Vec<[u32; 2]>,
    },
}

impl SemanticMaskTarget {
    pub fn validate(&self) -> Result<(), String> {
        match self {
            Self::Point {
                x_millionths,
                y_millionths,
            } if *x_millionths <= 1_000_000 && *y_millionths <= 1_000_000 => Ok(()),
            Self::Box {
                x_millionths,
                y_millionths,
                width_millionths,
                height_millionths,
            } if *x_millionths <= 1_000_000
                && *y_millionths <= 1_000_000
                && *width_millionths > 0
                && *height_millionths > 0
                && x_millionths.saturating_add(*width_millionths) <= 1_000_000
                && y_millionths.saturating_add(*height_millionths) <= 1_000_000 =>
            {
                Ok(())
            }
            Self::Scribble { points }
                if !points.is_empty()
                    && points.len() <= 1024
                    && points
                        .iter()
                        .all(|point| point[0] <= 1_000_000 && point[1] <= 1_000_000) =>
            {
                Ok(())
            }
            _ => Err("semantic mask target is outside bounded normalized coordinates".to_owned()),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct SemanticMaskRequest {
    pub operation: SemanticMaskOperation,
    pub target: SemanticMaskTarget,
}

#[async_trait]
pub trait SemanticMaskAdapter: Send + Sync + std::fmt::Debug {
    async fn infer(
        &self,
        request: SemanticMaskRequest,
        cancellation: &CancellationToken,
    ) -> SemanticMaskResult;
}

/// A real external semantic-mask effect boundary.
///
/// The helper is deliberately not discovered from `PATH`: callers must supply
/// a regular executable, its expected SHA-256, and a stable identity/version.
/// The helper receives a bounded JSON request file and writes a bounded PNG
/// mask to the path named in that request.  It cannot mutate the director;
/// its result re-enters the normal semantic-mask event path.
#[derive(Debug, Clone)]
pub struct ProcessSemanticMaskAdapter {
    executable: PathBuf,
    executable_sha256: String,
    helper_identity: String,
    helper_version: String,
    artifacts: ArtifactStore,
    timeout: Duration,
}

impl ProcessSemanticMaskAdapter {
    /// Resolve and pin the configured helper during composition. This performs
    /// only the executable identity check; artifact state is supplied later by
    /// the runtime composition root.
    pub fn resolve_spec(spec: &SemanticMaskSpec) -> Result<SemanticMaskSpec, String> {
        let (executable, executable_sha256) =
            resolve_executable(&spec.executable, spec.executable_sha256.as_str())?;
        let helper_identity = bounded_identity(spec.helper_identity.clone(), "helperIdentity")?;
        let helper_version = bounded_identity(spec.helper_version.clone(), "helperVersion")?;
        if !(SEMANTIC_MASK_HELPER_MIN_TIMEOUT_MS..=SEMANTIC_MASK_HELPER_MAX_TIMEOUT_MS)
            .contains(&spec.timeout_ms)
        {
            return Err(format!(
                "semantic-mask helper timeout is outside {SEMANTIC_MASK_HELPER_MIN_TIMEOUT_MS}..={SEMANTIC_MASK_HELPER_MAX_TIMEOUT_MS}ms"
            ));
        }
        Ok(SemanticMaskSpec {
            executable,
            executable_sha256,
            helper_identity,
            helper_version,
            timeout_ms: spec.timeout_ms,
        })
    }

    /// Build the configured adapter with the runtime's content-addressed
    /// artifact store. No helper is discovered from PATH.
    pub fn from_spec(spec: &SemanticMaskSpec, artifacts: ArtifactStore) -> Result<Self, String> {
        let resolved = Self::resolve_spec(spec)?;
        Ok(Self {
            executable: resolved.executable,
            executable_sha256: resolved.executable_sha256,
            helper_identity: resolved.helper_identity,
            helper_version: resolved.helper_version,
            artifacts,
            timeout: Duration::from_millis(resolved.timeout_ms),
        })
    }

    /// Construct an adapter only for a pinned, regular executable.
    pub fn new(
        executable: impl Into<PathBuf>,
        executable_sha256: impl Into<String>,
        helper_identity: impl Into<String>,
        helper_version: impl Into<String>,
        artifacts: ArtifactStore,
        timeout: Duration,
    ) -> Result<Self, String> {
        let executable = executable.into();
        let (executable, executable_sha256) = resolve_executable(&executable, executable_sha256)?;
        let helper_identity = bounded_identity(helper_identity.into(), "helperIdentity")?;
        let helper_version = bounded_identity(helper_version.into(), "helperVersion")?;
        if !(Duration::from_millis(1)..=Duration::from_mins(10)).contains(&timeout) {
            return Err("semantic-mask helper timeout is outside 1ms..=10min".to_owned());
        }
        Ok(Self {
            executable,
            executable_sha256,
            helper_identity,
            helper_version,
            artifacts,
            timeout,
        })
    }

    #[must_use]
    pub fn executable(&self) -> &Path {
        &self.executable
    }

    #[must_use]
    pub fn executable_sha256(&self) -> &str {
        &self.executable_sha256
    }

    #[must_use]
    pub fn helper_identity(&self) -> &str {
        &self.helper_identity
    }

    #[must_use]
    pub fn helper_version(&self) -> &str {
        &self.helper_version
    }

    async fn invoke(
        &self,
        request: &SemanticMaskRequest,
        source_bytes: &[u8],
        cancellation: &CancellationToken,
    ) -> Result<Vec<u8>, ProcessInvocationError> {
        if cancellation.is_cancelled() {
            return Err(ProcessInvocationError::Cancelled);
        }
        let temporary = tempfile::tempdir().map_err(ProcessInvocationError::Io)?;
        let source_path = temporary.path().join("source.png");
        let request_path = temporary.path().join("request.json");
        let output_path = temporary.path().join("mask.png");
        write_private(&source_path, source_bytes).map_err(ProcessInvocationError::Io)?;
        let source_path_text = source_path.to_str().ok_or_else(|| {
            ProcessInvocationError::Protocol("source path is not UTF-8".to_owned())
        })?;
        let output_path_text = output_path.to_str().ok_or_else(|| {
            ProcessInvocationError::Protocol("output path is not UTF-8".to_owned())
        })?;
        let process_request = ProcessSemanticMaskRequest {
            schema: SEMANTIC_MASK_PROCESS_REQUEST_SCHEMA,
            operation: &request.operation,
            target: &request.target,
            source_path: source_path_text,
            output_path: output_path_text,
            helper_identity: &self.helper_identity,
            helper_version: &self.helper_version,
            maximum_output_bytes: MAX_MASK_BYTES,
        };
        let request_bytes = serde_json::to_vec(&process_request)
            .map_err(|error| ProcessInvocationError::Protocol(error.to_string()))?;
        if request_bytes.len() > MAX_PROCESS_REQUEST_BYTES {
            return Err(ProcessInvocationError::Protocol(
                "semantic-mask process request exceeded its JSON bound".to_owned(),
            ));
        }
        write_private(&request_path, &request_bytes).map_err(ProcessInvocationError::Io)?;

        // Re-check the pinned binary immediately before spawn to close the
        // executable replacement window between construction and execution.
        validate_executable(&self.executable).map_err(ProcessInvocationError::Protocol)?;
        let observed = digest_file(&self.executable).map_err(ProcessInvocationError::Io)?;
        if observed != self.executable_sha256 {
            return Err(ProcessInvocationError::Protocol(
                "semantic-mask executable changed after it was pinned".to_owned(),
            ));
        }

        let mut command = Command::new(&self.executable);
        command
            .args([
                "--request",
                source_path_arg(&request_path)?,
                "--output",
                output_path_text,
            ])
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("LC_ALL", "C")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        configure_process_group(&mut command);
        let mut child = command.spawn().map_err(ProcessInvocationError::Io)?;
        // `process_group(0)` makes the spawned leader the group leader. Keep
        // this identity independently of `Child::wait()`: Tokio clears the
        // child PID after reaping, while a descendant may still hold the
        // group's pipes open.
        let process_group = ProcessGroupIdentity::capture(&child)?;
        let Some(stdout) = child.stdout.take() else {
            let cleanup = cleanup_process_group(&mut child, process_group).await;
            return Err(ProcessInvocationError::Protocol(match cleanup {
                Ok(_) => "semantic-mask helper did not expose stdout".to_owned(),
                Err(error) => format!(
                    "semantic-mask helper did not expose stdout; process cleanup failed: {error}"
                ),
            }));
        };
        let Some(stderr) = child.stderr.take() else {
            let cleanup = cleanup_process_group(&mut child, process_group).await;
            return Err(ProcessInvocationError::Protocol(match cleanup {
                Ok(_) => "semantic-mask helper did not expose stderr".to_owned(),
                Err(error) => format!(
                    "semantic-mask helper did not expose stderr; process cleanup failed: {error}"
                ),
            }));
        };
        let mut stdout_task = tokio::spawn(read_bounded(stdout, MAX_PROCESS_RESULT_BYTES));
        let mut stderr_task = tokio::spawn(read_bounded(stderr, MAX_PROCESS_STDERR_BYTES));
        let deadline = tokio::time::Instant::now() + self.timeout;
        let wait_outcome = tokio::select! {
            biased;
            () = cancellation.cancelled() => ProcessWaitOutcome::Cancelled,
            () = tokio::time::sleep_until(deadline) => ProcessWaitOutcome::TimedOut,
            result = wait_for_leader_exit(process_group) => {
                ProcessWaitOutcome::LeaderExited(result)
            },
        };
        match wait_outcome {
            ProcessWaitOutcome::Cancelled => {
                // A cancelled helper can keep inherited pipes open. Terminate
                // its exact group first, then finish the bounded drain.
                let cleanup = cleanup_process_group(&mut child, process_group).await;
                let _ = collect_process_output(&mut stdout_task, &mut stderr_task).await;
                if let Err(error) = cleanup {
                    return Err(ProcessInvocationError::Protocol(format!(
                        "semantic-mask helper cleanup failed after cancellation: {error}"
                    )));
                }
                Err(ProcessInvocationError::Cancelled)
            }
            ProcessWaitOutcome::TimedOut => {
                let cleanup = cleanup_process_group(&mut child, process_group).await;
                let _ = collect_process_output(&mut stdout_task, &mut stderr_task).await;
                if let Err(error) = cleanup {
                    return Err(ProcessInvocationError::Protocol(format!(
                        "semantic-mask helper cleanup failed after timeout: {error}"
                    )));
                }
                Err(ProcessInvocationError::TimedOut)
            }
            ProcessWaitOutcome::LeaderExited(Err(error)) => {
                let cleanup = cleanup_process_group(&mut child, process_group).await;
                let _ = collect_process_output(&mut stdout_task, &mut stderr_task).await;
                if let Err(cleanup_error) = cleanup {
                    return Err(ProcessInvocationError::Protocol(format!(
                        "semantic-mask helper leader observation failed ({error}); process cleanup failed: {cleanup_error}"
                    )));
                }
                Err(ProcessInvocationError::Protocol(format!(
                    "semantic-mask helper leader observation failed: {error}"
                )))
            }
            ProcessWaitOutcome::LeaderExited(Ok(())) => {
                // Normal and unsuccessful exits still require group cleanup:
                // a leader can exit while a silent descendant survives.
                let collected = collect_process_output(&mut stdout_task, &mut stderr_task).await;
                let cleanup = cleanup_process_group(&mut child, process_group).await;
                let status = match cleanup {
                    Ok(report) => report.status,
                    Err(error) => {
                        return Err(ProcessInvocationError::Protocol(format!(
                            "semantic-mask helper process cleanup failed: {error}"
                        )));
                    }
                };
                let (stdout, stderr) = collected.map_err(ProcessInvocationError::Protocol)?;
                if !status.success() {
                    return Err(ProcessInvocationError::Helper(format!(
                        "semantic-mask helper exited unsuccessfully: status={status}, stderr={}",
                        bounded_text(&stderr, 512)
                    )));
                }
                if cancellation.is_cancelled() {
                    return Err(ProcessInvocationError::Cancelled);
                }
                if stdout.is_empty() {
                    return Err(ProcessInvocationError::Protocol(
                        "semantic-mask helper returned an empty result".to_owned(),
                    ));
                }
                self.decode_successful_output(request, &output_path, &stdout)
            }
        }
    }

    fn decode_successful_output(
        &self,
        request: &SemanticMaskRequest,
        output_path: &Path,
        stdout: &[u8],
    ) -> Result<Vec<u8>, ProcessInvocationError> {
        let response: ProcessSemanticMaskResponse =
            serde_json::from_slice(stdout).map_err(|error| {
                ProcessInvocationError::Protocol(format!("invalid helper JSON: {error}"))
            })?;
        if response.schema != SEMANTIC_MASK_PROCESS_RESULT_SCHEMA
            || response.operation != request.operation
            || response.helper_identity != self.helper_identity
            || response.helper_version != self.helper_version
        {
            return Err(ProcessInvocationError::Protocol(
                "semantic-mask helper result identity does not match the request".to_owned(),
            ));
        }
        match response.status {
            ProcessResultStatus::Succeeded => {
                if response.failure.is_some() {
                    return Err(ProcessInvocationError::Protocol(
                        "successful semantic-mask helper result contains failure".to_owned(),
                    ));
                }
                let metadata = response.mask.ok_or_else(|| {
                    ProcessInvocationError::Protocol(
                        "successful semantic-mask helper result omitted mask metadata".to_owned(),
                    )
                })?;
                if metadata.media_type != "image/png"
                    || metadata.sha256.len() != 64
                    || !metadata.sha256.bytes().all(|byte| byte.is_ascii_hexdigit())
                {
                    return Err(ProcessInvocationError::Protocol(
                        "semantic-mask helper mask metadata is invalid".to_owned(),
                    ));
                }
                let bytes = read_bounded_file(output_path, MAX_MASK_BYTES)
                    .map_err(ProcessInvocationError::Io)?;
                if bytes.is_empty()
                    || bytes.len() > MAX_MASK_BYTES
                    || metadata.byte_length != u64::try_from(bytes.len()).unwrap_or(u64::MAX)
                    || metadata.sha256 != sha256_hex(&bytes)
                    || !valid_png_header(&bytes)
                {
                    return Err(ProcessInvocationError::Protocol(
                        "semantic-mask helper output does not match its bounded PNG metadata"
                            .to_owned(),
                    ));
                }
                Ok(bytes)
            }
            ProcessResultStatus::Failed | ProcessResultStatus::Cancelled => {
                let failure = response.failure.ok_or_else(|| {
                    ProcessInvocationError::Protocol(
                        "failed semantic-mask helper result omitted its failure cause".to_owned(),
                    )
                })?;
                if response.mask.is_some() {
                    return Err(ProcessInvocationError::Protocol(
                        "failed semantic-mask helper result contains a mask".to_owned(),
                    ));
                }
                if failure.message.is_empty() || failure.message.len() > 8 * 1024 {
                    return Err(ProcessInvocationError::Protocol(
                        "semantic-mask helper failure message is outside its bound".to_owned(),
                    ));
                }
                if matches!(response.status, ProcessResultStatus::Cancelled)
                    && !matches!(
                        failure.code,
                        FailureCode::Cancelled | FailureCode::BudgetExceeded
                    )
                {
                    return Err(ProcessInvocationError::Protocol(
                        "cancelled semantic-mask helper result has a non-cancellation cause"
                            .to_owned(),
                    ));
                }
                Err(ProcessInvocationError::ResultFailure {
                    status: response.status,
                    failure,
                })
            }
        }
    }
}

#[async_trait]
impl SemanticMaskAdapter for ProcessSemanticMaskAdapter {
    async fn infer(
        &self,
        request: SemanticMaskRequest,
        cancellation: &CancellationToken,
    ) -> SemanticMaskResult {
        let operation = request.operation.clone();
        let failure_result =
            |status: SemanticMaskResultStatus, failure: AgentFailure| SemanticMaskResult {
                schema: SEMANTIC_MASK_CONTRACT_SCHEMA.to_owned(),
                operation: operation.clone(),
                status,
                mask_artifact: None,
                failure: Some(failure),
            };
        if let Err(error) = request.operation.validate() {
            return failure_result(
                SemanticMaskResultStatus::Failed,
                AgentFailure::new(FailureCode::InvalidRequest, error),
            );
        }
        if let Err(error) = request.target.validate() {
            return failure_result(
                SemanticMaskResultStatus::Failed,
                AgentFailure::new(FailureCode::InvalidRequest, error),
            );
        }
        if request.operation.helper_identity != self.helper_identity {
            return failure_result(
                SemanticMaskResultStatus::Failed,
                AgentFailure::new(
                    FailureCode::InvalidRequest,
                    "semantic-mask operation helper identity is not configured for this adapter",
                )
                .with_detail("expectedHelperIdentity", self.helper_identity.clone())
                .with_detail(
                    "actualHelperIdentity",
                    request.operation.helper_identity.clone(),
                ),
            );
        }
        if cancellation.is_cancelled() {
            return failure_result(
                SemanticMaskResultStatus::Cancelled,
                AgentFailure::new(
                    FailureCode::Cancelled,
                    "semantic-mask inference was cancelled",
                ),
            );
        }
        let source_bytes = match self.artifacts.read(&request.operation.source_artifact) {
            Ok(bytes) => bytes,
            Err(error) => {
                return failure_result(
                    SemanticMaskResultStatus::Failed,
                    AgentFailure::new(
                        FailureCode::StorageFailed,
                        "semantic-mask source artifact could not be verified",
                    )
                    .with_detail("cause", error.to_string()),
                );
            }
        };
        let output = match self.invoke(&request, &source_bytes, cancellation).await {
            Ok(bytes) => bytes,
            Err(ProcessInvocationError::Cancelled) => {
                return failure_result(
                    SemanticMaskResultStatus::Cancelled,
                    AgentFailure::new(
                        FailureCode::Cancelled,
                        "semantic-mask inference was cancelled",
                    ),
                );
            }
            Err(ProcessInvocationError::TimedOut) => {
                return failure_result(
                    SemanticMaskResultStatus::Cancelled,
                    AgentFailure::new(
                        FailureCode::BudgetExceeded,
                        "semantic-mask helper exceeded its bounded timeout",
                    )
                    .with_detail("operation", "semantic_mask")
                    .with_detail("helperIdentity", self.helper_identity.clone()),
                );
            }
            Err(ProcessInvocationError::Io(error)) => {
                return failure_result(
                    SemanticMaskResultStatus::Failed,
                    AgentFailure::new(FailureCode::ProviderUnavailable, error.to_string()),
                );
            }
            Err(
                ProcessInvocationError::Helper(message) | ProcessInvocationError::Protocol(message),
            ) => {
                return failure_result(
                    SemanticMaskResultStatus::Failed,
                    AgentFailure::new(FailureCode::ProviderFailed, message)
                        .with_detail("operation", "semantic_mask")
                        .with_detail("helperIdentity", self.helper_identity.clone()),
                );
            }
            Err(ProcessInvocationError::ResultFailure { status, failure }) => {
                return failure_result(
                    match status {
                        ProcessResultStatus::Failed | ProcessResultStatus::Succeeded => {
                            SemanticMaskResultStatus::Failed
                        }
                        ProcessResultStatus::Cancelled => SemanticMaskResultStatus::Cancelled,
                    },
                    failure,
                );
            }
        };
        let mask_artifact = match self.artifacts.put(&output, "image/png", "png") {
            Ok(artifact) => artifact,
            Err(error) => {
                return failure_result(
                    SemanticMaskResultStatus::Failed,
                    AgentFailure::new(
                        FailureCode::StorageFailed,
                        "semantic-mask output could not be committed",
                    )
                    .with_detail("cause", error.to_string()),
                );
            }
        };
        SemanticMaskResult {
            schema: SEMANTIC_MASK_CONTRACT_SCHEMA.to_owned(),
            operation,
            status: SemanticMaskResultStatus::Succeeded,
            mask_artifact: Some(mask_artifact),
            failure: None,
        }
    }
}

#[derive(Debug)]
enum ProcessInvocationError {
    Cancelled,
    TimedOut,
    Io(std::io::Error),
    Helper(String),
    ResultFailure {
        status: ProcessResultStatus,
        failure: AgentFailure,
    },
    Protocol(String),
}

#[derive(Debug)]
enum ProcessWaitOutcome {
    LeaderExited(Result<(), String>),
    Cancelled,
    TimedOut,
}

#[derive(Debug, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct ProcessSemanticMaskRequest<'a> {
    schema: &'static str,
    operation: &'a SemanticMaskOperation,
    target: &'a SemanticMaskTarget,
    source_path: &'a str,
    output_path: &'a str,
    helper_identity: &'a str,
    helper_version: &'a str,
    maximum_output_bytes: usize,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ProcessResultStatus {
    Succeeded,
    Failed,
    Cancelled,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct ProcessSemanticMaskResponse {
    schema: String,
    operation: SemanticMaskOperation,
    helper_identity: String,
    helper_version: String,
    status: ProcessResultStatus,
    #[serde(default)]
    mask: Option<ProcessMaskMetadata>,
    #[serde(default)]
    failure: Option<AgentFailure>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct ProcessMaskMetadata {
    sha256: String,
    byte_length: u64,
    media_type: String,
}

fn resolve_executable(
    path: &Path,
    executable_sha256: impl Into<String>,
) -> Result<(PathBuf, String), String> {
    validate_executable(path)?;
    let executable = path
        .canonicalize()
        .map_err(|error| format!("cannot canonicalize semantic-mask executable: {error}"))?;
    let executable_sha256 = executable_sha256.into();
    let executable_sha256 = normalize_digest(&executable_sha256, "executableSha256")?;
    let observed = digest_file(&executable)
        .map_err(|error| format!("cannot hash semantic-mask executable: {error}"))?;
    if observed != executable_sha256 {
        return Err(format!(
            "semantic-mask executable digest mismatch: expected {executable_sha256}, observed {observed}"
        ));
    }
    Ok((executable, executable_sha256))
}

fn normalize_digest(value: &str, field: &str) -> Result<String, String> {
    if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(format!("{field} must be a 64-character hexadecimal digest"));
    }
    Ok(value.to_ascii_lowercase())
}

fn bounded_identity(value: String, field: &str) -> Result<String, String> {
    if value.is_empty() || value.len() > 256 || value.bytes().any(|byte| byte.is_ascii_control()) {
        return Err(format!("{field} is outside its bounded identity contract"));
    }
    Ok(value)
}

fn validate_executable(path: &Path) -> Result<(), String> {
    if !path.is_absolute() {
        return Err("semantic-mask executable must be an absolute path".to_owned());
    }
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| format!("semantic-mask executable is not accessible: {error}"))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err("semantic-mask executable must be a regular non-symlink file".to_owned());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o111 == 0 {
            return Err("semantic-mask executable is not executable".to_owned());
        }
    }
    Ok(())
}

fn digest_file(path: &Path) -> Result<String, std::io::Error> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0_u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn source_path_arg(path: &Path) -> Result<&str, ProcessInvocationError> {
    path.to_str().ok_or_else(|| {
        ProcessInvocationError::Protocol("semantic-mask request path is not UTF-8".to_owned())
    })
}

fn write_private(path: &Path, bytes: &[u8]) -> Result<(), std::io::Error> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    file.write_all(bytes)?;
    file.sync_all()
}

fn read_bounded_file(path: &Path, maximum: usize) -> Result<Vec<u8>, std::io::Error> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "semantic-mask helper output is not a regular file",
        ));
    }
    if metadata.len() > u64::try_from(maximum).unwrap_or(u64::MAX) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "semantic-mask helper output exceeded its byte bound",
        ));
    }
    let mut file = File::open(path)?;
    let mut bytes = Vec::new();
    Read::by_ref(&mut file)
        .take(u64::try_from(maximum).unwrap_or(u64::MAX).saturating_add(1))
        .read_to_end(&mut bytes)?;
    if bytes.len() > maximum {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "semantic-mask helper output exceeded its byte bound",
        ));
    }
    Ok(bytes)
}

fn valid_png_header(bytes: &[u8]) -> bool {
    if bytes.len() < 33 || bytes[..8] != *b"\x89PNG\r\n\x1a\n" {
        return false;
    }
    let chunk_length = u32::from_be_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]);
    if chunk_length != 13 || &bytes[12..16] != b"IHDR" {
        return false;
    }
    let width = u32::from_be_bytes([bytes[16], bytes[17], bytes[18], bytes[19]]);
    let height = u32::from_be_bytes([bytes[20], bytes[21], bytes[22], bytes[23]]);
    width > 0 && height > 0 && width <= 16_384 && height <= 16_384
}

pub(crate) fn configure_process_group(command: &mut Command) {
    #[cfg(unix)]
    {
        command.process_group(0);
    }
    #[cfg(windows)]
    {
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        command.creation_flags(CREATE_NEW_PROCESS_GROUP);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ProcessGroupIdentity {
    pid: u32,
    pgid: u32,
}

impl ProcessGroupIdentity {
    fn capture(child: &Child) -> Result<Self, ProcessInvocationError> {
        let process_id = child.id().ok_or_else(|| {
            ProcessInvocationError::Protocol(
                "semantic-mask helper process group identity was unavailable at spawn".to_owned(),
            )
        })?;
        #[cfg(unix)]
        let group_id = {
            use rustix::process::{Pid, getpgid};

            let raw_pid = i32::try_from(process_id).map_err(|_| {
                ProcessInvocationError::Protocol(
                    "semantic-mask helper PID does not fit the platform type".to_owned(),
                )
            })?;
            let process = Pid::from_raw(raw_pid).ok_or_else(|| {
                ProcessInvocationError::Protocol(
                    "semantic-mask helper PID must be non-zero".to_owned(),
                )
            })?;
            let process_group = getpgid(Some(process)).map_err(|error| {
                ProcessInvocationError::Protocol(format!(
                    "semantic-mask helper process group could not be observed at spawn: {error}"
                ))
            })?;
            let group_id = u32::try_from(process_group.as_raw_pid()).map_err(|_| {
                ProcessInvocationError::Protocol(
                    "semantic-mask helper process group does not fit the platform type".to_owned(),
                )
            })?;
            if group_id != process_id {
                return Err(ProcessInvocationError::Protocol(format!(
                    "semantic-mask helper was not observed in its dedicated process group: pid={process_id}, pgid={group_id}"
                )));
            }
            group_id
        };
        #[cfg(windows)]
        let group_id = process_id;
        #[cfg(not(any(unix, windows)))]
        let group_id = process_id;
        Ok(Self {
            pid: process_id,
            pgid: group_id,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProcessSignalOutcome {
    Delivered,
    AlreadyAbsent,
    PermissionDenied,
}

#[cfg(unix)]
fn terminate_process_group(
    identity: ProcessGroupIdentity,
    force: bool,
) -> Result<ProcessSignalOutcome, String> {
    use rustix::process::{Pid, Signal, kill_process_group};

    let raw_pgid = i32::try_from(identity.pgid)
        .map_err(|_| "semantic-mask process-group PGID does not fit the platform type")?;
    let process_group = Pid::from_raw(raw_pgid)
        .ok_or_else(|| "semantic-mask process-group PGID must be non-zero".to_owned())?;
    let signal = if force { Signal::KILL } else { Signal::TERM };
    match kill_process_group(process_group, signal) {
        Ok(()) => Ok(ProcessSignalOutcome::Delivered),
        Err(error) if error == rustix::io::Errno::SRCH => Ok(ProcessSignalOutcome::AlreadyAbsent),
        // macOS reports EPERM when the only remaining group member is the
        // unreaped zombie leader. The caller retains this outcome and the
        // liveness probe still has to prove that no other numeric PGID member
        // remains before cleanup can succeed.
        Err(error) if error == rustix::io::Errno::PERM => {
            Ok(ProcessSignalOutcome::PermissionDenied)
        }
        Err(error) => Err(format!(
            "failed to send {} to semantic-mask process group {}: {error}",
            if force { "KILL" } else { "TERM" },
            identity.pgid
        )),
    }
}

#[cfg(windows)]
async fn terminate_process_group(
    identity: ProcessGroupIdentity,
    force: bool,
) -> Result<ProcessSignalOutcome, String> {
    let mut arguments = vec!["/PID".to_owned(), identity.pid.to_string(), "/T".to_owned()];
    if force {
        arguments.push("/F".to_owned());
    }
    let mut taskkill = Command::new("taskkill")
        .args(arguments)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .map_err(|error| format!("could not start taskkill for semantic-mask group: {error}"))?;
    let status = tokio::time::timeout(PROCESS_CLEANUP_TIMEOUT, taskkill.wait())
        .await
        .map_err(|_| "taskkill exceeded the semantic-mask cleanup timeout".to_owned())?
        .map_err(|error| format!("taskkill failed for semantic-mask group: {error}"))?;
    if status.success() {
        Ok(ProcessSignalOutcome::Delivered)
    } else {
        Err(format!(
            "taskkill exited unsuccessfully for semantic-mask group {}: {status}",
            identity.pid
        ))
    }
}

#[cfg(not(any(unix, windows)))]
fn terminate_process_group(
    _identity: ProcessGroupIdentity,
    _force: bool,
) -> Result<ProcessSignalOutcome, String> {
    Err("semantic-mask process-group cleanup is unsupported on this platform".to_owned())
}

#[cfg(unix)]
fn leader_exit_observed(identity: ProcessGroupIdentity) -> Result<bool, String> {
    use rustix::process::{Pid, WaitId, WaitIdOptions, waitid};

    let raw_pid = i32::try_from(identity.pid)
        .map_err(|_| "semantic-mask leader PID does not fit the platform type")?;
    let process = Pid::from_raw(raw_pid)
        .ok_or_else(|| "semantic-mask leader PID must be non-zero".to_owned())?;
    let options = WaitIdOptions::EXITED | WaitIdOptions::NOHANG | WaitIdOptions::NOWAIT;
    let status = waitid(WaitId::Pid(process), options)
        .map_err(|error| format!("could not observe semantic-mask leader with waitid: {error}"))?;
    Ok(status.is_some_and(|status| status.exited() || status.killed() || status.dumped()))
}

#[cfg(windows)]
fn leader_exit_observed(identity: ProcessGroupIdentity) -> Result<bool, String> {
    let output = std::process::Command::new("tasklist")
        .args([
            "/FI",
            &format!("PID eq {}", identity.pid),
            "/FO",
            "CSV",
            "/NH",
        ])
        .output()
        .map_err(|error| format!("could not query semantic-mask leader: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "tasklist exited unsuccessfully while observing semantic-mask leader: {}",
            output.status
        ));
    }
    let pid = format!("\"{}\"", identity.pid);
    Ok(
        !String::from_utf8_lossy(&output.stdout).lines().any(|line| {
            line.split(',')
                .nth(1)
                .is_some_and(|value| value.trim() == pid)
        }),
    )
}

#[cfg(not(any(unix, windows)))]
fn leader_exit_observed(_identity: ProcessGroupIdentity) -> Result<bool, String> {
    Err("semantic-mask leader observation is unsupported on this platform".to_owned())
}

pub(crate) async fn wait_for_leader_exit(identity: ProcessGroupIdentity) -> Result<(), String> {
    loop {
        if leader_exit_observed(identity)? {
            return Ok(());
        }
        tokio::time::sleep(PROCESS_CLEANUP_POLL).await;
    }
}

#[cfg(unix)]
fn process_group_members_except_leader(identity: ProcessGroupIdentity) -> Result<bool, String> {
    // `killpg(..., 0)` reports a zombie leader as present on Linux and EPERM
    // on macOS. Enumerate only numeric PID/PGID columns to distinguish that
    // known, identity-held zombie from any surviving descendant. This is a
    // liveness proof, never a name-based signal target.
    let output = std::process::Command::new("/bin/ps")
        .args(["-axo", "pid=,pgid=,stat="])
        .env_clear()
        .output()
        .map_err(|error| format!("could not enumerate semantic-mask process group: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "ps exited unsuccessfully while proving semantic-mask group absence: {}",
            output.status
        ));
    }
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        let mut fields = line.split_whitespace();
        let Some(member_process_id) = fields.next().and_then(|value| value.parse::<u32>().ok())
        else {
            continue;
        };
        let Some(member_group_id) = fields.next().and_then(|value| value.parse::<u32>().ok())
        else {
            continue;
        };
        if member_group_id == identity.pgid && member_process_id != identity.pid {
            return Ok(true);
        }
    }
    Ok(false)
}

#[cfg(unix)]
fn process_group_present(
    identity: ProcessGroupIdentity,
    leader_exited: bool,
) -> Result<bool, String> {
    use rustix::process::{Pid, test_kill_process_group};

    let raw_pgid = i32::try_from(identity.pgid)
        .map_err(|_| "semantic-mask process-group PGID does not fit the platform type")?;
    let process_group = Pid::from_raw(raw_pgid)
        .ok_or_else(|| "semantic-mask process-group PGID must be non-zero".to_owned())?;
    match test_kill_process_group(process_group) {
        Err(error) if error == rustix::io::Errno::SRCH => Ok(false),
        // A live helper can transiently report EPERM for kill(0) on macOS;
        // conservatively keep it present and let the bounded force phase
        // decide. We never turn this into absence while the leader is live.
        Err(error) if error == rustix::io::Errno::PERM && !leader_exited => Ok(true),
        Err(error)
            if leader_exited
                && error != rustix::io::Errno::SRCH
                && error != rustix::io::Errno::PERM =>
        {
            Err(format!(
                "could not prove semantic-mask process group {} is absent: {error}",
                identity.pgid
            ))
        }
        Ok(()) | Err(_) if leader_exited => process_group_members_except_leader(identity),
        Ok(()) => Ok(true),
        Err(error) => Err(format!(
            "could not prove semantic-mask process group {} is absent: {error}",
            identity.pgid
        )),
    }
}

#[cfg(windows)]
fn process_group_present(
    identity: ProcessGroupIdentity,
    _leader_exited: bool,
) -> Result<bool, String> {
    // Windows exposes the process-tree boundary through taskkill rather than
    // a POSIX-style group probe. Query only the immutable leader PID; cleanup
    // still terminates the full tree with that exact PID.
    let output = std::process::Command::new("tasklist")
        .args([
            "/FI",
            &format!("PID eq {}", identity.pid),
            "/FO",
            "CSV",
            "/NH",
        ])
        .output()
        .map_err(|error| format!("could not query semantic-mask process group: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "tasklist exited unsuccessfully while proving semantic-mask group absence: {}",
            output.status
        ));
    }
    let pid = format!("\"{}\"", identity.pid);
    Ok(String::from_utf8_lossy(&output.stdout).lines().any(|line| {
        line.split(',')
            .nth(1)
            .is_some_and(|value| value.trim() == pid)
    }))
}

#[cfg(not(any(unix, windows)))]
fn process_group_present(
    _identity: ProcessGroupIdentity,
    _leader_exited: bool,
) -> Result<bool, String> {
    Err("semantic-mask process-group liveness is unsupported on this platform".to_owned())
}

async fn wait_for_process_group_absence(identity: ProcessGroupIdentity) -> Result<bool, String> {
    let deadline = tokio::time::Instant::now() + PROCESS_CLEANUP_TIMEOUT;
    loop {
        // `waitid(WNOWAIT|NOHANG)` observes the leader without reaping it. The
        // leader's PID/PGID remains reserved while every descendant is
        // signalled and numeric group membership is proven absent.
        let leader_exited = leader_exit_observed(identity)?;
        if !process_group_present(identity, leader_exited)? && leader_exited {
            return Ok(true);
        }
        if tokio::time::Instant::now() >= deadline {
            return Ok(false);
        }
        tokio::time::sleep(PROCESS_CLEANUP_POLL).await;
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct ProcessCleanupReport {
    pub(crate) status: std::process::ExitStatus,
    #[cfg_attr(not(test), allow(dead_code))]
    forced: bool,
}

pub(crate) async fn cleanup_process_group(
    child: &mut Child,
    identity: ProcessGroupIdentity,
) -> Result<ProcessCleanupReport, String> {
    // Always attempt graceful termination first, including after a successful
    // leader exit. A descendant may still be alive in the same exact group.
    let mut errors = Vec::new();
    #[cfg(unix)]
    let graceful_signal = terminate_process_group(identity, false);
    #[cfg(windows)]
    let graceful_signal = terminate_process_group(identity, false).await;
    #[cfg(not(any(unix, windows)))]
    let graceful_signal = terminate_process_group(identity, false);
    if let Err(error) = graceful_signal {
        errors.push(error);
    }
    let mut absent = match wait_for_process_group_absence(identity).await {
        Ok(absent) => absent,
        Err(error) => {
            errors.push(error);
            false
        }
    };
    let mut forced = false;
    if !absent {
        // A helper is allowed to ignore TERM only until the bounded force
        // phase. The KILL is still scoped to the captured process-group ID.
        forced = true;
        #[cfg(unix)]
        let forced_signal = terminate_process_group(identity, true);
        #[cfg(windows)]
        let forced_signal = terminate_process_group(identity, true).await;
        #[cfg(not(any(unix, windows)))]
        let forced_signal = terminate_process_group(identity, true);
        if let Err(error) = forced_signal {
            errors.push(error);
        }
        absent = match wait_for_process_group_absence(identity).await {
            Ok(absent) => absent,
            Err(error) => {
                errors.push(error);
                false
            }
        };
    }
    if !absent {
        errors.push(format!(
            "semantic-mask process group {} was still present after graceful and forced cleanup",
            identity.pid
        ));
    }
    if !absent {
        // Never release the leader identity while a group member is still
        // present. Returning the explicit cleanup failure lets the caller
        // report the unproven process state instead of risking PID reuse.
        return Err(errors.join("; "));
    }
    // This is the one and only reap. It happens after exact group cleanup and
    // before the identity can be released for reuse.
    let status = match tokio::time::timeout(PROCESS_CLEANUP_TIMEOUT, child.wait()).await {
        Ok(Ok(status)) => Some(status),
        Ok(Err(error)) => {
            errors.push(format!(
                "could not reap semantic-mask helper leader: {error}"
            ));
            None
        }
        Err(_) => {
            errors.push("semantic-mask helper leader wait exceeded cleanup timeout".to_owned());
            None
        }
    };
    if !errors.is_empty() {
        return Err(errors.join("; "));
    }
    let Some(status) = status else {
        return Err("semantic-mask helper cleanup produced no leader status".to_owned());
    };
    Ok(ProcessCleanupReport { status, forced })
}

async fn collect_process_output(
    stdout_task: &mut JoinHandle<Result<Vec<u8>, String>>,
    stderr_task: &mut JoinHandle<Result<Vec<u8>, String>>,
) -> Result<(Vec<u8>, Vec<u8>), String> {
    let output = tokio::time::timeout(PROCESS_DRAIN_TIMEOUT, async {
        let stdout = (&mut *stdout_task)
            .await
            .map_err(|error| format!("stdout reader task failed: {error}"))??;
        let stderr = (&mut *stderr_task)
            .await
            .map_err(|error| format!("stderr reader task failed: {error}"))??;
        Ok::<_, String>((stdout, stderr))
    })
    .await;
    if let Ok(result) = output {
        result
    } else {
        stdout_task.abort();
        stderr_task.abort();
        let _ = tokio::time::timeout(PROCESS_CLEANUP_TIMEOUT, stdout_task).await;
        let _ = tokio::time::timeout(PROCESS_CLEANUP_TIMEOUT, stderr_task).await;
        Err("semantic-mask helper output drain exceeded its bounded timeout".to_owned())
    }
}

async fn read_bounded<R>(reader: R, maximum: usize) -> Result<Vec<u8>, String>
where
    R: AsyncRead + Unpin,
{
    let mut output = Vec::new();
    reader
        .take(u64::try_from(maximum).unwrap_or(u64::MAX).saturating_add(1))
        .read_to_end(&mut output)
        .await
        .map_err(|error| error.to_string())?;
    if output.len() > maximum {
        return Err("semantic-mask helper output exceeded its bounded transport limit".to_owned());
    }
    Ok(output)
}

fn bounded_text(bytes: &[u8], maximum_chars: usize) -> String {
    String::from_utf8_lossy(bytes)
        .chars()
        .take(maximum_chars)
        .collect()
}

#[derive(Debug, Default)]
pub struct UnavailableSemanticMaskAdapter;

#[async_trait]
impl SemanticMaskAdapter for UnavailableSemanticMaskAdapter {
    async fn infer(
        &self,
        request: SemanticMaskRequest,
        cancellation: &CancellationToken,
    ) -> SemanticMaskResult {
        let (status, message) = if cancellation.is_cancelled() {
            (
                SemanticMaskResultStatus::Cancelled,
                "semantic mask inference was cancelled",
            )
        } else {
            (
                SemanticMaskResultStatus::Failed,
                "no semantic mask backend is configured",
            )
        };
        let failure = AgentFailure::new(
            if status == SemanticMaskResultStatus::Cancelled {
                FailureCode::Cancelled
            } else {
                FailureCode::CapabilityDenied
            },
            message,
        )
        .with_detail(
            "status",
            match status {
                SemanticMaskResultStatus::Cancelled => "cancelled",
                SemanticMaskResultStatus::Failed => "unavailable",
                SemanticMaskResultStatus::Succeeded => "succeeded",
            },
        )
        .with_detail("capability", SEMANTIC_MASK_CAPABILITY)
        .with_detail("operation", "semantic_mask")
        .with_detail("helperIdentity", request.operation.helper_identity.clone())
        .with_detail("requestRevision", request.operation.request_revision)
        .with_detail("toolCallId", request.operation.tool_call_id.to_string());
        SemanticMaskResult {
            schema: SEMANTIC_MASK_CONTRACT_SCHEMA.to_owned(),
            operation: request.operation,
            status,
            mask_artifact: None,
            failure: Some(failure),
        }
    }
}

// Runtime mechanisms share this process lifecycle; neither owns task state.
pub(crate) fn capture_process_group(child: &Child) -> Result<ProcessGroupIdentity, String> {
    ProcessGroupIdentity::capture(child).map_err(|error| format!("{error:?}"))
}

#[cfg(unix)]
pub(crate) fn force_process_group(identity: ProcessGroupIdentity) {
    // Drop cannot return cleanup evidence. The caller must retain an uncertain
    // effect outcome until normal asynchronous cleanup has been verified.
    let _ = terminate_process_group(identity, true);
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_core::{ArtifactId, ArtifactRef, ToolCallId};
    use std::os::unix::fs::PermissionsExt;

    fn request() -> SemanticMaskRequest {
        SemanticMaskRequest {
            operation: SemanticMaskOperation {
                source_artifact: ArtifactRef {
                    id: ArtifactId::parse(format!("art-{}", "a".repeat(64))).expect("artifact"),
                    sha256: "a".repeat(64),
                    media_type: "image/png".to_owned(),
                    byte_length: 1,
                    relative_path: "aa/source.png".to_owned(),
                },
                source_revision: 1,
                helper_identity: "semantic.fixture/1".to_owned(),
                request_revision: 1,
                tool_call_id: ToolCallId::parse("semantic-adapter-call").expect("call id"),
            },
            target: SemanticMaskTarget::Point {
                x_millionths: 500_000,
                y_millionths: 500_000,
            },
        }
    }

    #[tokio::test]
    async fn unavailable_adapter_returns_explicit_failure_without_mask() {
        let result = UnavailableSemanticMaskAdapter
            .infer(request(), &CancellationToken::new())
            .await;
        assert_eq!(result.status, SemanticMaskResultStatus::Failed);
        assert!(result.mask_artifact.is_none());
        assert_eq!(
            result
                .failure
                .as_ref()
                .and_then(|failure| failure.details.get("capability"))
                .and_then(serde_json::Value::as_str),
            Some(SEMANTIC_MASK_CAPABILITY)
        );
        result.validate().expect("failure result contract");
    }

    #[tokio::test]
    async fn unavailable_adapter_keeps_cancellation_distinct() {
        let cancellation = CancellationToken::new();
        cancellation.cancel();
        let result = UnavailableSemanticMaskAdapter
            .infer(request(), &cancellation)
            .await;
        assert_eq!(result.status, SemanticMaskResultStatus::Cancelled);
        assert_eq!(
            result.failure.as_ref().map(|failure| failure.code),
            Some(FailureCode::Cancelled)
        );
        result.validate().expect("cancel result contract");
    }

    fn valid_png() -> Vec<u8> {
        // A 1x1 RGBA PNG used only as a bounded adapter fixture.
        b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR\0\0\0\x01\0\0\0\x01\x08\x06\0\0\0\x1f\x15\xc4\x89\0\0\0\rIDATx\x9cc\x60\0\0\0\x02\0\x01\xe2!\xbc3\0\0\0\0IEND\xaeB\x60\x82".to_vec()
    }

    fn stored_source(store: &ArtifactStore) -> (ArtifactRef, SemanticMaskOperation) {
        let source = store
            .put(b"source image bytes", "image/png", "png")
            .expect("source artifact");
        let operation = SemanticMaskOperation {
            source_artifact: source,
            source_revision: 1,
            helper_identity: "fixture.semantic/1".to_owned(),
            request_revision: 1,
            tool_call_id: ToolCallId::parse("semantic-process-call").expect("call id"),
        };
        (operation.source_artifact.clone(), operation)
    }

    #[cfg(unix)]
    fn executable_fixture(
        directory: &Path,
        operation: &SemanticMaskOperation,
        output: &[u8],
        status: &str,
    ) -> PathBuf {
        let operation_json = serde_json::to_string(operation).expect("operation JSON");
        let output_b64 = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR4nGNgAAAAAgAB4iG8MwAAAABJRU5ErkJggg==";
        assert_eq!(
            output,
            valid_png(),
            "fixture output must match its portable encoding"
        );
        let response = if status == "succeeded" {
            format!(
                "{{\"schema\":\"{SEMANTIC_MASK_PROCESS_RESULT_SCHEMA}\",\"operation\":{operation_json},\"helperIdentity\":\"fixture.semantic/1\",\"helperVersion\":\"2026.08\",\"status\":\"succeeded\",\"mask\":{{\"sha256\":\"{}\",\"byteLength\":{},\"mediaType\":\"image/png\"}}}}",
                sha256_hex(output),
                output.len()
            )
        } else {
            format!(
                "{{\"schema\":\"{SEMANTIC_MASK_PROCESS_RESULT_SCHEMA}\",\"operation\":{operation_json},\"helperIdentity\":\"fixture.semantic/1\",\"helperVersion\":\"2026.08\",\"status\":\"failed\",\"failure\":{{\"code\":\"provider_failed\",\"message\":\"fixture helper failed\"}}}}"
            )
        };
        let script = format!(
            "#!/bin/sh\nprintf '%s' '{output_b64}' | /usr/bin/base64 -D > \"$4\"\nprintf '%s' '{response}'\n"
        );
        let path = directory.join("semantic-helper.sh");
        fs::write(&path, script).expect("helper script");
        let mut permissions = fs::metadata(&path).expect("helper metadata").permissions();
        permissions.set_mode(0o700);
        fs::set_permissions(&path, permissions).expect("helper permissions");
        path
    }

    #[cfg(unix)]
    fn shell_literal(path: &Path) -> String {
        format!("'{}'", path.to_string_lossy().replace('\'', "'\\''"))
    }

    #[cfg(unix)]
    fn executable_fixture_with_silent_background(
        directory: &Path,
        operation: &SemanticMaskOperation,
        output: &[u8],
        succeeds: bool,
        group_marker: &Path,
        child_marker: &Path,
        pgid_marker: &Path,
    ) -> PathBuf {
        let operation_json = serde_json::to_string(operation).expect("operation JSON");
        let output_b64 = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR4nGNgAAAAAgAB4iG8MwAAAABJRU5ErkJggg==";
        assert_eq!(output, valid_png());
        let response = if succeeds {
            format!(
                "{{\"schema\":\"{SEMANTIC_MASK_PROCESS_RESULT_SCHEMA}\",\"operation\":{operation_json},\"helperIdentity\":\"fixture.semantic/1\",\"helperVersion\":\"2026.08\",\"status\":\"succeeded\",\"mask\":{{\"sha256\":\"{}\",\"byteLength\":{},\"mediaType\":\"image/png\"}}}}",
                sha256_hex(output),
                output.len()
            )
        } else {
            format!(
                "{{\"schema\":\"{SEMANTIC_MASK_PROCESS_RESULT_SCHEMA}\",\"operation\":{operation_json},\"helperIdentity\":\"fixture.semantic/1\",\"helperVersion\":\"2026.08\",\"status\":\"failed\",\"failure\":{{\"code\":\"provider_failed\",\"message\":\"fixture helper failed\"}}}}"
            )
        };
        let script = format!(
            "#!/bin/sh\noutput=\"$4\"\nprintf '%s' '{output_b64}' | /usr/bin/base64 -D > \"$output\"\n(sleep 30) >/dev/null 2>&1 &\nchild=\"$!\"\nprintf '%s' \"$$\" > {}\nprintf '%s' \"$child\" > {}\nprintf '%s' \"$$\" > {}\nprintf '%s' '{response}'\nexit {}\n",
            shell_literal(group_marker),
            shell_literal(child_marker),
            shell_literal(pgid_marker),
            if succeeds { 0 } else { 17 }
        );
        let path = directory.join(if succeeds {
            "semantic-background-success.sh"
        } else {
            "semantic-background-failure.sh"
        });
        fs::write(&path, script).expect("background helper script");
        let mut permissions = fs::metadata(&path)
            .expect("background helper metadata")
            .permissions();
        permissions.set_mode(0o700);
        fs::set_permissions(&path, permissions).expect("background helper permissions");
        path
    }

    #[cfg(unix)]
    fn sleeping_fixture_with_markers(
        directory: &Path,
        name: &str,
        group_marker: &Path,
        child_marker: &Path,
        pgid_marker: &Path,
    ) -> PathBuf {
        let script = format!(
            "#!/bin/sh\n(sleep 30) >/dev/null 2>&1 &\nchild=\"$!\"\nprintf '%s' \"$$\" > {}\nprintf '%s' \"$child\" > {}\nprintf '%s' \"$$\" > {}\nwait\n",
            shell_literal(group_marker),
            shell_literal(child_marker),
            shell_literal(pgid_marker),
        );
        let path = directory.join(name);
        fs::write(&path, script).expect("sleeping helper script");
        let mut permissions = fs::metadata(&path)
            .expect("sleeping helper metadata")
            .permissions();
        permissions.set_mode(0o700);
        fs::set_permissions(&path, permissions).expect("sleeping helper permissions");
        path
    }

    #[cfg(unix)]
    fn term_ignoring_fixture(
        directory: &Path,
        group_marker: &Path,
        child_marker: &Path,
        pgid_marker: &Path,
    ) -> PathBuf {
        let script = format!(
            "#!/bin/sh\ntrap '' TERM\n(trap '' TERM; sleep 30) >/dev/null 2>&1 &\nchild=\"$!\"\nprintf '%s' \"$$\" > {}\nprintf '%s' \"$child\" > {}\nprintf '%s' \"$$\" > {}\nwait\n",
            shell_literal(group_marker),
            shell_literal(child_marker),
            shell_literal(pgid_marker),
        );
        let path = directory.join("term-ignoring-helper.sh");
        fs::write(&path, script).expect("TERM-ignoring helper script");
        let mut permissions = fs::metadata(&path)
            .expect("TERM-ignoring helper metadata")
            .permissions();
        permissions.set_mode(0o700);
        fs::set_permissions(&path, permissions).expect("TERM-ignoring helper permissions");
        path
    }

    #[cfg(unix)]
    fn pid_is_present(pid: u32) -> bool {
        use rustix::process::{Pid, test_kill_process};

        let raw_pid = i32::try_from(pid).expect("test PID fits platform type");
        let process = Pid::from_raw(raw_pid).expect("test PID is non-zero");
        match test_kill_process(process) {
            Ok(()) => true,
            Err(error) if error == rustix::io::Errno::SRCH => false,
            Err(error) => panic!("could not probe test PID {pid}: {error}"),
        }
    }

    #[cfg(unix)]
    fn process_identity(
        group_marker: &Path,
        child_marker: &Path,
        pgid_marker: &Path,
    ) -> (ProcessGroupIdentity, u32) {
        let leader_pid = fs::read_to_string(group_marker)
            .expect("group marker")
            .trim()
            .parse::<u32>()
            .expect("group PID");
        let child_pid = fs::read_to_string(child_marker)
            .expect("child marker")
            .trim()
            .parse::<u32>()
            .expect("child PID");
        let recorded_group_id = fs::read_to_string(pgid_marker)
            .expect("PGID marker")
            .trim()
            .parse::<u32>()
            .expect("PGID");
        assert_eq!(
            leader_pid, recorded_group_id,
            "fixture must run in its dedicated process group"
        );
        (
            ProcessGroupIdentity {
                pid: leader_pid,
                pgid: recorded_group_id,
            },
            child_pid,
        )
    }

    #[cfg(unix)]
    async fn assert_process_identity_absent(identity: ProcessGroupIdentity, child_pid: u32) {
        assert!(
            !process_group_present(identity, false).expect("immediate group absence probe"),
            "semantic helper group survived return: pid={}, pgid={}",
            identity.pid,
            identity.pgid
        );
        assert!(
            !pid_is_present(child_pid),
            "semantic helper child survived return"
        );
        tokio::time::sleep(Duration::from_millis(150)).await;
        assert!(
            !process_group_present(identity, false).expect("delayed group absence probe"),
            "semantic helper group reappeared: pid={}, pgid={}",
            identity.pid,
            identity.pgid
        );
        assert!(
            !pid_is_present(child_pid),
            "semantic helper child reappeared"
        );
    }

    #[cfg(unix)]
    async fn wait_for_markers(paths: &[&Path]) {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if paths.iter().all(|path| path.is_file()) {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("process fixture markers must be written before cleanup");
    }

    #[cfg(unix)]
    fn pinned_spec_fixture(directory: &Path) -> SemanticMaskSpec {
        let executable = directory.join("semantic-config-fixture.sh");
        let bytes = b"#!/bin/sh\nexit 0\n";
        fs::write(&executable, bytes).expect("helper fixture");
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o700))
            .expect("helper permissions");
        SemanticMaskSpec {
            executable,
            executable_sha256: sha256_hex(bytes).to_ascii_uppercase(),
            helper_identity: "fixture.semantic/1".to_owned(),
            helper_version: "2026.08".to_owned(),
            timeout_ms: 1_000,
        }
    }

    #[cfg(unix)]
    #[test]
    fn adapter_constructors_preserve_the_same_pinned_configuration() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let store = ArtifactStore::open(temporary.path().join("artifacts")).expect("store");
        let mut spec = pinned_spec_fixture(temporary.path());
        spec.executable = temporary.path().join("./semantic-config-fixture.sh");
        for timeout_ms in [1, 1_000, 600_000] {
            spec.timeout_ms = timeout_ms;
            let resolved =
                ProcessSemanticMaskAdapter::resolve_spec(&spec).expect("resolved helper");
            let from_spec =
                ProcessSemanticMaskAdapter::from_spec(&spec, store.clone()).expect("spec adapter");
            let direct = ProcessSemanticMaskAdapter::new(
                &spec.executable,
                &spec.executable_sha256,
                &spec.helper_identity,
                &spec.helper_version,
                store.clone(),
                Duration::from_millis(timeout_ms),
            )
            .expect("direct adapter");
            for adapter in [from_spec, direct] {
                assert_eq!(
                    adapter.executable(),
                    spec.executable.canonicalize().expect("canonical path")
                );
                assert_eq!(adapter.executable(), resolved.executable);
                assert_eq!(
                    adapter.executable_sha256(),
                    spec.executable_sha256.to_ascii_lowercase()
                );
                assert_eq!(adapter.helper_identity(), spec.helper_identity);
                assert_eq!(adapter.helper_version(), spec.helper_version);
                assert_eq!(adapter.timeout, Duration::from_millis(timeout_ms));
            }
        }
        let resolved = ProcessSemanticMaskAdapter::resolve_spec(&spec).expect("composed helper");
        fs::write(&spec.executable, b"#!/bin/sh\nexit 1\n").expect("replace composed helper");
        let error = ProcessSemanticMaskAdapter::from_spec(&resolved, store)
            .expect_err("runtime construction must recheck a composed helper");
        assert!(error.starts_with("semantic-mask executable digest mismatch:"));
    }

    #[cfg(unix)]
    #[test]
    fn adapter_constructors_preserve_configuration_errors_and_validation_order() {
        use std::os::unix::fs::symlink;

        let temporary = tempfile::tempdir().expect("temporary directory");
        let store = ArtifactStore::open(temporary.path().join("artifacts")).expect("store");
        let spec = pinned_spec_fixture(temporary.path());
        let mut cases = Vec::new();
        let mut relative = spec.clone();
        relative.executable = PathBuf::from("relative-helper.sh");
        relative.executable_sha256.clear();
        relative.timeout_ms = 0;
        cases.push((
            relative,
            "semantic-mask executable must be an absolute path".to_owned(),
        ));
        let mut linked = spec.clone();
        linked.executable = temporary.path().join("helper-symlink");
        symlink(&spec.executable, &linked.executable).expect("helper symlink");
        cases.push((
            linked,
            "semantic-mask executable must be a regular non-symlink file".to_owned(),
        ));
        let mut bad_digest = spec.clone();
        bad_digest.executable_sha256 = "not-a-digest".to_owned();
        cases.push((
            bad_digest,
            "executableSha256 must be a 64-character hexadecimal digest".to_owned(),
        ));
        let mut wrong_digest = spec.clone();
        wrong_digest.executable_sha256 = "0".repeat(64);
        cases.push((
            wrong_digest,
            format!(
                "semantic-mask executable digest mismatch: expected {}, observed {}",
                "0".repeat(64),
                spec.executable_sha256.to_ascii_lowercase()
            ),
        ));
        let mut missing_identity = spec.clone();
        missing_identity.helper_identity.clear();
        missing_identity.timeout_ms = 0;
        cases.push((
            missing_identity,
            "helperIdentity is outside its bounded identity contract".to_owned(),
        ));
        let mut bad_version = spec.clone();
        bad_version.helper_version = "bad\nversion".to_owned();
        cases.push((
            bad_version,
            "helperVersion is outside its bounded identity contract".to_owned(),
        ));
        for (invalid, expected) in cases {
            assert_eq!(
                ProcessSemanticMaskAdapter::resolve_spec(&invalid).expect_err("invalid spec"),
                expected
            );
            assert_eq!(
                ProcessSemanticMaskAdapter::from_spec(&invalid, store.clone())
                    .expect_err("invalid adapter"),
                expected
            );
            assert_eq!(
                ProcessSemanticMaskAdapter::new(
                    &invalid.executable,
                    &invalid.executable_sha256,
                    &invalid.helper_identity,
                    &invalid.helper_version,
                    store.clone(),
                    Duration::from_millis(invalid.timeout_ms),
                )
                .expect_err("invalid direct adapter"),
                expected
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn constructors_keep_distinct_timeout_errors_and_duration_precision() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let store = ArtifactStore::open(temporary.path().join("artifacts")).expect("store");
        let mut spec = pinned_spec_fixture(temporary.path());
        for timeout_ms in [0, 600_001] {
            spec.timeout_ms = timeout_ms;
            assert_eq!(
                ProcessSemanticMaskAdapter::from_spec(&spec, store.clone()).expect_err("timeout"),
                "semantic-mask helper timeout is outside 1..=600000ms"
            );
        }
        for timeout in [
            Duration::ZERO,
            Duration::from_nanos(999_999),
            Duration::from_mins(10) + Duration::from_nanos(1),
        ] {
            assert_eq!(
                ProcessSemanticMaskAdapter::new(
                    &spec.executable,
                    &spec.executable_sha256,
                    &spec.helper_identity,
                    &spec.helper_version,
                    store.clone(),
                    timeout,
                )
                .expect_err("duration outside bounds"),
                "semantic-mask helper timeout is outside 1ms..=10min"
            );
        }
        let timeout = Duration::from_nanos(1_000_001);
        let adapter = ProcessSemanticMaskAdapter::new(
            spec.executable,
            spec.executable_sha256,
            spec.helper_identity,
            spec.helper_version,
            store,
            timeout,
        )
        .expect("fractional-millisecond timeout");
        assert_eq!(adapter.timeout, timeout);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn process_adapter_publishes_verified_mask_and_preserves_failure() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let store = ArtifactStore::open(temporary.path().join("artifacts")).expect("store");
        let (_, operation) = stored_source(&store);
        let mask = valid_png();
        let helper = executable_fixture(temporary.path(), &operation, &mask, "succeeded");
        let digest = digest_file(&helper).expect("helper digest");
        let adapter = ProcessSemanticMaskAdapter::new(
            &helper,
            digest,
            "fixture.semantic/1",
            "2026.08",
            store.clone(),
            // This success-path helper runs in the same process-heavy test
            // binary as the timeout/cleanup fixtures. Keep its operation
            // budget generous enough for parallel workspace test scheduling;
            // cleanup itself remains bounded by the adapter constants above.
            Duration::from_secs(10),
        )
        .expect("pinned adapter");
        let result = adapter
            .infer(
                SemanticMaskRequest {
                    operation: operation.clone(),
                    target: SemanticMaskTarget::Point {
                        x_millionths: 500_000,
                        y_millionths: 500_000,
                    },
                },
                &CancellationToken::new(),
            )
            .await;
        assert_eq!(result.status, SemanticMaskResultStatus::Succeeded);
        let mask_ref = result.mask_artifact.as_ref().expect("mask artifact");
        assert_eq!(store.read(mask_ref).expect("stored mask"), mask);
        result.validate().expect("success result contract");

        let drift_marker = temporary.path().join("drifted-helper-spawned");
        fs::write(
            &helper,
            format!("#!/bin/sh\n: > {}\n", shell_literal(&drift_marker)),
        )
        .expect("replace pinned helper");
        let drifted = adapter
            .infer(
                SemanticMaskRequest {
                    operation: operation.clone(),
                    target: SemanticMaskTarget::Point {
                        x_millionths: 500_000,
                        y_millionths: 500_000,
                    },
                },
                &CancellationToken::new(),
            )
            .await;
        assert_eq!(drifted.status, SemanticMaskResultStatus::Failed);
        assert_eq!(
            drifted
                .failure
                .as_ref()
                .map(|failure| failure.message.as_str()),
            Some("semantic-mask executable changed after it was pinned")
        );
        assert!(!drift_marker.exists(), "drifted helper must never spawn");
        drifted.validate().expect("drift failure contract");

        let failed_helper = executable_fixture(temporary.path(), &operation, &mask, "failed");
        let failed_digest = digest_file(&failed_helper).expect("failed helper digest");
        let failed = ProcessSemanticMaskAdapter::new(
            failed_helper,
            failed_digest,
            "fixture.semantic/1",
            "2026.08",
            store,
            Duration::from_secs(10),
        )
        .expect("pinned failed adapter");
        let result = failed
            .infer(
                SemanticMaskRequest {
                    operation,
                    target: SemanticMaskTarget::Point {
                        x_millionths: 500_000,
                        y_millionths: 500_000,
                    },
                },
                &CancellationToken::new(),
            )
            .await;
        assert_eq!(result.status, SemanticMaskResultStatus::Failed);
        assert_eq!(
            result.failure.as_ref().map(|failure| failure.code),
            Some(FailureCode::ProviderFailed)
        );
        assert_eq!(
            result
                .failure
                .as_ref()
                .map(|failure| failure.message.as_str()),
            Some("fixture helper failed")
        );
        result.validate().expect("failure result contract");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn process_adapter_cancellation_timeout_and_digest_mismatch_are_explicit() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let store = ArtifactStore::open(temporary.path().join("artifacts")).expect("store");
        let (_, operation) = stored_source(&store);
        let cancellation_group_marker = temporary.path().join("cancel-group.pid");
        let cancellation_child_marker = temporary.path().join("cancel-child.pid");
        let cancellation_pgid_marker = temporary.path().join("cancel-pgid.pid");
        let cancellation_helper = sleeping_fixture_with_markers(
            temporary.path(),
            "sleeping-cancellation-helper.sh",
            &cancellation_group_marker,
            &cancellation_child_marker,
            &cancellation_pgid_marker,
        );
        let cancellation_digest = digest_file(&cancellation_helper).expect("cancel digest");
        assert!(
            ProcessSemanticMaskAdapter::new(
                &cancellation_helper,
                "0".repeat(64),
                "fixture.semantic/1",
                "2026.08",
                store.clone(),
                Duration::from_secs(1),
            )
            .is_err()
        );
        let adapter = ProcessSemanticMaskAdapter::new(
            &cancellation_helper,
            cancellation_digest,
            "fixture.semantic/1",
            "2026.08",
            store.clone(),
            Duration::from_secs(5),
        )
        .expect("sleep adapter");
        let cancellation = CancellationToken::new();
        let task_cancellation = cancellation.clone();
        let task = tokio::spawn(async move {
            adapter
                .infer(
                    SemanticMaskRequest {
                        operation,
                        target: SemanticMaskTarget::Point {
                            x_millionths: 500_000,
                            y_millionths: 500_000,
                        },
                    },
                    &task_cancellation,
                )
                .await
        });
        wait_for_markers(&[
            &cancellation_group_marker,
            &cancellation_child_marker,
            &cancellation_pgid_marker,
        ])
        .await;
        cancellation.cancel();
        let result = tokio::time::timeout(Duration::from_secs(2), task)
            .await
            .expect("cancellation cleanup bound")
            .expect("adapter task");
        assert_eq!(result.status, SemanticMaskResultStatus::Cancelled);
        assert_eq!(
            result.failure.as_ref().map(|failure| failure.code),
            Some(FailureCode::Cancelled)
        );
        result.validate().expect("cancel result contract");
        let (cancellation_identity, cancellation_child_pid) = process_identity(
            &cancellation_group_marker,
            &cancellation_child_marker,
            &cancellation_pgid_marker,
        );
        assert_process_identity_absent(cancellation_identity, cancellation_child_pid).await;

        let (_, timeout_operation) = stored_source(&store);
        let timeout_group_marker = temporary.path().join("timeout-group.pid");
        let timeout_child_marker = temporary.path().join("timeout-child.pid");
        let timeout_pgid_marker = temporary.path().join("timeout-pgid.pid");
        let timeout_helper = sleeping_fixture_with_markers(
            temporary.path(),
            "sleeping-timeout-helper.sh",
            &timeout_group_marker,
            &timeout_child_marker,
            &timeout_pgid_marker,
        );
        let timeout_digest = digest_file(&timeout_helper).expect("timeout digest");
        let timeout_adapter = ProcessSemanticMaskAdapter::new(
            &timeout_helper,
            timeout_digest,
            "fixture.semantic/1",
            "2026.08",
            store,
            Duration::from_secs(2),
        )
        .expect("timeout adapter");
        let result = timeout_adapter
            .infer(
                SemanticMaskRequest {
                    operation: timeout_operation,
                    target: SemanticMaskTarget::Point {
                        x_millionths: 500_000,
                        y_millionths: 500_000,
                    },
                },
                &CancellationToken::new(),
            )
            .await;
        assert_eq!(result.status, SemanticMaskResultStatus::Cancelled);
        assert_eq!(
            result.failure.as_ref().map(|failure| failure.code),
            Some(FailureCode::BudgetExceeded)
        );
        result.validate().expect("timeout result contract");
        let (timeout_identity, timeout_child_pid) = process_identity(
            &timeout_group_marker,
            &timeout_child_marker,
            &timeout_pgid_marker,
        );
        assert_process_identity_absent(timeout_identity, timeout_child_pid).await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn process_adapter_cleans_background_group_after_success_and_failure() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let store = ArtifactStore::open(temporary.path().join("artifacts")).expect("store");
        let (_, operation) = stored_source(&store);
        let output = valid_png();

        for succeeds in [true, false] {
            let group_marker = temporary.path().join(if succeeds {
                "success-group.pid"
            } else {
                "failure-group.pid"
            });
            let child_marker = temporary.path().join(if succeeds {
                "success-child.pid"
            } else {
                "failure-child.pid"
            });
            let pgid_marker = temporary.path().join(if succeeds {
                "success-pgid.pid"
            } else {
                "failure-pgid.pid"
            });
            let helper = executable_fixture_with_silent_background(
                temporary.path(),
                &operation,
                &output,
                succeeds,
                &group_marker,
                &child_marker,
                &pgid_marker,
            );
            let adapter = ProcessSemanticMaskAdapter::new(
                &helper,
                digest_file(&helper).expect("helper digest"),
                "fixture.semantic/1",
                "2026.08",
                store.clone(),
                Duration::from_secs(2),
            )
            .expect("pinned adapter");
            let result = adapter
                .infer(
                    SemanticMaskRequest {
                        operation: operation.clone(),
                        target: SemanticMaskTarget::Point {
                            x_millionths: 500_000,
                            y_millionths: 500_000,
                        },
                    },
                    &CancellationToken::new(),
                )
                .await;
            if succeeds {
                assert_eq!(
                    result.status,
                    SemanticMaskResultStatus::Succeeded,
                    "background success failure: {:?}",
                    result.failure
                );
            } else {
                assert_eq!(result.status, SemanticMaskResultStatus::Failed);
                assert_eq!(
                    result.failure.as_ref().map(|failure| failure.code),
                    Some(FailureCode::ProviderFailed)
                );
            }
            result.validate().expect("background result contract");

            let (identity, child_pid) =
                process_identity(&group_marker, &child_marker, &pgid_marker);
            assert_process_identity_absent(identity, child_pid).await;
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn process_group_identity_stays_unreaped_during_signal_cleanup() {
        let mut command = Command::new("/bin/sh");
        command
            .args(["-c", "(sleep 30) >/dev/null 2>&1 & exit 0"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        configure_process_group(&mut command);
        let mut child = command.spawn().expect("identity fixture spawn");
        let identity = ProcessGroupIdentity::capture(&child).expect("captured identity");
        assert_eq!(identity.pid, identity.pgid);

        wait_for_leader_exit(identity)
            .await
            .expect("leader exit observation");
        assert_eq!(
            child.id(),
            Some(identity.pid),
            "waitid(WNOWAIT) must not release the leader identity"
        );
        assert!(leader_exit_observed(identity).expect("non-reaping observation"));

        terminate_process_group(identity, false).expect("graceful group signal");
        assert!(
            wait_for_process_group_absence(identity)
                .await
                .expect("group absence observation")
        );
        assert_eq!(
            child.id(),
            Some(identity.pid),
            "leader identity must remain held until group absence is proven"
        );
        let status = child.wait().await.expect("single leader reap");
        assert!(status.success());
        assert_eq!(child.id(), None, "leader must be reaped only after cleanup");
        assert!(!process_group_present(identity, false).expect("post-reap group probe"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn process_adapter_forces_kill_for_term_ignoring_group() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let group_marker = temporary.path().join("term-ignore-group.pid");
        let child_marker = temporary.path().join("term-ignore-child.pid");
        let pgid_marker = temporary.path().join("term-ignore-pgid.pid");
        let helper =
            term_ignoring_fixture(temporary.path(), &group_marker, &child_marker, &pgid_marker);

        let mut command = Command::new(&helper);
        command
            .args(["--request", "/dev/null", "--output", "/dev/null"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        configure_process_group(&mut command);
        let mut child = command.spawn().expect("TERM-ignoring fixture spawn");
        let identity = ProcessGroupIdentity::capture(&child).expect("captured identity");
        wait_for_markers(&[&group_marker, &child_marker, &pgid_marker]).await;
        let report = cleanup_process_group(&mut child, identity)
            .await
            .expect("forced cleanup");
        assert!(
            report.forced,
            "TERM-ignoring group must take the KILL branch: {report:?}"
        );
        assert_eq!(child.id(), None, "forced cleanup must reap exactly once");

        let (marked_identity, child_pid) =
            process_identity(&group_marker, &child_marker, &pgid_marker);
        assert_eq!(marked_identity, identity);
        assert_process_identity_absent(marked_identity, child_pid).await;
    }
}
