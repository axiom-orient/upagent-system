use std::{collections::BTreeSet, path::PathBuf};

use agent_core::{
    AgentFailure, AgentResult, AgentStatus, ArtifactRef, FailureCode, KernelEvent, ModelMessage,
    TaskEnvelope, ToolCall,
};
use serde_json::{Value, json};

use crate::{ArtifactStore, journal::SessionJournal};

use super::checkpoint::{CHECKPOINT_MEDIA_TYPE, ResumeCheckpoint, SessionCheckpoint};
use super::machine::{SessionMachine, finish_failure, finish_result, undurable_failure};
use super::tool_dispatch::ToolExecution;
use super::tool_outcome::{ToolProgress, handle_tool_execution};

/// Durable session state that only ever mutates on a blocking thread.
///
/// Journal appends `fsync`, terminal results are written through a create-only
/// hard link, and artifact writes hash whole files. Running any of that on a
/// runtime worker would block the reactor that drives this task's own deadline
/// and cancellation timers, so the state is moved onto the blocking pool for
/// each step and moved back when the step completes.
///
/// The state is absent only while a step is in flight. A step that fails to
/// rejoin has lost its journal handle, which is reported as an undurable
/// failure rather than silently continuing against half-written evidence.
#[derive(Debug)]
pub(super) struct DurableSession {
    owned: Option<Box<Owned>>,
    artifacts: ArtifactStore,
    resume: Option<ResumeCheckpoint>,
}

#[derive(Debug)]
struct Owned {
    machine: SessionMachine,
    journal: SessionJournal,
}

pub(super) struct SuspensionBinding<'a> {
    pub(super) plugin_set_digest: &'a str,
    pub(super) plugin_execution_digest: &'a str,
    pub(super) preparation_digest: &'a str,
    pub(super) composition_digest: Option<&'a str>,
    pub(super) approved_subjects: &'a BTreeSet<String>,
}

impl DurableSession {
    /// Open the task journal off the async runtime.
    pub(super) async fn open(
        state_root: PathBuf,
        artifacts: ArtifactStore,
        task: &TaskEnvelope,
    ) -> Result<Self, AgentFailure> {
        let budget = task.budget.clone();
        let image_task = task.image_task.clone();
        let task_for_open = task.clone();
        let artifacts_for_open = artifacts.clone();
        let opened = blocking(move || {
            let opened = SessionJournal::open(&state_root, &task_for_open)
                .map_err(|e| storage_failure(&e))?;
            let resume = match opened.resume_checkpoint {
                Some(reference) => {
                    if reference.media_type != CHECKPOINT_MEDIA_TYPE {
                        return Err(AgentFailure::new(
                            FailureCode::StorageFailed,
                            "session checkpoint media type is invalid",
                        )
                        .with_detail("suspendedCheckpoint", true));
                    }
                    let bytes = artifacts_for_open.read(&reference).map_err(|error| {
                        AgentFailure::new(FailureCode::StorageFailed, error.to_string())
                            .with_detail("suspendedCheckpoint", true)
                    })?;
                    let value = SessionCheckpoint::decode(&bytes)
                        .and_then(|value| {
                            value.validate_task(&task_for_open)?;
                            Ok(value)
                        })
                        .map_err(|failure| failure.with_detail("suspendedCheckpoint", true))?;
                    Some(ResumeCheckpoint {
                        artifact: reference,
                        value,
                    })
                }
                None => None,
            };
            Ok((opened.journal, resume))
        })
        .await??;
        let (journal, resume) = opened;
        let machine = resume.as_ref().map_or_else(
            || SessionMachine::new(budget),
            |value| value.value.machine.clone(),
        );
        let mut machine = machine;
        machine
            .image_director
            .bind_contract(image_task.as_ref())
            .map_err(|message| {
                AgentFailure::new(
                    if resume.is_some() {
                        FailureCode::StorageFailed
                    } else {
                        FailureCode::InvalidRequest
                    },
                    message,
                )
            })?;
        if let Some(resume) = resume.as_ref() {
            let sequence = journal
                .next_sequence()
                .map_err(|error| storage_failure(&error))?
                .checked_sub(1)
                .ok_or_else(|| {
                    AgentFailure::new(
                        FailureCode::StorageFailed,
                        "resume journal sequence has no suspended event",
                    )
                })?;
            machine.register_event_payload(
                sequence,
                &KernelEvent::Suspended,
                &json!({"checkpoint": &resume.artifact}),
            )?;
        }
        Ok(Self {
            owned: Some(Box::new(Owned { machine, journal })),
            artifacts,
            resume,
        })
    }

