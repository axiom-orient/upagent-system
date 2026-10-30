use std::{fs::OpenOptions, path::Path};

use fs2::FileExt;
use rax_core::{ErrorCode, ErrorPhase, Plan, PolicyDecision, RaxError, SCHEMA_VERSION};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::{
    error::io_error,
    journal::{StateStore, atomic_write_json, create_new_json, read_json},
    timeutil::{now_rfc3339, parse_rfc3339},
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct GrantIssued {
    pub schema_version: String,
    pub plan_id: String,
    pub fingerprint: String,
    pub token: String,
    pub expires_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct DenialRecorded {
    pub schema_version: String,
    pub plan_id: String,
    pub fingerprint: String,
    pub denied_at: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum DecisionStatus {
    Approved,
    Denied,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct DecisionRecord {
    schema_version: String,
    plan_id: String,
    fingerprint: String,
    status: DecisionStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    token_sha256: Option<String>,
    created_at: String,
    expires_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    consumed_at: Option<String>,
}

pub fn approve(store: &StateStore, plan: &Plan) -> Result<GrantIssued, RaxError> {
    validate_plan_for_decision(plan)?;
    let directory = store.plan_dir(&plan.plan_id)?;
    let _guard = lock_decision(&directory)?;
    let decision_path = directory.join("decision.json");
    let token = format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
    let record = DecisionRecord {
        schema_version: SCHEMA_VERSION.to_string(),
        plan_id: plan.plan_id.clone(),
        fingerprint: plan.fingerprint.clone(),
        status: DecisionStatus::Approved,
        token_sha256: Some(hash_token(&token)),
        created_at: now_rfc3339()?,
        expires_at: plan.expires_at.clone(),
        consumed_at: None,
    };
    write_decision_exclusive(&decision_path, &record)?;
    Ok(GrantIssued {
        schema_version: SCHEMA_VERSION.to_string(),
        plan_id: plan.plan_id.clone(),
        fingerprint: plan.fingerprint.clone(),
        token,
        expires_at: plan.expires_at.clone(),
    })
}

pub fn deny(store: &StateStore, plan: &Plan) -> Result<DenialRecorded, RaxError> {
    validate_plan_for_decision(plan)?;
    let directory = store.plan_dir(&plan.plan_id)?;
    let _guard = lock_decision(&directory)?;
    let decision_path = directory.join("decision.json");
    let denied_at = now_rfc3339()?;
    let record = DecisionRecord {
        schema_version: SCHEMA_VERSION.to_string(),
        plan_id: plan.plan_id.clone(),
        fingerprint: plan.fingerprint.clone(),
        status: DecisionStatus::Denied,
        token_sha256: None,
        created_at: denied_at.clone(),
        expires_at: plan.expires_at.clone(),
        consumed_at: None,
    };
    write_decision_exclusive(&decision_path, &record)?;
    Ok(DenialRecorded {
        schema_version: SCHEMA_VERSION.to_string(),
        plan_id: plan.plan_id.clone(),
        fingerprint: plan.fingerprint.clone(),
        denied_at,
    })
}

pub fn consume(store: &StateStore, plan: &Plan, token: &str) -> Result<(), RaxError> {
    validate_plan_for_decision(plan)?;
    if token.is_empty() || token.len() > 256 || token.contains(char::is_whitespace) {
        return Err(RaxError::new(
            ErrorCode::GrantMismatch,
            ErrorPhase::Authorize,
            false,
            "grant token format is invalid",
        ));
    }
    let directory = store.plan_dir(&plan.plan_id)?;
    let _guard = lock_decision(&directory)?;
    let decision_path = directory.join("decision.json");
    let mut record: DecisionRecord = read_json(&decision_path)?;
    validate_decision(plan, &record, token)?;
    record.consumed_at = Some(now_rfc3339()?);
    atomic_write_json(&decision_path, &record)?;
    Ok(())
}

fn validate_plan_for_decision(plan: &Plan) -> Result<(), RaxError> {
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
            "plan fingerprint is invalid",
        ));
    }
    if !plan
        .effects
        .iter()
        .any(|effect| effect.decision == PolicyDecision::ApprovalRequired)
    {
        return Err(RaxError::new(
            ErrorCode::InvalidRequest,
            ErrorPhase::Authorize,
            false,
            "this plan does not require approval",
        ));
    }
    ensure_not_expired(&plan.expires_at)
}

fn validate_decision(plan: &Plan, record: &DecisionRecord, token: &str) -> Result<(), RaxError> {
    if record.schema_version != SCHEMA_VERSION {
        return Err(decision_invariant(
            "approval decision schema version does not match the runtime",
        ));
    }
    if record.plan_id != plan.plan_id || record.fingerprint != plan.fingerprint {
        return Err(RaxError::new(
            ErrorCode::GrantMismatch,
            ErrorPhase::Authorize,
            false,
            "approval decision does not match the immutable plan",
        ));
    }
    if record.expires_at != plan.expires_at {
        return Err(decision_invariant(
            "approval decision expiration does not match the immutable plan",
        ));
    }

    let created_at = parse_rfc3339(&record.created_at, ErrorPhase::Authorize)?;
    let expires_at = parse_rfc3339(&record.expires_at, ErrorPhase::Authorize)?;
    if created_at > expires_at {
        return Err(decision_invariant(
            "approval decision was created after its expiration",
        ));
    }
    if let Some(consumed_at) = &record.consumed_at {
        let consumed_at = parse_rfc3339(consumed_at, ErrorPhase::Authorize)?;
        if consumed_at < created_at {
            return Err(decision_invariant(
                "approval decision was consumed before it was created",
            ));
        }
    }

    match record.status {
        DecisionStatus::Denied => {
            if record.token_sha256.is_some() || record.consumed_at.is_some() {
                return Err(decision_invariant(
                    "denied approval decision contains grant state",
                ));
            }
            return Err(RaxError::new(
                ErrorCode::ApprovalDenied,
                ErrorPhase::Authorize,
                false,
                "the plan was denied",
            ));
        }
        DecisionStatus::Approved => {}
    }
    if record.consumed_at.is_some() {
        return Err(RaxError::new(
            ErrorCode::GrantConsumed,
            ErrorPhase::Authorize,
            false,
            "grant token has already been consumed",
        ));
    }
    ensure_not_expired(&record.expires_at)?;
    let expected_hash = record
        .token_sha256
        .as_deref()
        .ok_or_else(|| decision_invariant("approved decision is missing its grant token hash"))?;
    if expected_hash.len() != 64 || !expected_hash.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(decision_invariant(
            "approval decision contains an invalid grant token hash",
        ));
    }
    let presented_hash = hash_token(token);
    if !constant_time_eq(expected_hash.as_bytes(), presented_hash.as_bytes()) {
        return Err(RaxError::new(
            ErrorCode::GrantMismatch,
            ErrorPhase::Authorize,
            false,
            "grant token does not match the plan",
        ));
    }
    Ok(())
}

