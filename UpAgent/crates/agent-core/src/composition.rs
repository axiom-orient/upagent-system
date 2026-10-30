//! Explicit, immutable composition contracts for the agent kernel.
//!
//! `AgentSpec` is host-owned agent policy. It selects the profile, workflow,
//! skills, tools, state/effect policies, review behavior, host isolation adapter,
//! and extension artifacts before an agent is started. Concrete provider routes
//! are adapter-owned and enter the immutable receipt only as `ProviderBinding`.

use std::collections::BTreeSet;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    AGENT_IMAGE_BEHAVIOR_VERSION, AGENT_IMAGE_PROTOCOL_SCHEMA, AGENT_IMAGE_PROTOCOL_VERSION,
    ArtifactEvidence, ArtifactRef, SessionId, TaskId, ToolDefinition,
};

/// The public host-owned agent-policy contract. Version five pins the Codex
/// runtime used by an explicitly selected `PromptGen` semantic-edit effect.
pub const AGENT_SPEC_SCHEMA_VERSION: u32 = 5;
pub const COMPOSITION_RECEIPT_SCHEMA_VERSION: u32 = 5;

/// Maximum serialized size of the immutable review subject metadata emitted
/// by the runtime and accepted by the independent review ingress.
pub const MAX_REVIEW_SUBJECT_BUNDLE_BYTES: usize = 128 * 1024;

/// Provider capabilities are adapter-neutral composition facts. Concrete
/// executable paths, credentials, homes, and wire protocols remain owned by
/// the provider adapter outside `agent-core`.
pub const PROVIDER_CAP_TEXT: &str = "model/text";
pub const PROVIDER_CAP_IMAGE_OBSERVATION: &str = "model/image_observation";
pub const PROVIDER_CAP_IMAGE_GENERATION: &str = "model/image_generation";

/// Immutable identity of the provider route selected by the host. The route
/// digest is produced by the concrete adapter from its non-secret execution
/// configuration; `agent-core` never interprets provider-specific settings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ProviderBinding {
    pub id: String,
    pub model: String,
    pub route_sha256: String,
    pub capabilities: BTreeSet<String>,
    pub maximum_output_bytes: usize,
}

impl ProviderBinding {
    pub fn validate(&self) -> Result<(), CompositionError> {
        let id = bounded_text(&self.id, "provider id", 96)?;
        if id != self.id {
            return Err(CompositionError::Invalid(
                "provider id is not in canonical form".to_owned(),
            ));
        }
        let model = bounded_text(&self.model, "provider model", 160)?;
        if model != self.model {
            return Err(CompositionError::Invalid(
                "provider model is not in canonical form".to_owned(),
            ));
        }
        validate_digest(&self.route_sha256, "provider routeSha256")?;
        if self.capabilities.is_empty() || self.capabilities.len() > 32 {
            return Err(CompositionError::Invalid(
                "provider capabilities must contain 1..=32 entries".to_owned(),
            ));
        }
        for capability in &self.capabilities {
            let normalized = bounded_text(capability, "provider capability", 128)?;
            if normalized != *capability {
                return Err(CompositionError::Invalid(
                    "provider capability is not in canonical form".to_owned(),
                ));
            }
        }
        if self.maximum_output_bytes == 0 || self.maximum_output_bytes > 64 * 1024 * 1024 {
            return Err(CompositionError::Invalid(
                "provider maximumOutputBytes must be in 1..=67108864".to_owned(),
            ));
        }
        Ok(())
    }

    #[must_use]
    pub fn supports(&self, capability: &str) -> bool {
        self.capabilities.contains(capability)
    }
}

/// `PerfectPixel` identity bound by an image-capable composition. The runtime
/// verifies the executable and canonical capability manifest before every
/// image operation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ImageSpec {
    pub perfectpixel_executable: std::path::PathBuf,
    pub perfectpixel_sha256: String,
    pub protocol_schema: String,
    pub protocol_version: String,
    pub behavior_version: String,
    pub capability_manifest_sha256: String,
    pub implementation_version: String,
}

impl ImageSpec {
    fn validate(&self) -> Result<(), CompositionError> {
        validate_absolute_path(&self.perfectpixel_executable, "perfectpixelExecutable")?;
        validate_digest(&self.perfectpixel_sha256, "perfectpixelSha256")?;
        validate_digest(&self.capability_manifest_sha256, "capabilityManifestSha256")?;
        bounded_text(&self.implementation_version, "implementationVersion", 64)?;
        if self.protocol_schema != AGENT_IMAGE_PROTOCOL_SCHEMA
            || self.protocol_version != AGENT_IMAGE_PROTOCOL_VERSION
            || self.behavior_version != AGENT_IMAGE_BEHAVIOR_VERSION
        {
            return Err(CompositionError::Invalid(
                "PerfectPixel agent protocol identity is unsupported".to_owned(),
            ));
        }
        Ok(())
    }
}

/// Optional host-selected process adapter for semantic natural-image masks.
/// This is configuration data only; composition resolution pins the file
/// identity before a receipt is created and the runtime rechecks it before
/// every invocation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct SemanticMaskSpec {
    pub executable: std::path::PathBuf,
    pub executable_sha256: String,
    pub helper_identity: String,
    pub helper_version: String,
    pub timeout_ms: u64,
}

pub const SEMANTIC_MASK_HELPER_MIN_TIMEOUT_MS: u64 = 1;
pub const SEMANTIC_MASK_HELPER_MAX_TIMEOUT_MS: u64 = 600_000;

impl SemanticMaskSpec {
    fn resolve(&self) -> Result<Self, CompositionError> {
        let mut resolved = self.clone();
        validate_absolute_path(&resolved.executable, "semanticMask.executable")?;
        validate_digest(&resolved.executable_sha256, "semanticMask.executableSha256")?;
        resolved.executable_sha256 = resolved.executable_sha256.to_ascii_lowercase();
        resolved.helper_identity = bounded_text(
            &resolved.helper_identity,
            "semanticMask.helperIdentity",
            256,
        )?;
        resolved.helper_version =
            bounded_text(&resolved.helper_version, "semanticMask.helperVersion", 256)?;
        if !(SEMANTIC_MASK_HELPER_MIN_TIMEOUT_MS..=SEMANTIC_MASK_HELPER_MAX_TIMEOUT_MS)
            .contains(&resolved.timeout_ms)
        {
            return Err(CompositionError::Invalid(format!(
                "semanticMask.timeoutMs must be in {SEMANTIC_MASK_HELPER_MIN_TIMEOUT_MS}..={SEMANTIC_MASK_HELPER_MAX_TIMEOUT_MS}"
            )));
        }
        Ok(resolved)
    }
}

/// Explicit Codex runtime used by `PromptGen` semantic edits. The path is host
/// configuration; authentication state remains in the caller-owned home.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct PromptGenCodexRuntimeSpec {
    pub executable: std::path::PathBuf,
    pub executable_sha256: String,
    pub home: std::path::PathBuf,
}

impl PromptGenCodexRuntimeSpec {
    fn resolve(&self) -> Result<Self, CompositionError> {
        validate_absolute_path(&self.executable, "promptgen.codexRuntime.executable")?;
        validate_digest(
            &self.executable_sha256,
            "promptgen.codexRuntime.executableSha256",
        )?;
        validate_absolute_path(&self.home, "promptgen.codexRuntime.home")?;
        let mut resolved = self.clone();
        resolved.executable_sha256 = resolved.executable_sha256.to_ascii_lowercase();
        Ok(resolved)
    }
}

/// Pinned optional prompt compiler/edit mechanism. Configuration is not activation.
/// No provider, candidate, or session state is owned by this value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct PromptGenSpec {
    pub executable: std::path::PathBuf,
    pub executable_sha256: String,
    pub timeout_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub codex_runtime: Option<PromptGenCodexRuntimeSpec>,
}

pub const PROMPTGEN_MAX_TIMEOUT_MS: u64 = 600_000;

impl PromptGenSpec {
    fn resolve(&self) -> Result<Self, CompositionError> {
        validate_absolute_path(&self.executable, "promptgen.executable")?;
        validate_digest(&self.executable_sha256, "promptgen.executableSha256")?;
        if !(1..=PROMPTGEN_MAX_TIMEOUT_MS).contains(&self.timeout_ms) {
            return Err(CompositionError::Invalid(
                "promptgen.timeoutMs is outside 1..=600000".to_owned(),
            ));
        }
        let mut resolved = self.clone();
        resolved.executable_sha256 = resolved.executable_sha256.to_ascii_lowercase();
        resolved.codex_runtime = resolved
            .codex_runtime
            .as_ref()
            .map(PromptGenCodexRuntimeSpec::resolve)
            .transpose()?;
        Ok(resolved)
    }
}

/// Memory authority selected for one agent instance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "snake_case")]
pub enum MemorySpec {
    #[default]
    None,
    Session,
    Project,
    LongTermScoped,
}

/// Effect authority selected for one agent instance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "snake_case")]
pub enum EffectsSpec {
    #[default]
    None,
    Rax,
}

