use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
};

#[cfg(test)]
use std::sync::{Mutex, OnceLock};

use agent_core::{
    AgentResult, AgentStatus, ArtifactId, ArtifactRef, ArtifactUse, KernelEvent,
    ReviewSubjectBundle, SessionId, TaskEnvelope, TaskId, TaskState, transition,
};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use uuid::Uuid;

use crate::{
    ArtifactStore, CompositionStore,
    session::{
        checkpoint::{CHECKPOINT_MEDIA_TYPE, SessionCheckpoint},
        event_artifact_uses,
    },
};

#[cfg(target_os = "macos")]
use rustix::fs::{CWD, RenameFlags, renameat_with};

const MAXIMUM_EVENT_BYTES: usize = 4 * 1024 * 1024;
const MAXIMUM_EVENT_LOG_BYTES: u64 = 64 * 1024 * 1024;
const MAXIMUM_EVENT_RECORDS: usize = 2_048;
const MAXIMUM_TASK_REQUEST_BYTES: u64 = 16 * 1024 * 1024;
const MAXIMUM_RECOVERY_RESOLUTION_BYTES: u64 = 64 * 1024;
const MAXIMUM_RECOVERY_SCAN_ENTRIES: usize = 10_000;

#[cfg(test)]
static FORCE_PARENT_DIRECTORY_SYNC_FAILURE: OnceLock<Mutex<Vec<PathBuf>>> = OnceLock::new();

#[derive(Debug, Error)]
pub enum JournalError {
    #[error("journal I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("journal JSON failed: {0}")]
    Json(#[from] serde_json::Error),
    #[error("journal contract failed: {0}")]
    Contract(String),
    #[error("suspended task journal is unverified: {0}")]
    ResumeUnverified(String),
    /// The event writer was entered and durability could no longer be
    /// classified.  The underlying file may contain a complete, partial, or
    /// no record, so callers must stop rather than replaying or materializing
    /// a result.
    #[error("journal commit status is unknown: {0}")]
    UnknownCommit(String),
    /// Once an unknown commit is observed the handle is deliberately unusable.
    /// Continuing would allow a later sequence/result write to claim an
    /// ordering that the filesystem has not proven.
    #[error("journal writer is poisoned after an unknown commit")]
    Poisoned,
}

impl JournalError {
    pub(crate) fn durability_unknown(&self) -> bool {
        matches!(self, Self::UnknownCommit(_) | Self::Poisoned)
    }

    pub(crate) fn is_resume_unverified(&self) -> bool {
        matches!(self, Self::ResumeUnverified(_))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct JournalRecord {
    pub sequence: u64,
    pub recorded_at: String,
    pub previous_state: TaskState,
    pub next_state: TaskState,
    pub event: KernelEvent,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub payload: Value,
}

/// Verified, read-only projection of one stopped task journal.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct TaskInspection {
    pub task: TaskEnvelope,
    pub events: Vec<JournalRecord>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<AgentResult>,
}

#[derive(Debug)]
pub struct SessionJournal {
    directory: PathBuf,
    events: File,
    sequence: u64,
    poisoned: bool,
    #[cfg(test)]
    fail_sync_data: bool,
    #[cfg(test)]
    partial_write_bytes: Option<usize>,
    _lock: JournalLock,
}

#[derive(Debug)]
pub(crate) struct JournalOpen {
    pub(crate) journal: SessionJournal,
    pub(crate) resume_checkpoint: Option<ArtifactRef>,
}

impl SessionJournal {
    #[cfg(test)]
    pub fn create(root: &Path, task: &TaskEnvelope) -> Result<Self, JournalError> {
        let opened = Self::open(root, task)?;
        if opened.resume_checkpoint.is_some() {
            return Err(JournalError::Contract(
                "task ID is suspended and must be resumed".to_owned(),
            ));
        }
        Ok(opened.journal)
    }

    pub(crate) fn open(root: &Path, task: &TaskEnvelope) -> Result<JournalOpen, JournalError> {
        let root = prepare_private_root(root)?;
        let sessions = prepare_private_child(&root, "sessions")?;
        let session = prepare_private_child(&sessions, task.session_id.as_str())?;
        let directory = prepare_private_child(&session, task.task_id.as_str())?;
        let lock_path = directory.join("task.lock");
        let lock_file = open_private(&lock_path, true, false)?;
        lock_file.try_lock_exclusive().map_err(|error| {
            JournalError::Contract(format!("task journal is already active: {error}"))
        })?;
        let result_path = directory.join("result.json");
        if path_exists_without_symlink(&result_path)? {
            return Err(JournalError::Contract(
                "task ID already has a terminal result".to_owned(),
            ));
        }
        let request_path = directory.join("request.json");
        let events_path = directory.join("events.ndjson");
        let has_request = path_exists_without_symlink(&request_path)?;
        let has_events = path_exists_without_symlink(&events_path)?;
        let (resume_checkpoint, sequence) = match (has_request, has_events) {
            (false, false) => {
                atomic_json(&request_path, task)?;
                (None, 0)
            }
            (true, true) => {
                if path_exists_without_symlink(&directory.join("resolution.json"))? {
                    return Err(JournalError::Contract(
                        "suspended task already has a recovery resolution".to_owned(),
                    ));
                }
                let recorded: TaskEnvelope =
                    read_bounded_json(&request_path, MAXIMUM_TASK_REQUEST_BYTES)?;
                if recorded != *task {
                    return Err(JournalError::Contract(
                        "resume request does not exactly match the suspended task".to_owned(),
                    ));
                }
                verify_composition_binding(&root, task)
                    .map_err(|error| JournalError::ResumeUnverified(error.to_string()))?;
                let records = read_event_records(&events_path)
                    .map_err(|error| JournalError::ResumeUnverified(error.to_string()))?;
                verify_journal_artifacts(&root, &records)
                    .map_err(|error| JournalError::ResumeUnverified(error.to_string()))?;
                let (checkpoint, sequence) = resume_position(&records)
                    .map_err(|error| JournalError::ResumeUnverified(error.to_string()))?;
                (Some(checkpoint), sequence)
            }
            _ => {
                return Err(JournalError::Contract(
                    "task journal is incomplete and requires explicit recovery".to_owned(),
                ));
            }
        };
        let events = open_private(&events_path, true, true)?;
        Ok(JournalOpen {
            journal: Self {
                directory,
                events,
                sequence,
                poisoned: false,
                #[cfg(test)]
                fail_sync_data: false,
                #[cfg(test)]
                partial_write_bytes: None,
                _lock: JournalLock { file: lock_file },
            },
            resume_checkpoint,
        })
    }

    pub fn append(
        &mut self,
        previous_state: TaskState,
        next_state: TaskState,
        event: &KernelEvent,
        payload: Value,
    ) -> Result<(), JournalError> {
        self.ensure_healthy()?;
        let sequence = self
            .sequence
            .checked_add(1)
            .ok_or_else(|| JournalError::Contract("journal sequence overflow".to_owned()))?;
        let record = JournalRecord {
            sequence,
            recorded_at: now_rfc3339()?,
            previous_state,
            next_state,
            event: event.clone(),
            payload,
        };
        let mut bytes = serde_json::to_vec(&record)?;
        if bytes.len() > MAXIMUM_EVENT_BYTES {
            return Err(JournalError::Contract(
                "journal event exceeds its byte bound".to_owned(),
            ));
        }
        bytes.push(b'\n');
        // From this point the OS may have accepted all, part, or none of the
        // bytes.  Poison before entering the write so every error path is
        // fail-closed and the caller cannot append a follow-up event.
        self.poisoned = true;
        #[cfg(test)]
        if let Some(prefix) = self.partial_write_bytes.take() {
            let prefix = prefix.min(bytes.len());
            self.events.write_all(&bytes[..prefix]).map_err(|error| {
                JournalError::UnknownCommit(format!("event partial write failed: {error}"))
            })?;
            return Err(JournalError::UnknownCommit(
                "event write was interrupted after a test-injected partial write".to_owned(),
            ));
        }
        if let Err(error) = self.events.write_all(&bytes) {
            return Err(JournalError::UnknownCommit(format!(
                "event write failed after write start: {error}"
            )));
        }
        #[cfg(test)]
        if self.fail_sync_data {
            return Err(JournalError::UnknownCommit(
                "event sync_data failed (test injection)".to_owned(),
            ));
        }
        if let Err(error) = self.events.sync_data() {
            return Err(JournalError::UnknownCommit(format!(
                "event sync_data failed after write: {error}"
            )));
        }
        self.sequence = sequence;
        self.poisoned = false;
        Ok(())
    }

    pub(crate) fn next_sequence(&self) -> Result<u64, JournalError> {
        self.ensure_healthy()?;
        self.sequence
            .checked_add(1)
            .ok_or_else(|| JournalError::Contract("journal sequence overflow".to_owned()))
    }