fn decision_invariant(message: &str) -> RaxError {
    RaxError::new(
        ErrorCode::InvariantViolation,
        ErrorPhase::Authorize,
        false,
        message,
    )
}

fn ensure_not_expired(expires_at: &str) -> Result<(), RaxError> {
    if parse_rfc3339(expires_at, ErrorPhase::Authorize)? <= OffsetDateTime::now_utc() {
        return Err(RaxError::new(
            ErrorCode::GrantExpired,
            ErrorPhase::Authorize,
            false,
            "plan or grant has expired",
        ));
    }
    Ok(())
}

fn lock_decision(directory: &Path) -> Result<DecisionLock, RaxError> {
    let metadata = std::fs::metadata(directory).map_err(|error| {
        io_error(
            ErrorPhase::Authorize,
            "failed to access immutable plan directory",
            Some(directory),
            error,
        )
    })?;
    if !metadata.is_dir() {
        return Err(RaxError::new(
            ErrorCode::InvariantViolation,
            ErrorPhase::Authorize,
            false,
            "immutable plan path is not a directory",
        )
        .with_detail("path", directory.to_string_lossy().to_string()));
    }
    let path = directory.join("decision.lock");
    let file = open_private_lock_file(&path)?;
    let metadata = std::fs::symlink_metadata(&path).map_err(|error| {
        io_error(
            ErrorPhase::Authorize,
            "failed to inspect decision lock",
            Some(&path),
            error,
        )
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(RaxError::new(
            ErrorCode::InvariantViolation,
            ErrorPhase::Authorize,
            false,
            "decision lock is not a regular file",
        )
        .with_detail("path", path.to_string_lossy().to_string()));
    }
    secure_file(&path)?;
    FileExt::lock_exclusive(&file).map_err(|error| {
        io_error(
            ErrorPhase::Authorize,
            "failed to acquire decision lock",
            Some(&path),
            error,
        )
    })?;
    Ok(DecisionLock { file })
}

fn open_private_lock_file(path: &Path) -> Result<std::fs::File, RaxError> {
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true);
    configure_private_lock_mode(&mut options);
    options.open(path).map_err(|error| {
        io_error(
            ErrorPhase::Authorize,
            "failed to open decision lock",
            Some(path),
            error,
        )
    })
}

#[cfg(unix)]
fn configure_private_lock_mode(options: &mut OpenOptions) {
    use std::os::unix::fs::OpenOptionsExt;

    options.mode(0o600);
}

#[cfg(not(unix))]
fn configure_private_lock_mode(_options: &mut OpenOptions) {}

#[derive(Debug)]
struct DecisionLock {
    file: std::fs::File,
}

impl Drop for DecisionLock {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.file);
    }
}

#[cfg(unix)]
fn secure_file(path: &Path) -> Result<(), RaxError> {
    use std::os::unix::fs::PermissionsExt;

    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).map_err(|error| {
        io_error(
            ErrorPhase::Authorize,
            "failed to secure approval lock",
            Some(path),
            error,
        )
    })
}

#[cfg(not(unix))]
fn secure_file(_path: &Path) -> Result<(), RaxError> {
    Ok(())
}

fn write_decision_exclusive(path: &Path, value: &DecisionRecord) -> Result<(), RaxError> {
    create_new_json(path, value).map_err(|error| {
        if error.code == ErrorCode::AlreadyExists {
            RaxError::new(
                ErrorCode::AlreadyExists,
                ErrorPhase::Authorize,
                false,
                "a decision already exists for this plan",
            )
        } else {
            error
        }
    })
}

#[must_use]
fn hash_token(token: &str) -> String {
    format!("{:x}", Sha256::digest(token.as_bytes()))
}

#[must_use]
fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    let mut difference = 0_u8;
    for (left, right) in left.iter().zip(right) {
        difference |= left ^ right;
    }
    difference == 0
}

#[cfg(test)]
mod tests {
    use super::constant_time_eq;

    #[test]
    fn constant_time_comparison_requires_identical_bytes() {
        assert!(constant_time_eq(b"same", b"same"));
        assert!(!constant_time_eq(b"same", b"diff"));
        assert!(!constant_time_eq(b"short", b"longer"));
    }
}
