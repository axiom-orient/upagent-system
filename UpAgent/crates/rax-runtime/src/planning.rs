use std::{collections::BTreeMap, time::Duration};

use rax_core::{
    DefaultPolicy, ErrorCode, ErrorPhase, Operation, PathKind, Plan, PolicyDecision, RaxError,
    RequestEnvelope, SCHEMA_VERSION,
};
use serde_json::Value;
use sha2::{Digest, Sha256};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::{
    fs::{current_precondition, snapshot_hash, verify_precondition},
    git,
    process::CancellationToken,
    timeutil::{now_rfc3339, parse_rfc3339, rfc3339_after},
    workspace::Workspace,
};

pub const DEFAULT_PLAN_TTL: Duration = Duration::from_mins(15);

pub(super) fn normalize_request(mut request: RequestEnvelope) -> Result<RequestEnvelope, RaxError> {
    request.validate()?;
    normalize_expected_hashes(&mut request.operation);
    if request.request_id.is_none() {
        request.request_id = Some(Uuid::now_v7().to_string());
    }
    Ok(request)
}

fn normalize_expected_hashes(operation: &mut Operation) {
    match operation {
        Operation::FsWrite(input) => {
            if let Some(expected) = &mut input.expected_sha256 {
                expected.make_ascii_lowercase();
            }
        }
        Operation::FsPatch(input) => input.expected_sha256.make_ascii_lowercase(),
        Operation::FsRemove(input) => {
            if let Some(expected) = &mut input.expected_sha256 {
                expected.make_ascii_lowercase();
            }
        }
        _ => {}
    }
}

pub(super) fn build_plan(
    workspace: &Workspace,
    request: &RequestEnvelope,
    cancellation: &CancellationToken,
    ttl: Duration,
) -> Result<Plan, RaxError> {
    let request_id = request.request_id.clone().ok_or_else(|| {
        RaxError::new(
            ErrorCode::InvariantViolation,
            ErrorPhase::Normalize,
            false,
            "normalized request is missing requestId",
        )
    })?;
    let effects = DefaultPolicy::evaluate(&request.operation);
    let mut preconditions = collect_preconditions(workspace, &request.operation)?;
    preconditions.sort_by(|left, right| left.path.cmp(&right.path));
    preconditions.dedup_by(|left, right| left.path == right.path);
    let observations =
        collect_observations(workspace, &request.operation, &preconditions, cancellation)?;
    let plan = Plan {
        schema_version: SCHEMA_VERSION.to_string(),
        plan_id: Uuid::now_v7().to_string(),
        request_id,
        workspace: workspace.binding(),
        operation: request.operation.clone(),
        effects,
        preconditions,
        observations,
        created_at: now_rfc3339()?,
        expires_at: rfc3339_after(ttl)?,
        fingerprint: String::new(),
    };
    plan.seal().map_err(|error| {
        RaxError::new(
            ErrorCode::InvariantViolation,
            ErrorPhase::Plan,
            false,
            error.to_string(),
        )
    })
}

#[must_use]
pub(super) fn approval_required(plan: &Plan) -> bool {
    plan.effects
        .iter()
        .any(|effect| effect.decision == PolicyDecision::ApprovalRequired)
}

pub(super) fn revalidate_plan(
    workspace: &Workspace,
    plan: &Plan,
    cancellation: &CancellationToken,
) -> Result<(), RaxError> {
    if plan.schema_version != SCHEMA_VERSION {
        return Err(RaxError::new(
            ErrorCode::UnsupportedSchemaVersion,
            ErrorPhase::Authorize,
            false,
            "plan schema version is unsupported",
        ));
    }
    if plan.workspace.identity != workspace.identity() {
        return Err(RaxError::new(
            ErrorCode::GrantMismatch,
            ErrorPhase::Authorize,
            false,
            "plan belongs to a different workspace capability",
        )
        .with_detail("plannedWorkspace", plan.workspace.identity.clone())
        .with_detail("actualWorkspace", workspace.identity().to_string()));
    }
    if !plan.verify_fingerprint().map_err(|error| {
        RaxError::new(
            ErrorCode::InvariantViolation,
            ErrorPhase::Authorize,
            false,
            error.to_string(),
        )
    })? {
        return Err(RaxError::new(
            ErrorCode::GrantMismatch,
            ErrorPhase::Authorize,
            false,
            "plan fingerprint verification failed",
        ));
    }
    if parse_rfc3339(&plan.expires_at, ErrorPhase::Authorize)? <= OffsetDateTime::now_utc() {
        return Err(RaxError::new(
            ErrorCode::GrantExpired,
            ErrorPhase::Authorize,
            false,
            "plan has expired",
        ));
    }
    for precondition in &plan.preconditions {
        verify_precondition(workspace, precondition)?;
    }
    verify_observations(workspace, plan, cancellation)
}