    pub fn finish(&self, result: &AgentResult) -> Result<(), JournalError> {
        self.ensure_healthy()?;
        atomic_json(&self.directory.join("result.json"), result)
    }

    pub(crate) fn is_poisoned(&self) -> bool {
        self.poisoned
    }

    fn ensure_healthy(&self) -> Result<(), JournalError> {
        if self.poisoned {
            Err(JournalError::Poisoned)
        } else {
            Ok(())
        }
    }

    #[cfg(test)]
    fn inject_sync_failure_for_test(&mut self) {
        self.fail_sync_data = true;
    }

    #[cfg(test)]
    fn inject_partial_write_for_test(&mut self, bytes: usize) {
        self.partial_write_bytes = Some(bytes);
    }

    #[cfg(test)]
    pub(crate) fn inject_parent_sync_failure_for_test(path: &Path) {
        let path = path.canonicalize().unwrap_or_else(|_| path.to_owned());
        FORCE_PARENT_DIRECTORY_SYNC_FAILURE
            .get_or_init(|| Mutex::new(Vec::new()))
            .lock()
            .expect("parent sync test hook mutex is not poisoned")
            .push(path);
    }
}

pub(crate) fn read_resume_checkpoint(path: &Path) -> Result<ArtifactRef, JournalError> {
    let records = read_event_records(path)?;
    resume_position(&records).map(|(checkpoint, _)| checkpoint)
}

fn resume_position(records: &[JournalRecord]) -> Result<(ArtifactRef, u64), JournalError> {
    let last = records
        .last()
        .ok_or_else(|| JournalError::Contract("suspended task has no durable events".to_owned()))?;
    if last.event != KernelEvent::Suspended || last.next_state != TaskState::Suspended {
        return Err(JournalError::Contract(
            "incomplete task is not at a resumable suspension boundary".to_owned(),
        ));
    }
    let checkpoint = last.payload.get("checkpoint").cloned().ok_or_else(|| {
        JournalError::Contract("suspension event has no checkpoint reference".to_owned())
    })?;
    let checkpoint = serde_json::from_value(checkpoint).map_err(JournalError::Json)?;
    Ok((checkpoint, last.sequence))
}

/// Read an existing terminal result without opening the task journal. This is
/// intentionally exact: a result is cacheable only for the same serialized
/// request, including profile, track, budget, and envelope context.
pub(crate) fn read_terminal_result(
    root: &Path,
    task: &TaskEnvelope,
) -> Result<Option<AgentResult>, JournalError> {
    let root = root.canonicalize().map_err(|error| {
        JournalError::Contract(format!("runtime state root is unavailable: {error}"))
    })?;
    let sessions = root.join("sessions");
    let session = sessions.join(task.session_id.as_str());
    let directory = session.join(task.task_id.as_str());
    for path in [&sessions, &session, &directory] {
        let metadata = match fs::symlink_metadata(path) {
            Ok(value) => value,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(None);
            }
            Err(error) => return Err(error.into()),
        };
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(JournalError::Contract(
                "terminal result path must remain a real directory".to_owned(),
            ));
        }
    }
    let result_path = directory.join("result.json");
    if !path_exists_without_symlink(&result_path)? {
        return Ok(None);
    }
    let request: TaskEnvelope =
        read_bounded_json(&directory.join("request.json"), MAXIMUM_TASK_REQUEST_BYTES)?;
    if request != *task {
        return Err(JournalError::Contract(
            "terminal result request does not exactly match the child task".to_owned(),
        ));
    }
    verify_composition_binding(&root, task)?;
    let result: AgentResult = read_bounded_json(&result_path, MAXIMUM_TASK_REQUEST_BYTES)?;
    let events = read_event_records(&directory.join("events.ndjson"))?;
    let result = validate_terminal_result(task, result, &events)?;
    verify_review_subject_bundle(&root, task, &result, &events)?;
    Ok(Some(result))
}

fn validate_terminal_result(
    task: &TaskEnvelope,
    result: AgentResult,
    events: &[JournalRecord],
) -> Result<AgentResult, JournalError> {
    if result.task_id != task.task_id
        || result.session_id != task.session_id
        || result.profile != task.profile
        || result.track != task.track
    {
        return Err(JournalError::Contract(
            "terminal result identity does not match the child task".to_owned(),
        ));
    }
    let expected_composition = task
        .context
        .get("_upagent_composition_digest")
        .and_then(Value::as_str)
        .map(str::to_owned);
    if result.composition_digest != expected_composition {
        return Err(JournalError::Contract(
            "terminal result composition digest does not match the task binding".to_owned(),
        ));
    }
    if let Some(bundle) = &result.review_subject_bundle {
        bundle
            .validate()
            .map_err(|error| JournalError::Contract(error.to_string()))?;
        if !result
            .artifacts
            .iter()
            .any(|artifact| artifact == &bundle.artifact)
        {
            return Err(JournalError::Contract(
                "review subject bundle artifact is missing from terminal result artifacts"
                    .to_owned(),
            ));
        }
    }
    if result.schema_version != agent_core::PROTOCOL_VERSION {
        return Err(JournalError::Contract(
            "terminal result schema version does not match the runtime protocol".to_owned(),
        ));
    }
    if !matches!(
        (result.state, result.status),
        (TaskState::Succeeded, AgentStatus::Succeeded)
            | (TaskState::Failed, AgentStatus::Failed)
            | (TaskState::Cancelled, AgentStatus::Cancelled)
            | (
                TaskState::ResolutionRequired,
                AgentStatus::ResolutionRequired
            )
            | (TaskState::AwaitingInput, AgentStatus::AwaitingInput)
            | (TaskState::AwaitingApproval, AgentStatus::AwaitingApproval)
    ) {
        return Err(JournalError::Contract(
            "terminal result state and status pair is invalid".to_owned(),
        ));
    }
    if events.last().map(|record| record.next_state) != Some(result.state) {
        return Err(JournalError::Contract(
            "terminal result does not match the final journal state".to_owned(),
        ));
    }
    Ok(result)
}

/// Read one existing stopped task without creating or changing any filesystem
/// evidence. The lock is held until every returned projection field is read.
pub(crate) fn inspect_task(
    state_root: &Path,
    session_id: &str,
    task_id: &str,
) -> Result<TaskInspection, JournalError> {
    let session = SessionId::parse(session_id.to_owned())
        .map_err(|error| JournalError::Contract(error.to_string()))?;
    let task = TaskId::parse(task_id.to_owned())
        .map_err(|error| JournalError::Contract(error.to_string()))?;
    let root = existing_directory(state_root, "journal state root")?;
    let sessions = existing_child_directory(&root, "sessions", "session journal root")?;
    let session_directory = existing_child_directory(&sessions, session.as_str(), "session")?;
    let directory = existing_child_directory(&session_directory, task.as_str(), "task")?;
    let lock = open_inspection_lock(&directory)?;

    let request_path = directory.join("request.json");
    let recorded_task: TaskEnvelope = read_bounded_json(&request_path, MAXIMUM_TASK_REQUEST_BYTES)?;
    recorded_task.validate().map_err(JournalError::Contract)?;
    if recorded_task.session_id != session || recorded_task.task_id != task {
        return Err(JournalError::Contract(
            "task request identity does not match the inspection target".to_owned(),
        ));
    }
    verify_composition_binding(&root, &recorded_task)?;
    let events = read_event_records(&directory.join("events.ndjson"))?;
    verify_journal_artifacts(&root, &events)?;
    let result_path = directory.join("result.json");
    let result = if path_exists_without_symlink(&result_path)? {
        let value: AgentResult = read_bounded_json(&result_path, MAXIMUM_TASK_REQUEST_BYTES)?;
        let value = validate_terminal_result(&recorded_task, value, &events)?;
        verify_review_subject_bundle(&root, &recorded_task, &value, &events)?;
        Some(value)
    } else {
        None
    };
    drop(lock);
    Ok(TaskInspection {
        task: recorded_task,
        events,
        result,
    })
}

/// Re-open the create-only composition receipt during result/inspection reads.
/// A task's internal context is not evidence by itself; the durable receipt is
/// the canonical cross-binding for task, review policy, and composition.
fn verify_composition_binding(state_root: &Path, task: &TaskEnvelope) -> Result<(), JournalError> {
    let Some(expected_composition) = task
        .context
        .get("_upagent_composition_digest")
        .and_then(Value::as_str)
    else {
        return Ok(());
    };
    let store = CompositionStore::existing(state_root)
        .map_err(|error| JournalError::Contract(error.to_string()))?;
    let receipt = store
        .read(task.session_id.as_str(), task.task_id.as_str())
        .map_err(|error| JournalError::Contract(error.to_string()))?;
    if receipt.digest != expected_composition
        || task
            .context
            .get("_upagent_task_digest")
            .and_then(Value::as_str)
            != Some(receipt.task_digest.as_str())
        || task.context.get("_upagent_review").and_then(Value::as_str)
            != Some(match receipt.spec.review {
                agent_core::ReviewSpec::None => "none",
                agent_core::ReviewSpec::EmitSubjectBundle => "emit_subject_bundle",
            })
    {
        return Err(JournalError::Contract(
            "task context does not match the durable composition receipt".to_owned(),
        ));
    }
    Ok(())
}

