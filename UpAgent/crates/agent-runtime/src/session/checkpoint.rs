use std::collections::BTreeSet;

use agent_core::{
    AgentFailure, ArtifactRef, FailureCode, ModelMessage, SessionId, TaskEnvelope, TaskId,
    TaskState,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::SessionMachine;

const CHECKPOINT_SCHEMA_VERSION: u32 = 3;
pub(crate) const CHECKPOINT_MEDIA_TYPE: &str =
    "application/vnd.upagent.session-checkpoint+json; charset=utf-8";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(crate) struct SessionCheckpoint {
    schema_version: u32,
    task_id: TaskId,
    session_id: SessionId,
    request_digest: String,
    pub(super) plugin_set_digest: String,
    pub(super) plugin_execution_digest: String,
    pub(super) preparation_digest: String,
    pub(super) composition_digest: Option<String>,
    pub(super) approved_subjects: BTreeSet<String>,
    pub(super) messages: Vec<ModelMessage>,
    pub(super) machine: SessionMachine,
}

impl SessionCheckpoint {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn new(
        task: &TaskEnvelope,
        plugin_set_digest: String,
        plugin_execution_digest: String,
        preparation_digest: String,
        composition_digest: Option<String>,
        approved_subjects: BTreeSet<String>,
        messages: Vec<ModelMessage>,
        machine: SessionMachine,
    ) -> Result<Self, AgentFailure> {
        let value = Self {
            schema_version: CHECKPOINT_SCHEMA_VERSION,
            task_id: task.task_id.clone(),
            session_id: task.session_id.clone(),
            request_digest: request_digest(task)?,
            plugin_set_digest,
            plugin_execution_digest,
            preparation_digest,
            composition_digest,
            approved_subjects,
            messages,
            machine,
        };
        value.validate()?;
        Ok(value)
    }

    pub(crate) fn decode(bytes: &[u8]) -> Result<Self, AgentFailure> {
        let value: Self = serde_json::from_slice(bytes).map_err(|error| {
            AgentFailure::new(
                FailureCode::StorageFailed,
                "session checkpoint could not be decoded",
            )
            .with_detail("cause", error.to_string())
        })?;
        value.validate()?;
        Ok(value)
    }

    pub(super) fn encode(&self) -> Result<String, AgentFailure> {
        serde_json::to_string(self).map_err(|error| {
            AgentFailure::new(
                FailureCode::StorageFailed,
                "session checkpoint could not be encoded",
            )
            .with_detail("cause", error.to_string())
        })
    }

    pub(crate) fn validate_task(&self, task: &TaskEnvelope) -> Result<(), AgentFailure> {
        if self.task_id != task.task_id
            || self.session_id != task.session_id
            || self.request_digest != request_digest(task)?
        {
            return Err(AgentFailure::new(
                FailureCode::StorageFailed,
                "session checkpoint does not belong to the requested task",
            ));
        }
        let task_composition_digest = task
            .context
            .get("_upagent_composition_digest")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned);
        if self.composition_digest != task_composition_digest {
            return Err(AgentFailure::new(
                FailureCode::StorageFailed,
                "session checkpoint composition digest does not match the requested task",
            ));
        }
        Ok(())
    }

    fn validate(&self) -> Result<(), AgentFailure> {
        if self.schema_version != CHECKPOINT_SCHEMA_VERSION
            || self.plugin_set_digest.len() != 64
            || self.plugin_execution_digest.len() != 64
            || self.preparation_digest.len() != 64
            || self.request_digest.len() != 64
            || !self
                .plugin_set_digest
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit())
            || !self
                .plugin_execution_digest
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit())
            || !self
                .preparation_digest
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit())
            || !self
                .request_digest
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit())
            || self.messages.is_empty()
            || self.machine.state != TaskState::Suspended
            || self.composition_digest.as_ref().is_some_and(|digest| {
                digest.len() != 64 || !digest.bytes().all(|byte| byte.is_ascii_hexdigit())
            })
        {
            return Err(AgentFailure::new(
                FailureCode::StorageFailed,
                "session checkpoint contract is invalid",
            ));
        }
        Ok(())
    }
}

