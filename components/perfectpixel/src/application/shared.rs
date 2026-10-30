use std::path::{Path, PathBuf};

use serde::{de::DeserializeOwned, Serialize};

use crate::{
    AtomicArtifactSetWriter, EffectFailure, EffectFailureCode, FailureContext, ImageCodec,
    OperationErrorCode, OperationFailure, PpError, PpResult, Raster,
};

use super::{
    generation::GenerationWorkflow,
    generation_adapter::{
        plan_generation_publication, verify_generation_publication, GeneratedArtifact,
        GenerationPublicationRequest, InputSnapshot,
    },
    path::{managed_relative_path, reject_same_path},
};

pub(super) const MAX_CONTROL_READ_BYTES: usize = 8 * 1024 * 1024;
pub(super) const MAX_SVG_READ_BYTES: usize = 16 * 1024 * 1024;
pub(super) const MAX_ARTIFACT_FILE_BYTES: usize = 64 * 1024 * 1024;
pub(super) const MAX_RASTER_READ_BYTES: usize = 320 * 1024 * 1024;
pub(super) const MAX_GENERATION_DECODED_RASTER_BYTES: usize = 512 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApplicationOutput {
    pub stdout: String,
    pub exit_code: i32,
}

impl ApplicationOutput {
    pub(super) fn from_text(text: String, exit_code: i32) -> Self {
        Self {
            stdout: format!("{text}\n"),
            exit_code,
        }
    }
}

pub(super) fn render_result(
    result: PpResult<String>,
    operation: &str,
    phase: &'static str,
) -> ApplicationOutput {
    match result {
        Ok(text) => ApplicationOutput::from_text(text, 0),
        Err(PpError::VectorRejected { payload }) => ApplicationOutput::from_text(payload, 4),
        Err(PpError::CliTransactionFailed { exit_code, payload }) => {
            ApplicationOutput::from_text(payload, exit_code)
        }
        Err(error) => {
            let path = error_path(&error);
            let original_error = original_error(&error);
            let failure =
                operation_failure(operation, phase, path.as_deref(), &original_error, &error);
            let payload = ErrorPayload {
                ok: false,
                message: error_message(&error),
                phase,
                path,
                original_error,
                failure,
            };
            let text = serde_json::to_string(&payload).unwrap_or_else(|source| source.to_string());
            ApplicationOutput::from_text(text, exit_code(&error))
        }
    }
}

pub(super) fn operation_phase(name: &str) -> &'static str {
    match name {
        "image.inspect" | "image.convert" | "image.upscale" | "image.edit"
        | "image.chroma_plan" => "asset",
        "document.export_psd" | "document.compile_psd" => "document",
        "sprite.normalize" | "sprite.compile" => "sprite",
        "texture.compile" => "texture",
        "vector.compile" => "vector",
        "vector.analyze" => "vectorAnalyze",
        "vision.apple.foreground_instances" => "vision",
        "motion.scaffold" | "motion.compile" => "motion",
        _ => "application",
    }
}

fn operation_failure(
    operation: &str,
    phase: &'static str,
    path: Option<&str>,
    cause: &str,
    error: &PpError,
) -> OperationFailure {
    let mut context = vec![FailureContext {
        key: "phase".to_string(),
        value: phase.to_string(),
    }];
    if let Some(path) = path {
        context.push(FailureContext {
            key: "path".to_string(),
            value: path.to_string(),
        });
    }
    if let PpError::DependencyFailed { dependency, .. } = error {
        context.push(FailureContext {
            key: "dependency".to_string(),
            value: dependency.clone(),
        });
    }
    OperationFailure {
        code: operation_error_code(error),
        operation: semantic_operation(error).unwrap_or(operation).to_string(),
        cause: cause.to_string(),
        context,
        retryable: matches!(
            error,
            PpError::DependencyFailed {
                retryable: true,
                ..
            }
        ),
    }
}