/// Approval authority for selected effect tools. It is part of `AgentSpec` so a
/// task request cannot escalate a profile's host-owned permission boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "snake_case")]
pub enum EffectApprovalSpec {
    #[default]
    Deny,
    PlanOnly,
    TaskGrant,
}

/// `ReviewCampaign` is an independent executable/package boundary. The general
/// agent emits a handoff bundle but never executes the campaign.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "snake_case")]
pub enum ReviewSpec {
    #[default]
    None,
    /// On terminal success, emit a content-addressed immutable subject bundle
    /// for the independent `agent-review` package to consume.
    EmitSubjectBundle,
}

/// Isolation is selected by the host environment.  `host_adapter` is an
/// admission requirement, not a claim that the kernel can prove containment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
#[serde(tag = "kind", deny_unknown_fields)]
pub enum IsolationSpec {
    #[default]
    #[serde(rename = "native")]
    Native,
    #[serde(rename = "host_adapter")]
    HostAdapter {
        /// The only supported external adapter in this contract.
        adapter: String,
        /// Digest of the host policy expected by the external wrapper.
        #[serde(rename = "policySha256")]
        policy_sha256: String,
        /// Digest of the adapter binary expected by the external wrapper.
        #[serde(rename = "adapterSha256")]
        adapter_sha256: String,
    },
}

impl IsolationSpec {
    fn validate(&self) -> Result<(), CompositionError> {
        if let Self::HostAdapter {
            adapter,
            policy_sha256,
            adapter_sha256,
        } = self
        {
            if bounded_text(adapter, "isolation adapter", 64)? != "mbox" {
                return Err(CompositionError::Invalid(
                    "only the external mbox host adapter is supported".to_owned(),
                ));
            }
            validate_digest(policy_sha256, "policySha256")?;
            validate_digest(adapter_sha256, "adapterSha256")?;
        }
        Ok(())
    }
}

/// Data-only extension selection.  There is deliberately no process, Wasm, or
/// native loader in this contract.  Both tiers are UTF-8 JSON skill packages;
/// tier two additionally requires an operator-approved exact source digest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "tier", deny_unknown_fields)]
pub enum DataExtensionSpec {
    #[serde(rename = "approved")]
    Approved {
        id: String,
        source: String,
        #[serde(rename = "approvedSha256")]
        approved_sha256: String,
    },
    #[serde(rename = "dynamic")]
    Dynamic { id: String, source: String },
}

impl DataExtensionSpec {
    fn id(&self) -> &str {
        match self {
            Self::Approved { id, .. } | Self::Dynamic { id, .. } => id,
        }
    }

    fn source(&self) -> &str {
        match self {
            Self::Approved { source, .. } | Self::Dynamic { source, .. } => source,
        }
    }

    fn validate(&self) -> Result<(), CompositionError> {
        bounded_text(self.id(), "data extension id", 128)?;
        let source = bounded_text(self.source(), "data extension source", 4_096)?;
        if !std::path::Path::new(&source).is_absolute() {
            return Err(CompositionError::Invalid(
                "data extension source must be an absolute path".to_owned(),
            ));
        }
        if source.contains("/../") || source.ends_with("/..") {
            return Err(CompositionError::Invalid(
                "data extension source must not contain parent traversal".to_owned(),
            ));
        }
        if self.id().starts_with("_upagent") || self.id().starts_with("kernel/") {
            return Err(CompositionError::Invalid(
                "reserved data extension id".to_owned(),
            ));
        }
        if let Self::Approved {
            approved_sha256, ..
        } = self
        {
            validate_digest(approved_sha256, "approvedSha256")?;
        }
        Ok(())
    }
}

/// Plugin trust tiers are observable in the composition receipt.  Only the
/// first tier is executable code; tiers two and three are data packages.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Ord, PartialOrd, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum PluginTier {
    /// Product-owned, immutable Rust code linked into the kernel.
    BuiltIn,
    /// Host-approved exact SHA-256 UTF-8 JSON data package.
    Approved,
    /// Per-compose/run dynamic UTF-8 JSON data package.
    Dynamic,
}

/// Host-owned, strict composition request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct AgentSpec {
    pub schema_version: u32,
    pub profile: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow: Option<String>,
    #[serde(default)]
    pub skills: Vec<String>,
    #[serde(default)]
    pub tools: Vec<String>,
    #[serde(default)]
    pub memory: MemorySpec,
    #[serde(default)]
    pub review: ReviewSpec,
    #[serde(default)]
    pub effects: EffectsSpec,
    #[serde(default)]
    pub effect_approval: EffectApprovalSpec,
    #[serde(default)]
    pub isolation: IsolationSpec,
    /// Explicit tier-2/3 data skill packages. Tier-1 linked kernel plugins
    /// are always receipt-bound and are not represented as a selectable
    /// no-op extension.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extensions: Vec<DataExtensionSpec>,
    /// Explicit `PerfectPixel` identity required whenever image tools are
    /// selected. It is receipt-bound and is not an ambient executable lookup.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<ImageSpec>,
    /// Optional explicitly pinned semantic-mask process effect. Absence is a
    /// deliberate unavailable capability, never an ambient executable lookup.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub semantic_mask: Option<SemanticMaskSpec>,
    /// Optional compiler mechanism, independent of `PerfectPixel`. Selecting a tool
    /// is still mandatory; compile-only compositions need no image provider.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub promptgen: Option<PromptGenSpec>,
}

impl AgentSpec {
    /// Validate and apply only deterministic defaults. No provider, state, or
    /// network access occurs here.
    pub fn resolve(&self) -> Result<Self, CompositionError> {
        if self.schema_version != AGENT_SPEC_SCHEMA_VERSION {
            return Err(CompositionError::Invalid(format!(
                "unsupported AgentSpec schema {}; expected {AGENT_SPEC_SCHEMA_VERSION}",
                self.schema_version
            )));
        }
        let mut resolved = self.clone();
        resolved.profile = bounded_text(&resolved.profile, "profile", 96)?;
        if let Some(workflow) = resolved.workflow.as_mut() {
            *workflow = bounded_text(workflow, "workflow", 96)?;
        }
        if resolved.skills.len() > 64 || resolved.tools.len() > 128 {
            return Err(CompositionError::Invalid(
                "AgentSpec skill/tool selection exceeds its bound".to_owned(),
            ));
        }
        for skill in &mut resolved.skills {
            *skill = bounded_text(skill, "skill", 160)?;
        }
        for tool in &mut resolved.tools {
            *tool = bounded_text(tool, "tool", 160)?;
        }
        if resolved.skills.iter().collect::<BTreeSet<_>>().len() != resolved.skills.len()
            || resolved.tools.iter().collect::<BTreeSet<_>>().len() != resolved.tools.len()
        {
            return Err(CompositionError::Invalid(
                "AgentSpec selections must be unique".to_owned(),
            ));
        }
        // These collections are set-like. Normalize them before hashing so
        // two hosts that selected the same capabilities in a different input
        // order produce the same immutable composition proof.
        resolved.skills.sort();
        resolved.tools.sort();
        if resolved.extensions.len() > 32 {
            return Err(CompositionError::Invalid(
                "AgentSpec may select at most 32 extensions".to_owned(),
            ));
        }
        let mut extension_ids = BTreeSet::new();
        for extension in &mut resolved.extensions {
            extension.validate()?;
            if !extension_ids.insert(extension.id().to_owned()) {
                return Err(CompositionError::Invalid(
                    "AgentSpec extensions must have unique IDs".to_owned(),
                ));
            }
        }
        resolved
            .extensions
            .sort_by(|left, right| left.id().cmp(right.id()));
        resolved.isolation.validate()?;
        resolved.semantic_mask = resolved
            .semantic_mask
            .as_ref()
            .map(SemanticMaskSpec::resolve)
            .transpose()?;
        resolved.promptgen = resolved
            .promptgen
            .as_ref()
            .map(PromptGenSpec::resolve)
            .transpose()?;
        if resolved
            .tools
            .iter()
            .any(|tool| crate::is_promptgen_tool_name(tool))
            && resolved.promptgen.is_none()
        {
            return Err(CompositionError::Invalid(
                "selected promptGen tool requires an explicit promptgen identity".to_owned(),
            ));
        }
        if resolved
            .tools
            .iter()
            .any(|tool| tool == crate::IMAGE_EDIT_SEMANTIC_TOOL)
            && resolved
                .promptgen
                .as_ref()
                .is_none_or(|promptgen| promptgen.codex_runtime.is_none())
        {
            return Err(CompositionError::Invalid(
                "semantic editing requires an explicit pinned PromptGen Codex runtime".to_owned(),
            ));
        }
        let selected_image_tools = resolved
            .tools
            .iter()
            .any(|tool| crate::is_image_tool_name(tool));
        if resolved
            .tools
            .iter()
            .any(|tool| crate::is_external_image_tool_name(tool))
            && resolved.effect_approval == EffectApprovalSpec::Deny
        {
            return Err(CompositionError::Invalid(
                "external image tools require PlanOnly or TaskGrant effect approval".to_owned(),
            ));
        }
        match (selected_image_tools, resolved.image.as_ref()) {
            (true, Some(image)) => image.validate()?,
            (true, None) => {
                return Err(CompositionError::Invalid(
                    "image tools require an explicit image PerfectPixel identity".to_owned(),
                ));
            }
            (false, Some(_)) => {
                return Err(CompositionError::Invalid(
                    "image identity is only valid when image tools are selected".to_owned(),
                ));
            }
            (false, None) => {}
        }
        if resolved.semantic_mask.is_some()
            && !resolved
                .tools
                .iter()
                .any(|tool| tool == crate::IMAGE_EXTRACT_TOOL)
        {
            return Err(CompositionError::Invalid(
                "semanticMask config requires image_extract in AgentSpec.tools".to_owned(),
            ));
        }
        Ok(resolved)
    }

