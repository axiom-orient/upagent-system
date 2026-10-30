use std::{
    path::{Component, Path, PathBuf},
    time::Duration,
};

use serde::{Deserialize, Serialize};

use crate::{
    runtime::{run_process, PinnedExecutable, ProcessRunError, ProcessSpec, ProcessTermination},
    ArtifactRef, EffectCompletion, EffectFailure, EffectFailureCode, EffectIdentity, EffectResult,
    PpError, PpResult, Sha256Digest,
};

const MAX_CANDIDATE_BYTES: usize = 128 * 1024 * 1024;
const PROCESS_EVIDENCE_BYTES: usize = 256 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExternalToolReceipt {
    pub tool: String,
    pub executable_sha256: Sha256Digest,
    pub arguments_sha256: Sha256Digest,
    pub stdout_sha256: Sha256Digest,
    pub stderr_sha256: Sha256Digest,
    pub stdout_truncated: bool,
    pub stderr_truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExternalArtifactCandidate {
    pub artifact: ArtifactRef,
    pub bytes: Vec<u8>,
    pub receipt: ExternalToolReceipt,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KtxEncoding {
    BasisLz,
    Uastc,
}

impl KtxEncoding {
    fn cli_value(self) -> &'static str {
        match self {
            Self::BasisLz => "basis-lz",
            Self::Uastc => "uastc-ldr-4x4",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Ktx2EffectRequest {
    pub identity: EffectIdentity,
    pub executable: PinnedExecutable,
    pub input: PathBuf,
    pub staging_output: PathBuf,
    pub encoding: KtxEncoding,
    pub generate_mipmaps: bool,
    pub srgb: bool,
    pub timeout: Duration,
}

fn ktx_arguments(
    input: &Path,
    output: &Path,
    encoding: KtxEncoding,
    generate_mipmaps: bool,
    srgb: bool,
) -> Vec<String> {
    let mut args = vec![
        "create".to_string(),
        "--format".to_string(),
        if srgb {
            "R8G8B8A8_SRGB".to_string()
        } else {
            "R8G8B8A8_UNORM".to_string()
        },
        "--assign-tf".to_string(),
        if srgb {
            "srgb".to_string()
        } else {
            "linear".to_string()
        },
        "--encode".to_string(),
        encoding.cli_value().to_string(),
        "--threads".to_string(),
        "1".to_string(),
        "--fail-on-color-conversions".to_string(),
        "--fail-on-origin-changes".to_string(),
    ];
    if generate_mipmaps {
        args.extend([
            "--generate-mipmap".to_string(),
            "--mipmap-filter".to_string(),
            "lanczos4".to_string(),
            "--mipmap-wrap".to_string(),
            "clamp".to_string(),
        ]);
    }
    args.push(input.to_string_lossy().into_owned());
    args.push(output.to_string_lossy().into_owned());
    args
}

pub fn run_ktx2_effect(request: &Ktx2EffectRequest) -> EffectResult<ExternalArtifactCandidate> {
    let identity = &request.identity;
    let operation = "texture.encode_ktx2";
    let dependency = "ktx";
    let executable = &request.executable;
    let input = &request.input;
    let staging_output = &request.staging_output;
    let args = ktx_arguments(
        input,
        staging_output,
        request.encoding,
        request.generate_mipmaps,
        request.srgb,
    );
    if let Err(error) = validate_effect_paths(input, staging_output) {
        return failure(
            identity.clone(),
            operation,
            dependency,
            EffectFailureCode::InvalidArgument,
            error.to_string(),
            false,
        );
    }
    let spec = ProcessSpec {
        executable: executable.clone(),
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
        Err(error) => return process_error(identity.clone(), operation, dependency, error),
    };
    match output.termination {
        ProcessTermination::TimedOut => {
            return failure(
                identity.clone(),
                operation,
                dependency,
                EffectFailureCode::Timeout,
                format!("{dependency} exceeded timeout"),
                true,
            );
        }
        ProcessTermination::Signaled => {
            return failure(
                identity.clone(),
                operation,
                dependency,
                EffectFailureCode::DependencyFailed,
                format!("{dependency} terminated by signal"),
                true,
            );
        }
        ProcessTermination::Exited(code) if code != 0 => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return failure(
                identity.clone(),
                operation,
                dependency,
                EffectFailureCode::DependencyFailed,
                format!("{dependency} exited with {code}: {}", stderr.trim()),
                false,
            );
        }
        ProcessTermination::Exited(_) => {}
    }
    let bytes = match read_candidate(staging_output) {
        Ok(bytes) => bytes,
        Err(error) => {
            return failure(
                identity.clone(),
                operation,
                dependency,
                EffectFailureCode::DependencyFailed,
                error.to_string(),
                false,
            );
        }
    };
    let artifact = match ArtifactRef::from_bytes("image/ktx2", &bytes) {
        Ok(artifact) => artifact,
        Err(error) => {
            return failure(
                identity.clone(),
                operation,
                dependency,
                EffectFailureCode::Internal,
                error.to_string(),
                false,
            );
        }
    };
    EffectResult {
        identity: identity.clone(),
        completion: EffectCompletion::Succeeded {
            value: ExternalArtifactCandidate {
                artifact,
                bytes,
                receipt: ExternalToolReceipt {
                    tool: dependency.to_string(),
                    executable_sha256: executable.sha256().clone(),
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

fn validate_effect_paths(input: &Path, staging_output: &Path) -> PpResult<()> {
    if !input.is_absolute() || !staging_output.is_absolute() {
        return Err(PpError::InvalidRequest(
            "external Effect input and staging output paths must be absolute".to_string(),
        ));
    }
    if input == staging_output {
        return Err(PpError::InvalidRequest(
            "external Effect staging output must not overwrite input".to_string(),
        ));
    }
    if staging_output
        .components()
        .any(|component| matches!(component, Component::ParentDir))
    {
        return Err(PpError::InvalidRequest(
            "external Effect staging output must not contain '..'".to_string(),
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
            "external Effect input must be a regular no-follow file".to_string(),
        ));
    }

    let parent = staging_output.parent().ok_or_else(|| {
        PpError::InvalidRequest("external Effect staging output has no parent".to_string())
    })?;
    crate::io::capability::open_directory(parent).map_err(|error| PpError::FileIo {
        path: parent.to_path_buf(),
        message: error.to_string(),
    })?;

    match crate::io::capability::open_read(staging_output) {
        Ok(_) => {
            return Err(PpError::InvalidRequest(
                "external Effect staging output must not already exist".to_string(),
            ))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            if crate::io::capability::open_directory(staging_output).is_ok() {
                return Err(PpError::InvalidRequest(
                    "external Effect staging output must not already exist".to_string(),
                ));
            }
            return Err(PpError::InvalidRequest(format!(
                "external Effect staging output is not an absent regular path: {error}"
            )));
        }
    }
    Ok(())
}

fn read_candidate(path: &Path) -> PpResult<Vec<u8>> {
    let bytes =
        crate::io::capability::read_bounded(path, MAX_CANDIDATE_BYTES).map_err(|error| {
            PpError::FileIo {
                path: path.to_path_buf(),
                message: error.to_string(),
            }
        })?;
    if bytes.is_empty() {
        return Err(PpError::InvalidRequest(
            "external Effect candidate must not be empty".to_string(),
        ));
    }
    Ok(bytes)
}

pub(super) fn canonical_arguments(args: &[String]) -> String {
    let mut canonical = String::new();
    for argument in args {
        canonical.push_str(&argument.len().to_string());
        canonical.push(':');
        canonical.push_str(argument);
        canonical.push('\n');
    }
    canonical
}

fn process_error<T>(
    identity: EffectIdentity,
    operation: &str,
    dependency: &str,
    error: ProcessRunError,
) -> EffectResult<T> {
    let code = match error {
        ProcessRunError::InvalidRequest(_) | ProcessRunError::InvalidExecutable(_) => {
            EffectFailureCode::InvalidArgument
        }
        ProcessRunError::ExecutableDigestMismatch { .. }
        | ProcessRunError::ExecutableChanged(_) => EffectFailureCode::PreconditionFailed,
        ProcessRunError::Io(_) | ProcessRunError::Reader(_) => EffectFailureCode::DependencyFailed,
    };
    failure(
        identity,
        operation,
        dependency,
        code,
        error.to_string(),
        false,
    )
}

fn failure<T>(
    identity: EffectIdentity,
    operation: &str,
    dependency: &str,
    code: EffectFailureCode,
    cause: String,
    retryable: bool,
) -> EffectResult<T> {
    EffectResult {
        identity,
        completion: EffectCompletion::Failed {
            error: EffectFailure {
                code,
                operation: operation.to_string(),
                dependency: dependency.to_string(),
                retryable,
                cause,
                context: Vec::new(),
            },
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ktx_arguments_force_determinism_and_fail_closed_color() {
        let args = ktx_arguments(
            Path::new("/tmp/in.png"),
            Path::new("/tmp/out.ktx2"),
            KtxEncoding::Uastc,
            true,
            true,
        );
        assert!(args.windows(2).any(|pair| pair == ["--threads", "1"]));
        assert!(args.windows(2).any(|pair| pair == ["--assign-tf", "srgb"]));
        assert!(args
            .iter()
            .any(|value| value == "--fail-on-color-conversions"));
        assert!(args.iter().any(|value| value == "--fail-on-origin-changes"));
        assert!(args.iter().any(|value| value == "--generate-mipmap"));
        assert!(args.iter().any(|value| value == "uastc-ldr-4x4"));
    }

    #[test]
    fn ktx_linear_semantics_are_assigned_not_inferred() {
        let args = ktx_arguments(
            Path::new("/tmp/in.png"),
            Path::new("/tmp/out.ktx2"),
            KtxEncoding::BasisLz,
            false,
            false,
        );
        assert!(args
            .windows(2)
            .any(|pair| pair == ["--assign-tf", "linear"]));
        assert!(args
            .windows(2)
            .any(|pair| pair == ["--format", "R8G8B8A8_UNORM"]));
    }

    #[test]
    fn canonical_argument_digest_is_boundary_unambiguous() {
        assert_ne!(
            canonical_arguments(&["ab".to_string(), "c".to_string()]),
            canonical_arguments(&["a".to_string(), "bc".to_string()])
        );
    }
}