    pub(super) fn take_resume(&mut self) -> Option<ResumeCheckpoint> {
        self.resume.take()
    }

    pub(super) fn blocked(&mut self, task: &TaskEnvelope, failure: AgentFailure) -> AgentResult {
        match self.machine() {
            Ok(machine) => AgentResult {
                schema_version: agent_core::PROTOCOL_VERSION,
                task_id: task.task_id.clone(),
                session_id: task.session_id.clone(),
                profile: task.profile.clone(),
                track: task.track.clone(),
                state: machine.state,
                status: AgentStatus::Suspended,
                output: None,
                artifacts: machine.artifacts(),
                receipts: machine.receipts.clone(),
                composition_digest: task
                    .context
                    .get("_upagent_composition_digest")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                review_subject_bundle: None,
                approval: None,
                failure: Some(failure),
                metrics: machine.metrics(),
            },
            Err(detached) => undurable_failure(task, detached),
        }
    }

    /// Read or update the pure in-memory machine.
    ///
    /// These transitions perform no I/O, so they stay on the calling task.
    pub(super) fn machine(&mut self) -> Result<&mut SessionMachine, AgentFailure> {
        self.owned
            .as_deref_mut()
            .map(|owned| &mut owned.machine)
            .ok_or_else(detached)
    }

    pub(super) fn machine_ref(&self) -> Result<&SessionMachine, AgentFailure> {
        self.owned
            .as_deref()
            .map(|owned| &owned.machine)
            .ok_or_else(detached)
    }

    /// Run one durable step off the async runtime.
    pub(super) async fn step<T, F>(&mut self, operation: F) -> Result<T, AgentFailure>
    where
        F: FnOnce(&mut SessionMachine, &mut SessionJournal) -> T + Send + 'static,
        T: Send + 'static,
    {
        let mut owned = self.owned.take().ok_or_else(detached)?;
        let (owned, value) = blocking(move || {
            let value = operation(&mut owned.machine, &mut owned.journal);
            (owned, value)
        })
        .await?;
        self.owned = Some(owned);
        Ok(value)
    }

    /// Append one kernel event to the durable journal.
    pub(super) async fn record(
        &mut self,
        event: KernelEvent,
        payload: Value,
    ) -> Result<(), AgentFailure> {
        self.step(move |machine, journal| machine.record(journal, event, payload))
            .await?
    }

    pub(super) async fn record_control(
        &mut self,
        event: KernelEvent,
        payload: Value,
    ) -> Result<(), AgentFailure> {
        self.step(move |machine, journal| machine.record_control(journal, &event, payload))
            .await?
    }

    pub(super) async fn next_sequence(&mut self) -> Result<u64, AgentFailure> {
        self.step(|_, journal| {
            journal
                .next_sequence()
                .map_err(|error| storage_failure(&error))
        })
        .await?
    }

    /// Record a terminal failure and return the caller-visible result.
    pub(super) async fn fail(&mut self, task: &TaskEnvelope, failure: AgentFailure) -> AgentResult {
        let recorded = task.clone();
        match self
            .step(move |machine, journal| finish_failure(&recorded, machine, journal, failure))
            .await
        {
            Ok(result) => result,
            Err(detached) => undurable_failure(task, detached),
        }
    }

