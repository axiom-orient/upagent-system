use std::{
    fs::{self, File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use agent_core::ApprovalRequest;
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;
use time::{Duration as TimeDuration, OffsetDateTime, format_description::well_known::Rfc3339};
use uuid::Uuid;

/// How long an operator has to decide, and to redeem the decision.
///
/// This is deliberately independent of the RAX plan TTL. A redeeming run builds
/// a fresh plan and applies that, so the safety property is not plan freshness
/// but subject equality: if anything the operator saw has changed, the subject
/// differs and the grant no longer matches. That makes a human-scale window safe
/// where a 15-minute plan expiry would not be usable.
const DEFAULT_DECISION_WINDOW: Duration = Duration::from_hours(24);
const APPROVAL_SCHEMA_VERSION: u32 = 1;
const MAXIMUM_RECORD_BYTES: u64 = 64 * 1024;
const MAXIMUM_RECORDS: usize = 4_096;
const LOCK_WAIT: Duration = Duration::from_secs(30);
const MINIMUM_LOCK_BACKOFF: Duration = Duration::from_millis(2);
const MAXIMUM_LOCK_BACKOFF: Duration = Duration::from_millis(50);

#[derive(Debug, Error)]
pub enum ApprovalError {
    #[error("approval I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("approval JSON failed: {0}")]
    Json(#[from] serde_json::Error),
    #[error("approval contract failed: {0}")]
    Contract(String),
    #[error("approval {0} was not found")]
    NotFound(String),
    #[error("approval store lock was still held after {waited_ms} ms")]
    LockUnavailable { waited_ms: u64 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalStatus {
    Pending,
    Approved,
    Denied,
    Consumed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ApprovalRecord {
    pub schema_version: u32,
    pub subject: String,
    pub plan_id: String,
    pub operation: String,
    pub effects: Vec<String>,
    pub status: ApprovalStatus,
    pub requested_at: String,
    pub expires_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decided_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub consumed_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    token_sha256: Option<String>,
}

impl ApprovalRecord {
    fn expired(&self, now: OffsetDateTime) -> bool {
        OffsetDateTime::parse(&self.expires_at, &Rfc3339).is_ok_and(|deadline| deadline <= now)
    }
}

/// One issued decision, handed to the operator and replayed by a later run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ApprovalGrantIssued {
    pub subject: String,
    pub token: String,
    pub expires_at: String,
}

/// Durable operator decisions over effect and extension approvals.
///
/// The agent, not RAX, is the operator-facing approval authority: one store
/// covers workspace effects and trusted extension tools so a single command can
/// decide either. RAX keeps its own one-time grant for each individual apply.
#[derive(Debug, Clone)]
pub struct ApprovalStore {
    root: PathBuf,
    window: Duration,
}

impl ApprovalStore {
    pub fn open(root: impl AsRef<Path>) -> Result<Self, ApprovalError> {
        Self::open_with_window(root, DEFAULT_DECISION_WINDOW)
    }

    pub fn open_with_window(
        root: impl AsRef<Path>,
        window: Duration,
    ) -> Result<Self, ApprovalError> {
        if window.is_zero() || window > DEFAULT_DECISION_WINDOW {
            return Err(ApprovalError::Contract(
                "approval decision window is outside its supported bound".to_owned(),
            ));
        }
        let root = root.as_ref().to_path_buf();
        prepare_private_dir(&root)?;
        Ok(Self {
            root: root.canonicalize()?,
            window,
        })
    }

    /// Record a pending decision so an operator can act on it.
    ///
    /// Re-requesting the same subject refreshes nothing: an existing decision
    /// keeps its status, so a run cannot reset a denial by asking again.
    pub fn record_request(&self, request: &ApprovalRequest) -> Result<(), ApprovalError> {
        let subject = validated_subject(&request.subject)?;
        let _guard = self.lock()?;
        if self.read(&subject)?.is_some() {
            return Ok(());
        }
        let now = OffsetDateTime::now_utc();
        let record = ApprovalRecord {
            schema_version: APPROVAL_SCHEMA_VERSION,
            subject: subject.clone(),
            plan_id: request.plan_id.clone(),
            operation: request.operation.clone(),
            effects: request.effects.clone(),
            status: ApprovalStatus::Pending,
            requested_at: format_time(now)?,
            expires_at: format_time(now + self.decision_window()?)?,
            decided_at: None,
            consumed_at: None,
            token_sha256: None,
        };
        self.write(&record)
    }

    /// Approve a pending decision and issue its one-time token.
    pub fn approve(&self, subject: &str) -> Result<ApprovalGrantIssued, ApprovalError> {
        let subject = validated_subject(subject)?;
        let _guard = self.lock()?;
        let mut record = self
            .read(&subject)?
            .ok_or_else(|| ApprovalError::NotFound(subject.clone()))?;
        let now = OffsetDateTime::now_utc();
        if record.status != ApprovalStatus::Pending {
            return Err(ApprovalError::Contract(format!(
                "approval {subject} is already {:?}",
                record.status
            )));
        }
        if record.expired(now) {
            return Err(ApprovalError::Contract(format!(
                "approval {subject} expired before a decision was recorded"
            )));
        }
        let token = format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
        record.status = ApprovalStatus::Approved;
        record.decided_at = Some(format_time(now)?);
        record.token_sha256 = Some(hash(&token));
        let expires_at = record.expires_at.clone();
        self.write(&record)?;
        Ok(ApprovalGrantIssued {
            subject,
            token,
            expires_at,
        })
    }

    /// Refuse a pending decision. A denial is terminal for that subject.
    pub fn deny(&self, subject: &str) -> Result<ApprovalRecord, ApprovalError> {
        let subject = validated_subject(subject)?;
        let _guard = self.lock()?;
        let mut record = self
            .read(&subject)?
            .ok_or_else(|| ApprovalError::NotFound(subject.clone()))?;
        if record.status != ApprovalStatus::Pending {
            return Err(ApprovalError::Contract(format!(
                "approval {subject} is already {:?}",
                record.status
            )));
        }
        record.status = ApprovalStatus::Denied;
        record.decided_at = Some(format_time(OffsetDateTime::now_utc())?);
        self.write(&record)?;
        Ok(record)
    }

    /// Redeem a grant exactly once.
    ///
    /// Consumption happens before the authorised work runs, so a token can never
    /// authorise two operations even if the first one fails.
    pub fn consume(&self, subject: &str, token: &str) -> Result<(), ApprovalError> {
        let subject = validated_subject(subject)?;
        let _guard = self.lock()?;
        let mut record = self
            .read(&subject)?
            .ok_or_else(|| ApprovalError::NotFound(subject.clone()))?;
        if record.status != ApprovalStatus::Approved {
            return Err(ApprovalError::Contract(format!(
                "approval {subject} is {:?} and cannot be redeemed",
                record.status
            )));
        }
        if record.expired(OffsetDateTime::now_utc()) {
            return Err(ApprovalError::Contract(format!(
                "approval {subject} expired before it was redeemed"
            )));
        }
        let expected = record
            .token_sha256
            .as_deref()
            .ok_or_else(|| ApprovalError::Contract("approved record has no token".to_owned()))?;
        if !constant_time_equals(expected.as_bytes(), hash(token).as_bytes()) {
            return Err(ApprovalError::Contract(format!(
                "approval {subject} token does not match"
            )));
        }
        record.status = ApprovalStatus::Consumed;
        record.consumed_at = Some(format_time(OffsetDateTime::now_utc())?);
        record.token_sha256 = None;
        self.write(&record)
    }

    pub fn list(&self) -> Result<Vec<ApprovalRecord>, ApprovalError> {
        let _guard = self.lock()?;
        let mut records = Vec::new();
        for entry in fs::read_dir(&self.root)? {
            let path = entry?.path();
            if path.extension().is_none_or(|value| value != "json") {
                continue;
            }
            if records.len() >= MAXIMUM_RECORDS {
                return Err(ApprovalError::Contract(
                    "approval store exceeded its bounded size".to_owned(),
                ));
            }
            if let Some(record) = read_record(&path)? {
                records.push(record);
            }
        }
        records.sort_by(|left, right| {
            left.requested_at
                .cmp(&right.requested_at)
                .then_with(|| left.subject.cmp(&right.subject))
        });
        Ok(records)
    }

    fn decision_window(&self) -> Result<TimeDuration, ApprovalError> {
        i64::try_from(self.window.as_secs())
            .map(TimeDuration::seconds)
            .map_err(|_| ApprovalError::Contract("decision window is not representable".to_owned()))
    }

    fn path(&self, subject: &str) -> PathBuf {
        self.root.join(format!("{subject}.json"))
    }

    fn read(&self, subject: &str) -> Result<Option<ApprovalRecord>, ApprovalError> {
        read_record(&self.path(subject))
    }

    fn write(&self, record: &ApprovalRecord) -> Result<(), ApprovalError> {
        let bytes = serde_json::to_vec_pretty(record)?;
        if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > MAXIMUM_RECORD_BYTES {
            return Err(ApprovalError::Contract(
                "approval record exceeds its byte bound".to_owned(),
            ));
        }
        atomic_replace_private(&self.path(&record.subject), &bytes)
    }

    fn lock(&self) -> Result<StoreLock, ApprovalError> {
        let file = open_private(&self.root.join("store.lock"), true)?;
        let deadline = Instant::now() + LOCK_WAIT;
        let mut backoff = MINIMUM_LOCK_BACKOFF;
        loop {
            match file.try_lock_exclusive() {
                Ok(()) => return Ok(StoreLock { file }),
                Err(error) if is_lock_contended(&error) => {}
                Err(error) => return Err(error.into()),
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(ApprovalError::LockUnavailable {
                    waited_ms: u64::try_from(LOCK_WAIT.as_millis()).unwrap_or(u64::MAX),
                });
            }
            std::thread::sleep(backoff.min(remaining));
            backoff = (backoff * 2).min(MAXIMUM_LOCK_BACKOFF);
        }
    }
}

#[derive(Debug)]
struct StoreLock {
    file: File,
}

impl Drop for StoreLock {
    fn drop(&mut self) {
        let _ = fs2::FileExt::unlock(&self.file);
    }
}

fn read_record(path: &Path) -> Result<Option<ApprovalRecord>, ApprovalError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() || !metadata.is_file() {
                return Err(ApprovalError::Contract(
                    "approval record must be a regular file".to_owned(),
                ));
            }
            if metadata.len() > MAXIMUM_RECORD_BYTES {
                return Err(ApprovalError::Contract(
                    "approval record exceeds its byte bound".to_owned(),
                ));
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    }
    let record: ApprovalRecord = serde_json::from_slice(&fs::read(path)?)?;
    if record.schema_version != APPROVAL_SCHEMA_VERSION {
        return Err(ApprovalError::Contract(format!(
            "unsupported approval schema version {}",
            record.schema_version
        )));
    }
    OffsetDateTime::parse(&record.expires_at, &Rfc3339).map_err(|_| {
        ApprovalError::Contract(
            "approval record expiresAt must be a valid RFC 3339 timestamp".to_owned(),
        )
    })?;
    Ok(Some(record))
}

fn validated_subject(subject: &str) -> Result<String, ApprovalError> {
    if subject.len() != 64 || !subject.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(ApprovalError::Contract(
            "approval subject must be a SHA-256 hex digest".to_owned(),
        ));
    }
    Ok(subject.to_ascii_lowercase())
}

fn hash(value: &str) -> String {
    format!("{:x}", Sha256::digest(value.as_bytes()))
}

fn constant_time_equals(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    left.iter()
        .zip(right)
        .fold(0_u8, |difference, (a, b)| difference | (a ^ b))
        == 0
}

fn format_time(value: OffsetDateTime) -> Result<String, ApprovalError> {
    value
        .format(&Rfc3339)
        .map_err(|error| ApprovalError::Contract(error.to_string()))
}

fn is_lock_contended(error: &std::io::Error) -> bool {
    const ERROR_LOCK_VIOLATION: i32 = 33;
    error.kind() == std::io::ErrorKind::WouldBlock
        || error.raw_os_error() == Some(ERROR_LOCK_VIOLATION)
}

fn prepare_private_dir(path: &Path) -> Result<(), ApprovalError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                return Err(ApprovalError::Contract(
                    "approval root must be a real directory".to_owned(),
                ));
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => fs::create_dir_all(path)?,
        Err(error) => return Err(error.into()),
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

fn open_private(path: &Path, create: bool) -> Result<File, ApprovalError> {
    if let Ok(metadata) = fs::symlink_metadata(path)
        && (metadata.file_type().is_symlink() || !metadata.is_file())
    {
        return Err(ApprovalError::Contract(
            "approval state file must be a regular file".to_owned(),
        ));
    }
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(create);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    Ok(options.open(path)?)
}

fn atomic_replace_private(path: &Path, bytes: &[u8]) -> Result<(), ApprovalError> {
    let parent = path
        .parent()
        .ok_or_else(|| ApprovalError::Contract("approval path has no parent".to_owned()))?;
    if let Ok(metadata) = fs::symlink_metadata(path)
        && (metadata.file_type().is_symlink() || !metadata.is_file())
    {
        return Err(ApprovalError::Contract(
            "approval destination must be a regular file".to_owned(),
        ));
    }
    let temporary = parent.join(format!(".approval-{}.tmp", Uuid::now_v7()));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&temporary)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    drop(file);
    fs::rename(&temporary, path).inspect_err(|_| {
        let _ = fs::remove_file(&temporary);
    })?;
    #[cfg(unix)]
    File::open(parent)?.sync_all()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(subject: &str) -> ApprovalRequest {
        ApprovalRequest {
            plan_id: "plan-1".to_owned(),
            fingerprint: "f".repeat(64),
            subject: subject.to_owned(),
            expires_at: "2026-01-01T00:00:00Z".to_owned(),
            effects: vec!["write_workspace".to_owned()],
            operation: "fs_write".to_owned(),
        }
    }

    fn store() -> Result<(tempfile::TempDir, ApprovalStore), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let store = ApprovalStore::open(temp.path().join("approvals"))?;
        Ok((temp, store))
    }

    #[test]
    fn a_grant_is_redeemable_exactly_once() -> Result<(), Box<dyn std::error::Error>> {
        let subject = "a".repeat(64);
        let (_temp, store) = store()?;
        store.record_request(&request(&subject))?;

        let grant = store.approve(&subject)?;
        store.consume(&subject, &grant.token)?;

        let replay = store
            .consume(&subject, &grant.token)
            .expect_err("a consumed grant must not be redeemable again");
        assert!(replay.to_string().contains("Consumed"));
        Ok(())
    }

    #[test]
    fn a_forged_token_is_refused() -> Result<(), Box<dyn std::error::Error>> {
        let subject = "b".repeat(64);
        let (_temp, store) = store()?;
        store.record_request(&request(&subject))?;
        store.approve(&subject)?;

        let error = store
            .consume(&subject, &"0".repeat(64))
            .expect_err("a forged token must be refused");
        assert!(error.to_string().contains("does not match"));
        Ok(())
    }

    #[test]
    fn a_denial_is_terminal_and_cannot_be_reset_by_asking_again()
    -> Result<(), Box<dyn std::error::Error>> {
        let subject = "c".repeat(64);
        let (_temp, store) = store()?;
        store.record_request(&request(&subject))?;
        store.deny(&subject)?;

        // A later run requesting the same subject must not clear the denial.
        store.record_request(&request(&subject))?;
        let error = store
            .approve(&subject)
            .expect_err("a denied subject must stay denied");
        assert!(error.to_string().contains("Denied"));
        assert!(store.consume(&subject, &"0".repeat(64)).is_err());
        Ok(())
    }

    #[test]
    fn an_expired_decision_cannot_be_redeemed() -> Result<(), Box<dyn std::error::Error>> {
        let subject = "d".repeat(64);
        let temp = tempfile::tempdir()?;
        let store =
            ApprovalStore::open_with_window(temp.path().join("approvals"), Duration::from_secs(1))?;
        store.record_request(&request(&subject))?;
        let grant = store.approve(&subject)?;

        std::thread::sleep(Duration::from_millis(1_100));
        let error = store
            .consume(&subject, &grant.token)
            .expect_err("an expired grant must not be redeemable");
        assert!(error.to_string().contains("expired"));
        Ok(())
    }

    fn corrupt_expiration(store: &ApprovalStore, subject: &str) -> Result<Vec<u8>, ApprovalError> {
        let path = store.path(subject);
        let mut value: serde_json::Value = serde_json::from_slice(&fs::read(&path)?)?;
        value["expiresAt"] = serde_json::json!("not-a-timestamp");
        let bytes = serde_json::to_vec_pretty(&value)?;
        atomic_replace_private(&path, &bytes)?;
        Ok(bytes)
    }

    #[test]
    fn invalid_expiration_cannot_be_approved() -> Result<(), Box<dyn std::error::Error>> {
        let subject = "f".repeat(64);
        let (_temp, store) = store()?;
        store.record_request(&request(&subject))?;
        let corrupted = corrupt_expiration(&store, &subject)?;

        let error = store
            .approve(&subject)
            .expect_err("a malformed expiration must not allow approval");
        assert!(matches!(error, ApprovalError::Contract(_)));
        assert_eq!(fs::read(store.path(&subject))?, corrupted);
        Ok(())
    }

    #[test]
    fn invalid_expiration_cannot_be_redeemed() -> Result<(), Box<dyn std::error::Error>> {
        let subject = "0".repeat(64);
        let (_temp, store) = store()?;
        store.record_request(&request(&subject))?;
        let grant = store.approve(&subject)?;
        let corrupted = corrupt_expiration(&store, &subject)?;

        let error = store
            .consume(&subject, &grant.token)
            .expect_err("a malformed expiration must not allow redemption");
        assert!(matches!(error, ApprovalError::Contract(_)));
        assert_eq!(fs::read(store.path(&subject))?, corrupted);
        Ok(())
    }

    #[test]
    fn subjects_must_be_digests() -> Result<(), Box<dyn std::error::Error>> {
        let (_temp, store) = store()?;
        // A subject reaches the filesystem as a name, so anything but a digest
        // is refused before it can address a path.
        for hostile in ["../escape", "short", &"z".repeat(64), ""] {
            assert!(
                store.record_request(&request(hostile)).is_err(),
                "accepted subject {hostile:?}"
            );
        }
        Ok(())
    }

    #[test]
    fn persisted_approval_schema_versions_fail_closed() -> Result<(), Box<dyn std::error::Error>> {
        let subject = "e".repeat(64);
        let (_temp, store) = store()?;
        store.record_request(&request(&subject))?;
        let path = store.path(&subject);
        let mut value: serde_json::Value = serde_json::from_slice(&fs::read(&path)?)?;
        value["schemaVersion"] = serde_json::json!(2);
        atomic_replace_private(&path, &serde_json::to_vec_pretty(&value)?)?;
        let error = store
            .list()
            .expect_err("unknown persisted schema version must be rejected");
        assert!(
            error
                .to_string()
                .contains("unsupported approval schema version 2")
        );
        Ok(())
    }
}