fn collect_preconditions(
    workspace: &Workspace,
    operation: &Operation,
) -> Result<Vec<rax_core::PathPrecondition>, RaxError> {
    let paths: Vec<&str> = match operation {
        Operation::FsWrite(input) => vec![&input.path],
        Operation::FsPatch(input) => vec![&input.path],
        Operation::FsMkdir(input) => vec![&input.path],
        Operation::FsMove(input) => vec![&input.from, &input.to],
        Operation::FsCopy(input) => vec![&input.from, &input.to],
        Operation::FsRemove(input) => vec![&input.path],
        Operation::GitStage(input) | Operation::GitUnstage(input) => vec![&input.repository],
        Operation::GitDiscard(input) => vec![&input.repository],
        Operation::GitCommit(input) => vec![&input.repository],
        Operation::GitCheckout(input) => vec![&input.repository],
        Operation::GitFetch(input) | Operation::GitPullFf(input) => vec![&input.repository],
        Operation::GitPush(input) => vec![&input.repository],
        Operation::FsRead(_)
        | Operation::FsStat(_)
        | Operation::FsList(_)
        | Operation::FsGlob(_)
        | Operation::FsGrep(_)
        | Operation::ProcessRun(_)
        | Operation::ProcessShell(_)
        | Operation::GitStatus(_)
        | Operation::GitDiff(_)
        | Operation::GitLog(_)
        | Operation::GitShow(_)
        | Operation::GitRemote(_)
        | Operation::GitBranches(_) => Vec::new(),
    };
    paths
        .into_iter()
        .map(|path| current_precondition(workspace, path))
        .collect()
}

fn collect_observations(
    workspace: &Workspace,
    operation: &Operation,
    preconditions: &[rax_core::PathPrecondition],
    cancellation: &CancellationToken,
) -> Result<BTreeMap<String, Value>, RaxError> {
    let mut observations = BTreeMap::new();
    match operation {
        Operation::FsMove(input) => {
            add_tree_observation(workspace, &input.from, preconditions, &mut observations)?;
        }
        Operation::FsCopy(input) => {
            add_tree_observation(workspace, &input.from, preconditions, &mut observations)?;
        }
        Operation::FsRemove(input) => {
            add_tree_observation(workspace, &input.path, preconditions, &mut observations)?;
        }
        _ => {}
    }

    if let Some(repository) = mutating_git_repository(operation) {
        let (head, status) = git::repository_observation(workspace, repository, cancellation)?;
        observations.insert("gitRepository".to_string(), repository.to_string().into());
        observations.insert("gitHead".to_string(), optional_string_value(head));
        observations.insert("gitStatusSha256".to_string(), hash_json(&status)?.into());
    }
    Ok(observations)
}

fn add_tree_observation(
    workspace: &Workspace,
    path: &str,
    preconditions: &[rax_core::PathPrecondition],
    observations: &mut BTreeMap<String, Value>,
) -> Result<(), RaxError> {
    if preconditions
        .iter()
        .any(|value| value.path == path && value.exists && value.kind == Some(PathKind::Directory))
    {
        observations.insert("treePath".to_string(), path.to_string().into());
        observations.insert(
            "treeSha256".to_string(),
            snapshot_hash(workspace, path)?.into(),
        );
    }
    Ok(())
}