    pub(super) async fn suspend(
        &mut self,
        task: &TaskEnvelope,
        messages: Vec<ModelMessage>,
        binding: SuspensionBinding<'_>,
        reason: AgentFailure,
    ) -> AgentResult {
        let snapshot = match self
            .machine()
            .and_then(|machine| machine.suspended_snapshot())
        {
            Ok(value) => value,
            Err(failure) => return self.fail(task, failure).await,
        };
        let checkpoint = match SessionCheckpoint::new(
            task,
            binding.plugin_set_digest.to_owned(),
            binding.plugin_execution_digest.to_owned(),
            binding.preparation_digest.to_owned(),
            binding.composition_digest.map(str::to_owned),
            binding.approved_subjects.clone(),
            messages,
            snapshot,
        )
        .and_then(|value| value.encode())
        {
            Ok(value) => value,
            Err(failure) => return self.fail(task, failure).await,
        };
        let artifact = match self
            .put_artifact_text(checkpoint, CHECKPOINT_MEDIA_TYPE)
            .await
        {
            Ok(value) => value,
            Err(failure) => return self.fail(task, failure).await,
        };
        let recorded_task = task.clone();
        let recorded_artifact = artifact.clone();
        match self
            .step(move |machine, journal| {
                if let Err(failure) = machine.record_control(
                    journal,
                    &KernelEvent::Suspended,
                    json!({"checkpoint": &recorded_artifact, "reason": &reason}),
                ) {
                    return finish_failure(&recorded_task, machine, journal, failure);
                }
                let mut metrics = machine.metrics();
                metrics.insert(
                    "resumeCheckpoint".to_owned(),
                    Value::String(recorded_artifact.id.to_string()),
                );
                AgentResult {
                    schema_version: agent_core::PROTOCOL_VERSION,
                    task_id: recorded_task.task_id.clone(),
                    session_id: recorded_task.session_id.clone(),
                    profile: recorded_task.profile.clone(),
                    track: recorded_task.track.clone(),
                    state: machine.state,
                    status: AgentStatus::Suspended,
                    output: None,
                    artifacts: machine.artifacts(),
                    receipts: machine.receipts.clone(),
                    composition_digest: recorded_task
                        .context
                        .get("_upagent_composition_digest")
                        .and_then(Value::as_str)
                        .map(str::to_owned),
                    review_subject_bundle: None,
                    approval: None,
                    failure: Some(reason.retryable(true)),
                    metrics,
                }
            })
            .await
        {
            Ok(result) => result,
            Err(detached) => undurable_failure(task, detached),
        }
    }

    /// Commit a terminal success or pause and return the caller-visible result.
    pub(super) async fn finish(&mut self, task: &TaskEnvelope, result: AgentResult) -> AgentResult {
        let irreversible = match self.machine() {
            Ok(machine) => machine.irreversible_effect_observed,
            Err(detached) => return undurable_failure(task, detached),
        };
        match self
            .step(move |_, journal| finish_result(journal, result, irreversible))
            .await
        {
            Ok(result) => result,
            Err(detached) => undurable_failure(task, detached),
        }
    }

    /// Persist one artifact off the async runtime.
    pub(super) async fn put_artifact_text(
        &self,
        text: String,
        media_type: &'static str,
    ) -> Result<ArtifactRef, AgentFailure> {
        let artifacts = self.artifacts.clone();
        blocking(move || {
            artifacts
                .put_text(&text, media_type)
                .map_err(|error| AgentFailure::new(FailureCode::StorageFailed, error.to_string()))
        })
        .await?
    }