/// Verify the bytes behind a terminal review subject reference. Structural
/// result validation alone is insufficient: a forged result could point at a
/// valid-looking artifact without proving that the immutable bundle binds the
/// exact task, composition, output, and evidence set.
fn verify_review_subject_bundle(
    state_root: &Path,
    task: &TaskEnvelope,
    result: &AgentResult,
    events: &[JournalRecord],
) -> Result<(), JournalError> {
    let ledger = verify_journal_artifacts(state_root, events)?;
    let projected = ledger
        .values()
        .map(|(artifact, _)| artifact.clone())
        .collect::<Vec<_>>();
    if result.artifacts != projected {
        return Err(JournalError::Contract(
            "terminal result artifacts do not equal the typed journal artifact ledger projection"
                .to_owned(),
        ));
    }
    let requested =
        task.context.get("_upagent_review").and_then(Value::as_str) == Some("emit_subject_bundle");
    if result.state == TaskState::Succeeded && requested && result.review_subject_bundle.is_none() {
        return Err(JournalError::Contract(
            "successful terminal result is missing the requested review subject bundle".to_owned(),
        ));
    }
    let Some(bundle_ref) = result.review_subject_bundle.as_ref() else {
        return Ok(());
    };
    if !requested {
        return Err(JournalError::Contract(
            "terminal result contains an unrequested review subject bundle".to_owned(),
        ));
    }
    if bundle_ref.artifact.media_type
        != "application/vnd.upagent.review-subject+json; charset=utf-8"
    {
        return Err(JournalError::Contract(
            "review subject bundle artifact has an unexpected media type".to_owned(),
        ));
    }
    let store = ArtifactStore::existing(state_root.join("artifacts"))
        .map_err(|error| JournalError::Contract(error.to_string()))?;
    let bytes = store
        .read(&bundle_ref.artifact)
        .map_err(|error| JournalError::Contract(error.to_string()))?;
    let bundle: ReviewSubjectBundle = serde_json::from_slice(&bytes).map_err(|error| {
        JournalError::Contract(format!("review subject bundle JSON failed: {error}"))
    })?;
    bundle
        .validate()
        .map_err(|error| JournalError::Contract(error.to_string()))?;
    let expected_composition = task
        .context
        .get("_upagent_composition_digest")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            JournalError::Contract(
                "review subject bundle requires a composition digest binding".to_owned(),
            )
        })?;
    let expected_task = task
        .context
        .get("_upagent_task_digest")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            JournalError::Contract(
                "review subject bundle requires a task digest binding".to_owned(),
            )
        })?;
    if bundle.task_id != task.task_id
        || bundle.session_id != task.session_id
        || bundle.task_digest != expected_task
        || bundle.composition_digest != expected_composition
    {
        return Err(JournalError::Contract(
            "review subject bundle does not match the task or composition binding".to_owned(),
        ));
    }
    let completed_output = events
        .iter()
        .rev()
        .find(|record| record.event == KernelEvent::Completed)
        .and_then(|record| record.payload.get("outputArtifact"))
        .ok_or_else(|| {
            JournalError::Contract(
                "review subject bundle requires a terminal output artifact".to_owned(),
            )
        })?;
    let completed_output: ArtifactRef =
        serde_json::from_value(completed_output.clone()).map_err(|error| {
            JournalError::Contract(format!("terminal output artifact is invalid: {error}"))
        })?;
    if completed_output != bundle.result_artifact {
        return Err(JournalError::Contract(
            "review subject result artifact is not the terminal output artifact".to_owned(),
        ));
    }
    let mut expected = vec![bundle.result_artifact.clone()];
    expected.extend(bundle.evidence.iter().map(|entry| entry.artifact.clone()));
    expected.push(bundle_ref.artifact.clone());
    expected.sort_by(|left, right| left.id.cmp(&right.id));
    if expected != result.artifacts {
        return Err(JournalError::Contract(
            "terminal result artifact set is not exactly result + evidence + bundle".to_owned(),
        ));
    }
    let result_entry = ledger.get(&bundle.result_artifact.id).ok_or_else(|| {
        JournalError::Contract("review result artifact is absent from the typed ledger".to_owned())
    })?;
    if result_entry.1.is_empty() || result_entry.1.as_slice() != bundle.result_uses.as_slice() {
        return Err(JournalError::Contract(
            "review result artifact provenance does not match the typed journal ledger".to_owned(),
        ));
    }
    for evidence in &bundle.evidence {
        let Some((artifact, uses)) = ledger.get(&evidence.artifact.id) else {
            return Err(JournalError::Contract(
                "review evidence artifact is absent from the typed ledger".to_owned(),
            ));
        };
        if artifact != &evidence.artifact || uses != &evidence.uses {
            return Err(JournalError::Contract(
                "review evidence provenance does not match the typed journal ledger".to_owned(),
            ));
        }
    }
    let bundle_entry = ledger.get(&bundle_ref.artifact.id).ok_or_else(|| {
        JournalError::Contract("review subject bundle has no Completed-event provenance".to_owned())
    })?;
    if bundle_entry.1.is_empty() {
        return Err(JournalError::Contract(
            "review subject bundle has no journal provenance use".to_owned(),
        ));
    }
    Ok(())
}

fn derive_artifact_ledger(
    events: &[JournalRecord],
) -> Result<BTreeMap<ArtifactId, (ArtifactRef, Vec<ArtifactUse>)>, JournalError> {
    let mut ledger = BTreeMap::new();
    for record in events {
        let uses = event_artifact_uses(record.sequence, &record.event, &record.payload)
            .map_err(JournalError::Contract)?;
        for (artifact, use_record) in uses {
            let entry = ledger
                .entry(artifact.id.clone())
                .or_insert_with(|| (artifact.clone(), Vec::new()));
            if entry.0 != artifact {
                return Err(JournalError::Contract(
                    "journal artifact id is bound to different metadata".to_owned(),
                ));
            }
            if entry.1.iter().any(|existing| existing == &use_record) {
                return Err(JournalError::Contract(
                    "journal contains duplicate artifact provenance use".to_owned(),
                ));
            }
            if entry.1.len() >= 256 {
                return Err(JournalError::Contract(
                    "journal artifact provenance use bound exceeded".to_owned(),
                ));
            }
            entry.1.push(use_record);
        }
    }
    for (_, uses) in ledger.values_mut() {
        uses.sort_by(|left, right| {
            left.sequence
                .cmp(&right.sequence)
                .then(left.event.cmp(&right.event))
        });
    }
    Ok(ledger)
}

fn verify_journal_artifacts(
    state_root: &Path,
    events: &[JournalRecord],
) -> Result<BTreeMap<ArtifactId, (ArtifactRef, Vec<ArtifactUse>)>, JournalError> {
    let ledger = derive_artifact_ledger(events)?;
    let store = ArtifactStore::existing(state_root.join("artifacts"))
        .map_err(|error| JournalError::Contract(error.to_string()))?;
    for (artifact, _) in ledger.values() {
        store
            .read(artifact)
            .map_err(|error| JournalError::Contract(error.to_string()))?;
    }
    Ok(ledger)
}

fn existing_directory(path: &Path, label: &str) -> Result<PathBuf, JournalError> {
    if !path.is_absolute() {
        return Err(JournalError::Contract(format!(
            "{label} path must be absolute"
        )));
    }
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(JournalError::Contract(format!(
            "{label} must be a real directory"
        )));
    }
    path.canonicalize().map_err(JournalError::Io)
}

fn existing_child_directory(
    parent: &Path,
    name: &str,
    label: &str,
) -> Result<PathBuf, JournalError> {
    if name.is_empty() || name == "." || name == ".." || name.contains('/') || name.contains('\\') {
        return Err(JournalError::Contract(format!(
            "{label} directory component is invalid"
        )));
    }
    let path = parent.join(name);
    let child = existing_directory(&path, label)?;
    if child.parent() != Some(parent) {
        return Err(JournalError::Contract(format!(
            "{label} directory escaped its parent"
        )));
    }
    Ok(child)
}

