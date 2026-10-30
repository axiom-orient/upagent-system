use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
    time::Duration,
};

use agent_core::{
    AgentFailure, ApprovalRequest, CapabilityId, EffectReceiptRef, FailureCode, ProfileId,
    ReasoningEffort, SemanticMaskOperation, SkillId, TaskEnvelope, TaskId, ToolCall,
    ToolDefinition, ToolResult, TrackId,
};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use thiserror::Error;
use time::{Duration as TimeDuration, OffsetDateTime, format_description::well_known::Rfc3339};

use crate::ArtifactReader;
use crate::plugin::{PluginOrigin, PluginToolModule};

const MAXIMUM_EXTENSION_TOOLS: usize = 96;
pub(crate) const MAXIMUM_EXTENSION_RECEIPTS: usize = 32;
pub(crate) const MAXIMUM_EXTENSION_RESULT_BYTES: usize = 4 * 1024 * 1024;
const EXTENSION_CANCELLATION_DRAIN_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolSideEffect {
    ReadOnly,
    /// May change state outside the workspace or in another authority domain.
    External,
}

impl ToolSideEffect {
    #[must_use]
    pub const fn requires_approval(self) -> bool {
        !matches!(self, Self::ReadOnly)
    }

    #[must_use]
    pub const fn recovery_required_after_record_failure(self) -> bool {
        !matches!(self, Self::ReadOnly)
    }

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ReadOnly => "read_only",
            Self::External => "external",
        }
    }
}

/// One compile-time registered tool exposed by a trusted module.
///
/// A tool is visible only when the active profile is explicitly listed, every
/// required capability is present, and at least one activation skill is loaded
/// when `activation_skills` is non-empty. This keeps extension discovery
/// bounded and prevents registered modules from silently widening all profiles.
#[derive(Debug, Clone)]
pub struct ModuleTool {
    pub definition: ToolDefinition,
    pub side_effect: ToolSideEffect,
    pub allowed_profiles: BTreeSet<ProfileId>,
    pub required_capabilities: BTreeSet<CapabilityId>,
    pub activation_skills: BTreeSet<SkillId>,
}

#[derive(Debug, Clone)]
pub struct ToolContext {
    pub task_id: TaskId,
    pub profile: ProfileId,
    pub track: Option<TrackId>,
    /// Reasoning selected by the prepared profile plan for this task. Tool
    /// modules forward it unchanged when their provider contract supports it.
    pub reasoning: ReasoningEffort,
    pub timeout_ms: u64,
    /// Cooperative cancellation owned by this one module execution. Modules
    /// must stop child work and finish cleanup before returning.
    pub cancellation: tokio_util::sync::CancellationToken,
    /// The runtime's single content-addressed artifact authority. Modules may
    /// read validated references from this handle, but do not receive its
    /// filesystem root and cannot create a second store.
    pub artifacts: ArtifactReader,
    /// The task state's exact semantic-mask operation, when this call was
    /// admitted as an image extraction effect. Modules must echo this
    /// identity in their result; they may not derive a second revision.
    pub semantic_mask_operation: Option<SemanticMaskOperation>,
}

#[derive(Debug, Clone)]
pub struct ModuleArtifact {
    pub bytes: Vec<u8>,
    pub media_type: String,
    pub extension: String,
}

#[derive(Debug, Clone)]
pub struct ModuleExecution {
    pub result: ToolResult,
    pub receipts: Vec<EffectReceiptRef>,
    pub artifacts: Vec<ModuleArtifact>,
}