fn request_digest(task: &TaskEnvelope) -> Result<String, AgentFailure> {
    let bytes = serde_json::to_vec(task).map_err(|error| {
        AgentFailure::new(
            FailureCode::StorageFailed,
            "task request could not be encoded for checkpoint binding",
        )
        .with_detail("cause", error.to_string())
    })?;
    let mut hasher = Sha256::new();
    hasher.update(b"upagent-session-request-v1");
    hasher.update(bytes);
    Ok(format!("{:x}", hasher.finalize()))
}

#[derive(Debug)]
pub(super) struct ResumeCheckpoint {
    pub(super) artifact: ArtifactRef,
    pub(super) value: SessionCheckpoint,
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};

    use agent_core::{
        ArtifactId, EffectApprovalMode, IMAGE_TASK_CONTRACT_SCHEMA, IMAGE_VERIFICATION_SPEC_SCHEMA,
        ImageCompareArtifactKind, ImageCompareAssertion, ImageCompareSeverity, ImageSource,
        ImageTaskContract, ImageVerificationContract, ImageVerificationSpec, MessageRole,
        ProfileId, TaskBudget, TaskEnvelope, TaskId,
    };

    use super::*;

    fn task(id: &str) -> Result<TaskEnvelope, Box<dyn std::error::Error>> {
        Ok(TaskEnvelope {
            schema_version: agent_core::PROTOCOL_VERSION,
            task_id: TaskId::parse(id)?,
            session_id: SessionId::parse("session-checkpoint-binding")?,
            parent_task_id: None,
            profile: ProfileId::creator(),
            track: None,
            objective: "bind this exact task".to_owned(),
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
    fn checkpoint_schema_v3_rejects_previous_version() -> Result<(), Box<dyn std::error::Error>> {
        let owner = task("task-checkpoint-schema-v3")?;
        let mut machine = SessionMachine::new(owner.budget.clone());
        machine.state = TaskState::Suspended;
        let checkpoint = SessionCheckpoint::new(
            &owner,
            "a".repeat(64),
            "b".repeat(64),
            "c".repeat(64),
            None,
            BTreeSet::new(),
            vec![ModelMessage::text(MessageRole::User, "resume")],
            machine,
        )?;
        let encoded = checkpoint.encode()?;
        let mut value: serde_json::Value = serde_json::from_str(&encoded)?;
        assert_eq!(value["schemaVersion"], serde_json::json!(3));
        value["schemaVersion"] = serde_json::json!(2);
        let previous = serde_json::to_vec(&value)?;
        assert!(SessionCheckpoint::decode(&previous).is_err());
        Ok(())
    }

    #[test]
    fn checkpoint_cannot_be_substituted_across_tasks() -> Result<(), Box<dyn std::error::Error>> {
        let owner = task("task-checkpoint-owner")?;
        let mut machine = SessionMachine::new(owner.budget.clone());
        machine.state = TaskState::Suspended;
        let checkpoint = SessionCheckpoint::new(
            &owner,
            "a".repeat(64),
            "b".repeat(64),
            "c".repeat(64),
            None,
            BTreeSet::new(),
            vec![ModelMessage::text(MessageRole::User, "resume")],
            machine,
        )?;

        assert!(checkpoint.validate_task(&owner).is_ok());
        assert!(
            checkpoint
                .validate_task(&task("task-checkpoint-other")?)
                .is_err()
        );
        Ok(())
    }

    #[test]
    fn checkpoint_binds_the_composition_digest_before_resume()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut owner = task("task-checkpoint-composition")?;
        owner.context.insert(
            "_upagent_composition_digest".to_owned(),
            serde_json::Value::String("a".repeat(64)),
        );
        let mut machine = SessionMachine::new(owner.budget.clone());
        machine.state = TaskState::Suspended;
        let checkpoint = SessionCheckpoint::new(
            &owner,
            "a".repeat(64),
            "b".repeat(64),
            "c".repeat(64),
            Some("a".repeat(64)),
            BTreeSet::new(),
            vec![ModelMessage::text(MessageRole::User, "resume")],
            machine,
        )?;
        assert!(checkpoint.validate_task(&owner).is_ok());
        owner.context.insert(
            "_upagent_composition_digest".to_owned(),
            serde_json::Value::String("d".repeat(64)),
        );
        assert!(checkpoint.validate_task(&owner).is_err());
        Ok(())
    }

    #[test]
    fn checkpoint_rejects_exact_image_specification_drift() -> Result<(), Box<dyn std::error::Error>>
    {
        let mut owner = task("task-checkpoint-image-spec")?;
        let digest = "e".repeat(64);
        let before_artifact = ArtifactRef {
            id: ArtifactId::parse(format!("art-{digest}"))?,
            sha256: digest.clone(),
            media_type: "image/png".to_owned(),
            byte_length: 1,
            relative_path: format!("ee/{digest}.png"),
        };
        owner.image_task = Some(ImageTaskContract {
            schema: IMAGE_TASK_CONTRACT_SCHEMA.to_owned(),
            goal: "preserve".to_owned(),
            source: ImageSource::Provided {
                artifact: before_artifact.clone(),
            },
            maximum_attempts: 4,
            preserve: Vec::new(),
            visual_criteria: vec!["review".to_owned()],
            verification: ImageVerificationContract {
                visual_review: true,
                deterministic: Some(ImageVerificationSpec {
                    schema: IMAGE_VERIFICATION_SPEC_SCHEMA.to_owned(),
                    before_artifact,
                    after_kind: ImageCompareArtifactKind::Raster,
                    assertions: vec![ImageCompareAssertion::MaximumChannelError {
                        id: "channel_error".to_owned(),
                        severity: ImageCompareSeverity::Required,
                        maximum: 0,
                    }],
                }),
            },
        });
        let mut machine = SessionMachine::new(owner.budget.clone());
        machine
            .image_director
            .bind_contract(owner.image_task.as_ref())?;
        let candidate_digest = "f".repeat(64);
        let candidate = ArtifactRef {
            id: ArtifactId::parse(format!("art-{candidate_digest}"))?,
            sha256: candidate_digest.clone(),
            media_type: "image/png".to_owned(),
            byte_length: 1,
            relative_path: format!("ff/{candidate_digest}.png"),
        };
        machine
            .image_director
            .record_candidate_with_kinds([(candidate, ImageCompareArtifactKind::Raster)], true)?;
        machine.state = TaskState::Suspended;
        let checkpoint = SessionCheckpoint::new(
            &owner,
            "a".repeat(64),
            "b".repeat(64),
            "c".repeat(64),
            None,
            BTreeSet::new(),
            vec![ModelMessage::text(MessageRole::User, "resume")],
            machine,
        )?;
        assert!(checkpoint.validate_task(&owner).is_ok());
        let encoded = checkpoint.encode()?;
        let resumed = SessionCheckpoint::decode(encoded.as_bytes())?;
        assert_eq!(
            resumed.machine.image_director,
            checkpoint.machine.image_director
        );

        let mut changed = owner.clone();
        changed
            .image_task
            .as_mut()
            .and_then(|contract| contract.verification.deterministic.as_mut())
            .expect("verification specification")
            .assertions[0] = ImageCompareAssertion::MaximumChannelError {
            id: "channel_error".to_owned(),
            severity: ImageCompareSeverity::Required,
            maximum: 1,
        };
        assert!(checkpoint.validate_task(&changed).is_err());
        Ok(())
    }
}