    /// Apply one tool outcome, including every durable write it implies.
    pub(super) async fn apply_tool_outcome(
        &mut self,
        task: &TaskEnvelope,
        call: &ToolCall,
        execution: ToolExecution,
    ) -> ToolProgress {
        let artifacts = self.artifacts.clone();
        let recorded_task = task.clone();
        let recorded_call = call.clone();
        self.step(move |machine, journal| {
            handle_tool_execution(
                &artifacts,
                &recorded_task,
                &recorded_call,
                execution,
                machine,
                journal,
            )
        })
        .await
        .unwrap_or_else(|detached| ToolProgress::Finished(undurable_failure(task, detached)))
    }
}

/// Move one blocking closure onto the runtime's blocking pool.
async fn blocking<T, F>(operation: F) -> Result<T, AgentFailure>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    tokio::task::spawn_blocking(operation)
        .await
        .map_err(|error| {
            AgentFailure::new(
                FailureCode::InternalInvariant,
                "a durable session step did not complete",
            )
            .with_detail("cause", error.to_string())
        })
}

fn detached() -> AgentFailure {
    AgentFailure::new(
        FailureCode::InternalInvariant,
        "durable session state was lost by an incomplete step",
    )
}

fn storage_failure(error: &crate::journal::JournalError) -> AgentFailure {
    if error.durability_unknown() {
        return AgentFailure::new(
            FailureCode::ResolutionRequired,
            "journal durability is unknown; recover the task before retrying",
        )
        .with_detail("durability", "unknown")
        .with_detail("recovery", "agent recover task-list")
        .with_detail("cause", error.to_string());
    }
    let failure = AgentFailure::new(FailureCode::StorageFailed, error.to_string());
    if error.is_resume_unverified() {
        failure.with_detail("suspendedCheckpoint", true)
    } else {
        failure
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::journal::{SessionJournal, list_incomplete};
    use agent_core::{EffectApprovalMode, ProfileId, SessionId, TaskBudget, TaskId, TaskState};
    use std::collections::BTreeMap;

    #[tokio::test]
    async fn request_parent_sync_unknown_projects_resolution_and_recovery()
    -> Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let state = temp.path().join("state");
        let artifacts = ArtifactStore::open(state.join("artifacts"))?;
        let task = TaskEnvelope {
            schema_version: agent_core::PROTOCOL_VERSION,
            task_id: TaskId::parse("task-request-parent-sync")?,
            session_id: SessionId::parse("session-request-parent-sync")?,
            parent_task_id: None,
            profile: ProfileId::creator(),
            track: None,
            objective: "request parent sync".to_owned(),
            image_task: None,
            history: Vec::new(),
            workspace: None,
            skills: Vec::new(),
            context: BTreeMap::new(),
            budget: TaskBudget::default(),
            effect_approval: EffectApprovalMode::Deny,
            allow_memory_write: false,
            approvals: Vec::new(),
        };

        SessionJournal::inject_parent_sync_failure_for_test(&state);
        let failure = DurableSession::open(state.clone(), artifacts, &task)
            .await
            .expect_err("request parent sync failure must stop durable open");
        assert_eq!(failure.code, FailureCode::ResolutionRequired);
        assert_eq!(
            failure.details.get("durability").and_then(Value::as_str),
            Some("unknown")
        );
        assert_eq!(
            failure.details.get("recovery").and_then(Value::as_str),
            Some("agent recover task-list")
        );

        let projected = undurable_failure(&task, failure);
        assert_eq!(projected.state, TaskState::ResolutionRequired);
        assert_eq!(projected.status, AgentStatus::ResolutionRequired);
        assert_eq!(
            projected
                .failure
                .as_ref()
                .and_then(|failure| failure.details.get("durability"))
                .and_then(Value::as_str),
            Some("unknown")
        );

        let entries = list_incomplete(&state)?;
        assert_eq!(entries.len(), 1);
        assert!(entries[0].has_request);
        assert!(!entries[0].has_events);
        assert!(entries[0].durability_unknown);
        assert!(!entries[0].resumable);
        assert_eq!(entries[0].reason.as_deref(), Some("journal_unverified"));
        Ok(())
    }
}