fn verify_observations(
    workspace: &Workspace,
    plan: &Plan,
    cancellation: &CancellationToken,
) -> Result<(), RaxError> {
    if let (Some(path), Some(expected)) = (
        plan.observations.get("treePath").and_then(Value::as_str),
        plan.observations.get("treeSha256").and_then(Value::as_str),
    ) {
        let actual = snapshot_hash(workspace, path)?;
        if actual != expected {
            return Err(stale_observation("directory tree changed after planning")
                .with_detail("path", path.to_string())
                .with_detail("expectedSha256", expected.to_string())
                .with_detail("actualSha256", actual));
        }
    }
    if let Some(repository) = plan
        .observations
        .get("gitRepository")
        .and_then(Value::as_str)
    {
        let (actual_head, status) =
            git::repository_observation(workspace, repository, cancellation)?;
        let expected_head =
            parse_optional_string_observation(plan.observations.get("gitHead"), "gitHead")?;
        if actual_head != expected_head {
            return Err(stale_observation("Git HEAD changed after planning")
                .with_detail("repository", repository.to_string())
                .with_detail("expectedHead", optional_string_value(expected_head))
                .with_detail("actualHead", optional_string_value(actual_head)));
        }
        let actual_status = hash_json(&status)?;
        let expected_status = plan
            .observations
            .get("gitStatusSha256")
            .and_then(Value::as_str)
            .ok_or_else(|| stale_observation("Git status observation is missing"))?;
        if actual_status != expected_status {
            return Err(
                stale_observation("Git worktree/index changed after planning")
                    .with_detail("repository", repository.to_string())
                    .with_detail("expectedSha256", expected_status.to_string())
                    .with_detail("actualSha256", actual_status),
            );
        }
    }
    Ok(())
}

fn mutating_git_repository(operation: &Operation) -> Option<&str> {
    match operation {
        Operation::GitStage(input) | Operation::GitUnstage(input) => Some(&input.repository),
        Operation::GitDiscard(input) => Some(&input.repository),
        Operation::GitCommit(input) => Some(&input.repository),
        Operation::GitCheckout(input) => Some(&input.repository),
        Operation::GitFetch(input) | Operation::GitPullFf(input) => Some(&input.repository),
        Operation::GitPush(input) => Some(&input.repository),
        _ => None,
    }
}

fn optional_string_value(value: Option<String>) -> Value {
    value.map_or(Value::Null, Value::String)
}

fn parse_optional_string_observation(
    value: Option<&Value>,
    name: &str,
) -> Result<Option<String>, RaxError> {
    match value {
        Some(Value::String(value)) => Ok(Some(value.clone())),
        Some(Value::Null) => Ok(None),
        Some(_) => Err(RaxError::new(
            ErrorCode::InvariantViolation,
            ErrorPhase::Authorize,
            false,
            format!("plan observation {name} has an invalid type"),
        )),
        None => Err(stale_observation(format!(
            "plan observation {name} is missing"
        ))),
    }
}

fn hash_json<T: serde::Serialize>(value: &T) -> Result<String, RaxError> {
    let bytes = serde_json::to_vec(value).map_err(serialize_observation_error)?;
    Ok(format!("{:x}", Sha256::digest(bytes)))
}

#[allow(clippy::needless_pass_by_value)]
fn serialize_observation_error(error: serde_json::Error) -> RaxError {
    RaxError::new(
        ErrorCode::InvariantViolation,
        ErrorPhase::Plan,
        false,
        format!("failed to encode plan observation: {error}"),
    )
}

fn stale_observation(message: impl Into<String>) -> RaxError {
    RaxError::new(
        ErrorCode::PreconditionFailed,
        ErrorPhase::Authorize,
        false,
        message,
    )
}

#[cfg(test)]
mod tests {
    use rax_core::{ContentEncoding, FsWriteInput, Operation, RequestEnvelope, SCHEMA_VERSION};

    use super::normalize_request;

    #[test]
    fn normalization_canonicalizes_expected_hashes() {
        let request = RequestEnvelope {
            schema_version: SCHEMA_VERSION.to_string(),
            request_id: Some("request".to_string()),
            operation: Operation::FsWrite(FsWriteInput {
                path: "README.md".to_string(),
                content: "updated".to_string(),
                encoding: ContentEncoding::Utf8,
                create_only: false,
                expected_sha256: Some("A".repeat(64)),
            }),
            record: true,
        };

        let normalized = normalize_request(request).expect("normalized request");
        let Operation::FsWrite(input) = normalized.operation else {
            panic!("expected fs_write operation");
        };
        let expected = "a".repeat(64);
        assert_eq!(input.expected_sha256.as_deref(), Some(expected.as_str()));
    }
}