    /// Validate only the adapter-neutral provider capabilities required by this
    /// resolved agent policy. Concrete protocol names are intentionally absent.
    pub fn validate_provider(&self, provider: &ProviderBinding) -> Result<(), CompositionError> {
        let resolved = self.resolve()?;
        provider.validate()?;
        if !provider.supports(PROVIDER_CAP_TEXT) {
            return Err(CompositionError::Invalid(
                "selected provider does not expose model/text".to_owned(),
            ));
        }
        let selected_image_tools = resolved
            .tools
            .iter()
            .any(|tool| crate::is_image_tool_name(tool));
        if selected_image_tools && !provider.supports(PROVIDER_CAP_IMAGE_OBSERVATION) {
            return Err(CompositionError::Invalid(
                "image tools require provider capability model/image_observation".to_owned(),
            ));
        }
        if resolved
            .tools
            .iter()
            .any(|tool| tool == crate::IMAGE_GENERATE_TOOL)
            && !provider.supports(PROVIDER_CAP_IMAGE_GENERATION)
        {
            return Err(CompositionError::Invalid(
                "image_generate requires provider capability model/image_generation".to_owned(),
            ));
        }
        Ok(())
    }

    /// Stable digest of the resolved agent policy. Provider route identity is
    /// deliberately excluded and is sealed separately by `CompositionReceipt`.
    pub fn digest(&self) -> Result<String, CompositionError> {
        let resolved = self.resolve()?;
        digest_json("upagent-agent-spec-v5", &resolved)
    }
}

/// Task-only v3 ingress. Authority belongs to `AgentSpec`, not this value.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct TaskEnvelopeV3 {
    pub schema_version: u32,
    pub task_id: TaskId,
    pub session_id: SessionId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_task_id: Option<TaskId>,
    pub objective: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_task: Option<crate::ImageTaskContract>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub history: Vec<crate::ConversationTurn>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<String>,
    #[serde(default)]
    pub context: serde_json::Map<String, serde_json::Value>,
    #[serde(default)]
    pub budget: crate::TaskBudget,
    /// One-time operator grants remain task data; the effect authority that
    /// makes them meaningful belongs to `AgentSpec`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub approvals: Vec<crate::ApprovalGrant>,
}

impl TaskEnvelopeV3 {
    pub fn validate(&self) -> Result<(), CompositionError> {
        if self.schema_version != crate::TASK_ENVELOPE_V3_VERSION {
            return Err(CompositionError::Invalid(format!(
                "unsupported TaskEnvelope v3 schema {}; expected {}",
                self.schema_version,
                crate::TASK_ENVELOPE_V3_VERSION
            )));
        }
        if self.objective.trim().is_empty() || self.objective.len() > 256 * 1024 {
            return Err(CompositionError::Invalid(
                "task objective must be 1..=262144 bytes".to_owned(),
            ));
        }
        if let Some(image_task) = &self.image_task {
            image_task.validate().map_err(CompositionError::Invalid)?;
        }
        if self.parent_task_id.as_ref() == Some(&self.task_id) {
            return Err(CompositionError::Invalid(
                "task cannot be its own parent".to_owned(),
            ));
        }
        if self.history.len() > 128 || self.context.len() > 128 {
            return Err(CompositionError::Invalid(
                "task history/context exceeds its bound".to_owned(),
            ));
        }
        crate::task::validate_history_contents(&self.history).map_err(CompositionError::Invalid)?;
        for key in self.context.keys() {
            if key.is_empty()
                || key.len() > 256
                || key.contains('\0')
                || key.chars().any(char::is_control)
            {
                return Err(CompositionError::Invalid(
                    "task context keys must be bounded printable values".to_owned(),
                ));
            }
            if key.starts_with("_upagent_") {
                return Err(CompositionError::Invalid(
                    "task context cannot provide reserved composition authority".to_owned(),
                ));
            }
        }
        let context_bytes = serde_json::to_vec(&self.context).map_err(|error| {
            CompositionError::Invalid(format!("task context is not serializable: {error}"))
        })?;
        if context_bytes.len() > 256 * 1024 {
            return Err(CompositionError::Invalid(
                "task context exceeds the 256 KiB bound".to_owned(),
            ));
        }
        if self.approvals.len() > 16 {
            return Err(CompositionError::Invalid(
                "task may redeem at most 16 approvals".to_owned(),
            ));
        }
        let mut subjects = BTreeSet::new();
        for grant in &self.approvals {
            grant.validate().map_err(CompositionError::Invalid)?;
            if !subjects.insert(grant.subject.clone()) {
                return Err(CompositionError::Invalid(
                    "task contains duplicate approval subjects".to_owned(),
                ));
            }
        }
        if let Some(workspace) = self.workspace.as_deref()
            && (workspace.is_empty()
                || workspace.len() > 4_096
                || workspace.contains('\0')
                || workspace.chars().any(char::is_control))
        {
            return Err(CompositionError::Invalid(
                "workspace must be a bounded printable path".to_owned(),
            ));
        }
        self.budget.validate().map_err(CompositionError::Invalid)
    }

    pub fn digest(&self) -> Result<String, CompositionError> {
        self.validate()?;
        digest_json("upagent-task-envelope-v3", self)
    }
}

/// One immutable proof of the exact selected composition. This is safe to
/// persist and inspect; it contains no credential value.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct CompositionReceipt {
    pub schema_version: u32,
    pub task_id: TaskId,
    pub session_id: SessionId,
    pub task_digest: String,
    pub spec_digest: String,
    pub provider: ProviderBinding,
    pub spec: AgentSpec,
    pub selected_skills: Vec<SelectedSkillBinding>,
    pub selected_tools: Vec<SelectedToolBinding>,
    pub plugin_tiers: Vec<PluginTierBinding>,
    /// Exact linked provider plugin selected by the composition envelope.
    pub selected_provider_plugin: SelectedPluginBinding,
    /// Exact linked profile plugin that owns the resolved profile.
    pub selected_profile_plugin: SelectedPluginBinding,
    pub digest: String,
}

/// Immutable, content-addressed subject metadata emitted by a normal agent
/// only when `review: emit_subject_bundle` was selected.  The independent
/// review package receives this artifact; the general runtime never executes
/// `ReviewCampaign` itself.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ReviewSubjectBundle {
    pub schema_version: u32,
    pub task_id: TaskId,
    pub session_id: SessionId,
    pub task_digest: String,
    pub composition_digest: String,
    /// The exact final output artifact exposed to the independent reviewer.
    pub result_artifact: ArtifactRef,
    /// The typed journal uses that prove the final output artifact.
    pub result_uses: Vec<crate::ArtifactUse>,
    /// Every additional evidence artifact referenced by the result, with its
    /// exact typed journal uses. The bundle artifact itself is intentionally
    /// excluded to avoid a self-reference cycle.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<ArtifactEvidence>,
}

pub const REVIEW_SUBJECT_BUNDLE_SCHEMA_VERSION: u32 = 3;

/// Reference stored in `AgentResult`; the artifact bytes are stored in the
/// runtime artifact store and can be verified independently.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ReviewSubjectBundleRef {
    pub artifact: ArtifactRef,
    pub bundle_sha256: String,
}

impl ReviewSubjectBundle {
    pub fn validate(&self) -> Result<(), CompositionError> {
        if self.schema_version != REVIEW_SUBJECT_BUNDLE_SCHEMA_VERSION
            || !is_digest(&self.task_digest)
            || !is_digest(&self.composition_digest)
        {
            return Err(CompositionError::Invalid(
                "review subject bundle has invalid schema or digest".to_owned(),
            ));
        }
        validate_artifact_ref_shape(&self.result_artifact)?;
        validate_artifact_uses(&self.result_uses, "result")?;
        let mut ids = BTreeSet::from([self.result_artifact.id.clone()]);
        for evidence in &self.evidence {
            validate_artifact_ref_shape(&evidence.artifact)?;
            if !ids.insert(evidence.artifact.id.clone()) {
                return Err(CompositionError::Invalid(
                    "review subject bundle contains duplicate artifact evidence".to_owned(),
                ));
            }
            validate_artifact_uses(&evidence.uses, "evidence")?;
        }
        Ok(())
    }
}