#[derive(Debug, Error)]
pub enum ToolModuleError {
    #[error("tool module contract failed: {0}")]
    Contract(String),
    /// Execution failures must mean that no durable effect occurred. A module
    /// that may have changed external state must instead return a
    /// `ModuleExecution` with an error result and an evidence receipt.
    #[error("tool module execution failed: {0}")]
    Failure(AgentFailure),
    /// The runtime cancelled the module future at the task deadline. For a
    /// side-effecting tool this is an uncertain outcome and requires manual
    /// resolution because the external operation may already have started.
    #[error("tool module execution timed out: {0}")]
    TimedOut(AgentFailure),
    /// The caller explicitly cancelled the task. A side-effecting tool still
    /// requires resolution because dropping its future cannot prove that an
    /// external operation did not begin.
    #[error("tool module execution was cancelled: {0}")]
    Cancelled(AgentFailure),
}

#[async_trait]
pub trait ToolModule: Send + Sync + std::fmt::Debug {
    fn tools(&self) -> Vec<ModuleTool>;

    async fn execute(
        &self,
        context: ToolContext,
        call: ToolCall,
    ) -> Result<ModuleExecution, ToolModuleError>;
}

#[derive(Clone)]
pub(crate) struct RegisteredTool {
    pub module: Arc<dyn ToolModule>,
    pub origin: PluginOrigin,
    pub definition: ToolDefinition,
    pub side_effect: ToolSideEffect,
    pub allowed_profiles: BTreeSet<ProfileId>,
    pub required_capabilities: BTreeSet<CapabilityId>,
    pub activation_skills: BTreeSet<SkillId>,
}

impl std::fmt::Debug for RegisteredTool {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RegisteredTool")
            .field("name", &self.definition.name)
            .field("origin", &self.origin)
            .field("side_effect", &self.side_effect)
            .field("allowed_profiles", &self.allowed_profiles)
            .field("required_capabilities", &self.required_capabilities)
            .field("activation_skills", &self.activation_skills)
            .finish_non_exhaustive()
    }
}

impl RegisteredTool {
    pub(crate) fn active_for(
        &self,
        profile: &agent_core::ProfileManifest,
        loaded_skills: &BTreeSet<SkillId>,
    ) -> bool {
        self.allowed_profiles.contains(&profile.id)
            && self.required_capabilities.is_subset(&profile.capabilities)
            && (self.activation_skills.is_empty()
                || self
                    .activation_skills
                    .iter()
                    .any(|skill| loaded_skills.contains(skill)))
    }
}

pub(crate) fn register_modules(
    modules: Vec<PluginToolModule>,
) -> Result<BTreeMap<String, RegisteredTool>, String> {
    let mut output = BTreeMap::new();
    for contribution in modules {
        let module = contribution.module;
        let origin = contribution.origin;
        let tools = module.tools();
        if tools.is_empty() {
            return Err("tool module exposes no tools".to_owned());
        }
        for tool in tools {
            if output.len() >= MAXIMUM_EXTENSION_TOOLS {
                return Err(format!(
                    "extension tool catalog exceeds {MAXIMUM_EXTENSION_TOOLS} tools"
                ));
            }
            validate_tool(&tool)?;
            let name = tool.definition.name.clone();
            if output
                .insert(
                    name.clone(),
                    RegisteredTool {
                        module: Arc::clone(&module),
                        origin: origin.clone(),
                        definition: tool.definition,
                        side_effect: tool.side_effect,
                        allowed_profiles: tool.allowed_profiles,
                        required_capabilities: tool.required_capabilities,
                        activation_skills: tool.activation_skills,
                    },
                )
                .is_some()
            {
                return Err(format!("duplicate extension tool {name}"));
            }
        }
    }
    Ok(output)
}