fn open_inspection_lock(directory: &Path) -> Result<File, JournalError> {
    let path = directory.join("task.lock");
    let metadata = fs::symlink_metadata(&path)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(JournalError::Contract(
            "task lock must be a regular file".to_owned(),
        ));
    }
    let lock = OpenOptions::new().read(true).open(path)?;
    lock.try_lock_shared().map_err(|error| {
        JournalError::Contract(format!(
            "task journal is active and cannot be inspected: {error}"
        ))
    })?;
    Ok(lock)
}

fn read_event_records(path: &Path) -> Result<Vec<JournalRecord>, JournalError> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(JournalError::Contract(
            "task event journal must be a regular file".to_owned(),
        ));
    }
    if metadata.len() == 0 || metadata.len() > MAXIMUM_EVENT_LOG_BYTES {
        return Err(JournalError::Contract(
            "task event journal is outside its byte bound".to_owned(),
        ));
    }
    let bytes = fs::read(path)?;
    if u64::try_from(bytes.len())
        .ok()
        .is_none_or(|length| length == 0 || length > MAXIMUM_EVENT_LOG_BYTES)
    {
        return Err(JournalError::Contract(
            "task event journal is outside its byte bound".to_owned(),
        ));
    }
    if !bytes.ends_with(b"\n") {
        return Err(JournalError::Contract(
            "task event journal ends with an incomplete record".to_owned(),
        ));
    }
    let mut records = Vec::new();
    let mut expected_state = TaskState::Created;
    for line in bytes[..bytes.len() - 1].split(|byte| *byte == b'\n') {
        if line.is_empty() {
            return Err(JournalError::Contract(
                "task event journal contains an empty record".to_owned(),
            ));
        }
        if line.len() > MAXIMUM_EVENT_BYTES || records.len() >= MAXIMUM_EVENT_RECORDS {
            return Err(JournalError::Contract(
                "task event journal exceeded its record bound".to_owned(),
            ));
        }
        let record: JournalRecord = serde_json::from_slice(line)?;
        OffsetDateTime::parse(&record.recorded_at, &Rfc3339).map_err(|error| {
            JournalError::Contract(format!("journal recorded_at is not RFC3339: {error}"))
        })?;
        let expected_sequence = u64::try_from(records.len())
            .unwrap_or(u64::MAX)
            .saturating_add(1);
        let expected_next = transition(expected_state, &record.event)
            .map_err(|error| JournalError::Contract(error.to_string()))?;
        if record.sequence != expected_sequence
            || record.previous_state != expected_state
            || record.next_state != expected_next
        {
            return Err(JournalError::Contract(
                "task event journal transition chain is invalid".to_owned(),
            ));
        }
        validate_resume_event_pair(records.last(), &record)?;
        expected_state = record.next_state;
        records.push(record);
    }
    Ok(records)
}

fn validate_resume_event_pair(
    previous: Option<&JournalRecord>,
    record: &JournalRecord,
) -> Result<(), JournalError> {
    match record.event {
        KernelEvent::Suspended => {
            artifact_from_event_payload(&record.payload, "checkpoint")?;
        }
        KernelEvent::Resumed => {
            // A resumed event carries exactly the typed reference that was
            // persisted by its immediately preceding suspension.  Do not
            // accept a loose map here: preparation/plugin/composition digests
            // belong to the checkpoint bytes and duplicating them in the
            // event creates a second, independently forgeable binding.
            let resumed: ResumedEventPayload = serde_json::from_value(record.payload.clone())
                .map_err(|error| {
                    JournalError::Contract(format!(
                        "resumed event payload is not the typed checkpoint reference: {error}"
                    ))
                })?;
            let checkpoint = resumed.checkpoint;
            let previous = previous.ok_or_else(|| {
                JournalError::Contract(
                    "resumed event has no preceding suspended checkpoint".to_owned(),
                )
            })?;
            if previous.event != KernelEvent::Suspended {
                return Err(JournalError::Contract(
                    "resumed event is not immediately preceded by suspension".to_owned(),
                ));
            }
            let suspended = artifact_from_event_payload(&previous.payload, "checkpoint")?;
            if checkpoint != suspended {
                return Err(JournalError::Contract(
                    "resumed event checkpoint does not exactly match the suspended checkpoint"
                        .to_owned(),
                ));
            }
        }
        _ => {}
    }
    Ok(())
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct ResumedEventPayload {
    checkpoint: ArtifactRef,
}

fn artifact_from_event_payload(payload: &Value, key: &str) -> Result<ArtifactRef, JournalError> {
    let value = payload.get(key).cloned().ok_or_else(|| {
        JournalError::Contract(format!("{key} is required for a checkpoint event"))
    })?;
    serde_json::from_value(value).map_err(|error| {
        JournalError::Contract(format!("{key} is not a valid artifact reference: {error}"))
    })
}

#[derive(Debug)]
struct JournalLock {
    file: File,
}

impl Drop for JournalLock {
    fn drop(&mut self) {
        let _ = fs2::FileExt::unlock(&self.file);
    }
}

fn atomic_json(path: &Path, value: &impl Serialize) -> Result<(), JournalError> {
    let bytes = serde_json::to_vec_pretty(value)?;
    let parent = path
        .parent()
        .ok_or_else(|| JournalError::Contract("journal path has no parent".to_owned()))?;
    verify_private_directory(parent)?;
    if let Ok(metadata) = fs::symlink_metadata(path)
        && (metadata.file_type().is_symlink() || !metadata.is_file())
    {
        return Err(JournalError::Contract(
            "journal destination must be a regular file".to_owned(),
        ));
    }
    let temporary = parent.join(format!(".journal-{}.tmp", Uuid::now_v7()));
    let mut file = open_private_create_new(&temporary)?;
    file.write_all(&bytes)?;
    file.sync_all()?;
    drop(file);
    install_noclobber(&temporary, path)?;

    // Installation has succeeded at this point. Any failure to make the
    // containing directory durable leaves the caller unable to classify the
    // commit, so do not turn it into an ordinary storage error or a success.
    #[cfg(test)]
    if parent_sync_failure_is_injected(path) {
        return Err(JournalError::UnknownCommit(
            "journal destination was installed but parent directory sync failed (test injection)"
                .to_owned(),
        ));
    }
    #[cfg(unix)]
    {
        let directory = File::open(parent).map_err(|error| {
            JournalError::UnknownCommit(format!(
                "journal destination was installed but parent directory could not be opened: {error}"
            ))
        })?;
        directory.sync_all().map_err(|error| {
            JournalError::UnknownCommit(format!(
                "journal destination was installed but parent directory sync failed: {error}"
            ))
        })?;
    }
    Ok(())
}

#[cfg(test)]
fn parent_sync_failure_is_injected(path: &Path) -> bool {
    let mut targets = FORCE_PARENT_DIRECTORY_SYNC_FAILURE
        .get_or_init(|| Mutex::new(Vec::new()))
        .lock()
        .expect("parent sync test hook mutex is not poisoned");
    let injected = targets
        .iter()
        .position(|injected| path == injected || path.starts_with(injected));
    if let Some(index) = injected {
        targets.remove(index);
        true
    } else {
        false
    }
}

fn install_noclobber(temporary: &Path, destination: &Path) -> Result<(), JournalError> {
    #[cfg(target_os = "macos")]
    {
        // RENAME_EXCL and RENAME_NOFOLLOW_ANY make the final create atomic:
        // an existing file or a symlink can never be replaced between the
        // metadata check and installation. rustix exposes the native
        // renameatx_np call without requiring unsafe code in this crate.
        const RENAME_NOFOLLOW_ANY: u32 = 0x0000_0010;
        let flags = RenameFlags::NOREPLACE | RenameFlags::from_bits_retain(RENAME_NOFOLLOW_ANY);
        let result = renameat_with(CWD, temporary, CWD, destination, flags);
        match result {
            Ok(()) => Ok(()),
            Err(error) => {
                let _ = fs::remove_file(temporary);
                Err(std::io::Error::from(error).into())
            }
        }
    }

    #[cfg(not(target_os = "macos"))]
    {
        let result = fs::hard_link(temporary, destination);
        let cleanup = fs::remove_file(temporary);
        match result {
            Ok(()) => cleanup.map_err(|error| installed_temporary_cleanup_failure(&error)),
            Err(error) => {
                let _ = cleanup;
                Err(error.into())
            }
        }
    }
}

#[cfg(any(not(target_os = "macos"), test))]
fn installed_temporary_cleanup_failure(error: &std::io::Error) -> JournalError {
    JournalError::UnknownCommit(format!(
        "journal destination was installed but temporary cleanup failed: {error}"
    ))
}

fn open_private(path: &Path, create: bool, append: bool) -> Result<File, JournalError> {
    if let Ok(metadata) = fs::symlink_metadata(path)
        && (metadata.file_type().is_symlink() || !metadata.is_file())
    {
        return Err(JournalError::Contract(
            "journal file must be a regular file".to_owned(),
        ));
    }
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(create).append(append);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options.open(path)?;
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(JournalError::Contract(
            "journal file must be a regular file".to_owned(),
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(fs::Permissions::from_mode(0o600))?;
    }
    Ok(file)
}

fn open_private_create_new(path: &Path) -> Result<File, JournalError> {
    let mut options = OpenOptions::new();
    options.read(true).write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options.open(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(fs::Permissions::from_mode(0o600))?;
    }
    Ok(file)
}

fn prepare_private_root(path: &Path) -> Result<PathBuf, JournalError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                return Err(JournalError::Contract(
                    "journal root must be a real directory".to_owned(),
                ));
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            fs::create_dir_all(path)?;
        }
        Err(error) => return Err(error.into()),
    }
    verify_private_directory(path)?;
    Ok(path.canonicalize()?)
}