fn validate_artifact_uses(
    uses: &[crate::ArtifactUse],
    label: &str,
) -> Result<(), CompositionError> {
    if uses.is_empty() || uses.len() > 256 {
        return Err(CompositionError::Invalid(format!(
            "review subject bundle {label} must contain 1..=256 uses"
        )));
    }
    let mut unique = std::collections::BTreeSet::new();
    for use_record in uses {
        use_record.validate().map_err(CompositionError::Invalid)?;
        if !unique.insert((
            use_record.sequence,
            use_record.event.clone(),
            use_record.call_index,
            use_record.call_id.clone(),
        )) {
            return Err(CompositionError::Invalid(format!(
                "review subject bundle contains duplicate {label} artifact provenance use"
            )));
        }
    }
    Ok(())
}

fn validate_artifact_ref_shape(artifact: &ArtifactRef) -> Result<(), CompositionError> {
    if !is_digest(&artifact.sha256)
        || artifact.media_type.trim().is_empty()
        || artifact.media_type.len() > 256
        || artifact.byte_length == 0
        || artifact.relative_path.is_empty()
    {
        return Err(CompositionError::Invalid(
            "review subject bundle contains an invalid artifact reference".to_owned(),
        ));
    }
    let relative = std::path::Path::new(&artifact.relative_path);
    if relative.is_absolute()
        || relative.components().any(|component| {
            matches!(
                component,
                std::path::Component::CurDir
                    | std::path::Component::ParentDir
                    | std::path::Component::RootDir
                    | std::path::Component::Prefix(_)
            )
        })
    {
        return Err(CompositionError::Invalid(
            "review subject bundle artifact path must be relative and contained".to_owned(),
        ));
    }
    Ok(())
}

impl ReviewSubjectBundleRef {
    pub fn validate(&self) -> Result<(), CompositionError> {
        self.artifact
            .sha256
            .eq_ignore_ascii_case(&self.bundle_sha256)
            .then_some(())
            .ok_or_else(|| CompositionError::DigestMismatch {
                id: "reviewSubjectBundle".to_owned(),
            })
    }
}

/// Content identity for one skill activated by the resolved composition.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct SelectedSkillBinding {
    pub id: String,
    pub instructions_sha256: String,
}

impl SelectedSkillBinding {
    pub fn verify(&self) -> Result<(), CompositionError> {
        if bounded_text(&self.id, "selected skill id", 160)? != self.id {
            return Err(CompositionError::Invalid(
                "selected skill id is not in canonical form".to_owned(),
            ));
        }
        validate_digest(&self.instructions_sha256, "instructionsSha256")
    }
}

/// Side-effect class sealed with the canonical tool definition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SelectedToolSideEffect {
    ReadOnly,
    External,
}

/// Exact active tool content. The digest covers the canonical definition,
/// schema, side-effect class, and origin identity; originDigest separately
/// seals the origin string used by the host.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct SelectedToolBinding {
    pub name: String,
    pub definition: ToolDefinition,
    pub side_effect: SelectedToolSideEffect,
    pub origin: String,
    pub origin_digest: String,
    pub digest: String,
}

impl SelectedToolBinding {
    pub fn create(
        definition: ToolDefinition,
        side_effect: SelectedToolSideEffect,
        origin: &str,
    ) -> Result<Self, CompositionError> {
        let name = bounded_text(&definition.name, "selected tool name", 160)?;
        let origin = bounded_text(origin, "selected tool origin", 256)?;
        let origin_digest = digest_json("upagent-tool-origin-v1", &origin)?;
        let unsigned = SelectedToolUnsigned {
            name: name.clone(),
            definition: definition.clone(),
            side_effect,
            origin: origin.clone(),
            origin_digest: origin_digest.clone(),
        };
        let digest = digest_json("upagent-selected-tool-v1", &unsigned)?;
        Ok(Self {
            name,
            definition,
            side_effect,
            origin,
            origin_digest,
            digest,
        })
    }

