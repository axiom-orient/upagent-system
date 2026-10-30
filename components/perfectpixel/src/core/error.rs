use std::path::PathBuf;

use thiserror::Error;

pub type PpResult<T> = Result<T, PpError>;

#[derive(Debug, Error)]
pub enum PpError {
    #[error("invalid option: {0}")]
    InvalidOption(String),
    #[error("invalid option: {message}")]
    InvalidOptionSource {
        message: String,
        path: PathBuf,
        original_error: String,
    },
    #[error("invalid request: {0}")]
    InvalidRequest(String),
    #[error("invalid request: {message}")]
    InvalidRequestSource {
        message: String,
        path: PathBuf,
        original_error: String,
    },
    #[error("unsupported during {operation}: {cause}")]
    Unsupported { operation: String, cause: String },
    #[error("not found during {operation}: {cause}")]
    NotFound { operation: String, cause: String },
    #[error("conflict during {operation}: {cause}")]
    Conflict { operation: String, cause: String },
    #[error("precondition failed during {operation}: {cause}")]
    PreconditionFailed { operation: String, cause: String },
    #[error("resource limit during {operation}: {cause}")]
    ResourceLimit { operation: String, cause: String },
    #[error("timeout during {operation}: {cause}")]
    Timeout { operation: String, cause: String },
    #[error("cancelled during {operation}: {cause}")]
    Cancelled { operation: String, cause: String },
    #[error("dependency '{dependency}' failed during {operation}: {cause}")]
    DependencyFailed {
        operation: String,
        dependency: String,
        cause: String,
        retryable: bool,
    },
    #[error("verification failed during {operation}: {cause}")]
    VerificationFailed { operation: String, cause: String },
    #[error("image decode failed for {path}: {message}")]
    ImageDecode { path: PathBuf, message: String },
    #[error("image encode failed: {message}")]
    ImageEncode { message: String },
    #[error(
        "image too large for {path}: {width}x{height}; max dimension {max_dimension}, max pixels {max_pixels}"
    )]
    ImageTooLarge {
        path: PathBuf,
        width: u32,
        height: u32,
        max_dimension: u32,
        max_pixels: u64,
    },
    #[error("file I/O failed for {path}: {message}")]
    FileIo { path: PathBuf, message: String },
    #[error("json failed for {path}: {message}")]
    Json { path: PathBuf, message: String },
    #[error("vectorizer failed: {0}")]
    Vectorizer(String),
    #[error("{payload}")]
    VectorRejected { payload: String },
    #[error("{payload}")]
    CliTransactionFailed { exit_code: i32, payload: String },
    #[error("unsupported vector content: {0}")]
    UnsupportedVectorContent(String),
    #[error("svg render failed: {0}")]
    SvgRender(String),
    #[error(
        "vector quality check failed for {metric}: score {score:.6} below required {min_score:.6}"
    )]
    VectorQuality {
        metric: &'static str,
        score: f64,
        min_score: f64,
    },
    #[error("svg contract violation: {0}")]
    SvgContract(String),
    #[error("quality gate failed for {gate}: {message}")]
    QualityGate { gate: String, message: String },
}
