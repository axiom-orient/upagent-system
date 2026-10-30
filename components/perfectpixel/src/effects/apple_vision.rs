use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
    time::Duration,
};

use serde::{Deserialize, Serialize};

use crate::runtime::{
    run_process, PinnedExecutable, ProcessRunError, ProcessSpec, ProcessTermination,
};
use crate::{
    ArtifactRef, EffectCompletion, EffectFailure, EffectFailureCode, EffectIdentity, EffectResult,
    PpError, PpResult, Sha256Digest,
};

use super::ExternalToolReceipt;

const RECEIPT_SCHEMA: &str = "perfectpixel.apple-vision-helper-receipt/1";
const MAX_RECEIPT_BYTES: usize = 256 * 1024;
const MAX_MASK_BYTES: usize = 64 * 1024 * 1024;
const MAX_TOTAL_MASK_BYTES: usize = 256 * 1024 * 1024;
const PROCESS_EVIDENCE_BYTES: usize = 256 * 1024;

#[derive(Debug, Clone)]
pub struct AppleVisionInstancesEffectRequest {
    pub identity: EffectIdentity,
    pub executable: PinnedExecutable,
    pub input: PathBuf,
    pub staging_directory: PathBuf,
    pub request_revision: u32,
    pub timeout: Duration,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AppleVisionHelperReceipt {
    pub schema: String,
    pub provider: String,
    pub adapter_version: String,
    pub request_type: String,
    pub request_revision: u32,
    pub os_version: String,
    pub instances: Vec<AppleVisionHelperInstance>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AppleVisionHelperInstance {
    pub id: u32,
    pub file: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppleVisionInstanceCandidate {
    pub id: u32,
    pub artifact: ArtifactRef,
    pub bytes: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppleVisionInstancesCandidateSet {
    pub instances: Vec<AppleVisionInstanceCandidate>,
    pub helper_receipt: AppleVisionHelperReceipt,
    pub tool_receipt: ExternalToolReceipt,
}

pub fn run_apple_vision_instances_effect(
    request: &AppleVisionInstancesEffectRequest,
) -> EffectResult<AppleVisionInstancesCandidateSet> {
    if request.request_revision != 1 {
        return failed(
            request.identity.clone(),
            EffectFailureCode::Unsupported,
            format!(
                "Apple Vision request revision {} is unsupported",
                request.request_revision
            ),
            false,
        );
    }
    if let Err(error) = validate_paths(&request.input, &request.staging_directory) {
        return failed(
            request.identity.clone(),
            EffectFailureCode::InvalidArgument,
            error.to_string(),
            false,
        );
    }

    let args = vec![
        "foreground-instances".to_string(),
        "--input".to_string(),
        request.input.to_string_lossy().into_owned(),
        "--output-dir".to_string(),
        request.staging_directory.to_string_lossy().into_owned(),
        "--revision".to_string(),
        request.request_revision.to_string(),
    ];
    let spec = ProcessSpec {
        executable: request.executable.clone(),
        args,
        environment: vec![
            ("LANG".to_string(), "C".to_string()),
            ("LC_ALL".to_string(), "C".to_string()),
            ("TZ".to_string(), "UTC".to_string()),
        ],
        timeout: request.timeout,
        max_stdout_bytes: PROCESS_EVIDENCE_BYTES,
        max_stderr_bytes: PROCESS_EVIDENCE_BYTES,
    };
    let process = match run_process(&spec) {
        Ok(process) => process,
        Err(error) => return process_failure(request.identity.clone(), error),
    };
    match process.termination {
        ProcessTermination::TimedOut => {
            return failed(
                request.identity.clone(),
                EffectFailureCode::Timeout,
                "Apple Vision helper exceeded timeout".to_string(),
                true,
            );
        }
        ProcessTermination::Signaled => {
            return failed(
                request.identity.clone(),
                EffectFailureCode::DependencyFailed,
                "Apple Vision helper terminated by signal".to_string(),
                true,
            );
        }
        ProcessTermination::Exited(code) if code != 0 => {
            return failed(
                request.identity.clone(),
                EffectFailureCode::DependencyFailed,
                format!(
                    "Apple Vision helper exited with {code}: {}",
                    String::from_utf8_lossy(&process.stderr).trim()
                ),
                false,
            );
        }
        ProcessTermination::Exited(_) => {}
    }

    let candidate = match read_candidate_set(request, &spec.args, &process) {
        Ok(candidate) => candidate,
        Err(error) => {
            return failed(
                request.identity.clone(),
                EffectFailureCode::DependencyFailed,
                error.to_string(),
                false,
            );
        }
    };
    EffectResult {
        identity: request.identity.clone(),
        completion: EffectCompletion::Succeeded { value: candidate },
    }
}

fn read_candidate_set(
    request: &AppleVisionInstancesEffectRequest,
    args: &[String],
    process: &crate::runtime::ProcessOutput,
) -> PpResult<AppleVisionInstancesCandidateSet> {
    let receipt_path = request.staging_directory.join("receipt.json");
    let receipt_bytes = crate::io::capability::read_bounded(&receipt_path, MAX_RECEIPT_BYTES)
        .map_err(|error| PpError::FileIo {
            path: receipt_path.clone(),
            message: error.to_string(),
        })?;
    let receipt: AppleVisionHelperReceipt =
        serde_json::from_slice(&receipt_bytes).map_err(|error| PpError::InvalidRequestSource {
            message: "Apple Vision helper receipt is not valid typed JSON".to_string(),
            path: receipt_path.clone(),
            original_error: error.to_string(),
        })?;
    validate_receipt(&receipt, request.request_revision)?;

    let mut ids = BTreeSet::new();
    let mut total = 0usize;
    let mut instances = Vec::with_capacity(receipt.instances.len());
    for entry in &receipt.instances {
        if !ids.insert(entry.id) {
            return Err(PpError::InvalidRequest(
                "Apple Vision helper repeated an instance id".to_string(),
            ));
        }
        let expected = format!("mask-{:08}.png", entry.id);
        if entry.file != expected {
            return Err(PpError::InvalidRequest(format!(
                "Apple Vision helper returned non-canonical mask filename '{}' for instance {}",
                entry.file, entry.id
            )));
        }
        let path = request.staging_directory.join(&entry.file);
        let bytes =
            crate::io::capability::read_bounded(&path, MAX_MASK_BYTES).map_err(|error| {
                PpError::FileIo {
                    path: path.clone(),
                    message: error.to_string(),
                }
            })?;
        if bytes.is_empty() {
            return Err(PpError::InvalidRequest(format!(
                "Apple Vision instance {} mask is empty",
                entry.id
            )));
        }
        total = total
            .checked_add(bytes.len())
            .ok_or_else(|| PpError::ResourceLimit {
                operation: "vision.apple.foreground_instances".to_string(),
                cause: "Vision mask byte count overflow".to_string(),
            })?;
        if total > MAX_TOTAL_MASK_BYTES {
            return Err(PpError::ResourceLimit {
                operation: "vision.apple.foreground_instances".to_string(),
                cause: format!("Vision mask set exceeds {MAX_TOTAL_MASK_BYTES} bytes"),
            });
        }
        let artifact = ArtifactRef::from_bytes("image/png", &bytes)?;
        instances.push(AppleVisionInstanceCandidate {
            id: entry.id,
            artifact,
            bytes,
        });
    }

    Ok(AppleVisionInstancesCandidateSet {
        instances,
        helper_receipt: receipt,
        tool_receipt: ExternalToolReceipt {
            tool: "perfectpixel-vision-helper".to_string(),
            executable_sha256: request.executable.sha256().clone(),
            arguments_sha256: Sha256Digest::from_bytes(canonical_arguments(args).as_bytes()),
            stdout_sha256: Sha256Digest::from_bytes(&process.stdout),
            stderr_sha256: Sha256Digest::from_bytes(&process.stderr),
            stdout_truncated: process.stdout_truncated,
            stderr_truncated: process.stderr_truncated,
        },
    })
}

fn validate_receipt(receipt: &AppleVisionHelperReceipt, revision: u32) -> PpResult<()> {
    if receipt.schema != RECEIPT_SCHEMA
        || receipt.provider != "apple_vision"
        || receipt.adapter_version != "1"
        || receipt.request_type != "VNGenerateForegroundInstanceMaskRequest"
        || receipt.request_revision != revision
        || receipt.os_version.is_empty()
        || receipt.instances.is_empty()
    {
        return Err(PpError::InvalidRequest(
            "Apple Vision helper receipt does not satisfy the pinned adapter contract".to_string(),
        ));
    }
    Ok(())
}

fn validate_paths(input: &Path, staging_directory: &Path) -> PpResult<()> {
    if !input.is_absolute() || !staging_directory.is_absolute() {
        return Err(PpError::InvalidRequest(
            "Apple Vision Effect paths must be absolute".to_string(),
        ));
    }
    let input_file = crate::io::capability::open_read(input).map_err(|error| PpError::FileIo {
        path: input.to_path_buf(),
        message: error.to_string(),
    })?;
    if !input_file
        .metadata()
        .map_err(|error| PpError::FileIo {
            path: input.to_path_buf(),
            message: error.to_string(),
        })?
        .is_file()
    {
        return Err(PpError::InvalidRequest(
            "Apple Vision input must be a regular no-follow file".to_string(),
        ));
    }
    let parent = staging_directory.parent().ok_or_else(|| {
        PpError::InvalidRequest("Apple Vision staging directory has no parent".to_string())
    })?;
    crate::io::capability::open_directory(parent).map_err(|error| PpError::FileIo {
        path: parent.to_path_buf(),
        message: error.to_string(),
    })?;
    match std::fs::symlink_metadata(staging_directory) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Ok(_) => Err(PpError::InvalidRequest(
            "Apple Vision staging directory must not already exist".to_string(),
        )),
        Err(error) => Err(PpError::FileIo {
            path: staging_directory.to_path_buf(),
            message: error.to_string(),
        }),
    }
}

fn canonical_arguments(args: &[String]) -> String {
    let mut output = String::new();
    for arg in args {
        output.push_str(&arg.len().to_string());
        output.push(':');
        output.push_str(arg);
        output.push(';');
    }
    output
}

fn process_failure(
    identity: EffectIdentity,
    error: ProcessRunError,
) -> EffectResult<AppleVisionInstancesCandidateSet> {
    let (code, retryable) = match error {
        ProcessRunError::InvalidRequest(_) => (EffectFailureCode::InvalidArgument, false),
        ProcessRunError::InvalidExecutable(_)
        | ProcessRunError::ExecutableDigestMismatch { .. }
        | ProcessRunError::ExecutableChanged(_) => (EffectFailureCode::PreconditionFailed, false),
        ProcessRunError::Io(_) | ProcessRunError::Reader(_) => {
            (EffectFailureCode::DependencyFailed, true)
        }
    };
    failed(identity, code, error.to_string(), retryable)
}

fn failed(
    identity: EffectIdentity,
    code: EffectFailureCode,
    cause: String,
    retryable: bool,
) -> EffectResult<AppleVisionInstancesCandidateSet> {
    EffectResult {
        identity,
        completion: EffectCompletion::Failed {
            error: EffectFailure {
                code,
                operation: "vision.apple.foreground_instances".to_string(),
                dependency: "perfectpixel-vision-helper".to_string(),
                retryable,
                cause,
                context: Vec::new(),
            },
        },
    }
}