fn semantic_operation(error: &PpError) -> Option<&str> {
    match error {
        PpError::Unsupported { operation, .. }
        | PpError::NotFound { operation, .. }
        | PpError::Conflict { operation, .. }
        | PpError::PreconditionFailed { operation, .. }
        | PpError::ResourceLimit { operation, .. }
        | PpError::Timeout { operation, .. }
        | PpError::Cancelled { operation, .. }
        | PpError::DependencyFailed { operation, .. }
        | PpError::VerificationFailed { operation, .. } => Some(operation.as_str()),
        _ => None,
    }
}

fn operation_error_code(error: &PpError) -> OperationErrorCode {
    match error {
        PpError::InvalidOption(_)
        | PpError::InvalidOptionSource { .. }
        | PpError::InvalidRequest(_)
        | PpError::InvalidRequestSource { .. }
        | PpError::SvgContract(_) => OperationErrorCode::InvalidArgument,
        PpError::Unsupported { .. } | PpError::UnsupportedVectorContent(_) => {
            OperationErrorCode::Unsupported
        }
        PpError::NotFound { .. } => OperationErrorCode::NotFound,
        PpError::Conflict { .. } => OperationErrorCode::Conflict,
        PpError::PreconditionFailed { .. } => OperationErrorCode::PreconditionFailed,
        PpError::ResourceLimit { .. } | PpError::ImageTooLarge { .. } => {
            OperationErrorCode::ResourceLimit
        }
        PpError::Timeout { .. } => OperationErrorCode::Timeout,
        PpError::Cancelled { .. } => OperationErrorCode::Cancelled,
        PpError::DependencyFailed { .. } => OperationErrorCode::DependencyFailed,
        PpError::VerificationFailed { .. }
        | PpError::VectorQuality { .. }
        | PpError::QualityGate { .. } => OperationErrorCode::VerificationFailed,
        PpError::FileIo { .. }
        | PpError::ImageDecode { .. }
        | PpError::ImageEncode { .. }
        | PpError::Json { .. }
        | PpError::Vectorizer(_)
        | PpError::SvgRender(_) => OperationErrorCode::DependencyFailed,
        PpError::VectorRejected { .. } | PpError::CliTransactionFailed { .. } => {
            OperationErrorCode::Internal
        }
    }
}

fn error_message(error: &PpError) -> String {
    match error {
        PpError::InvalidOption(message) if message.starts_with("unknown command '") => {
            format!("invalid option: {message}")
        }
        PpError::InvalidOption(message) | PpError::InvalidRequest(message) => message.clone(),
        PpError::InvalidOptionSource { message, .. }
        | PpError::InvalidRequestSource { message, .. } => message.clone(),
        PpError::Unsupported { cause, .. }
        | PpError::NotFound { cause, .. }
        | PpError::Conflict { cause, .. }
        | PpError::PreconditionFailed { cause, .. }
        | PpError::ResourceLimit { cause, .. }
        | PpError::Timeout { cause, .. }
        | PpError::Cancelled { cause, .. }
        | PpError::DependencyFailed { cause, .. }
        | PpError::VerificationFailed { cause, .. } => cause.clone(),
        _ => error.to_string(),
    }
}

pub(super) fn original_error(error: &PpError) -> String {
    match error {
        PpError::InvalidOptionSource { original_error, .. }
        | PpError::InvalidRequestSource { original_error, .. } => original_error.clone(),
        PpError::Unsupported { cause, .. }
        | PpError::NotFound { cause, .. }
        | PpError::Conflict { cause, .. }
        | PpError::PreconditionFailed { cause, .. }
        | PpError::ResourceLimit { cause, .. }
        | PpError::Timeout { cause, .. }
        | PpError::Cancelled { cause, .. }
        | PpError::DependencyFailed { cause, .. }
        | PpError::VerificationFailed { cause, .. } => cause.clone(),
        _ => error.to_string(),
    }
}