fn prepare_private_child(parent: &Path, name: &str) -> Result<PathBuf, JournalError> {
    if name.is_empty() || name == "." || name == ".." || name.contains('/') || name.contains('\\') {
        return Err(JournalError::Contract(
            "journal directory component is invalid".to_owned(),
        ));
    }
    let path = parent.join(name);
    match fs::symlink_metadata(&path) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                return Err(JournalError::Contract(
                    "journal directory must be a real directory".to_owned(),
                ));
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => match fs::create_dir(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error.into()),
        },
        Err(error) => return Err(error.into()),
    }
    verify_private_directory(&path)?;
    let canonical_parent = parent.canonicalize()?;
    let canonical = path.canonicalize()?;
    if canonical.parent() != Some(canonical_parent.as_path()) {
        return Err(JournalError::Contract(
            "journal directory escaped its parent".to_owned(),
        ));
    }
    Ok(canonical)
}

fn verify_private_directory(path: &Path) -> Result<(), JournalError> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(JournalError::Contract(
            "journal directory must be a real directory".to_owned(),
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

fn now_rfc3339() -> Result<String, JournalError> {
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .map_err(|error| JournalError::Contract(error.to_string()))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct RecoveryEntry {
    pub session_id: String,
    pub task_id: String,
    pub has_events: bool,
    /// False when the task directory exists without a durable request record.
    ///
    /// `SessionJournal::create` takes the task lock before it writes
    /// `request.json`, so a crash or a full disk inside that window leaves a
    /// directory with no request. The task is still incomplete and must remain
    /// resolvable, so it is reported rather than treated as corruption.
    #[serde(default = "default_has_request")]
    pub has_request: bool,
    /// True only when the last verified event is a controlled suspension with
    /// a checkpoint reference. Resuming is done by submitting the exact task
    /// envelope again, not by resolving the recovery record.
    #[serde(default)]
    pub resumable: bool,
    /// True when the stopped-task evidence cannot prove whether a terminal
    /// result was committed. Such entries are intentionally visible and can
    /// be explicitly resolved; they are never auto-resumed.
    #[serde(default)]
    pub durability_unknown: bool,
    /// Bounded machine-readable recovery classifier. `None` is reserved for
    /// a verified resumable suspension; valid terminal tasks are omitted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolution: Option<String>,
}

const fn default_has_request() -> bool {
    true
}

pub fn list_incomplete(root: &Path) -> Result<Vec<RecoveryEntry>, JournalError> {
    let root = prepare_private_root(root)?;
    let sessions = root.join("sessions");
    match fs::symlink_metadata(&sessions) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                return Err(JournalError::Contract(
                    "session journal root must be a real directory".to_owned(),
                ));
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
    }
    let canonical_sessions = sessions.canonicalize()?;
    if canonical_sessions.parent() != Some(root.as_path()) {
        return Err(JournalError::Contract(
            "session journal root escaped the state root".to_owned(),
        ));
    }

    let mut output = Vec::new();
    let mut scanned = 0_usize;
    let mut session_dirs = fs::read_dir(&canonical_sessions)?.collect::<Result<Vec<_>, _>>()?;
    session_dirs.sort_by_key(fs::DirEntry::file_name);
    for session in session_dirs {
        let session_path = session.path();
        let session_meta = fs::symlink_metadata(&session_path)?;
        // A symbolic link is tamper evidence and always fails the scan. A plain
        // file is an unrelated artefact such as a filesystem index and must not
        // make the recovery surface permanently unreadable.
        if session_meta.file_type().is_symlink() {
            return Err(JournalError::Contract(
                "session journal contains a symbolic link entry".to_owned(),
            ));
        }
        if !session_meta.is_dir() {
            continue;
        }
        let session_id = session.file_name().into_string().map_err(|_| {
            JournalError::Contract("session journal directory name must be valid UTF-8".to_owned())
        })?;
        SessionId::parse(session_id.clone())
            .map_err(|error| JournalError::Contract(error.to_string()))?;
        let canonical_session = session_path.canonicalize()?;
        if canonical_session.parent() != Some(canonical_sessions.as_path()) {
            return Err(JournalError::Contract(
                "session journal directory escaped its root".to_owned(),
            ));
        }

        let mut task_dirs = fs::read_dir(&canonical_session)?.collect::<Result<Vec<_>, _>>()?;
        task_dirs.sort_by_key(fs::DirEntry::file_name);
        for task in task_dirs {
            scanned = scanned
                .checked_add(1)
                .ok_or_else(|| JournalError::Contract("recovery scan overflowed".to_owned()))?;
            if scanned > MAXIMUM_RECOVERY_SCAN_ENTRIES {
                return Err(JournalError::Contract(format!(
                    "recovery scan exceeded {MAXIMUM_RECOVERY_SCAN_ENTRIES} task directories"
                )));
            }
            let task_path = task.path();
            let task_meta = fs::symlink_metadata(&task_path)?;
            if task_meta.file_type().is_symlink() {
                return Err(JournalError::Contract(
                    "session journal contains a symbolic link task entry".to_owned(),
                ));
            }
            if !task_meta.is_dir() {
                continue;
            }
            let task_id = task.file_name().into_string().map_err(|_| {
                JournalError::Contract("task journal directory name must be valid UTF-8".to_owned())
            })?;
            TaskId::parse(task_id.clone())
                .map_err(|error| JournalError::Contract(error.to_string()))?;
            let canonical_task = task_path.canonicalize()?;
            if canonical_task.parent() != Some(canonical_session.as_path()) {
                return Err(JournalError::Contract(
                    "task journal directory escaped its session".to_owned(),
                ));
            }
            let Some(_lock) = try_lock_task(&canonical_task)? else {
                continue;
            };
            let resolution_path = canonical_task.join("resolution.json");
            let resolution = if path_exists_without_symlink(&resolution_path)? {
                let value: RecoveryResolution =
                    read_bounded_json(&resolution_path, MAXIMUM_RECOVERY_RESOLUTION_BYTES)?;
                Some(value.resolution)
            } else {
                None
            };
            if let Some(entry) = classify_recovery_entry(
                &root,
                &canonical_task,
                session_id.clone(),
                task_id,
                resolution,
            )? {
                output.push(entry);
            }
        }
    }
    Ok(output)
}

const REASON_TERMINAL_RESULT_MISSING: &str = "terminal_result_missing";
const REASON_TERMINAL_RESULT_UNVERIFIED: &str = "terminal_result_unverified";
const REASON_JOURNAL_UNVERIFIED: &str = "journal_unverified";

fn classify_recovery_entry(
    root: &Path,
    directory: &Path,
    session_id: String,
    task_id: String,
    resolution: Option<String>,
) -> Result<Option<RecoveryEntry>, JournalError> {
    let request_path = directory.join("request.json");
    let events_path = directory.join("events.ndjson");
    let result_path = directory.join("result.json");
    // Security violations (symlinks/non-regular entries) remain hard errors;
    // ordinary missing, partial, or malformed evidence is instead surfaced as
    // a resolvable durability-unknown entry.
    let has_request = path_exists_without_symlink(&request_path)?;
    let has_events = path_exists_without_symlink(&events_path)?;
    let has_result = path_exists_without_symlink(&result_path)?;

    let task = if has_request {
        match read_bounded_json::<TaskEnvelope>(&request_path, MAXIMUM_TASK_REQUEST_BYTES) {
            Ok(value)
                if value.session_id.as_str() == session_id
                    && value.task_id.as_str() == task_id
                    && value.validate().is_ok() =>
            {
                Some(value)
            }
            _ => None,
        }
    } else {
        None
    };

    let events = if has_events {
        read_event_records(&events_path).ok()
    } else {
        None
    };
    let journal_verified = match (&task, &events) {
        (Some(task), Some(events)) => {
            verify_composition_binding(root, task).is_ok()
                && verify_journal_artifacts(root, events).is_ok()
        }
        _ => false,
    };

    if journal_verified && has_result {
        let (Some(task), Some(events)) = (task.as_ref(), events.as_ref()) else {
            return Err(JournalError::Contract(
                "journal verification lost its request or event projection".to_owned(),
            ));
        };
        let terminal = read_bounded_json::<AgentResult>(&result_path, MAXIMUM_TASK_REQUEST_BYTES)
            .ok()
            .and_then(|result| validate_terminal_result(task, result, events).ok())
            .and_then(|result| {
                verify_review_subject_bundle(root, task, &result, events)
                    .ok()
                    .map(|()| result)
            });
        if terminal.is_some() {
            // A terminal result is excluded only after every cross-binding and
            // artifact check succeeds. An existing but invalid result remains
            // visible below for explicit recovery.
            return Ok(None);
        }
    }

    let (resumable, durability_unknown, reason) = if journal_verified && !has_result {
        let resumable =
            resolution.is_none() && verified_resume_checkpoint(root, &request_path, &events_path);
        if resumable {
            (true, false, None)
        } else if events
            .as_ref()
            .and_then(|records| records.last())
            .is_some_and(|record| record.next_state == TaskState::Suspended)
        {
            (false, true, Some(REASON_JOURNAL_UNVERIFIED.to_owned()))
        } else {
            (false, true, Some(REASON_TERMINAL_RESULT_MISSING.to_owned()))
        }
    } else if !journal_verified {
        (false, true, Some(REASON_JOURNAL_UNVERIFIED.to_owned()))
    } else {
        (
            false,
            true,
            Some(REASON_TERMINAL_RESULT_UNVERIFIED.to_owned()),
        )
    };

    Ok(Some(RecoveryEntry {
        session_id,
        task_id,
        has_events,
        has_request,
        resumable,
        durability_unknown,
        reason,
        resolution,
    }))
}

fn verified_resume_checkpoint(root: &Path, request: &Path, events: &Path) -> bool {
    let Ok(task) = read_bounded_json::<TaskEnvelope>(request, MAXIMUM_TASK_REQUEST_BYTES) else {
        return false;
    };
    let Ok(reference) = read_resume_checkpoint(events) else {
        return false;
    };
    if reference.media_type != CHECKPOINT_MEDIA_TYPE {
        return false;
    }
    let Ok(store) = ArtifactStore::open(root.join("artifacts")) else {
        return false;
    };
    let Ok(bytes) = store.read(&reference) else {
        return false;
    };
    SessionCheckpoint::decode(&bytes)
        .and_then(|checkpoint| checkpoint.validate_task(&task))
        .is_ok()
}

pub fn resolve_incomplete(
    root: &Path,
    session_id: &str,
    task_id: &str,
    resolution: &str,
) -> Result<RecoveryEntry, JournalError> {
    if !matches!(
        resolution,
        "acknowledged" | "discarded" | "retry_with_new_task"
    ) {
        return Err(JournalError::Contract(
            "recovery resolution must be acknowledged, discarded, or retry_with_new_task"
                .to_owned(),
        ));
    }
    let root = prepare_private_root(root)?;
    let session = SessionId::parse(session_id.to_owned())
        .map_err(|error| JournalError::Contract(error.to_string()))?;
    let task = TaskId::parse(task_id.to_owned())
        .map_err(|error| JournalError::Contract(error.to_string()))?;
    let sessions = root.join("sessions");
    let sessions_metadata = fs::symlink_metadata(&sessions)?;
    if sessions_metadata.file_type().is_symlink() || !sessions_metadata.is_dir() {
        return Err(JournalError::Contract(
            "session journal root must be a real directory".to_owned(),
        ));
    }
    let canonical_sessions = sessions.canonicalize()?;
    if canonical_sessions.parent() != Some(root.as_path()) {
        return Err(JournalError::Contract(
            "session journal root escaped the state root".to_owned(),
        ));
    }
    let session_directory = canonical_sessions.join(session.as_str());
    let directory = session_directory.join(task.as_str());
    let session_directory_metadata = fs::symlink_metadata(&session_directory)?;
    let task_metadata = fs::symlink_metadata(&directory)?;
    if session_directory_metadata.file_type().is_symlink()
        || !session_directory_metadata.is_dir()
        || task_metadata.file_type().is_symlink()
        || !task_metadata.is_dir()
    {
        return Err(JournalError::Contract(
            "recovery target must use real directories".to_owned(),
        ));
    }
    let canonical_session = session_directory.canonicalize()?;
    let canonical_directory = directory.canonicalize()?;
    if canonical_session.parent() != Some(canonical_sessions.as_path())
        || canonical_directory.parent() != Some(canonical_session.as_path())
    {
        return Err(JournalError::Contract(
            "recovery target is outside the session journal root".to_owned(),
        ));
    }
    // Recovery is a write operation.  If the task directory was created but
    // the runtime had not yet created its lock file, atomically claim that
    // missing lock before reading or writing any recovery evidence.  The
    // read-only scanner keeps its `Missing` result and therefore does not
    // create evidence merely by listing tasks.
    let Some(_lock) = lock_task_for_resolution(&canonical_directory)? else {
        return Err(JournalError::Contract(
            "task journal is still active; recovery requires the task to stop first".to_owned(),
        ));
    };
    let resolution_path = canonical_directory.join("resolution.json");
    if path_exists_without_symlink(&resolution_path)? {
        return Err(JournalError::Contract(
            "incomplete task already has a recovery resolution".to_owned(),
        ));
    }
    // Only an exact terminal projection is non-resolvable. A malformed or
    // partially written result remains a visible recovery candidate.
    if classify_recovery_entry(
        &root,
        &canonical_directory,
        session_id.to_owned(),
        task_id.to_owned(),
        None,
    )?
    .is_none()
    {
        return Err(JournalError::Contract(
            "recovery target is not an incomplete task".to_owned(),
        ));
    }
    let value = RecoveryResolution {
        resolution: resolution.to_owned(),
        resolved_at: now_rfc3339()?,
    };
    atomic_json(&resolution_path, &value)?;
    classify_recovery_entry(
        &root,
        &canonical_directory,
        session_id.to_owned(),
        task_id.to_owned(),
        Some(resolution.to_owned()),
    )?
    .ok_or_else(|| {
        JournalError::Contract(
            "recovery target became a valid terminal task while its lock was held".to_owned(),
        )
    })
}

fn path_exists_without_symlink(path: &Path) -> Result<bool, JournalError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() {
                return Err(JournalError::Contract(
                    "journal evidence must not be a symbolic link".to_owned(),
                ));
            }
            if !metadata.is_file() {
                return Err(JournalError::Contract(
                    "journal evidence must be a regular file".to_owned(),
                ));
            }
            Ok(true)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}

/// Acquire and retain the task lock for the entire recovery classification or
/// resolution operation. Returning only a boolean and dropping the lock would
/// create a check-then-use race where a new runtime can write evidence between
/// classification and the subsequent read/write.
enum TaskLock {
    Held {
        _file: File,
    },
    /// A task directory created before its first durable write has no lock
    /// file. No runtime can be active through the canonical open path in this
    /// shape, so keep an explicit guard value for the classification lifetime
    /// without creating new evidence during a read-only scan.
    Missing,
}

fn try_lock_task(directory: &Path) -> Result<Option<TaskLock>, JournalError> {
    let lock_path = directory.join("task.lock");
    let lock_file = match open_private(&lock_path, false, false) {
        Ok(file) => file,
        Err(JournalError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Some(TaskLock::Missing));
        }
        Err(error) => return Err(error),
    };
    try_lock_exclusive(lock_file)
}

