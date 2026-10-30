use std::{io, path::Path};

use rax_core::{ErrorCode, ErrorPhase, RaxError};

pub(crate) fn invalid_path(message: impl Into<String>, path: impl Into<String>) -> RaxError {
    RaxError::new(ErrorCode::InvalidPath, ErrorPhase::Validate, false, message)
        .with_detail("path", path.into())
}

pub(crate) fn operation_error(
    code: ErrorCode,
    phase: ErrorPhase,
    message: impl Into<String>,
) -> RaxError {
    RaxError::new(code, phase, false, message)
}

#[allow(clippy::needless_pass_by_value)]
pub(crate) fn io_error(
    phase: ErrorPhase,
    message: impl Into<String>,
    path: Option<&Path>,
    error: io::Error,
) -> RaxError {
    let code = match error.kind() {
        io::ErrorKind::NotFound => ErrorCode::NotFound,
        io::ErrorKind::AlreadyExists => ErrorCode::AlreadyExists,
        io::ErrorKind::InvalidInput | io::ErrorKind::InvalidData => ErrorCode::InvalidRequest,
        _ => ErrorCode::OperationFailed,
    };
    let retryable = matches!(
        error.kind(),
        io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
    );
    let mut output = RaxError::new(
        code,
        phase,
        retryable,
        format!("{}: {error}", message.into()),
    )
    .with_detail("ioKind", format!("{:?}", error.kind()));
    if let Some(path) = path {
        output = output.with_detail("path", path.to_string_lossy().to_string());
    }
    output
}