pub(super) fn error_path(error: &PpError) -> Option<String> {
    match error {
        PpError::FileIo { path, .. }
        | PpError::ImageDecode { path, .. }
        | PpError::ImageTooLarge { path, .. }
        | PpError::InvalidOptionSource { path, .. }
        | PpError::InvalidRequestSource { path, .. }
        | PpError::Json { path, .. } => Some(path.display().to_string()),
        PpError::InvalidRequest(message) => message
            .strip_prefix("destination '")
            .and_then(|value| value.split_once("':"))
            .map(|(path, _)| path.to_owned()),
        _ => None,
    }
}

fn exit_code(error: &PpError) -> i32 {
    match error {
        PpError::InvalidOption(_)
        | PpError::InvalidOptionSource { .. }
        | PpError::InvalidRequest(_)
        | PpError::InvalidRequestSource { .. }
        | PpError::SvgContract(_) => 2,
        PpError::FileIo { .. }
        | PpError::ImageDecode { .. }
        | PpError::ImageEncode { .. }
        | PpError::ImageTooLarge { .. }
        | PpError::SvgRender(_)
        | PpError::Json { .. }
        | PpError::NotFound { .. }
        | PpError::DependencyFailed { .. }
        | PpError::Timeout { .. } => 3,
        PpError::VectorRejected { .. }
        | PpError::VectorQuality { .. }
        | PpError::QualityGate { .. }
        | PpError::VerificationFailed { .. }
        | PpError::Unsupported { .. }
        | PpError::UnsupportedVectorContent(_) => 4,
        PpError::Conflict { .. }
        | PpError::PreconditionFailed { .. }
        | PpError::Cancelled { .. }
        | PpError::ResourceLimit { .. } => 5,
        PpError::CliTransactionFailed { exit_code, .. } => *exit_code,
        _ => 1,
    }
}