/// Acquire the task lock for a recovery write without leaving a gap when the
/// runtime has created its directory but not yet installed `task.lock`.
/// `create_new` wins that race; an existing file is then opened and locked in
/// the same way as the read-only scanner.
fn lock_task_for_resolution(directory: &Path) -> Result<Option<TaskLock>, JournalError> {
    let lock_path = directory.join("task.lock");
    match open_private_create_new(&lock_path) {
        Ok(file) => try_lock_exclusive(file),
        Err(JournalError::Io(error)) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            let file = open_private(&lock_path, false, false)?;
            try_lock_exclusive(file)
        }
        Err(error) => Err(error),
    }
}

fn try_lock_exclusive(lock_file: File) -> Result<Option<TaskLock>, JournalError> {
    match lock_file.try_lock_exclusive() {
        Ok(()) => Ok(Some(TaskLock::Held { _file: lock_file })),
        Err(error)
            if error.kind() == std::io::ErrorKind::WouldBlock
                || error.raw_os_error() == Some(33) =>
        {
            Ok(None)
        }
        Err(error) => Err(error.into()),
    }
}

fn read_bounded_json<T: for<'de> Deserialize<'de>>(
    path: &Path,
    maximum_bytes: u64,
) -> Result<T, JournalError> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(JournalError::Contract(
            "journal evidence must be a regular file".to_owned(),
        ));
    }
    if metadata.len() > maximum_bytes {
        return Err(JournalError::Contract(
            "journal evidence exceeds its byte bound".to_owned(),
        ));
    }
    let bytes = fs::read(path)?;
    let length = u64::try_from(bytes.len())
        .map_err(|_| JournalError::Contract("journal evidence length exceeds u64".to_owned()))?;
    if length > maximum_bytes {
        return Err(JournalError::Contract(
            "journal evidence exceeds its byte bound".to_owned(),
        ));
    }
    Ok(serde_json::from_slice(&bytes)?)
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct RecoveryResolution {
    resolution: String,
    resolved_at: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_core::{EffectApprovalMode, ProfileId, TaskBudget};
    use serde_json::json;

    fn test_task(id: &str) -> Result<TaskEnvelope, Box<dyn std::error::Error>> {
        Ok(TaskEnvelope {
            schema_version: agent_core::PROTOCOL_VERSION,
            task_id: TaskId::parse(id)?,
            session_id: SessionId::parse("session-journal-tests")?,
            parent_task_id: None,
            profile: ProfileId::creator(),
            track: None,
            objective: "journal fault injection".to_owned(),
            image_task: None,
            history: Vec::new(),
            workspace: None,
            skills: Vec::new(),
            context: BTreeMap::new(),
            budget: TaskBudget::default(),
            effect_approval: EffectApprovalMode::Deny,
            allow_memory_write: false,
            approvals: Vec::new(),
        })
    }

    #[test]
    fn append_sync_failure_poison_disallows_fallback_writes()
    -> Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let task = test_task("task-sync-poison")?;
        let mut journal = SessionJournal::create(&temp.path().join("state"), &task)?;
        journal.inject_sync_failure_for_test();
        let error = journal.append(
            TaskState::Created,
            TaskState::Ready,
            &KernelEvent::Validated,
            Value::Null,
        );
        assert!(matches!(error, Err(JournalError::UnknownCommit(_))));
        assert!(matches!(
            journal.next_sequence(),
            Err(JournalError::Poisoned)
        ));
        let result = AgentResult {
            schema_version: agent_core::PROTOCOL_VERSION,
            task_id: task.task_id.clone(),
            session_id: task.session_id.clone(),
            profile: task.profile.clone(),
            track: task.track.clone(),
            state: TaskState::Ready,
            status: AgentStatus::Failed,
            output: None,
            artifacts: Vec::new(),
            receipts: Vec::new(),
            composition_digest: None,
            review_subject_bundle: None,
            approval: None,
            failure: None,
            metrics: BTreeMap::new(),
        };
        assert!(matches!(
            journal.finish(&result),
            Err(JournalError::Poisoned)
        ));
        assert!(!journal.directory.join("result.json").exists());
        Ok(())
    }

    #[test]
    fn partial_event_write_is_unknown_and_recovery_is_nonresumable()
    -> Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let state = temp.path().join("state");
        let task = test_task("task-partial-write")?;
        let mut journal = SessionJournal::create(&state, &task)?;
        journal.inject_partial_write_for_test(1);
        assert!(matches!(
            journal.append(
                TaskState::Created,
                TaskState::Ready,
                &KernelEvent::Validated,
                Value::Null,
            ),
            Err(JournalError::UnknownCommit(_))
        ));
        drop(journal);
        let entries = list_incomplete(&state)?;
        assert_eq!(entries.len(), 1);
        assert!(entries[0].durability_unknown);
        assert!(!entries[0].resumable);
        assert_eq!(
            entries[0].reason.as_deref(),
            Some(REASON_JOURNAL_UNVERIFIED)
        );
        Ok(())
    }

    #[test]
    fn full_line_after_sync_failure_is_visible_for_recovery()
    -> Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let state = temp.path().join("state");
        fs::create_dir_all(state.join("artifacts"))?;
        let task = test_task("task-full-line-sync-failure")?;
        let mut journal = SessionJournal::create(&state, &task)?;
        journal.inject_sync_failure_for_test();
        assert!(matches!(
            journal.append(
                TaskState::Created,
                TaskState::Ready,
                &KernelEvent::Validated,
                Value::Null,
            ),
            Err(JournalError::UnknownCommit(_))
        ));
        drop(journal);
        let entries = list_incomplete(&state)?;
        assert_eq!(entries.len(), 1);
        assert!(entries[0].durability_unknown);
        assert!(!entries[0].resumable);
        assert_eq!(
            entries[0].reason.as_deref(),
            Some(REASON_TERMINAL_RESULT_MISSING)
        );
        Ok(())
    }

    #[test]
    fn installed_result_parent_sync_is_unknown_without_a_fake_success()
    -> Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let state = temp.path().join("state");
        fs::create_dir_all(state.join("artifacts"))?;
        let task = test_task("task-result-parent-sync")?;
        let mut journal = SessionJournal::create(&state, &task)?;
        let output_artifact = ArtifactStore::open(state.join("artifacts"))?
            .put_text("terminal output", "text/plain")?;
        journal.append(
            TaskState::Created,
            TaskState::Ready,
            &KernelEvent::Validated,
            Value::Null,
        )?;
        journal.append(
            TaskState::Ready,
            TaskState::Running,
            &KernelEvent::Started,
            Value::Null,
        )?;
        journal.append(
            TaskState::Running,
            TaskState::Succeeded,
            &KernelEvent::Completed,
            json!({"outputArtifact": &output_artifact}),
        )?;
        let result = AgentResult {
            schema_version: agent_core::PROTOCOL_VERSION,
            task_id: task.task_id.clone(),
            session_id: task.session_id.clone(),
            profile: task.profile.clone(),
            track: task.track.clone(),
            state: TaskState::Succeeded,
            status: AgentStatus::Succeeded,
            output: None,
            artifacts: vec![output_artifact],
            receipts: Vec::new(),
            composition_digest: None,
            review_subject_bundle: None,
            approval: None,
            failure: None,
            metrics: BTreeMap::new(),
        };
        SessionJournal::inject_parent_sync_failure_for_test(&journal.directory.join("result.json"));
        assert!(matches!(
            journal.finish(&result),
            Err(JournalError::UnknownCommit(_))
        ));
        assert!(journal.directory.join("result.json").is_file());
        drop(journal);

        // Restart/recovery must validate the exact installed terminal result,
        // not manufacture a second result or report the task as incomplete.
        assert!(list_incomplete(&state)?.is_empty());
        assert!(
            inspect_task(&state, task.session_id.as_str(), task.task_id.as_str())?
                .result
                .is_some()
        );
        Ok(())
    }

    #[test]
    fn resumed_checkpoint_must_equal_the_preceding_suspension_reference()
    -> Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let path = temp.path().join("events.ndjson");
        let checkpoint = ArtifactRef {
            id: ArtifactId::parse("art-checkpoint-a")?,
            sha256: "a".repeat(64),
            media_type: CHECKPOINT_MEDIA_TYPE.to_owned(),
            byte_length: 1,
            relative_path: "aa/checkpoint-a.json".to_owned(),
        };
        let forged = ArtifactRef {
            id: ArtifactId::parse("art-checkpoint-b")?,
            sha256: "b".repeat(64),
            media_type: CHECKPOINT_MEDIA_TYPE.to_owned(),
            byte_length: 1,
            relative_path: "bb/checkpoint-b.json".to_owned(),
        };
        let records = vec![
            JournalRecord {
                sequence: 1,
                recorded_at: now_rfc3339()?,
                previous_state: TaskState::Created,
                next_state: TaskState::Ready,
                event: KernelEvent::Validated,
                payload: Value::Null,
            },
            JournalRecord {
                sequence: 2,
                recorded_at: now_rfc3339()?,
                previous_state: TaskState::Ready,
                next_state: TaskState::Running,
                event: KernelEvent::Started,
                payload: Value::Null,
            },
            JournalRecord {
                sequence: 3,
                recorded_at: now_rfc3339()?,
                previous_state: TaskState::Running,
                next_state: TaskState::Suspended,
                event: KernelEvent::Suspended,
                payload: serde_json::json!({"checkpoint": checkpoint}),
            },
            JournalRecord {
                sequence: 4,
                recorded_at: now_rfc3339()?,
                previous_state: TaskState::Suspended,
                next_state: TaskState::Running,
                event: KernelEvent::Resumed,
                payload: serde_json::json!({"checkpoint": forged}),
            },
        ];
        let mut bytes = Vec::new();
        for record in records {
            bytes.extend(serde_json::to_vec(&record)?);
            bytes.push(b'\n');
        }
        fs::write(path.clone(), bytes)?;
        let error =
            read_event_records(&path).expect_err("forged resume reference must be rejected");
        assert!(error.to_string().contains("does not exactly match"));
        Ok(())
    }

    #[test]
    fn resumed_event_rejects_redundant_preparation_digests()
    -> Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let path = temp.path().join("events.ndjson");
        let checkpoint = ArtifactRef {
            id: ArtifactId::parse("art-checkpoint-typed")?,
            sha256: "a".repeat(64),
            media_type: CHECKPOINT_MEDIA_TYPE.to_owned(),
            byte_length: 1,
            relative_path: "aa/checkpoint-typed.json".to_owned(),
        };
        let records = vec![
            JournalRecord {
                sequence: 1,
                recorded_at: now_rfc3339()?,
                previous_state: TaskState::Created,
                next_state: TaskState::Ready,
                event: KernelEvent::Validated,
                payload: Value::Null,
            },
            JournalRecord {
                sequence: 2,
                recorded_at: now_rfc3339()?,
                previous_state: TaskState::Ready,
                next_state: TaskState::Running,
                event: KernelEvent::Started,
                payload: Value::Null,
            },
            JournalRecord {
                sequence: 3,
                recorded_at: now_rfc3339()?,
                previous_state: TaskState::Running,
                next_state: TaskState::Suspended,
                event: KernelEvent::Suspended,
                payload: serde_json::json!({"checkpoint": checkpoint}),
            },
            JournalRecord {
                sequence: 4,
                recorded_at: now_rfc3339()?,
                previous_state: TaskState::Suspended,
                next_state: TaskState::Running,
                event: KernelEvent::Resumed,
                payload: serde_json::json!({
                    "checkpoint": checkpoint,
                    "preparationDigest": "b".repeat(64),
                }),
            },
        ];
        let mut bytes = Vec::new();
        for record in records {
            bytes.extend(serde_json::to_vec(&record)?);
            bytes.push(b'\n');
        }
        fs::write(path.clone(), bytes)?;
        let error = read_event_records(&path)
            .expect_err("resumed event must not carry a duplicate preparation digest");
        assert!(error.to_string().contains("typed checkpoint reference"));
        Ok(())
    }

    #[test]
    fn invalid_existing_result_remains_visible_and_resolvable()
    -> Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let root = temp.path().join("state");
        let directory = root.join("sessions/session-invalid-result/task-invalid-result");
        fs::create_dir_all(&directory)?;
        fs::write(directory.join("task.lock"), b"")?;
        fs::write(directory.join("request.json"), b"{}")?;
        fs::write(directory.join("result.json"), b"{not-json")?;
        let entries = list_incomplete(&root)?;
        assert_eq!(entries.len(), 1);
        assert!(entries[0].durability_unknown);
        assert!(!entries[0].resumable);
        assert_eq!(
            entries[0].reason.as_deref(),
            Some(REASON_JOURNAL_UNVERIFIED)
        );
        let resolved = resolve_incomplete(
            &root,
            "session-invalid-result",
            "task-invalid-result",
            "discarded",
        )?;
        assert_eq!(resolved.resolution.as_deref(), Some("discarded"));
        Ok(())
    }

    #[test]
    fn terminal_evidence_is_create_only() -> Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let path = temp.path().canonicalize()?.join("result.json");
        atomic_json(&path, &serde_json::json!({"value":1}))?;
        assert!(atomic_json(&path, &serde_json::json!({"value":2})).is_err());
        let stored: Value = serde_json::from_slice(&fs::read(path)?)?;
        assert_eq!(stored, serde_json::json!({"value":1}));
        Ok(())
    }

    #[test]
    fn installed_temporary_cleanup_failure_is_unknown_commit() {
        let error = installed_temporary_cleanup_failure(&std::io::Error::other("injected"));
        assert!(matches!(error, JournalError::UnknownCommit(_)));
    }

    #[cfg(unix)]
    #[test]
    fn atomic_json_refuses_a_symlink_destination() -> Result<(), Box<dyn std::error::Error>> {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir()?;
        let outside = temp.path().join("outside.json");
        let path = temp.path().join("result.json");
        fs::write(&outside, br#"{"value":"outside"}"#)?;
        symlink(&outside, &path)?;

        assert!(atomic_json(&path, &serde_json::json!({"value":"inside"})).is_err());
        assert_eq!(fs::read(&outside)?, br#"{"value":"outside"}"#);
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn recovery_scan_rejects_non_utf8_directory_names() -> Result<(), Box<dyn std::error::Error>> {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;

        let temp = tempfile::tempdir()?;
        let sessions = temp.path().join("state/sessions");
        fs::create_dir_all(&sessions)?;
        let invalid_name = sessions.join(OsString::from_vec(vec![b's', 0xff]));
        if let Err(error) = fs::create_dir(&invalid_name) {
            // macOS filesystems reject non-UTF-8 path components before the
            // journal scanner can observe them.
            if macos_rejects_non_utf8_directory_names(&error) {
                return Ok(());
            }
            return Err(error.into());
        }

        let error = list_incomplete(&temp.path().join("state"))
            .expect_err("non-UTF-8 journal directory names must not be lossily aliased");
        assert!(
            error
                .to_string()
                .contains("directory name must be valid UTF-8")
        );
        Ok(())
    }

    #[cfg(unix)]
    fn macos_rejects_non_utf8_directory_names(error: &std::io::Error) -> bool {
        cfg!(target_os = "macos") && matches!(error.raw_os_error(), Some(1 | 92))
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_non_utf8_directory_name_errno_is_classified_as_unsupported() {
        assert!(macos_rejects_non_utf8_directory_names(
            &std::io::Error::from_raw_os_error(1)
        ));
        assert!(macos_rejects_non_utf8_directory_names(
            &std::io::Error::from_raw_os_error(92)
        ));
        assert!(!macos_rejects_non_utf8_directory_names(
            &std::io::Error::from_raw_os_error(2)
        ));
    }

    #[test]
    fn recovery_scan_reports_a_task_directory_without_a_durable_request()
    -> Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let root = temp.path().join("state");
        let task = root.join("sessions/session-test/task-test");
        fs::create_dir_all(&task)?;
        // `SessionJournal::create` takes the task lock before it writes the
        // request, so a crash inside that window leaves exactly this shape.
        fs::write(task.join("task.lock"), b"")?;

        let entries = list_incomplete(&root)?;
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].task_id, "task-test");
        assert!(!entries[0].has_request);

        let resolved = resolve_incomplete(&root, "session-test", "task-test", "discarded")?;
        assert_eq!(resolved.resolution.as_deref(), Some("discarded"));
        Ok(())
    }

    #[test]
    fn active_task_cannot_be_resolved_until_its_lock_is_released()
    -> Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let root = temp.path().join("state");
        let task = TaskEnvelope {
            schema_version: agent_core::PROTOCOL_VERSION,
            task_id: TaskId::parse("task-active")?,
            session_id: SessionId::parse("session-active")?,
            parent_task_id: None,
            profile: ProfileId::creator(),
            track: None,
            objective: "active task".to_owned(),
            image_task: None,
            history: Vec::new(),
            workspace: None,
            skills: Vec::new(),
            context: std::collections::BTreeMap::new(),
            budget: TaskBudget::default(),
            effect_approval: EffectApprovalMode::Deny,
            allow_memory_write: false,
            approvals: Vec::new(),
        };
        let journal = SessionJournal::create(&root, &task)?;
        assert!(list_incomplete(&root)?.is_empty());
        let error = resolve_incomplete(
            &root,
            task.session_id.as_str(),
            task.task_id.as_str(),
            "discarded",
        )
        .expect_err("an active task must not be resolved");
        assert!(error.to_string().contains("still active"));

        drop(journal);
        let resolved = resolve_incomplete(
            &root,
            task.session_id.as_str(),
            task.task_id.as_str(),
            "discarded",
        )?;
        assert_eq!(resolved.resolution.as_deref(), Some("discarded"));
        Ok(())
    }

    #[test]
    fn recovery_scan_skips_unrelated_files_beside_journal_directories()
    -> Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let root = temp.path().join("state");
        let session = root.join("sessions/session-test");
        let task = session.join("task-test");
        fs::create_dir_all(&task)?;
        fs::write(task.join("request.json"), b"{}")?;
        // A filesystem index file must not make recovery permanently unreadable.
        fs::write(root.join("sessions/.DS_Store"), b"\0")?;
        fs::write(session.join(".DS_Store"), b"\0")?;

        let entries = list_incomplete(&root)?;
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].session_id, "session-test");
        assert!(entries[0].has_request);
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn recovery_scan_rejects_symbolic_link_evidence() -> Result<(), Box<dyn std::error::Error>> {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir()?;
        let root = temp.path().join("state");
        let task = root.join("sessions/session-test/task-test");
        fs::create_dir_all(&task)?;
        let outside = temp.path().join("outside.json");
        fs::write(&outside, b"{}")?;
        symlink(&outside, task.join("request.json"))?;
        assert!(list_incomplete(&root).is_err());
        Ok(())
    }
}
