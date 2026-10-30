use std::{collections::BTreeMap, path::Path};

use cap_std::fs::Dir;
use rax_core::{ErrorCode, ErrorPhase, FsMutationOutput, RaxError};

use crate::error::{io_error, operation_error};

pub(super) fn ensure_target_absent(dir: &Dir, path: &Path, display: &str) -> Result<(), RaxError> {
    if dir.try_exists(path).map_err(|error| {
        io_error(
            ErrorPhase::Apply,
            "failed to inspect target",
            Some(path),
            error,
        )
    })? {
        return Err(operation_error(
            ErrorCode::AlreadyExists,
            ErrorPhase::Apply,
            "target already exists",
        )
        .with_detail("path", display.to_string()));
    }
    Ok(())
}

pub(super) fn precondition_error(
    path: &str,
    expected: Option<&String>,
    actual: Option<&str>,
) -> RaxError {
    let mut error = operation_error(
        ErrorCode::PreconditionFailed,
        ErrorPhase::Authorize,
        "file content no longer matches the expected SHA-256",
    )
    .with_detail("path", path.to_string());
    if let Some(expected) = expected {
        error = error.with_detail("expectedSha256", expected.clone());
    }
    if let Some(actual) = actual {
        error = error.with_detail("actualSha256", actual.to_string());
    }
    error
}

pub(super) fn mutation_output(
    changed_paths: Vec<String>,
    before: Option<(String, String)>,
    after: Option<(String, String)>,
) -> FsMutationOutput {
    FsMutationOutput {
        changed_paths,
        before_sha256: before.into_iter().collect::<BTreeMap<_, _>>(),
        after_sha256: after.into_iter().collect::<BTreeMap<_, _>>(),
        verified: true,
    }
}

pub(super) fn mark_partial(mut error: RaxError, reason: &str) -> RaxError {
    error
        .details
        .insert("partialApplied".to_string(), true.into());
    error
        .details
        .insert("partialReason".to_string(), reason.to_string().into());
    error
}

pub(super) fn mark_rollback_complete(mut error: RaxError, removed: bool) -> RaxError {
    error.details.remove("partialApplied");
    error.details.remove("partialReason");
    error
        .details
        .insert("rollbackComplete".to_string(), true.into());
    error
        .details
        .insert("rolledBack".to_string(), removed.into());
    error
}

#[cfg(test)]
mod tests {
    use rax_core::{ErrorCode, ErrorPhase, RaxError};

    use super::{mark_partial, mark_rollback_complete};

    #[test]
    fn completed_rollback_clears_partial_applied_marker() {
        let partial = mark_partial(
            RaxError::new(
                ErrorCode::OperationFailed,
                ErrorPhase::Apply,
                false,
                "copy failed",
            ),
            "target may exist",
        );
        let recovered = mark_rollback_complete(partial, true);
        assert_eq!(recovered.details.get("partialApplied"), None);
        assert_eq!(recovered.details.get("partialReason"), None);
        assert_eq!(
            recovered
                .details
                .get("rollbackComplete")
                .and_then(serde_json::Value::as_bool),
            Some(true)
        );
        assert_eq!(
            recovered
                .details
                .get("rolledBack")
                .and_then(serde_json::Value::as_bool),
            Some(true)
        );
    }
}
