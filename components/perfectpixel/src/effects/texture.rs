use std::{path::PathBuf, time::Duration};

use crate::{
    runtime::{run_process, PinnedExecutable, ProcessRunError, ProcessSpec, ProcessTermination},
    ArtifactRef, EffectCompletion, EffectFailure, EffectFailureCode, EffectIdentity, EffectResult,
    Sha256Digest,
};

use super::{external::canonical_arguments, ExternalArtifactCandidate, ExternalToolReceipt};

const PROCESS_EVIDENCE_BYTES: usize = 256 * 1024;
const MAX_EXTRACT_BYTES: usize = 128 * 1024 * 1024;

#[derive(Debug, Clone)]
pub struct Ktx2ExtractRequest {
    pub identity: EffectIdentity,
    pub executable: PinnedExecutable,
    pub input: PathBuf,
    pub staging_output: PathBuf,
    pub timeout: Duration,
}

/// External verification Effect: decode KTX2 level 0 through the official tool into PNG.
/// The returned PNG is still only a candidate; the application verifies it against the source.
pub fn run_ktx2_extract_effect(
    request: &Ktx2ExtractRequest,
) -> EffectResult<ExternalArtifactCandidate> {
    if !request.input.is_absolute() || !request.staging_output.is_absolute() {
        return failed(
            &request.identity,
            EffectFailureCode::InvalidArgument,
            "KTX2 extract paths must be absolute",
            false,
        );
    }
    if request.input == request.staging_output {
        return failed(
            &request.identity,
            EffectFailureCode::InvalidArgument,
            "KTX2 extract output must differ from input",
            false,
        );
    }
    if let Err(error) = crate::io::capability::open_read(&request.input) {
        return failed(
            &request.identity,
            EffectFailureCode::NotFound,
            &format!("KTX2 candidate is not readable: {error}"),
            false,
        );
    }
    match crate::io::capability::open_read(&request.staging_output) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        _ => {
            return failed(
                &request.identity,
                EffectFailureCode::PreconditionFailed,
                "KTX2 extract staging output already exists",
                false,
            )
        }
    }

    let args = vec![
        "extract".to_string(),
        "--transcode".to_string(),
        "rgba8".to_string(),
        request.input.to_string_lossy().into_owned(),
        request.staging_output.to_string_lossy().into_owned(),
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
    let output = match run_process(&spec) {
        Ok(output) => output,
        Err(error) => return process_failure(&request.identity, error),
    };
    match output.termination {
        ProcessTermination::TimedOut => {
            return failed(
                &request.identity,
                EffectFailureCode::Timeout,
                "ktx extract exceeded timeout",
                true,
            )
        }
        ProcessTermination::Signaled => {
            return failed(
                &request.identity,
                EffectFailureCode::DependencyFailed,
                "ktx extract terminated by signal",
                true,
            )
        }
        ProcessTermination::Exited(code) if code != 0 => {
            return failed(
                &request.identity,
                EffectFailureCode::DependencyFailed,
                &format!(
                    "ktx extract exited with {code}: {}",
                    String::from_utf8_lossy(&output.stderr).trim()
                ),
                false,
            )
        }
        ProcessTermination::Exited(_) => {}
    }

    let bytes =
        match crate::io::capability::read_bounded(&request.staging_output, MAX_EXTRACT_BYTES) {
            Ok(bytes) if !bytes.is_empty() => bytes,
            Ok(_) => {
                return failed(
                    &request.identity,
                    EffectFailureCode::VerificationFailed,
                    "ktx extract produced an empty candidate",
                    false,
                )
            }
            Err(error) => {
                return failed(
                    &request.identity,
                    EffectFailureCode::DependencyFailed,
                    &format!("ktx extract output read failed: {error}"),
                    false,
                )
            }
        };
    let artifact = match ArtifactRef::from_bytes("image/png", &bytes) {
        Ok(artifact) => artifact,
        Err(error) => {
            return failed(
                &request.identity,
                EffectFailureCode::Internal,
                &error.to_string(),
                false,
            )
        }
    };
    EffectResult {
        identity: request.identity.clone(),
        completion: EffectCompletion::Succeeded {
            value: ExternalArtifactCandidate {
                artifact,
                bytes,
                receipt: ExternalToolReceipt {
                    tool: "ktx".to_string(),
                    executable_sha256: request.executable.sha256().clone(),
                    arguments_sha256: Sha256Digest::from_bytes(
                        canonical_arguments(&spec.args).as_bytes(),
                    ),
                    stdout_sha256: Sha256Digest::from_bytes(&output.stdout),
                    stderr_sha256: Sha256Digest::from_bytes(&output.stderr),
                    stdout_truncated: output.stdout_truncated,
                    stderr_truncated: output.stderr_truncated,
                },
            },
        },
    }
}

fn process_failure<T>(identity: &EffectIdentity, error: ProcessRunError) -> EffectResult<T> {
    let code = match error {
        ProcessRunError::InvalidRequest(_) | ProcessRunError::InvalidExecutable(_) => {
            EffectFailureCode::InvalidArgument
        }
        ProcessRunError::ExecutableDigestMismatch { .. }
        | ProcessRunError::ExecutableChanged(_) => EffectFailureCode::PreconditionFailed,
        ProcessRunError::Io(_) | ProcessRunError::Reader(_) => EffectFailureCode::DependencyFailed,
    };
    failed(identity, code, &error.to_string(), false)
}

fn failed<T>(
    identity: &EffectIdentity,
    code: EffectFailureCode,
    cause: &str,
    retryable: bool,
) -> EffectResult<T> {
    EffectResult {
        identity: identity.clone(),
        completion: EffectCompletion::Failed {
            error: EffectFailure {
                code,
                operation: "texture.decode_ktx2".to_string(),
                dependency: "ktx".to_string(),
                retryable,
                cause: cause.to_string(),
                context: Vec::new(),
            },
        },
    }
}