fn validate_tool(tool: &ModuleTool) -> Result<(), String> {
    let definition = &tool.definition;
    if definition.name.is_empty()
        || definition.name.len() > 128
        || !definition
            .name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_'))
        || definition.description.trim().is_empty()
        || definition.description.len() > 8_192
        || !definition.input_schema.is_object()
        || !definition.strict
    {
        return Err(format!(
            "extension tool definition {} is invalid",
            definition.name
        ));
    }
    if tool.allowed_profiles.is_empty() {
        return Err(format!(
            "extension tool {} must explicitly allow at least one profile",
            definition.name
        ));
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn execute_extension(
    registered: &RegisteredTool,
    task: &TaskEnvelope,
    call: &ToolCall,
    reasoning: ReasoningEffort,
    timeout: Duration,
    cancellation: &tokio_util::sync::CancellationToken,
    artifacts: ArtifactReader,
    semantic_mask_operation: Option<SemanticMaskOperation>,
) -> Result<ModuleExecution, ToolModuleError> {
    if cancellation.is_cancelled() {
        return Err(ToolModuleError::Failure(AgentFailure::new(
            FailureCode::Cancelled,
            "task was cancelled before the extension tool started",
        )));
    }
    if timeout.is_zero() {
        return Err(ToolModuleError::Failure(AgentFailure::new(
            FailureCode::BudgetExceeded,
            "task deadline elapsed before the extension tool started",
        )));
    }
    let timeout_ms = match u64::try_from(timeout.as_millis()) {
        Ok(value) => value.max(1),
        Err(_) => u64::MAX,
    };
    let module_cancellation = tokio_util::sync::CancellationToken::new();
    let context = ToolContext {
        task_id: task.task_id.clone(),
        profile: task.profile.clone(),
        track: task.track.clone(),
        reasoning,
        timeout_ms,
        cancellation: module_cancellation.clone(),
        artifacts,
        semantic_mask_operation,
    };
    let execution = registered.module.execute(context, call.clone());
    tokio::pin!(execution);
    let deadline = tokio::time::sleep(timeout);
    tokio::pin!(deadline);
    let interruption = tokio::select! {
        biased;
        () = cancellation.cancelled() => {
            ToolModuleError::Cancelled(AgentFailure::new(
                FailureCode::Cancelled,
                "extension tool was cancelled",
            ))
        }
        () = &mut deadline => {
            ToolModuleError::TimedOut(AgentFailure::new(
                FailureCode::BudgetExceeded,
                "extension tool exceeded the remaining task deadline",
            ))
        }
        result = &mut execution => {
            let execution = result?;
            validate_extension_execution(registered, call, &execution)?;
            return Ok(execution);
        }
    };
    module_cancellation.cancel();
    match tokio::time::timeout(EXTENSION_CANCELLATION_DRAIN_TIMEOUT, &mut execution).await {
        Ok(Ok(execution)) => {
            validate_extension_execution(registered, call, &execution)?;
            Ok(execution)
        }
        Ok(Err(ToolModuleError::Contract(message))) => Err(ToolModuleError::Contract(message)),
        // The semantic adapter returns Failure only before external dispatch.
        // Do not turn a known local preflight failure into a remote uncertainty
        // merely because the caller's cancellation won the outer select.
        Ok(Err(error @ ToolModuleError::Failure(_)))
            if registered.definition.name == agent_core::IMAGE_EDIT_SEMANTIC_TOOL =>
        {
            Err(error)
        }
        Ok(Err(_)) | Err(_) => Err(interruption),
    }
}

fn validate_extension_execution(
    registered: &RegisteredTool,
    call: &ToolCall,
    execution: &ModuleExecution,
) -> Result<(), ToolModuleError> {
    if execution.result.call_id != call.id || execution.result.name != call.name {
        return Err(ToolModuleError::Contract(format!(
            "extension tool {} returned a result for another call",
            call.name
        )));
    }
    if execution.result.is_error != execution.result.failure.is_some() {
        return Err(ToolModuleError::Contract(format!(
            "extension tool {} returned inconsistent error metadata",
            call.name
        )));
    }
    if execution.receipts.len() > MAXIMUM_EXTENSION_RECEIPTS {
        return Err(ToolModuleError::Contract(format!(
            "extension tool {} returned too many receipts",
            call.name
        )));
    }
    if registered.side_effect.requires_approval() && execution.receipts.is_empty() {
        return Err(ToolModuleError::Contract(format!(
            "side-effecting extension tool {} returned no effect receipt",
            call.name
        )));
    }
    if !registered.side_effect.requires_approval() && !execution.receipts.is_empty() {
        return Err(ToolModuleError::Contract(format!(
            "read-only extension tool {} returned effect receipts",
            call.name
        )));
    }
    if execution.artifacts.len() > 16 {
        return Err(ToolModuleError::Contract(format!(
            "extension tool {} returned too many artifacts",
            call.name
        )));
    }
    let result_bytes = serde_json::to_vec(&execution.result).map_err(|error| {
        ToolModuleError::Contract(format!(
            "extension tool {} returned an unencodable result: {error}",
            call.name
        ))
    })?;
    if result_bytes.len() > MAXIMUM_EXTENSION_RESULT_BYTES {
        return Err(ToolModuleError::Contract(format!(
            "extension tool {} result exceeds the {} byte bound",
            call.name, MAXIMUM_EXTENSION_RESULT_BYTES
        )));
    }
    for receipt in &execution.receipts {
        let digest_is_hex = receipt.digest.len() == 64
            && receipt.digest.bytes().all(|byte| byte.is_ascii_hexdigit());
        if !digest_is_hex || receipt.operation.trim().is_empty() {
            return Err(ToolModuleError::Contract(format!(
                "extension tool {} returned an invalid effect receipt",
                call.name
            )));
        }
    }
    Ok(())
}

fn digest_of(value: &serde_json::Value, label: &str) -> Result<String, AgentFailure> {
    let encoded = serde_json::to_vec(value).map_err(|error| {
        AgentFailure::new(
            FailureCode::InternalInvariant,
            format!("{label} could not be encoded"),
        )
        .with_detail("cause", error.to_string())
    })?;
    Ok(format!("{:x}", Sha256::digest(encoded)))
}

pub(crate) fn extension_approval(
    task: &TaskEnvelope,
    call: &ToolCall,
    side_effect: ToolSideEffect,
) -> Result<ApprovalRequest, AgentFailure> {
    // The subject is what the operator decides about, so it excludes the task
    // and session identifiers: a task identifier is single-use, so a decision
    // bound to one could never be redeemed by the run that follows it. What
    // remains still binds the profile, track, tool, arguments and effect class,
    // so a decision authorises exactly the call that was shown.
    let subject_payload = json!({
        "schemaVersion": agent_core::PROTOCOL_VERSION,
        "profile": task.profile.as_str(),
        "track": task.track.as_ref().map(TrackId::as_str),
        "tool": call.name.as_str(),
        "arguments": &call.arguments,
        "sideEffect": side_effect.as_str(),
    });
    let subject = digest_of(&subject_payload, "extension approval subject")?;
    // The fingerprint additionally seals this one request, mirroring how a RAX
    // plan fingerprint identifies a single plan instance.
    let fingerprint = digest_of(
        &json!({
            "subject": &subject,
            "taskId": task.task_id.as_str(),
            "sessionId": task.session_id.as_str(),
        }),
        "extension approval fingerprint",
    )?;
    let expires_at = OffsetDateTime::now_utc()
        .checked_add(TimeDuration::minutes(15))
        .ok_or_else(|| {
            AgentFailure::new(
                FailureCode::InternalInvariant,
                "extension approval expiration overflowed",
            )
        })?
        .format(&Rfc3339)
        .map_err(|error| {
            AgentFailure::new(
                FailureCode::InternalInvariant,
                "extension approval expiration could not be formatted",
            )
            .with_detail("cause", error.to_string())
        })?;
    Ok(ApprovalRequest {
        plan_id: format!("extension-{fingerprint}"),
        subject,
        fingerprint,
        expires_at,
        effects: vec![side_effect.as_str().to_owned()],
        operation: call.name.clone(),
    })
}
