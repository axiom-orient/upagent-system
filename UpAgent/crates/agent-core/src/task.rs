use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    AgentFailure, ProfileId, ReceiptId, ReviewSubjectBundleRef, SessionId, SkillId, TaskId,
    TaskState, ToolCallId, TrackId,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "snake_case")]
pub enum EffectApprovalMode {
    #[default]
    Deny,
    PlanOnly,
    TaskGrant,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct TaskBudget {
    pub maximum_transitions: u32,
    pub maximum_model_calls: u32,
    pub maximum_tool_calls: u32,
    pub maximum_output_bytes: usize,
    pub timeout_ms: u64,
}

impl Default for TaskBudget {
    fn default() -> Self {
        Self {
            maximum_transitions: 128,
            maximum_model_calls: 12,
            maximum_tool_calls: 32,
            maximum_output_bytes: 8 * 1024 * 1024,
            timeout_ms: 15 * 60 * 1000,
        }
    }
}

impl TaskBudget {
    pub fn validate(&self) -> Result<(), String> {
        if self.maximum_transitions == 0
            || self.maximum_transitions > 1_024
            || self.maximum_model_calls == 0
            || self.maximum_model_calls > 64
            || self.maximum_tool_calls > 256
            || self.maximum_output_bytes < 1024
            || self.maximum_output_bytes > 64 * 1024 * 1024
            || self.timeout_ms < 1000
            || self.timeout_ms > 24 * 60 * 60 * 1000
        {
            return Err("task budget is outside supported bounds".to_owned());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ConversationRole {
    User,
    Assistant,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ConversationTurn {
    pub role: ConversationRole,
    pub content: String,
}

pub(crate) fn validate_history_contents(history: &[ConversationTurn]) -> Result<(), String> {
    let mut history_bytes = 0_usize;
    for turn in history {
        if turn.content.trim().is_empty() || turn.content.len() > 256 * 1024 {
            return Err(
                "conversation history turns must be non-empty and at most 256 KiB".to_owned(),
            );
        }
        history_bytes = history_bytes
            .checked_add(turn.content.len())
            .ok_or_else(|| "conversation history byte length overflowed".to_owned())?;
    }
    if history_bytes > 8 * 1024 * 1024 {
        return Err("conversation history exceeds the 8 MiB bound".to_owned());
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct TaskEnvelope {
    pub schema_version: u32,
    pub task_id: TaskId,
    pub session_id: SessionId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_task_id: Option<TaskId>,
    pub profile: ProfileId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub track: Option<TrackId>,
    pub objective: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_task: Option<crate::ImageTaskContract>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub history: Vec<ConversationTurn>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<String>,
    #[serde(default)]
    pub skills: Vec<SkillId>,
    #[serde(default)]
    pub context: BTreeMap<String, Value>,
    #[serde(default)]
    pub budget: TaskBudget,
    #[serde(default)]
    pub effect_approval: EffectApprovalMode,
    #[serde(default)]
    pub allow_memory_write: bool,
    /// Operator decisions redeemed by this run.
    ///
    /// Each grant is consumed once at task start, so a decision authorises
    /// exactly one run of the operation it was recorded against.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub approvals: Vec<ApprovalGrant>,
}

impl TaskEnvelope {
    pub fn validate(&self) -> Result<(), String> {
        if self.schema_version != crate::PROTOCOL_VERSION {
            return Err(format!(
                "unsupported schema version {}; expected {}",
                self.schema_version,
                crate::PROTOCOL_VERSION
            ));
        }
        if self.objective.trim().is_empty() || self.objective.len() > 256 * 1024 {
            return Err("task objective must be 1..=262144 bytes".to_owned());
        }
        if let Some(image_task) = &self.image_task {
            image_task.validate()?;
        }
        if self.history.len() > 128 {
            return Err("conversation history may contain at most 128 turns".to_owned());
        }
        validate_history_contents(&self.history)?;
        if self.skills.len() > 32 {
            return Err("task may request at most 32 skills".to_owned());
        }
        if self.parent_task_id.as_ref() == Some(&self.task_id) {
            return Err("task cannot be its own parent".to_owned());
        }
        if let Some(workspace) = self.workspace.as_deref()
            && (workspace.is_empty()
                || workspace.len() > 4_096
                || workspace.contains('\0')
                || workspace.chars().any(char::is_control))
        {
            return Err("workspace must be a bounded printable path".to_owned());
        }
        if self.context.len() > 128 {
            return Err("task context may contain at most 128 entries".to_owned());
        }
        for key in self.context.keys() {
            if key.is_empty()
                || key.len() > 256
                || key.contains('\0')
                || key.chars().any(char::is_control)
            {
                return Err("task context keys must be bounded printable values".to_owned());
            }
        }
        if self.approvals.len() > 16 {
            return Err("task may redeem at most 16 approvals".to_owned());
        }
        let mut unique_subjects = std::collections::BTreeSet::new();
        for grant in &self.approvals {
            grant.validate()?;
            if !unique_subjects.insert(grant.subject.as_str()) {
                return Err("task contains duplicate approval subjects".to_owned());
            }
        }
        let context_bytes = serde_json::to_vec(&self.context)
            .map_err(|error| format!("task context is not serializable: {error}"))?;
        if context_bytes.len() > 256 * 1024 {
            return Err("task context exceeds the 256 KiB bound".to_owned());
        }
        self.budget.validate()
    }
}

pub use agent_contract::provider::ArtifactRef;

/// One durable use of an artifact by a typed kernel event.  The sequence is
/// the journal sequence that carried the reference; `call_id` is present for
/// tool-bound evidence.  This is deliberately data-only: it cannot authorize
/// a provider, tool, or effect.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ArtifactUse {
    pub sequence: u64,
    pub event: String,
    /// Model request/response artifacts carry the one-based model call index.
    /// Tool artifacts use `call_id` instead; other event artifacts leave both
    /// identity fields empty.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub call_index: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub call_id: Option<ToolCallId>,
}

impl ArtifactUse {
    pub fn validate(&self) -> Result<(), String> {
        if self.sequence == 0
            || self.event.is_empty()
            || self.event.len() > 64
            || !self
                .event
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
        {
            return Err("artifact provenance use has an invalid sequence or event".to_owned());
        }
        if self.call_index == Some(0) || (self.call_index.is_some() && self.call_id.is_some()) {
            return Err("artifact provenance use has an invalid call identity".to_owned());
        }
        Ok(())
    }
}

/// Artifact plus its complete, bounded provenance.  The result projection is
/// content-deduplicated, while uses are retained individually.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ArtifactEvidence {
    pub artifact: ArtifactRef,
    pub uses: Vec<ArtifactUse>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct EffectReceiptRef {
    pub id: ReceiptId,
    pub digest: String,
    pub operation: String,
    pub verified: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ApprovalRequest {
    pub plan_id: String,
    pub fingerprint: String,
    /// Stable identity of the decision, independent of this run.
    ///
    /// `fingerprint` seals one plan instance and changes on every run. The
    /// subject covers only the workspace, operation, effect classes, and state
    /// preconditions, so an approval recorded against it can be redeemed by a
    /// later run that requests the same operation against the same state.
    pub subject: String,
    pub expires_at: String,
    pub effects: Vec<String>,
    pub operation: String,
}

/// One redeemed operator decision, supplied by the caller on a later run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ApprovalGrant {
    pub subject: String,
    pub token: String,
}

impl ApprovalGrant {
    pub fn validate(&self) -> Result<(), String> {
        if self.subject.len() != 64 || !self.subject.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err("approval subject must be a SHA-256 hex digest".to_owned());
        }
        if self.token.len() != 64 || !self.token.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err("approval token must be a 64-character hex value".to_owned());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AgentStatus {
    Succeeded,
    Suspended,
    AwaitingInput,
    AwaitingApproval,
    Failed,
    Cancelled,
    ResolutionRequired,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct AgentResult {
    pub schema_version: u32,
    pub task_id: TaskId,
    pub session_id: SessionId,
    pub profile: ProfileId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub track: Option<TrackId>,
    pub state: TaskState,
    pub status: AgentStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
    #[serde(default)]
    pub artifacts: Vec<ArtifactRef>,
    #[serde(default)]
    pub receipts: Vec<EffectReceiptRef>,
    /// Exact composition proof used for this terminal result. Internal test
    /// harness results may omit it; normal `AgentSpec` runs must set it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub composition_digest: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review_subject_bundle: Option<ReviewSubjectBundleRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approval: Option<ApprovalRequest>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure: Option<AgentFailure>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub metrics: BTreeMap<String, Value>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request() -> Result<TaskEnvelope, Box<dyn std::error::Error>> {
        Ok(TaskEnvelope {
            schema_version: crate::PROTOCOL_VERSION,
            task_id: TaskId::parse("task-test")?,
            session_id: SessionId::parse("session-test")?,
            parent_task_id: None,
            profile: ProfileId::creator(),
            track: None,
            objective: "write".to_owned(),
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
    fn task_rejects_self_parenting_and_oversized_model_messages()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut value = request()?;
        value.parent_task_id = Some(value.task_id.clone());
        assert!(value.validate().is_err());

        let mut value = request()?;
        value.objective = "x".repeat(256 * 1024 + 1);
        assert!(value.validate().is_err());

        let mut value = request()?;
        value.history.push(ConversationTurn {
            role: ConversationRole::User,
            content: "x".repeat(256 * 1024 + 1),
        });
        assert!(value.validate().is_err());
        Ok(())
    }
}