    pub fn verify(&self) -> Result<(), CompositionError> {
        let expected = Self::create(self.definition.clone(), self.side_effect, &self.origin)?;
        if expected.name != self.name
            || expected.origin != self.origin
            || expected.origin_digest != self.origin_digest
            || expected.digest != self.digest
        {
            return Err(CompositionError::DigestMismatch {
                id: format!("tool.{}", self.name),
            });
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
struct SelectedToolUnsigned {
    name: String,
    definition: ToolDefinition,
    side_effect: SelectedToolSideEffect,
    origin: String,
    origin_digest: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct PluginTierBinding {
    pub id: String,
    pub tier: PluginTier,
    /// Only tier one has a linked manifest identity. This is not called an
    /// artifact digest because a manifest hash is not a binary hash.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub linked_manifest_sha256: Option<String>,
    /// Exact content hash for tier two/three JSON data packages.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_sha256: Option<String>,
}

/// Linked plugin identity selected for a provider or profile role.  The
/// corresponding built-in manifest binding must be present in the receipt's
/// `pluginTiers`; this explicit pair prevents an all-plugins catalog snapshot
/// from hiding which provider/profile actually drove the composition.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct SelectedPluginBinding {
    pub plugin_id: String,
    pub manifest_sha256: String,
}

impl SelectedPluginBinding {
    fn verify(&self, role: &str) -> Result<(), CompositionError> {
        if bounded_text(&self.plugin_id, &format!("selected {role} plugin id"), 128)?
            != self.plugin_id
        {
            return Err(CompositionError::Invalid(format!(
                "selected {role} plugin id is not in canonical form"
            )));
        }
        validate_digest(
            &self.manifest_sha256,
            &format!("selected {role} manifestSha256"),
        )
    }
}

impl CompositionReceipt {
    #[allow(clippy::needless_pass_by_value)]
    #[allow(clippy::too_many_arguments)]
    pub fn create(
        task_id: TaskId,
        session_id: SessionId,
        task_digest: String,
        provider: ProviderBinding,
        spec: AgentSpec,
        selected_skills: Vec<SelectedSkillBinding>,
        selected_tools: Vec<SelectedToolBinding>,
        plugin_tiers: Vec<PluginTierBinding>,
        selected_provider_plugin: SelectedPluginBinding,
        selected_profile_plugin: SelectedPluginBinding,
    ) -> Result<Self, CompositionError> {
        let spec = spec.resolve()?;
        provider.validate()?;
        spec.validate_provider(&provider)?;
        let spec_digest = spec.digest()?;
        if !is_digest(&task_digest) {
            return Err(CompositionError::Invalid(
                "task digest must be a SHA-256 hex digest".to_owned(),
            ));
        }
        let mut selected_skills = selected_skills;
        selected_skills.sort_by(|left, right| left.id.cmp(&right.id));
        let mut skill_ids = BTreeSet::new();
        for binding in &selected_skills {
            binding.verify()?;
            if !skill_ids.insert(binding.id.clone()) {
                return Err(CompositionError::Invalid(
                    "composition selected skills must have unique IDs".to_owned(),
                ));
            }
        }
        if skill_ids != spec.skills.iter().cloned().collect::<BTreeSet<_>>() {
            return Err(CompositionError::Invalid(
                "composition selected skills do not match AgentSpec".to_owned(),
            ));
        }
        let mut selected_tools = selected_tools;
        selected_tools.sort_by(|left, right| left.name.cmp(&right.name));
        let mut tool_names = BTreeSet::new();
        for binding in &selected_tools {
            binding.verify()?;
            if !tool_names.insert(binding.name.clone()) {
                return Err(CompositionError::Invalid(
                    "composition selected tools must have unique names".to_owned(),
                ));
            }
            if binding.definition.name != binding.name {
                return Err(CompositionError::Invalid(
                    "selected tool binding name does not match its definition".to_owned(),
                ));
            }
        }
        if tool_names != spec.tools.iter().cloned().collect::<BTreeSet<_>>() {
            return Err(CompositionError::Invalid(
                "composition selected tools do not match AgentSpec".to_owned(),
            ));
        }
        let mut plugin_tiers = plugin_tiers;
        sort_plugin_tiers(&mut plugin_tiers);
        let mut plugin_ids = BTreeSet::new();
        for binding in &mut plugin_tiers {
            binding.id = bounded_text(&binding.id, "plugin id", 128)?;
            match binding.tier {
                PluginTier::BuiltIn => {
                    let Some(digest) = binding.linked_manifest_sha256.as_deref() else {
                        return Err(CompositionError::Invalid(format!(
                            "built-in plugin {} is missing linkedManifestSha256",
                            binding.id
                        )));
                    };
                    validate_digest(digest, "linkedManifestSha256")?;
                    if binding.source_sha256.is_some() {
                        return Err(CompositionError::Invalid(format!(
                            "built-in plugin {} cannot carry sourceSha256",
                            binding.id
                        )));
                    }
                }
                PluginTier::Approved | PluginTier::Dynamic => {
                    let Some(digest) = binding.source_sha256.as_deref() else {
                        return Err(CompositionError::Invalid(format!(
                            "data plugin {} is missing sourceSha256",
                            binding.id
                        )));
                    };
                    validate_digest(digest, "sourceSha256")?;
                    if binding.linked_manifest_sha256.is_some() {
                        return Err(CompositionError::Invalid(format!(
                            "data plugin {} cannot carry linkedManifestSha256",
                            binding.id
                        )));
                    }
                }
            }
            if !plugin_ids.insert(binding.id.clone()) {
                return Err(CompositionError::Invalid(
                    "composition plugin bindings must have unique IDs".to_owned(),
                ));
            }
        }
        validate_data_plugin_bindings(&spec, &plugin_tiers)?;
        selected_provider_plugin.verify("provider")?;
        selected_profile_plugin.verify("profile")?;
        if selected_provider_plugin.plugin_id == selected_profile_plugin.plugin_id {
            return Err(CompositionError::Invalid(
                "selected provider and profile plugins must have distinct IDs".to_owned(),
            ));
        }
        verify_selected_plugin_binding(&selected_provider_plugin, &plugin_tiers, "provider")?;
        verify_selected_plugin_binding(&selected_profile_plugin, &plugin_tiers, "profile")?;
        let unsigned = ReceiptUnsigned {
            schema_version: COMPOSITION_RECEIPT_SCHEMA_VERSION,
            task_id: task_id.clone(),
            session_id: session_id.clone(),
            task_digest: task_digest.clone(),
            spec_digest: spec_digest.clone(),
            provider: provider.clone(),
            spec: spec.clone(),
            selected_skills: selected_skills.clone(),
            selected_tools: selected_tools.clone(),
            plugin_tiers: plugin_tiers.clone(),
            selected_provider_plugin: selected_provider_plugin.clone(),
            selected_profile_plugin: selected_profile_plugin.clone(),
        };
        let digest = digest_json("upagent-composition-receipt-v5", &unsigned)?;
        Ok(Self {
            schema_version: COMPOSITION_RECEIPT_SCHEMA_VERSION,
            task_id,
            session_id,
            task_digest,
            spec_digest,
            provider,
            spec,
            selected_skills,
            selected_tools,
            plugin_tiers,
            selected_provider_plugin,
            selected_profile_plugin,
            digest,
        })
    }

    pub fn verify(&self) -> Result<(), CompositionError> {
        if self.schema_version != COMPOSITION_RECEIPT_SCHEMA_VERSION
            || !is_digest(&self.digest)
            || !is_digest(&self.spec_digest)
            || !is_digest(&self.task_digest)
        {
            return Err(CompositionError::Invalid(
                "composition receipt digest/schema is invalid".to_owned(),
            ));
        }
        self.provider.validate()?;
        self.spec.validate_provider(&self.provider)?;
        let expected_spec = self.spec.digest()?;
        if expected_spec != self.spec_digest {
            return Err(CompositionError::DigestMismatch {
                id: "composition.spec".to_owned(),
            });
        }
        let mut expected_skills = self.selected_skills.clone();
        expected_skills.sort_by(|left, right| left.id.cmp(&right.id));
        let mut expected_tools = self.selected_tools.clone();
        expected_tools.sort_by(|left, right| left.name.cmp(&right.name));
        let mut skill_ids = BTreeSet::new();
        for binding in &expected_skills {
            binding.verify()?;
            if !skill_ids.insert(binding.id.clone()) {
                return Err(CompositionError::Invalid(
                    "composition selected skills must have unique IDs".to_owned(),
                ));
            }
        }
        if skill_ids != self.spec.skills.iter().cloned().collect::<BTreeSet<_>>() {
            return Err(CompositionError::DigestMismatch {
                id: "composition.skills".to_owned(),
            });
        }
        let mut tool_names = BTreeSet::new();
        for binding in &expected_tools {
            binding.verify()?;
            if !tool_names.insert(binding.name.clone()) || binding.definition.name != binding.name {
                return Err(CompositionError::DigestMismatch {
                    id: "composition.tools".to_owned(),
                });
            }
        }
        if tool_names != self.spec.tools.iter().cloned().collect::<BTreeSet<_>>() {
            return Err(CompositionError::DigestMismatch {
                id: "composition.tools".to_owned(),
            });
        }
        let mut expected_plugins = self.plugin_tiers.clone();
        sort_plugin_tiers(&mut expected_plugins);
        let mut plugin_ids = BTreeSet::new();
        for binding in &mut expected_plugins {
            let canonical_id = bounded_text(&binding.id, "plugin id", 128)?;
            if canonical_id != binding.id {
                return Err(CompositionError::Invalid(
                    "composition plugin binding ID is not in canonical form".to_owned(),
                ));
            }
            match binding.tier {
                PluginTier::BuiltIn => {
                    let Some(digest) = binding.linked_manifest_sha256.as_deref() else {
                        return Err(CompositionError::Invalid(
                            "built-in plugin is missing linkedManifestSha256".to_owned(),
                        ));
                    };
                    validate_digest(digest, "linkedManifestSha256")?;
                    if binding.source_sha256.is_some() {
                        return Err(CompositionError::Invalid(
                            "built-in plugin cannot carry sourceSha256".to_owned(),
                        ));
                    }
                }
                PluginTier::Approved | PluginTier::Dynamic => {
                    let Some(digest) = binding.source_sha256.as_deref() else {
                        return Err(CompositionError::Invalid(
                            "data plugin is missing sourceSha256".to_owned(),
                        ));
                    };
                    validate_digest(digest, "sourceSha256")?;
                    if binding.linked_manifest_sha256.is_some() {
                        return Err(CompositionError::Invalid(
                            "data plugin cannot carry linkedManifestSha256".to_owned(),
                        ));
                    }
                }
            }
            if !plugin_ids.insert(binding.id.clone()) {
                return Err(CompositionError::Invalid(
                    "composition plugin bindings must have unique IDs".to_owned(),
                ));
            }
        }
        validate_data_plugin_bindings(&self.spec, &expected_plugins)?;
        self.selected_provider_plugin.verify("provider")?;
        self.selected_profile_plugin.verify("profile")?;
        if self.selected_provider_plugin.plugin_id == self.selected_profile_plugin.plugin_id {
            return Err(CompositionError::Invalid(
                "selected provider and profile plugins must have distinct IDs".to_owned(),
            ));
        }
        verify_selected_plugin_binding(
            &self.selected_provider_plugin,
            &expected_plugins,
            "provider",
        )?;
        verify_selected_plugin_binding(
            &self.selected_profile_plugin,
            &expected_plugins,
            "profile",
        )?;
        let unsigned = ReceiptUnsigned {
            schema_version: self.schema_version,
            task_id: self.task_id.clone(),
            session_id: self.session_id.clone(),
            task_digest: self.task_digest.clone(),
            spec_digest: self.spec_digest.clone(),
            provider: self.provider.clone(),
            spec: self.spec.clone(),
            selected_skills: expected_skills,
            selected_tools: expected_tools,
            plugin_tiers: expected_plugins,
            selected_provider_plugin: self.selected_provider_plugin.clone(),
            selected_profile_plugin: self.selected_profile_plugin.clone(),
        };
        let expected = digest_json("upagent-composition-receipt-v5", &unsigned)?;
        if expected != self.digest {
            return Err(CompositionError::DigestMismatch {
                id: "composition.receipt".to_owned(),
            });
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ReceiptUnsigned {
    schema_version: u32,
    task_id: TaskId,
    session_id: SessionId,
    task_digest: String,
    spec_digest: String,
    provider: ProviderBinding,
    spec: AgentSpec,
    selected_skills: Vec<SelectedSkillBinding>,
    selected_tools: Vec<SelectedToolBinding>,
    plugin_tiers: Vec<PluginTierBinding>,
    selected_provider_plugin: SelectedPluginBinding,
    selected_profile_plugin: SelectedPluginBinding,
}

fn sort_plugin_tiers(bindings: &mut [PluginTierBinding]) {
    bindings.sort_by(|left, right| {
        left.id
            .cmp(&right.id)
            .then(left.tier.cmp(&right.tier))
            .then(
                left.linked_manifest_sha256
                    .cmp(&right.linked_manifest_sha256),
            )
            .then(left.source_sha256.cmp(&right.source_sha256))
    });
}

fn verify_selected_plugin_binding(
    selected: &SelectedPluginBinding,
    bindings: &[PluginTierBinding],
    role: &str,
) -> Result<(), CompositionError> {
    let Some(binding) = bindings
        .iter()
        .find(|binding| binding.id == selected.plugin_id)
    else {
        return Err(CompositionError::Invalid(format!(
            "selected {role} plugin {} has no receipt binding",
            selected.plugin_id
        )));
    };
    if binding.tier != PluginTier::BuiltIn
        || binding.linked_manifest_sha256.as_deref() != Some(selected.manifest_sha256.as_str())
        || binding.source_sha256.is_some()
    {
        return Err(CompositionError::DigestMismatch {
            id: format!("selected.{role}.plugin.{}", selected.plugin_id),
        });
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CompositionError {
    #[error("invalid composition: {0}")]
    Invalid(String),
    #[error("composition digest mismatch for {id}")]
    DigestMismatch { id: String },
}

fn bounded_text(value: &str, field: &str, maximum: usize) -> Result<String, CompositionError> {
    if value.trim().is_empty()
        || value.len() > maximum
        || value.contains('\0')
        || value.chars().any(char::is_control)
    {
        return Err(CompositionError::Invalid(format!(
            "{field} must be a non-empty printable value of at most {maximum} bytes"
        )));
    }
    Ok(value.trim().to_owned())
}

fn validate_absolute_path(path: &std::path::Path, field: &str) -> Result<(), CompositionError> {
    if !path.is_absolute()
        || path.as_os_str().is_empty()
        || path.to_string_lossy().contains('\0')
        || path
            .components()
            .any(|component| component == std::path::Component::ParentDir)
    {
        return Err(CompositionError::Invalid(format!(
            "{field} must be an absolute path without parent traversal"
        )));
    }
    Ok(())
}

fn validate_digest(value: &str, field: &str) -> Result<(), CompositionError> {
    if !is_digest(value) {
        return Err(CompositionError::Invalid(format!(
            "{field} must be a SHA-256 hex digest"
        )));
    }
    Ok(())
}

fn validate_data_plugin_bindings(
    spec: &AgentSpec,
    bindings: &[PluginTierBinding],
) -> Result<(), CompositionError> {
    let extension_ids = spec
        .extensions
        .iter()
        .map(DataExtensionSpec::id)
        .collect::<BTreeSet<_>>();
    for extension in &spec.extensions {
        let id = extension.id();
        let binding = bindings
            .iter()
            .find(|binding| binding.id == id)
            .ok_or_else(|| {
                CompositionError::Invalid(format!("data extension {id} has no receipt binding"))
            })?;
        match extension {
            DataExtensionSpec::Approved {
                approved_sha256, ..
            } => {
                if binding.tier != PluginTier::Approved
                    || binding.source_sha256.as_deref() != Some(approved_sha256.as_str())
                {
                    return Err(CompositionError::DigestMismatch {
                        id: format!("plugin.{id}"),
                    });
                }
            }
            DataExtensionSpec::Dynamic { .. } => {
                if binding.tier != PluginTier::Dynamic {
                    return Err(CompositionError::DigestMismatch {
                        id: format!("plugin.{id}"),
                    });
                }
            }
        }
    }
    if bindings.iter().any(|binding| {
        matches!(binding.tier, PluginTier::Approved | PluginTier::Dynamic)
            && !extension_ids.contains(&binding.id.as_str())
    }) {
        return Err(CompositionError::Invalid(
            "receipt contains a data plugin not selected by AgentSpec".to_owned(),
        ));
    }
    Ok(())
}

fn is_digest(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn digest_json<T: Serialize>(domain: &str, value: &T) -> Result<String, CompositionError> {
    let bytes = serde_json::to_vec(value).map_err(|error| {
        CompositionError::Invalid(format!("composition encoding failed: {error}"))
    })?;
    let mut hasher = Sha256::new();
    hasher.update(domain.as_bytes());
    hasher.update([0]);
    hasher.update(bytes);
    Ok(format!("{:x}", hasher.finalize()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        ApprovalGrant, ArtifactId, ArtifactRef, IMAGE_TASK_CONTRACT_SCHEMA,
        IMAGE_VERIFICATION_SPEC_SCHEMA, ImageCompareArtifactKind, ImageCompareAssertion,
        ImageCompareSeverity, ImageSource, ImageTaskContract, ImageVerificationContract,
        ImageVerificationSpec, TaskBudget, TaskId,
    };
    use std::path::PathBuf;

    fn provider_binding() -> ProviderBinding {
        ProviderBinding {
            id: "test".to_owned(),
            model: "test-model".to_owned(),
            route_sha256: "f".repeat(64),
            capabilities: BTreeSet::from([
                PROVIDER_CAP_TEXT.to_owned(),
                PROVIDER_CAP_IMAGE_OBSERVATION.to_owned(),
                PROVIDER_CAP_IMAGE_GENERATION.to_owned(),
            ]),
            maximum_output_bytes: 8 * 1024 * 1024,
        }
    }

    fn image_artifact(seed: char) -> ArtifactRef {
        let digest = seed.to_string().repeat(64);
        ArtifactRef {
            id: ArtifactId::parse(format!("art-{digest}")).expect("artifact id"),
            sha256: digest.clone(),
            media_type: "image/png".to_owned(),
            byte_length: 1,
            relative_path: format!("{}/{}.png", &digest[..2], digest),
        }
    }

    fn task() -> TaskEnvelopeV3 {
        TaskEnvelopeV3 {
            schema_version: crate::TASK_ENVELOPE_V3_VERSION,
            task_id: TaskId::parse("task-v3").expect("valid id"),
            session_id: SessionId::parse("session-v3").expect("valid id"),
            parent_task_id: None,
            objective: "compose".to_owned(),
            image_task: None,
            history: Vec::new(),
            workspace: None,
            context: serde_json::Map::new(),
            budget: TaskBudget::default(),
            approvals: Vec::new(),
        }
    }

    #[test]
    fn image_task_contract_is_sealed_by_task_digest() {
        let base = task();
        let base_digest = base.digest().expect("base task digest");
        let mut image = base;
        image.image_task = Some(ImageTaskContract {
            schema: IMAGE_TASK_CONTRACT_SCHEMA.to_owned(),
            goal: "edit".to_owned(),
            source: ImageSource::Provided {
                artifact: image_artifact('a'),
            },
            maximum_attempts: 4,
            preserve: Vec::new(),
            visual_criteria: vec!["review".to_owned()],
            verification: ImageVerificationContract {
                visual_review: true,
                deterministic: None,
            },
        });
        let image_digest = image.digest().expect("image task digest");
        assert_ne!(base_digest, image_digest);
    }

    #[test]
    fn exact_image_assertion_threshold_is_sealed_by_task_digest() {
        let digest = "b".repeat(64);
        let before = ArtifactRef {
            id: ArtifactId::parse(format!("art-{digest}")).expect("artifact id"),
            sha256: digest.clone(),
            media_type: "image/png".to_owned(),
            byte_length: 1,
            relative_path: format!("bb/{digest}.png"),
        };
        let mut original = task();
        original.image_task = Some(ImageTaskContract {
            schema: IMAGE_TASK_CONTRACT_SCHEMA.to_owned(),
            goal: "preserve".to_owned(),
            source: ImageSource::Provided {
                artifact: before.clone(),
            },
            maximum_attempts: 4,
            preserve: Vec::new(),
            visual_criteria: vec!["review".to_owned()],
            verification: ImageVerificationContract {
                visual_review: true,
                deterministic: Some(ImageVerificationSpec {
                    schema: IMAGE_VERIFICATION_SPEC_SCHEMA.to_owned(),
                    before_artifact: before,
                    after_kind: ImageCompareArtifactKind::Raster,
                    assertions: vec![ImageCompareAssertion::MaximumChannelError {
                        id: "channel_error".to_owned(),
                        severity: ImageCompareSeverity::Required,
                        maximum: 0,
                    }],
                }),
            },
        });
        let mut changed = original.clone();
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
        assert_ne!(
            original.digest().expect("original digest"),
            changed.digest().expect("changed digest")
        );
    }

    fn spec() -> AgentSpec {
        AgentSpec {
            schema_version: AGENT_SPEC_SCHEMA_VERSION,
            profile: "creator".to_owned(),
            workflow: None,
            skills: Vec::new(),
            tools: Vec::new(),
            memory: MemorySpec::None,
            review: ReviewSpec::None,
            effects: EffectsSpec::None,
            effect_approval: EffectApprovalSpec::Deny,
            isolation: IsolationSpec::Native,
            extensions: Vec::new(),
            image: None,
            semantic_mask: None,
            promptgen: None,
        }
    }

    fn selected_plugins() -> (SelectedPluginBinding, SelectedPluginBinding) {
        (
            SelectedPluginBinding {
                plugin_id: "upagent.provider.test".to_owned(),
                manifest_sha256: "a".repeat(64),
            },
            SelectedPluginBinding {
                plugin_id: "upagent.profile.test".to_owned(),
                manifest_sha256: "b".repeat(64),
            },
        )
    }

    fn plugin_tiers() -> Vec<PluginTierBinding> {
        vec![
            PluginTierBinding {
                id: "upagent.provider.test".to_owned(),
                tier: PluginTier::BuiltIn,
                linked_manifest_sha256: Some("a".repeat(64)),
                source_sha256: None,
            },
            PluginTierBinding {
                id: "upagent.profile.test".to_owned(),
                tier: PluginTier::BuiltIn,
                linked_manifest_sha256: Some("b".repeat(64)),
                source_sha256: None,
            },
        ]
    }

    #[test]
    fn task_v3_contains_only_task_data_and_has_stable_digest() {
        let value = task();
        let json = serde_json::to_value(&value).expect("serialize");
        assert!(json.get("profile").is_none());
        assert_eq!(
            value.digest().expect("digest"),
            value.digest().expect("digest")
        );
    }

    #[test]
    fn agent_spec_is_provider_neutral_and_validates_capabilities_explicitly() {
        let resolved = spec().resolve().expect("resolve");
        assert!(resolved.validate_provider(&provider_binding()).is_ok());
        let mut missing = provider_binding();
        missing.capabilities.remove(PROVIDER_CAP_TEXT);
        assert!(resolved.validate_provider(&missing).is_err());
    }

    #[test]
    fn receipt_detects_tampering() {
        let value = task();
        let task_digest = value.digest().expect("task digest");
        let (selected_provider_plugin, selected_profile_plugin) = selected_plugins();
        let mut tiers = plugin_tiers();
        tiers.push(PluginTierBinding {
            id: "upagent.kernel".to_owned(),
            tier: PluginTier::BuiltIn,
            linked_manifest_sha256: Some("a".repeat(64)),
            source_sha256: None,
        });
        let receipt = CompositionReceipt::create(
            value.task_id.clone(),
            value.session_id.clone(),
            task_digest.clone(),
            provider_binding(),
            spec(),
            Vec::new(),
            Vec::new(),
            tiers,
            selected_provider_plugin,
            selected_profile_plugin,
        )
        .expect("receipt");
        assert!(receipt.verify().is_ok());
        let mut changed_provider = receipt.clone();
        changed_provider.provider.model = "different-model".to_owned();
        assert!(changed_provider.verify().is_err());

        let mut changed = receipt;
        changed.spec.profile = "developer".to_owned();
        assert!(changed.verify().is_err());
    }

    #[test]
    fn approved_data_extensions_require_an_absolute_source() {
        let mut value = spec();
        value.extensions.push(DataExtensionSpec::Approved {
            id: "external".to_owned(),
            source: "relative.json".to_owned(),
            approved_sha256: "a".repeat(64),
        });
        assert!(value.resolve().is_err());
    }

    #[test]
    fn data_extensions_are_explicit_data_specs() {
        let mut value = spec();
        value.extensions.push(DataExtensionSpec::Dynamic {
            id: "prompt-pack".to_owned(),
            source: "/tmp/prompt-pack.json".to_owned(),
        });
        assert!(value.resolve().is_ok());
    }

    #[test]
    fn v2_wire_uses_explicit_camel_case_hash_fields() {
        let isolation: IsolationSpec = serde_json::from_value(serde_json::json!({
            "kind": "host_adapter",
            "adapter": "mbox",
            "policySha256": "a".repeat(64),
            "adapterSha256": "b".repeat(64)
        }))
        .expect("host adapter wire");
        assert!(matches!(isolation, IsolationSpec::HostAdapter { .. }));
        assert!(
            serde_json::from_value::<IsolationSpec>(serde_json::json!({
                "kind": "host_adapter",
                "adapter": "mbox",
                "policy_sha256": "a".repeat(64),
                "adapter_sha256": "b".repeat(64)
            }))
            .is_err()
        );

        let extension: DataExtensionSpec = serde_json::from_value(serde_json::json!({
            "tier": "approved",
            "id": "creator/pack",
            "source": "/tmp/pack.json",
            "approvedSha256": "c".repeat(64)
        }))
        .expect("approved data extension wire");
        assert!(matches!(extension, DataExtensionSpec::Approved { .. }));
    }

    #[test]
    fn set_like_selection_order_does_not_change_spec_or_receipt_digest() {
        let mut left = spec();
        left.skills = vec!["b".to_owned(), "a".to_owned()];
        left.tools = vec!["tool.b".to_owned(), "tool.a".to_owned()];
        let mut right = left.clone();
        right.skills.reverse();
        right.tools.reverse();
        assert_eq!(
            left.digest().expect("left digest"),
            right.digest().expect("right digest")
        );
    }

    #[test]
    fn receipt_plugin_binding_order_does_not_change_digest() {
        let value = task();
        let task_digest = value.digest().expect("task digest");
        let bindings = vec![
            PluginTierBinding {
                id: "upagent.z".to_owned(),
                tier: PluginTier::BuiltIn,
                linked_manifest_sha256: Some("b".repeat(64)),
                source_sha256: None,
            },
            PluginTierBinding {
                id: "upagent.a".to_owned(),
                tier: PluginTier::BuiltIn,
                linked_manifest_sha256: Some("a".repeat(64)),
                source_sha256: None,
            },
        ];
        let (selected_provider_plugin, selected_profile_plugin) = selected_plugins();
        let mut bindings = bindings;
        bindings.extend(plugin_tiers());
        let mut reversed = bindings.clone();
        reversed.reverse();
        let left = CompositionReceipt::create(
            value.task_id.clone(),
            value.session_id.clone(),
            task_digest.clone(),
            provider_binding(),
            spec(),
            Vec::new(),
            Vec::new(),
            bindings,
            selected_provider_plugin.clone(),
            selected_profile_plugin.clone(),
        )
        .expect("left receipt");
        let right = CompositionReceipt::create(
            value.task_id.clone(),
            value.session_id.clone(),
            task_digest,
            provider_binding(),
            spec(),
            Vec::new(),
            Vec::new(),
            reversed,
            selected_provider_plugin,
            selected_profile_plugin,
        )
        .expect("right receipt");
        assert_eq!(left.digest, right.digest);
    }

    #[test]
    fn receipt_seals_selected_skill_and_tool_content() {
        let value = task();
        let task_digest = value.digest().expect("task digest");
        let mut spec = spec();
        spec.skills = vec!["skill.a".to_owned(), "skill.b".to_owned()];
        spec.tools = vec!["tool.a".to_owned(), "tool.b".to_owned()];
        let selected_skills = vec![
            SelectedSkillBinding {
                id: "skill.b".to_owned(),
                instructions_sha256: "b".repeat(64),
            },
            SelectedSkillBinding {
                id: "skill.a".to_owned(),
                instructions_sha256: "a".repeat(64),
            },
        ];
        let selected_tools = vec![
            SelectedToolBinding::create(
                ToolDefinition {
                    name: "tool.b".to_owned(),
                    description: "B".to_owned(),
                    input_schema: serde_json::json!({"type":"object"}),
                    strict: true,
                },
                SelectedToolSideEffect::External,
                "origin.b",
            )
            .expect("tool b"),
            SelectedToolBinding::create(
                ToolDefinition {
                    name: "tool.a".to_owned(),
                    description: "A".to_owned(),
                    input_schema: serde_json::json!({"type":"object"}),
                    strict: true,
                },
                SelectedToolSideEffect::ReadOnly,
                "origin.a",
            )
            .expect("tool a"),
        ];
        let receipt = CompositionReceipt::create(
            value.task_id.clone(),
            value.session_id.clone(),
            task_digest.clone(),
            provider_binding(),
            spec.clone(),
            selected_skills.clone(),
            selected_tools.clone(),
            plugin_tiers(),
            selected_plugins().0.clone(),
            selected_plugins().1.clone(),
        )
        .expect("receipt");
        let reversed = CompositionReceipt::create(
            value.task_id,
            value.session_id,
            task_digest,
            provider_binding(),
            spec,
            selected_skills.into_iter().rev().collect(),
            selected_tools.into_iter().rev().collect(),
            plugin_tiers(),
            selected_plugins().0,
            selected_plugins().1,
        )
        .expect("reversed receipt");
        assert_eq!(receipt.digest, reversed.digest);

        let mut changed_skill = receipt.clone();
        changed_skill.selected_skills[0].instructions_sha256 = "c".repeat(64);
        assert!(changed_skill.verify().is_err());
        let mut changed_tool = receipt;
        changed_tool.selected_tools[0].definition.description = "changed".to_owned();
        assert!(changed_tool.verify().is_err());
    }

    #[test]
    fn duplicate_extension_and_plugin_ids_are_rejected() {
        let mut value = spec();
        value.extensions = vec![
            DataExtensionSpec::Dynamic {
                id: "data/a".to_owned(),
                source: "/tmp/a.json".to_owned(),
            },
            DataExtensionSpec::Dynamic {
                id: "data/a".to_owned(),
                source: "/tmp/b.json".to_owned(),
            },
        ];
        assert!(value.resolve().is_err());

        let task = task();
        assert!(
            CompositionReceipt::create(
                task.task_id.clone(),
                task.session_id.clone(),
                task.digest().expect("task digest"),
                provider_binding(),
                spec(),
                Vec::new(),
                Vec::new(),
                {
                    let mut tiers = plugin_tiers();
                    tiers.extend([
                        PluginTierBinding {
                            id: "upagent.same".to_owned(),
                            tier: PluginTier::BuiltIn,
                            linked_manifest_sha256: Some("a".repeat(64)),
                            source_sha256: None,
                        },
                        PluginTierBinding {
                            id: "upagent.same".to_owned(),
                            tier: PluginTier::BuiltIn,
                            linked_manifest_sha256: Some("a".repeat(64)),
                            source_sha256: None,
                        },
                    ]);
                    tiers
                },
                selected_plugins().0,
                selected_plugins().1,
            )
            .is_err()
        );
    }

    #[test]
    fn provider_binding_is_sealed_independently_from_agent_policy() {
        let policy = spec().resolve().expect("agent policy");
        let policy_digest = policy.digest().expect("agent policy digest");
        let mut provider = provider_binding();
        assert!(policy.validate_provider(&provider).is_ok());
        provider.model = "other-model".to_owned();
        assert!(policy.validate_provider(&provider).is_ok());
        assert_eq!(
            policy_digest,
            policy.digest().expect("stable policy digest")
        );
    }

    #[test]
    fn image_generation_requires_explicit_effect_approval() {
        let mut value = spec();
        value.tools = vec!["image_generate".to_owned(), "image_inspect".to_owned()];
        value.image = Some(ImageSpec {
            perfectpixel_executable: "/tmp/perfectpixel".into(),
            perfectpixel_sha256: "b".repeat(64),
            protocol_schema: AGENT_IMAGE_PROTOCOL_SCHEMA.to_owned(),
            protocol_version: AGENT_IMAGE_PROTOCOL_VERSION.to_owned(),
            behavior_version: AGENT_IMAGE_BEHAVIOR_VERSION.to_owned(),
            capability_manifest_sha256: "c".repeat(64),
            implementation_version: "0.3.0".to_owned(),
        });
        assert!(value.resolve().is_err());
        value.effect_approval = EffectApprovalSpec::TaskGrant;
        let resolved = value.resolve().expect("image generation policy");
        assert!(resolved.validate_provider(&provider_binding()).is_ok());
        let mut text_only = provider_binding();
        text_only.capabilities = BTreeSet::from([PROVIDER_CAP_TEXT.to_owned()]);
        assert!(resolved.validate_provider(&text_only).is_err());

        value.tools = vec!["image_inspect".to_owned()];
        value.effect_approval = EffectApprovalSpec::Deny;
        assert!(value.resolve().is_ok());
    }

    #[test]
    fn image_protocol_identity_is_sealed_and_protocol_drift_is_rejected() {
        let mut value = spec();
        value.tools = vec!["image_inspect".to_owned()];
        value.image = Some(ImageSpec {
            perfectpixel_executable: "/tmp/perfectpixel".into(),
            perfectpixel_sha256: "b".repeat(64),
            protocol_schema: AGENT_IMAGE_PROTOCOL_SCHEMA.to_owned(),
            protocol_version: AGENT_IMAGE_PROTOCOL_VERSION.to_owned(),
            behavior_version: AGENT_IMAGE_BEHAVIOR_VERSION.to_owned(),
            capability_manifest_sha256: "c".repeat(64),
            implementation_version: "0.3.0".to_owned(),
        });
        let base_digest = value.digest().expect("base identity digest");

        let mut executable_changed = value.clone();
        executable_changed
            .image
            .as_mut()
            .expect("image identity")
            .perfectpixel_executable = "/tmp/perfectpixel-next".into();
        assert_ne!(
            executable_changed
                .digest()
                .expect("executable path identity digest"),
            base_digest
        );

        let mut executable_hash_changed = value.clone();
        executable_hash_changed
            .image
            .as_mut()
            .expect("image identity")
            .perfectpixel_sha256 = "d".repeat(64);
        assert_ne!(
            executable_hash_changed
                .digest()
                .expect("executable hash identity digest"),
            base_digest
        );

        let mut manifest_changed = value.clone();
        manifest_changed
            .image
            .as_mut()
            .expect("image identity")
            .capability_manifest_sha256 = "e".repeat(64);
        assert_ne!(
            manifest_changed.digest().expect("manifest identity digest"),
            base_digest
        );

        let mut implementation_changed = value.clone();
        implementation_changed
            .image
            .as_mut()
            .expect("image identity")
            .implementation_version = "0.3.1".to_owned();
        assert_ne!(
            implementation_changed
                .digest()
                .expect("implementation identity digest"),
            base_digest
        );

        for field in ["schema", "protocol", "behavior"] {
            let mut drifted = value.clone();
            let image = drifted.image.as_mut().expect("image identity");
            match field {
                "schema" => image.protocol_schema = "perfectpixel.agent-image/3".to_owned(),
                "protocol" => image.protocol_version = "3.0.0".to_owned(),
                "behavior" => image.behavior_version = "3.0.0".to_owned(),
                _ => unreachable!(),
            }
            assert!(drifted.resolve().is_err(), "{field} drift must fail");
        }
    }

    #[test]
    fn semantic_mask_helper_config_is_explicitly_bound_to_image_extract() {
        let mut value = spec();
        value.semantic_mask = Some(SemanticMaskSpec {
            executable: "/tmp/semantic-mask-helper".into(),
            executable_sha256: "A".repeat(64),
            helper_identity: "fixture.semantic/1".to_owned(),
            helper_version: "2026.08".to_owned(),
            timeout_ms: 30_000,
        });
        assert!(value.resolve().is_err());

        value.tools = vec!["image_extract".to_owned()];
        value.image = Some(ImageSpec {
            perfectpixel_executable: "/tmp/perfectpixel".into(),
            perfectpixel_sha256: "b".repeat(64),
            protocol_schema: AGENT_IMAGE_PROTOCOL_SCHEMA.to_owned(),
            protocol_version: AGENT_IMAGE_PROTOCOL_VERSION.to_owned(),
            behavior_version: AGENT_IMAGE_BEHAVIOR_VERSION.to_owned(),
            capability_manifest_sha256: "c".repeat(64),
            implementation_version: "0.3.0".to_owned(),
        });
        let resolved = value.resolve().expect("semantic helper contract");
        assert!(resolved.validate_provider(&provider_binding()).is_ok());
        assert_eq!(
            resolved
                .semantic_mask
                .as_ref()
                .expect("semantic helper")
                .executable_sha256,
            "a".repeat(64)
        );
        let base_digest = value.digest().expect("base semantic identity");
        let mut changed = value;
        changed
            .semantic_mask
            .as_mut()
            .expect("semantic helper")
            .timeout_ms += 1;
        assert_ne!(
            base_digest,
            changed.digest().expect("changed semantic identity")
        );
    }

    #[test]
    fn task_v3_retains_bounded_approval_grants_as_task_data() {
        let mut value = task();
        value.approvals.push(ApprovalGrant {
            subject: "a".repeat(64),
            token: "b".repeat(64),
        });
        let encoded = serde_json::to_value(&value).expect("serialize");
        assert_eq!(encoded["approvals"][0]["subject"], "a".repeat(64));
        assert!(value.validate().is_ok());
    }

    fn promptgen_spec() -> PromptGenSpec {
        PromptGenSpec {
            executable: PathBuf::from("/opt/upagent/promptgen"),
            executable_sha256: "a".repeat(64),
            timeout_ms: 30_000,
            codex_runtime: None,
        }
    }

    fn promptgen_codex_runtime_spec() -> PromptGenCodexRuntimeSpec {
        PromptGenCodexRuntimeSpec {
            executable: PathBuf::from("/opt/codex/bin/codex"),
            executable_sha256: "b".repeat(64),
            home: PathBuf::from("/var/lib/upagent/codex-home"),
        }
    }

    #[test]
    fn prompt_compile_is_local_explicit_and_independent_of_pixel_provider() {
        let mut value = spec();
        value.tools = vec![crate::PROMPTGEN_COMPILE_TOOL.to_owned()];
        assert!(value.resolve().is_err());
        value.promptgen = Some(promptgen_spec());
        let resolved = value.resolve().expect("compile-only composition");
        assert_eq!(resolved.effect_approval, EffectApprovalSpec::Deny);
        assert!(resolved.image.is_none());
        assert!(resolved.validate_provider(&provider_binding()).is_ok());
        assert!(
            !resolved
                .tools
                .iter()
                .any(|tool| crate::is_external_image_tool_name(tool))
        );
        value.tools.clear();
        let configured_only = value.resolve().expect("ambient config");
        assert_eq!(configured_only.tools, [] as [std::string::String; 0]);
    }

    #[test]
    fn semantic_edit_needs_explicit_external_approval_and_pixel_configuration() {
        let mut value = spec();
        value.tools = vec![crate::IMAGE_EDIT_SEMANTIC_TOOL.to_owned()];
        let mut promptgen = promptgen_spec();
        promptgen.codex_runtime = Some(promptgen_codex_runtime_spec());
        value.promptgen = Some(promptgen);
        assert!(
            value
                .resolve()
                .expect_err("approval required")
                .to_string()
                .contains("approval")
        );
        value.effect_approval = EffectApprovalSpec::PlanOnly;
        assert!(
            value
                .resolve()
                .expect_err("pixel identity required")
                .to_string()
                .contains("PerfectPixel")
        );
    }

    #[test]
    fn semantic_edit_requires_an_explicit_codex_runtime_identity() {
        let mut value = spec();
        value.tools = vec![crate::IMAGE_EDIT_SEMANTIC_TOOL.to_owned()];
        value.promptgen = Some(promptgen_spec());
        assert!(
            value
                .resolve()
                .expect_err("Codex runtime pin required")
                .to_string()
                .contains("pinned PromptGen Codex runtime")
        );

        let mut promptgen = promptgen_spec();
        promptgen.codex_runtime = Some(promptgen_codex_runtime_spec());
        value.promptgen = Some(promptgen);
        value.effect_approval = EffectApprovalSpec::PlanOnly;
        assert!(value.resolve().is_err());
    }

    #[test]
    fn promptgen_identity_rejects_relative_path_invalid_hash_and_timeout() {
        let mut value = promptgen_spec();
        value.executable = PathBuf::from("promptgen");
        assert!(value.resolve().is_err());
        value = promptgen_spec();
        value.executable_sha256 = "bad".to_owned();
        assert!(value.resolve().is_err());
        value = promptgen_spec();
        value.timeout_ms = 0;
        assert!(value.resolve().is_err());
        value.timeout_ms = PROMPTGEN_MAX_TIMEOUT_MS + 1;
        assert!(value.resolve().is_err());
    }
}