pub(super) fn effect_failure_to_error(failure: EffectFailure) -> PpError {
    match failure.code {
        EffectFailureCode::InvalidArgument => {
            PpError::InvalidRequest(format!("{}: {}", failure.operation, failure.cause))
        }
        EffectFailureCode::Unsupported => PpError::Unsupported {
            operation: failure.operation,
            cause: failure.cause,
        },
        EffectFailureCode::NotFound => PpError::NotFound {
            operation: failure.operation,
            cause: failure.cause,
        },
        EffectFailureCode::Conflict => PpError::Conflict {
            operation: failure.operation,
            cause: failure.cause,
        },
        EffectFailureCode::PreconditionFailed => PpError::PreconditionFailed {
            operation: failure.operation,
            cause: failure.cause,
        },
        EffectFailureCode::ResourceLimit => PpError::ResourceLimit {
            operation: failure.operation,
            cause: failure.cause,
        },
        EffectFailureCode::Timeout => PpError::Timeout {
            operation: failure.operation,
            cause: failure.cause,
        },
        EffectFailureCode::DependencyFailed => PpError::DependencyFailed {
            operation: failure.operation,
            dependency: failure.dependency,
            cause: failure.cause,
            retryable: failure.retryable,
        },
        EffectFailureCode::VerificationFailed => PpError::VerificationFailed {
            operation: failure.operation,
            cause: failure.cause,
        },
        EffectFailureCode::Internal => PpError::DependencyFailed {
            operation: failure.operation,
            dependency: failure.dependency,
            cause: failure.cause,
            retryable: false,
        },
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ErrorPayload {
    ok: bool,
    message: String,
    phase: &'static str,
    path: Option<String>,
    original_error: String,
    failure: OperationFailure,
}

pub(super) fn serialize_json<T: Serialize>(value: &T, path: &str) -> PpResult<String> {
    serde_json::to_string_pretty(value).map_err(|source| PpError::Json {
        path: PathBuf::from(path),
        message: source.to_string(),
    })
}

pub(super) fn read_bytes_limited(path: &Path, limit: usize) -> PpResult<Vec<u8>> {
    crate::io::capability::read_bounded(path, limit).map_err(|source| PpError::FileIo {
        path: path.to_path_buf(),
        message: source.to_string(),
    })
}

pub(super) fn read_utf8_limited(path: &Path, limit: usize) -> PpResult<String> {
    String::from_utf8(read_bytes_limited(path, limit)?).map_err(|source| PpError::FileIo {
        path: path.to_path_buf(),
        message: source.to_string(),
    })
}

pub(super) fn read_utf8_snapshot(path: &Path, limit: usize) -> PpResult<(String, InputSnapshot)> {
    let (bytes, snapshot) = InputSnapshot::capture(path, limit)?;
    let text = String::from_utf8(bytes).map_err(|source| PpError::FileIo {
        path: path.to_path_buf(),
        message: source.to_string(),
    })?;
    Ok((text, snapshot))
}

pub(super) fn read_json_request_snapshot<T: DeserializeOwned>(
    path: &Path,
) -> PpResult<(T, InputSnapshot)> {
    let (text, snapshot) = read_utf8_snapshot(path, MAX_CONTROL_READ_BYTES)?;
    let request = serde_json::from_str(&text).map_err(|source| PpError::InvalidRequestSource {
        message: format!("invalid JSON request at '{}': {source}", path.display()),
        path: path.to_path_buf(),
        original_error: source.to_string(),
    })?;
    Ok((request, snapshot))
}

pub(super) fn verify_request_digest(
    snapshot: &InputSnapshot,
    expected: Option<&str>,
) -> PpResult<()> {
    if expected.is_some_and(|expected| snapshot.sha256() != expected) {
        return Err(PpError::PreconditionFailed {
            operation: "request.admission".to_string(),
            cause: format!(
                "request '{}' changed after root validation",
                snapshot.source_path().display()
            ),
        });
    }
    Ok(())
}

pub(super) fn decode_raster_snapshot(path: &Path) -> PpResult<(Raster, InputSnapshot)> {
    let (bytes, snapshot) = InputSnapshot::capture(path, MAX_RASTER_READ_BYTES)?;
    let raster = ImageCodec::decode_rgba_bytes(path, &bytes, Default::default())?;
    Ok((raster, snapshot))
}

pub(super) fn account_decoded_raster_bytes(total: &mut usize, raster: &Raster) -> PpResult<()> {
    *total = total
        .checked_add(raster.pixels().len())
        .ok_or_else(|| PpError::InvalidRequest("decoded raster byte count overflow".to_string()))?;
    if *total > MAX_GENERATION_DECODED_RASTER_BYTES {
        return Err(PpError::InvalidRequest(format!(
            "generation decoded rasters exceed {MAX_GENERATION_DECODED_RASTER_BYTES}-byte limit"
        )));
    }
    Ok(())
}

pub(super) struct BundleArtifact {
    pub relative_path: String,
    pub bytes: Vec<u8>,
}

pub(super) fn text_artifact(relative_path: &str, text: String) -> BundleArtifact {
    BundleArtifact {
        relative_path: relative_path.to_string(),
        bytes: text.into_bytes(),
    }
}

pub(super) fn publish_generated_artifacts(
    out_dir: &Path,
    workflow: GenerationWorkflow,
    artifacts: Vec<BundleArtifact>,
    invalidates: Vec<GenerationWorkflow>,
    input_snapshots: Vec<InputSnapshot>,
) -> PpResult<()> {
    AtomicArtifactSetWriter::publish_with_planner_checked(
        out_dir,
        move |locked_root| {
            plan_generation_publication(
                locked_root,
                GenerationPublicationRequest {
                    workflow,
                    artifacts: artifacts
                        .into_iter()
                        .map(|artifact| GeneratedArtifact {
                            relative_path: artifact.relative_path,
                            bytes: artifact.bytes,
                        })
                        .collect(),
                    invalidates,
                    input_snapshots,
                },
            )
        },
        verify_generation_publication,
    )
}

pub(super) fn reject_generated_output_collisions(
    out_dir: &Path,
    outputs: &[String],
    inputs: &[InputSnapshot],
) -> PpResult<()> {
    for relative in outputs {
        let output = out_dir.join(managed_relative_path(relative)?);
        for input in inputs {
            reject_same_path(
                input.source_path(),
                &output,
                "generated output must not overwrite its request or input",
            )?;
        }
    }
    Ok(())
}
