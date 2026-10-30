#![forbid(unsafe_code)]

use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

use agent_core::{
    ApprovalPolicy, CapabilityId, ConcurrencyPolicy, ConversationRole, MemoryPolicy, MessageRole,
    ModelMessage, ProfileId, ProfileManifest, ReasoningEffort, SkillId, TaskBudget, TaskEnvelope,
    TrackId,
};
use agent_plugin_kernel::{ContributionKind, PluginId, PluginManifest, PluginTrust};
use agent_profile_api::{
    ExecutionClass, ExecutionTopology, ProfileContext, ProfileDefinition, ProfileError,
    ProfilePlan, ProfilePlugin, ProfileRegistry,
};
use agent_skills::SkillDocument;
use serde_json::Value;
use std::fmt;

mod axd_policy;
mod developer_track;

use axd_policy::apply_axd_execution_policy;
use developer_track::track_policy;

/// Stock product profile catalog. The replaceable profile contract itself
/// lives in `agent-profile-api`; this crate owns only built-in policy.
pub fn built_in_registry() -> Result<ProfileRegistry, ProfileError> {
    let mut registry = ProfileRegistry::default();
    registry.register(Arc::new(CreatorProfile::new()?))?;
    registry.register(Arc::new(DeveloperProfile::new()?))?;
    registry.register(Arc::new(ReviewerProfile::new()?))?;
    registry.register(Arc::new(TwinProfile::new()?))?;
    Ok(registry)
}

pub const BUILT_IN_PROFILE_PLUGIN_ID: &str = "upagent.profiles.builtin";

/// Linked-Rust profile plugin for the product's stock Creator/Developer/
/// Reviewer/Twin policies. A host may replace this plugin with another
/// implementation of `ProfilePlugin` without changing the runtime kernel.
#[derive(Clone)]
pub struct BuiltInProfilePlugin {
    manifest: PluginManifest,
}

impl fmt::Debug for BuiltInProfilePlugin {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("BuiltInProfilePlugin")
            .field("id", &self.manifest.id())
            .field("manifestSha256", &self.manifest.digest())
            .finish()
    }
}

impl BuiltInProfilePlugin {
    pub fn new() -> Result<Self, String> {
        let id = PluginId::parse(BUILT_IN_PROFILE_PLUGIN_ID.to_owned())
            .map_err(|error| error.to_string())?;
        let manifest = PluginManifest::linked(id, env!("CARGO_PKG_VERSION"), PluginTrust::BuiltIn)
            .map_err(|error| error.to_string())?
            .contributes(ContributionKind::Profiles);
        Ok(Self { manifest })
    }
}

/// Linked profile plugins exported by this package.  Host composition roots
/// register the list without knowing any concrete profile implementation.
pub fn linked_profile_plugins() -> Result<Vec<Arc<dyn ProfilePlugin>>, String> {
    Ok(vec![Arc::new(BuiltInProfilePlugin::new()?)])
}

impl ProfilePlugin for BuiltInProfilePlugin {
    fn manifest(&self) -> &PluginManifest {
        &self.manifest
    }

    fn register_profiles(&self, registry: &mut ProfileRegistry) -> Result<(), ProfileError> {
        let built_in = built_in_registry()?;
        for profile in built_in.definitions() {
            registry.register(profile)?;
        }
        Ok(())
    }
}

#[derive(Debug)]
struct CreatorProfile {
    manifest: ProfileManifest,
}

impl CreatorProfile {
    fn new() -> Result<Self, ProfileError> {
        Ok(Self {
            manifest: ProfileManifest {
                id: ProfileId::creator(),
                display_name: "Creator".to_owned(),
                description:
                    "Conversation, writing, structured documents, prompts, and creative artifacts."
                        .to_owned(),
                allowed_tracks: BTreeSet::new(),
                capabilities: capabilities([
                    "model/text",
                    "model/image",
                    "artifact/read",
                    "artifact/write",
                    "effect/file_read",
                    "effect/file_write",
                ])?,
                default_skills: skills(["creator/chat"])?,
                allowed_skill_prefixes: BTreeSet::from(["creator/".to_owned()]),
                memory_policy: MemoryPolicy::Session,
                approval_policy: ApprovalPolicy::AllowTaskGrant,
                concurrency: ConcurrencyPolicy::single(),
            },
        })
    }
}

impl ProfileDefinition for CreatorProfile {
    fn manifest(&self) -> &ProfileManifest {
        &self.manifest
    }

    fn prepare(
        &self,
        task: &TaskEnvelope,
        skills: &[SkillDocument],
        context: &ProfileContext,
    ) -> Result<ProfilePlan, ProfileError> {
        validate_task(&self.manifest, task, skills)?;
        let mut messages = vec![ModelMessage::text(
            MessageRole::System,
            "You are the Creator profile. Clarify intent only when ambiguity changes the result. Produce the requested artifact directly, preserve supplied constraints, distinguish facts from assumptions, and do not claim tool effects that were not observed. For image work, the task contract owns source artifact authority and exact verification. Treat image generation as base-image creation only; after deterministic editing starts, repair with image_extract/image_render rather than regenerating the image. Use image_compare when the task contract includes a deterministic specification. Tool execution success is not image-task success: inspect every current multimodal candidate and revise any candidate that fails required assertions. Submit explicit IMAGE_REVIEW evidence using the current contract, revision, candidate-set digest and criterion IDs supplied by the runtime; observation alone is not approval. The review response is internal evidence and must be followed by a separate normal final answer. Use optional semantic editing only when selected and approved; an uncertain external effect must never be automatically re-dispatched.",
        )];
        if let Some(image_task) = &task.image_task {
            let encoded = serde_json::to_string(image_task)
                .map_err(|error| ProfileError::Invalid(error.to_string()))?;
            messages.push(ModelMessage::text(
                MessageRole::System,
                format!("Authoritative image task contract: {encoded}"),
            ));
        }
        append_skills(&mut messages, skills);
        append_context(&mut messages, context)?;
        append_history(&mut messages, task);
        messages.push(ModelMessage::text(
            MessageRole::User,
            task.objective.clone(),
        ));
        let mut allowed_tools = BTreeSet::from(["request_input".to_owned()]);
        allowed_tools.extend(
            agent_core::IMAGE_TOOL_NAMES
                .iter()
                .chain(agent_core::IMAGE_MAKER_TOOL_NAMES)
                .map(|name| (*name).to_owned()),
        );
        if task.workspace.is_some() {
            allowed_tools.insert("read_file".to_owned());
            allowed_tools.insert("write_file".to_owned());
        }
        let mut plan = ProfilePlan {
            manifest: self.manifest.clone(),
            messages,
            track: task.track.clone(),
            allowed_tools,
            reasoning: ReasoningEffort::Medium,
            memory_scope: None,
            execution_class: ExecutionClass::Guided,
            required_successful_tools: BTreeSet::new(),
            required_checks: Vec::new(),
            required_check_commands: BTreeMap::new(),
            required_successful_tools_after_mutation: BTreeSet::new(),
            maximum_workers: 1,
            topology: ExecutionTopology::Single,
            output_contract: None,
            axd_result_binding: None,
            effective_budget: task.budget.clone(),
            policy_budget_cap: None,
        };
        apply_axd_execution_policy(&mut plan, task, context)?;
        Ok(plan)
    }
}

#[derive(Debug)]
struct DeveloperProfile {
    manifest: ProfileManifest,
}

impl DeveloperProfile {
    fn new() -> Result<Self, ProfileError> {
        Ok(Self {
            manifest: ProfileManifest {
                id: ProfileId::developer(),
                display_name: "Developer".to_owned(),
                description: "Repository-grounded software analysis, implementation, verification, and recovery.".to_owned(),
                allowed_tracks: BTreeSet::from([
                    TrackId::prototyper(),
                    TrackId::builder(),
                    TrackId::sweeper(),
                    TrackId::grower(),
                    TrackId::maintainer(),
                ]),
                capabilities: capabilities([
                    "model/text",
                    "repo/read",
                    "repo/write",
                    "process/run",
                    "git/read",
                    "artifact/write",
                    "effect/file_read",
                    "effect/file_write",
                    "effect/process",
                    "effect/git",
                ])?,
                default_skills: BTreeSet::new(),
                allowed_skill_prefixes: BTreeSet::from(["developer/".to_owned()]),
                memory_policy: MemoryPolicy::Session,
                approval_policy: ApprovalPolicy::AllowTaskGrant,
                concurrency: ConcurrencyPolicy {
                    default_workers: 2,
                    maximum_workers: 4,
                    recursive_workers: false,
                    shared_workspace_writers: 1,
                },
            },
        })
    }
}

impl ProfileDefinition for DeveloperProfile {
    fn manifest(&self) -> &ProfileManifest {
        &self.manifest
    }

    fn default_skills(&self, track: Option<&TrackId>) -> Result<BTreeSet<SkillId>, ProfileError> {
        let track = track.ok_or_else(|| {
            ProfileError::Invalid("developer profile requires one development track".to_owned())
        })?;
        if !self.manifest.allowed_tracks.contains(track) {
            return Err(ProfileError::Invalid(format!(
                "developer track {track} is not allowed"
            )));
        }
        let id = SkillId::parse(format!("developer/{}", track.as_str()))
            .map_err(|error| ProfileError::Invalid(error.to_string()))?;
        Ok(BTreeSet::from([id]))
    }

    fn prepare(
        &self,
        task: &TaskEnvelope,
        skills: &[SkillDocument],
        context: &ProfileContext,
    ) -> Result<ProfilePlan, ProfileError> {
        validate_task(&self.manifest, task, skills)?;
        if task.workspace.is_none() {
            return Err(ProfileError::Invalid(
                "developer profile requires an explicit workspace".to_owned(),
            ));
        }
        let track = task.track.as_ref().ok_or_else(|| {
            ProfileError::Invalid("developer profile requires one development track".to_owned())
        })?;
        let policy = track_policy(track)?;
        let mut messages = vec![ModelMessage::text(
            MessageRole::System,
            format!(
                "You are the Developer profile operating only in the {} track. Treat code, tests, configuration, and command output as evidence. Use semantic tools rather than inventing results. Filesystem and Git paths are workspace-relative. Preserve public behavior unless evidence requires change. One shared workspace has one mutation authority. Follow the selected track skill and do not invoke other development tracks. {}",
                track.as_str(),
                policy.instruction
            ),
        )];
        append_skills(&mut messages, skills);
        append_context(&mut messages, context)?;
        append_history(&mut messages, task);
        messages.push(ModelMessage::text(
            MessageRole::User,
            task.objective.clone(),
        ));
        let mut plan = ProfilePlan {
            manifest: self.manifest.clone(),
            messages,
            track: Some(track.clone()),
            allowed_tools: BTreeSet::from([
                "repo_map".to_owned(),
                "search_code".to_owned(),
                "read_file".to_owned(),
                "write_file".to_owned(),
                "replace_text".to_owned(),
                "run_check".to_owned(),
                "git_status".to_owned(),
                "inspect_diff".to_owned(),
                "request_input".to_owned(),
            ]),
            reasoning: policy.reasoning,
            memory_scope: task.workspace.clone(),
            execution_class: policy.execution_class,
            required_successful_tools: BTreeSet::new(),
            required_checks: Vec::new(),
            required_check_commands: BTreeMap::new(),
            required_successful_tools_after_mutation: policy
                .required_successful_tools_after_mutation
                .clone(),
            maximum_workers: policy.maximum_workers,
            topology: policy.topology,
            output_contract: None,
            axd_result_binding: None,
            effective_budget: task.budget.clone(),
            policy_budget_cap: Some(TaskBudget {
                maximum_transitions: policy.maximum_transitions,
                maximum_model_calls: policy.maximum_model_calls,
                maximum_tool_calls: policy.maximum_tool_calls,
                maximum_output_bytes: task.budget.maximum_output_bytes,
                timeout_ms: task.budget.timeout_ms,
            }),
        };
        apply_axd_execution_policy(&mut plan, task, context)?;
        Ok(plan)
    }
}

#[derive(Debug)]
struct ReviewerProfile {
    manifest: ProfileManifest,
}

impl ReviewerProfile {
    fn new() -> Result<Self, ProfileError> {
        Ok(Self {
            manifest: ProfileManifest {
                id: ProfileId::reviewer(),
                display_name: "Reviewer".to_owned(),
                description:
                    "Read-only, evidence-bound independent review of an immutable subject bundle."
                        .to_owned(),
                allowed_tracks: BTreeSet::from([
                    TrackId::review_neutral(),
                    TrackId::review_critical(),
                    TrackId::review_adversarial(),
                    TrackId::review_fact_checker(),
                ]),
                capabilities: capabilities(["model/text"])?,
                default_skills: BTreeSet::new(),
                allowed_skill_prefixes: BTreeSet::from(["reviewer/".to_owned()]),
                memory_policy: MemoryPolicy::Disabled,
                approval_policy: ApprovalPolicy::DenyEffects,
                concurrency: ConcurrencyPolicy::single(),
            },
        })
    }
}

impl ProfileDefinition for ReviewerProfile {
    fn manifest(&self) -> &ProfileManifest {
        &self.manifest
    }

    fn default_skills(&self, track: Option<&TrackId>) -> Result<BTreeSet<SkillId>, ProfileError> {
        let track = track.ok_or_else(|| {
            ProfileError::Invalid("reviewer profile requires one review lane".to_owned())
        })?;
        let skill = match track.as_str() {
            "review-neutral" => "reviewer/neutral",
            "review-critical" => "reviewer/critical",
            "review-adversarial" => "reviewer/adversarial",
            "review-fact-checker" => "reviewer/fact-checker",
            other => {
                return Err(ProfileError::Invalid(format!(
                    "reviewer lane {other} is not supported"
                )));
            }
        };
        Ok(BTreeSet::from([SkillId::parse(skill).map_err(
            |error| ProfileError::Invalid(error.to_string()),
        )?]))
    }

    fn prepare(
        &self,
        task: &TaskEnvelope,
        skills: &[SkillDocument],
        context: &ProfileContext,
    ) -> Result<ProfilePlan, ProfileError> {
        validate_task(&self.manifest, task, skills)?;
        if task.workspace.is_some() {
            return Err(ProfileError::Invalid(
                "reviewer profile cannot access a live workspace".to_owned(),
            ));
        }
        let track = task.track.as_ref().ok_or_else(|| {
            ProfileError::Invalid("reviewer profile requires one review lane".to_owned())
        })?;
        if !task.skills.is_empty()
            && task.skills.iter().cloned().collect::<BTreeSet<_>>()
                != self.default_skills(Some(track))?
        {
            return Err(ProfileError::Invalid(
                "reviewer task skills must match the selected review lane".to_owned(),
            ));
        }
        let mut messages = vec![ModelMessage::text(
            MessageRole::System,
            format!(
                "You are a read-only {} reviewer. You may reason only over the immutable subject bundle and explicitly supplied candidate findings. Never request tools, write files, use memory, inspect a live workspace, or claim evidence you did not receive. Return exactly the lane's JSON contract; uncertainty is a valid result.",
                track.as_str()
            ),
        )];
        append_skills(&mut messages, skills);
        append_context(&mut messages, context)?;
        append_history(&mut messages, task);
        messages.push(ModelMessage::text(
            MessageRole::User,
            task.objective.clone(),
        ));
        let plan = ProfilePlan {
            manifest: self.manifest.clone(),
            messages,
            track: Some(track.clone()),
            allowed_tools: BTreeSet::new(),
            reasoning: ReasoningEffort::High,
            memory_scope: None,
            // Review evidence is closed by `agent-review` after the runtime
            // returns: the lane parser, immutable lane record, and reducer
            // verify the model's JSON. The reviewer has no check/tool surface,
            // so the runtime's Verified class (which requires an executable
            // check or tool evidence) would be an invalid contract here.
            execution_class: ExecutionClass::Direct,
            required_successful_tools: BTreeSet::new(),
            required_checks: Vec::new(),
            required_check_commands: BTreeMap::new(),
            required_successful_tools_after_mutation: BTreeSet::new(),
            maximum_workers: 1,
            topology: ExecutionTopology::Single,
            output_contract: None,
            axd_result_binding: None,
            effective_budget: task.budget.clone(),
            policy_budget_cap: Some(TaskBudget {
                maximum_transitions: 32,
                maximum_model_calls: 1,
                maximum_tool_calls: 0,
                maximum_output_bytes: task.budget.maximum_output_bytes,
                timeout_ms: task.budget.timeout_ms,
            }),
        };
        Ok(plan)
    }

    fn validate_output(&self, output: &str, maximum_bytes: usize) -> Result<(), ProfileError> {
        if output.len() > maximum_bytes || output.trim().is_empty() {
            return Err(ProfileError::Invalid(
                "reviewer output is empty or exceeds its byte bound".to_owned(),
            ));
        }
        let value: serde_json::Value = serde_json::from_str(output).map_err(|error| {
            ProfileError::Invalid(format!("reviewer output must be JSON: {error}"))
        })?;
        if !value.is_object() {
            return Err(ProfileError::Invalid(
                "reviewer output must be a JSON object".to_owned(),
            ));
        }
        Ok(())
    }
}

#[derive(Debug)]
struct TwinProfile {
    manifest: ProfileManifest,
}

impl TwinProfile {
    fn new() -> Result<Self, ProfileError> {
        Ok(Self {
            manifest: ProfileManifest {
                id: ProfileId::twin(),
                display_name: "Twin".to_owned(),
                description: "A correctable, provenance-aware user model for long-term conversation and goals.".to_owned(),
                allowed_tracks: BTreeSet::new(),
                capabilities: capabilities(["model/text", "memory/read", "memory/write"] )?,
                default_skills: skills(["twin/conversation"] )?,
                allowed_skill_prefixes: BTreeSet::from(["twin/".to_owned()]),
                memory_policy: MemoryPolicy::LongTermScoped,
                approval_policy: ApprovalPolicy::AllowTaskGrant,
                concurrency: ConcurrencyPolicy::single(),
            },
        })
    }
}

impl ProfileDefinition for TwinProfile {
    fn manifest(&self) -> &ProfileManifest {
        &self.manifest
    }

    fn prepare(
        &self,
        task: &TaskEnvelope,
        skills: &[SkillDocument],
        context: &ProfileContext,
    ) -> Result<ProfilePlan, ProfileError> {
        validate_task(&self.manifest, task, skills)?;
        let scope = context
            .values
            .get("memoryScope")
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| {
                ProfileError::Invalid("twin profile requires context.memoryScope".to_owned())
            })?;
        let mut messages = vec![ModelMessage::text(
            MessageRole::System,
            "You are the Twin profile. You model the user probabilistically; never claim to be the user. Separate observed facts from inference, preserve provenance and uncertainty, expose conflicts, and ask approval before durable memory changes or high-impact effects.",
        )];
        append_skills(&mut messages, skills);
        append_context(&mut messages, context)?;
        if !context.recalled_memory.is_empty() {
            let memory = context
                .recalled_memory
                .iter()
                .enumerate()
                .map(|(index, value)| format!("{}. {value}", index + 1))
                .collect::<Vec<_>>()
                .join("\n");
            messages.push(ModelMessage::text(
                MessageRole::Developer,
                format!("Scoped memory candidates. Treat them as evidence, not absolute truth:\n{memory}"),
            ));
        }
        append_history(&mut messages, task);
        messages.push(ModelMessage::text(
            MessageRole::User,
            task.objective.clone(),
        ));
        let mut plan = ProfilePlan {
            manifest: self.manifest.clone(),
            messages,
            track: task.track.clone(),
            allowed_tools: BTreeSet::from([
                "remember".to_owned(),
                "forget_memory".to_owned(),
                "request_input".to_owned(),
            ]),
            reasoning: ReasoningEffort::High,
            memory_scope: Some(scope.to_owned()),
            execution_class: ExecutionClass::Guided,
            required_successful_tools: BTreeSet::new(),
            required_checks: Vec::new(),
            required_check_commands: BTreeMap::new(),
            required_successful_tools_after_mutation: BTreeSet::new(),
            maximum_workers: 1,
            topology: ExecutionTopology::Single,
            output_contract: None,
            axd_result_binding: None,
            effective_budget: task.budget.clone(),
            policy_budget_cap: None,
        };
        apply_axd_execution_policy(&mut plan, task, context)?;
        Ok(plan)
    }
}

fn validate_task(
    manifest: &ProfileManifest,
    task: &TaskEnvelope,
    loaded_skills: &[SkillDocument],
) -> Result<(), ProfileError> {
    task.validate().map_err(ProfileError::Invalid)?;
    if task.profile != manifest.id {
        return Err(ProfileError::Invalid(
            "task profile does not match definition".to_owned(),
        ));
    }
    if !manifest.allows_track(task.track.as_ref()) {
        return Err(ProfileError::Invalid(
            "task track is not allowed for profile".to_owned(),
        ));
    }
    for skill in loaded_skills {
        let id = &skill.descriptor.manifest.id;
        if !manifest.allows_skill(id) {
            return Err(ProfileError::Invalid(format!(
                "skill {id} is outside the profile allowlist"
            )));
        }
        if !skill
            .descriptor
            .manifest
            .required_capabilities
            .is_subset(&manifest.capabilities)
        {
            return Err(ProfileError::Invalid(format!(
                "skill {id} requires capabilities outside the profile"
            )));
        }
    }
    Ok(())
}

fn append_skills(messages: &mut Vec<ModelMessage>, skills: &[SkillDocument]) {
    for skill in skills {
        messages.push(ModelMessage::text(
            MessageRole::Developer,
            format!(
                "Activated task skill {} @ {}:\n{}",
                skill.descriptor.manifest.id,
                skill.descriptor.manifest.version,
                skill.instructions.trim()
            ),
        ));
    }
}

fn append_history(messages: &mut Vec<ModelMessage>, task: &TaskEnvelope) {
    messages.extend(task.history.iter().map(|turn| {
        let role = match turn.role {
            ConversationRole::User => MessageRole::User,
            ConversationRole::Assistant => MessageRole::Assistant,
        };
        ModelMessage::text(role, turn.content.clone())
    }));
}

fn append_context(
    messages: &mut Vec<ModelMessage>,
    context: &ProfileContext,
) -> Result<(), ProfileError> {
    if context.values.is_empty() {
        return Ok(());
    }
    let encoded = serde_json::to_string(&context.values)
        .map_err(|error| ProfileError::Invalid(error.to_string()))?;
    if encoded.len() > 256 * 1024 {
        return Err(ProfileError::Invalid(
            "profile context exceeds the 256 KiB bound".to_owned(),
        ));
    }
    messages.push(ModelMessage::text(
        MessageRole::Developer,
        format!("Task-scoped context JSON:\n{encoded}"),
    ));
    Ok(())
}

fn capabilities<const N: usize>(values: [&str; N]) -> Result<BTreeSet<CapabilityId>, ProfileError> {
    values
        .into_iter()
        .map(|value| {
            CapabilityId::parse(value).map_err(|error| ProfileError::Invalid(error.to_string()))
        })
        .collect()
}

fn skills<const N: usize>(values: [&str; N]) -> Result<BTreeSet<SkillId>, ProfileError> {
    values
        .into_iter()
        .map(|value| {
            SkillId::parse(value).map_err(|error| ProfileError::Invalid(error.to_string()))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn built_in_profile_plugin_exposes_the_same_closed_catalog() {
        let plugin = BuiltInProfilePlugin::new().expect("profile plugin");
        assert_eq!(plugin.manifest().id().as_str(), BUILT_IN_PROFILE_PLUGIN_ID);
        assert!(
            plugin
                .manifest()
                .contributions()
                .contains(&ContributionKind::Profiles)
        );
        let mut registry = ProfileRegistry::default();
        plugin
            .register_profiles(&mut registry)
            .expect("register built-in profiles");
        assert_eq!(registry.manifests().len(), 4);
    }
    use agent_core::{
        ArtifactId, ArtifactRef, ConversationRole, ConversationTurn, EffectApprovalMode,
        IMAGE_TASK_CONTRACT_SCHEMA, ImageSource, ImageTaskContract, ImageVerificationContract,
        SessionId, TaskId,
    };

    #[test]
    fn built_in_profiles_are_closed_and_extensible_through_registration()
    -> Result<(), Box<dyn std::error::Error>> {
        let registry = built_in_registry()?;
        assert_eq!(registry.manifests().len(), 4);
        assert!(registry.resolve(&ProfileId::developer()).is_ok());
        assert!(registry.resolve(&ProfileId::reviewer()).is_ok());
        Ok(())
    }

    #[test]
    fn reviewer_profile_is_read_only_and_selects_one_lane_skill()
    -> Result<(), Box<dyn std::error::Error>> {
        let profile = built_in_registry()?.resolve(&ProfileId::reviewer())?;
        let track = TrackId::review_adversarial();
        assert_eq!(
            profile.default_skills(Some(&track))?,
            BTreeSet::from([SkillId::parse("reviewer/adversarial")?])
        );
        let task = TaskEnvelope {
            schema_version: agent_core::PROTOCOL_VERSION,
            task_id: TaskId::parse("reviewer-task")?,
            session_id: SessionId::parse("reviewer-session")?,
            parent_task_id: None,
            profile: ProfileId::reviewer(),
            track: Some(track),
            objective: "{\"summary\":\"ok\",\"findings\":[]}".to_owned(),
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
        let plan = profile.prepare(&task, &[], &ProfileContext::default())?;
        assert!(plan.allowed_tools.is_empty());
        assert_eq!(plan.execution_class, ExecutionClass::Direct);
        assert_eq!(plan.manifest.memory_policy, MemoryPolicy::Disabled);
        assert_eq!(plan.manifest.approval_policy, ApprovalPolicy::DenyEffects);
        assert!(profile.validate_output("{}", 32).is_ok());
        assert!(profile.validate_output("[]", 32).is_err());
        Ok(())
    }

    #[test]
    fn duplicate_registration_preserves_the_original_profile()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut registry = built_in_registry()?;
        let mut replacement = CreatorProfile::new()?;
        replacement.manifest.display_name = "Replacement".to_owned();

        let error = registry
            .register(Arc::new(replacement))
            .expect_err("duplicate profile registration must fail");
        assert!(matches!(error, ProfileError::Duplicate(id) if id == ProfileId::creator()));
        assert_eq!(
            registry
                .resolve(&ProfileId::creator())?
                .manifest()
                .display_name,
            "Creator"
        );
        Ok(())
    }

    #[test]
    fn developer_prompt_selects_exactly_one_track() -> Result<(), Box<dyn std::error::Error>> {
        let profile = DeveloperProfile::new()?;
        let task = TaskEnvelope {
            schema_version: agent_core::PROTOCOL_VERSION,
            task_id: TaskId::parse("task-prompt-contract")?,
            session_id: SessionId::parse("session-prompt-contract")?,
            parent_task_id: None,
            profile: ProfileId::developer(),
            track: Some(TrackId::builder()),
            objective: "diagnose the repository".to_owned(),
            image_task: None,
            history: Vec::new(),
            workspace: Some("workspace".to_owned()),
            skills: Vec::new(),
            context: BTreeMap::new(),
            budget: agent_core::TaskBudget::default(),
            effect_approval: EffectApprovalMode::Deny,
            allow_memory_write: false,
            approvals: Vec::new(),
        };
        let plan = profile.prepare(&task, &[], &ProfileContext::default())?;
        let system = &plan.messages[0].content;
        assert!(system.contains("operating only in the builder track"));
        assert!(system.contains("do not invoke other development tracks"));
        for track in [
            TrackId::prototyper(),
            TrackId::builder(),
            TrackId::grower(),
            TrackId::sweeper(),
            TrackId::maintainer(),
        ] {
            assert_eq!(
                profile.default_skills(Some(&track))?,
                BTreeSet::from([SkillId::parse(format!("developer/{}", track.as_str()))?])
            );
        }
        Ok(())
    }

    #[test]
    fn axd_policy_preserves_provider_turn_boundary_for_developer_tracks()
    -> Result<(), Box<dyn std::error::Error>> {
        let profile = DeveloperProfile::new()?;
        for (index, track) in [
            TrackId::prototyper(),
            TrackId::builder(),
            TrackId::sweeper(),
            TrackId::grower(),
            TrackId::maintainer(),
        ]
        .into_iter()
        .enumerate()
        {
            let task = TaskEnvelope {
                schema_version: agent_core::PROTOCOL_VERSION,
                task_id: TaskId::parse(format!("task-axd-turn-{index}"))?,
                session_id: SessionId::parse(format!("session-axd-turn-{index}"))?,
                parent_task_id: None,
                profile: ProfileId::developer(),
                track: Some(track),
                objective: "complete the bounded task".to_owned(),
                image_task: None,
                history: vec![
                    ConversationTurn {
                        role: ConversationRole::User,
                        content: "preserve this request".to_owned(),
                    },
                    ConversationTurn {
                        role: ConversationRole::Assistant,
                        content: "preserve this response".to_owned(),
                    },
                ],
                workspace: Some("workspace".to_owned()),
                skills: Vec::new(),
                context: BTreeMap::new(),
                budget: agent_core::TaskBudget::default(),
                effect_approval: EffectApprovalMode::Deny,
                allow_memory_write: false,
                approvals: Vec::new(),
            };
            let plan = profile.prepare(&task, &[], &ProfileContext::default())?;
            let final_message = plan
                .messages
                .last()
                .ok_or("missing final provider message")?;
            assert_eq!(final_message.role, MessageRole::User);
            assert_eq!(final_message.content, task.objective);

            let policy_indices = plan
                .messages
                .iter()
                .enumerate()
                .filter_map(|(index, message)| {
                    message
                        .content
                        .starts_with("AXD execution policy:")
                        .then_some(index)
                })
                .collect::<Vec<_>>();
            assert_eq!(policy_indices.len(), 1);
            assert_eq!(policy_indices[0] + 1, plan.messages.len() - 1);
            assert!(plan.messages.iter().any(|message| {
                message.role == MessageRole::User && message.content == "preserve this request"
            }));
            assert!(plan.messages.iter().any(|message| {
                message.role == MessageRole::Assistant
                    && message.content == "preserve this response"
            }));
        }
        Ok(())
    }

    #[test]
    fn built_in_profile_defaults_apply_real_execution_classes_and_budgets()
    -> Result<(), Box<dyn std::error::Error>> {
        let developer = DeveloperProfile::new()?;
        let mut task = TaskEnvelope {
            schema_version: agent_core::PROTOCOL_VERSION,
            task_id: TaskId::parse("task-default-grounded")?,
            session_id: SessionId::parse("session-default-grounded")?,
            parent_task_id: None,
            profile: ProfileId::developer(),
            track: Some(TrackId::builder()),
            objective: "inspect and repair the repository".to_owned(),
            image_task: None,
            history: Vec::new(),
            workspace: Some("workspace".to_owned()),
            skills: Vec::new(),
            context: BTreeMap::new(),
            budget: agent_core::TaskBudget::default(),
            effect_approval: EffectApprovalMode::Deny,
            allow_memory_write: false,
            approvals: Vec::new(),
        };
        task.budget.timeout_ms = 42 * 60 * 1_000;
        let plan = developer.prepare(&task, &[], &ProfileContext::default())?;
        assert_eq!(plan.execution_class, ExecutionClass::Grounded);
        assert_eq!(plan.reasoning, ReasoningEffort::High);
        assert_eq!(
            plan.topology,
            ExecutionTopology::SingleWithIndependentVerifier
        );
        assert_eq!(plan.maximum_workers, 1);
        assert_eq!(plan.effective_budget.maximum_model_calls, 12);
        assert_eq!(plan.effective_budget.maximum_tool_calls, 32);
        assert_eq!(plan.effective_budget.timeout_ms, task.budget.timeout_ms);

        let creator = CreatorProfile::new()?;
        let creator_task = TaskEnvelope {
            schema_version: agent_core::PROTOCOL_VERSION,
            task_id: TaskId::parse("task-default-guided")?,
            session_id: SessionId::parse("session-default-guided")?,
            parent_task_id: None,
            profile: ProfileId::creator(),
            track: None,
            objective: "write a short design note".to_owned(),
            image_task: None,
            history: Vec::new(),
            workspace: None,
            skills: Vec::new(),
            context: BTreeMap::new(),
            budget: agent_core::TaskBudget::default(),
            effect_approval: EffectApprovalMode::Deny,
            allow_memory_write: false,
            approvals: Vec::new(),
        };
        let creator_plan = creator.prepare(&creator_task, &[], &ProfileContext::default())?;
        assert_eq!(creator_plan.execution_class, ExecutionClass::Guided);
        assert_eq!(creator_plan.reasoning, ReasoningEffort::Medium);
        assert_eq!(creator_plan.topology, ExecutionTopology::Single);
        assert_eq!(creator_plan.effective_budget.maximum_model_calls, 6);
        assert_eq!(creator_plan.effective_budget.maximum_tool_calls, 20);
        Ok(())
    }

    #[test]
    fn creator_projects_image_contract_on_canonical_tools() -> Result<(), Box<dyn std::error::Error>>
    {
        let creator = CreatorProfile::new()?;
        let mut task = TaskEnvelope {
            schema_version: agent_core::PROTOCOL_VERSION,
            task_id: TaskId::parse("task-image-contract")?,
            session_id: SessionId::parse("session-image-contract")?,
            parent_task_id: None,
            profile: ProfileId::creator(),
            track: None,
            objective: "edit the provided image".to_owned(),
            image_task: None,
            history: Vec::new(),
            workspace: None,
            skills: Vec::new(),
            context: BTreeMap::new(),
            budget: agent_core::TaskBudget::default(),
            effect_approval: EffectApprovalMode::Deny,
            allow_memory_write: false,
            approvals: Vec::new(),
        };
        let source_digest = "a".repeat(64);
        task.image_task = Some(ImageTaskContract {
            schema: IMAGE_TASK_CONTRACT_SCHEMA.to_owned(),
            goal: "edit the provided image".to_owned(),
            source: ImageSource::Provided {
                artifact: ArtifactRef {
                    id: ArtifactId::parse(format!("art-{source_digest}"))?,
                    sha256: source_digest.clone(),
                    media_type: "image/png".to_owned(),
                    byte_length: 1,
                    relative_path: format!("aa/{source_digest}.png"),
                },
            },
            maximum_attempts: 3,
            preserve: vec!["preserve unspecified pixels".to_owned()],
            visual_criteria: vec!["review the latest candidate".to_owned()],
            verification: ImageVerificationContract {
                visual_review: true,
                deterministic: None,
            },
        });

        let plan = creator.prepare(&task, &[], &ProfileContext::default())?;

        assert!(plan.allowed_tools.contains("image_extract"));
        assert!(plan.allowed_tools.contains("image_render"));
        assert!(plan.messages.iter().any(|message| {
            message
                .content
                .contains("Authoritative image task contract:")
                && message.content.contains(IMAGE_TASK_CONTRACT_SCHEMA)
        }));
        Ok(())
    }

    #[test]
    fn developer_tracks_have_distinct_execution_and_mutation_evidence_policies()
    -> Result<(), Box<dyn std::error::Error>> {
        let profile = DeveloperProfile::new()?;
        let tracks = [
            (
                TrackId::prototyper(),
                ExecutionClass::Guided,
                ExecutionTopology::ParallelRead,
                2,
                0,
            ),
            (
                TrackId::builder(),
                ExecutionClass::Grounded,
                ExecutionTopology::SingleWithIndependentVerifier,
                1,
                1,
            ),
            (
                TrackId::sweeper(),
                ExecutionClass::Grounded,
                ExecutionTopology::SingleWithIndependentVerifier,
                1,
                2,
            ),
            (
                TrackId::grower(),
                ExecutionClass::Grounded,
                ExecutionTopology::ParallelRead,
                2,
                1,
            ),
            (
                TrackId::maintainer(),
                ExecutionClass::Verified,
                ExecutionTopology::SingleWithIndependentVerifier,
                1,
                2,
            ),
        ];
        let track_count = tracks.len();
        let mut fingerprints = BTreeSet::new();
        for (index, (track, class, topology, workers, mutation_evidence)) in
            tracks.into_iter().enumerate()
        {
            let task = TaskEnvelope {
                schema_version: agent_core::PROTOCOL_VERSION,
                task_id: TaskId::parse(format!("task-track-matrix-{index}"))?,
                session_id: SessionId::parse(format!("session-track-matrix-{index}"))?,
                parent_task_id: None,
                profile: ProfileId::developer(),
                track: Some(track),
                objective: "inspect the bounded task".to_owned(),
                image_task: None,
                history: Vec::new(),
                workspace: Some("workspace".to_owned()),
                skills: Vec::new(),
                context: BTreeMap::new(),
                budget: agent_core::TaskBudget::default(),
                effect_approval: EffectApprovalMode::Deny,
                allow_memory_write: false,
                approvals: Vec::new(),
            };
            let plan = profile.prepare(&task, &[], &ProfileContext::default())?;
            assert_eq!(plan.execution_class, class);
            assert_eq!(plan.topology, topology);
            assert_eq!(plan.maximum_workers, workers);
            assert_eq!(
                plan.required_successful_tools_after_mutation.len(),
                mutation_evidence
            );
            fingerprints.insert(format!(
                "{}:{}:{}:{}",
                plan.execution_class.as_str(),
                plan.topology.as_str(),
                plan.maximum_workers,
                plan.required_successful_tools_after_mutation.len()
            ));
        }
        assert_eq!(fingerprints.len(), track_count);
        Ok(())
    }

    #[test]
    fn direct_and_verified_policies_change_the_actual_profile_plan()
    -> Result<(), Box<dyn std::error::Error>> {
        let profile = DeveloperProfile::new()?;
        let mut direct = TaskEnvelope {
            schema_version: agent_core::PROTOCOL_VERSION,
            task_id: TaskId::parse("task-direct-policy")?,
            session_id: SessionId::parse("session-direct-policy")?,
            parent_task_id: None,
            profile: ProfileId::developer(),
            track: Some(TrackId::builder()),
            objective: "inspect the repository".to_owned(),
            image_task: None,
            history: Vec::new(),
            workspace: Some("workspace".to_owned()),
            skills: Vec::new(),
            context: BTreeMap::from([(
                "axd".to_owned(),
                serde_json::json!({
                    "execution_level":"direct",
                    "required_tools":["workspace.read"]
                }),
            )]),
            budget: agent_core::TaskBudget::default(),
            effect_approval: EffectApprovalMode::Deny,
            allow_memory_write: false,
            approvals: Vec::new(),
        };
        let direct_plan = profile.prepare(&direct, &[], &ProfileContext::default())?;
        assert_eq!(direct_plan.execution_class, ExecutionClass::Direct);
        assert_eq!(direct_plan.reasoning, ReasoningEffort::Low);
        assert!(!direct_plan.allowed_tools.contains("write_file"));

        direct.task_id = TaskId::parse("task-direct-mutation-policy")?;
        direct.session_id = SessionId::parse("session-direct-mutation-policy")?;
        direct.context.insert(
            "axd".to_owned(),
            serde_json::json!({
                "execution_level":"direct",
                "required_tools":["workspace.write"]
            }),
        );
        assert!(
            profile
                .prepare(&direct, &[], &ProfileContext::default())
                .expect_err("a direct AXD policy must not reintroduce mutation authority")
                .to_string()
                .contains("required tools")
        );

        direct.task_id = TaskId::parse("task-verified-policy")?;
        direct.session_id = SessionId::parse("session-verified-policy")?;
        direct.context.insert(
            "axd".to_owned(),
            serde_json::json!({
                "execution_level":"verified",
                "required_tools":["workspace.read","workspace.write","check.run"],
                "required_checks":["build","unit"],
                "max_workers":1,
                "output_contract":"structured_result_with_evidence"
            }),
        );
        let verified = profile.prepare(&direct, &[], &ProfileContext::default())?;
        assert_eq!(verified.execution_class, ExecutionClass::Verified);
        assert_eq!(verified.reasoning, ReasoningEffort::High);
        assert!(verified.allowed_tools.contains("write_file"));
        assert!(verified.allowed_tools.contains("run_check"));
        assert!(verified.required_successful_tools.contains("run_check"));
        assert!(
            verified
                .required_successful_tools_after_mutation
                .contains("inspect_diff")
        );
        assert_eq!(verified.maximum_workers, 1);
        Ok(())
    }

    #[test]
    fn malformed_axd_policy_fails_closed() -> Result<(), Box<dyn std::error::Error>> {
        let profile = CreatorProfile::new()?;
        let task = TaskEnvelope {
            schema_version: agent_core::PROTOCOL_VERSION,
            task_id: TaskId::parse("task-malformed-policy")?,
            session_id: SessionId::parse("session-malformed-policy")?,
            parent_task_id: None,
            profile: ProfileId::creator(),
            track: None,
            objective: "write".to_owned(),
            image_task: None,
            history: Vec::new(),
            workspace: None,
            skills: Vec::new(),
            context: BTreeMap::from([("axd".to_owned(), Value::String("not-json".to_owned()))]),
            budget: agent_core::TaskBudget::default(),
            effect_approval: EffectApprovalMode::Deny,
            allow_memory_write: false,
            approvals: Vec::new(),
        };
        assert!(
            profile
                .prepare(&task, &[], &ProfileContext::default())
                .is_err()
        );
        Ok(())
    }

    #[test]
    fn prepared_v2_policy_accepts_an_opaque_source_revision_and_binds_checks()
    -> Result<(), Box<dyn std::error::Error>> {
        let profile = DeveloperProfile::new()?;
        let task = TaskEnvelope {
            schema_version: agent_core::PROTOCOL_VERSION,
            task_id: TaskId::parse("task-v2-policy")?,
            session_id: SessionId::parse("session-v2-policy")?,
            parent_task_id: None,
            profile: ProfileId::developer(),
            track: Some(TrackId::builder()),
            objective: "implement and verify the selected work item".to_owned(),
            image_task: None,
            history: Vec::new(),
            workspace: Some("workspace".to_owned()),
            skills: Vec::new(),
            context: BTreeMap::from([(
                "axd".to_owned(),
                serde_json::json!({
                    "contract_digest":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                    "work_item_id":"work-1",
                    "source_revision":"source-1",
                    "contract_revision":2,
                    "runtime_policy":{
                        "execution_class":"verified",
                        "maximum_workers":1,
                        "topology":"single_with_independent_verifier",
                        "required_tools":["workspace.read","workspace.write","check.run"],
                        "checks":[
                            {
                                "requirement_id":"build",
                                "kind":"build",
                                "required":true,
                                "argv":["cargo","check","--workspace"]
                            },
                            {"requirement_id":"optional-lint","kind":"lint","required":false}
                        ],
                        "output_schema":"axd.agent-result/2"
                    }
                }),
            )]),
            budget: agent_core::TaskBudget::default(),
            effect_approval: EffectApprovalMode::Deny,
            allow_memory_write: false,
            approvals: Vec::new(),
        };
        let plan = profile.prepare(&task, &[], &ProfileContext::default())?;
        assert_eq!(plan.execution_class, ExecutionClass::Verified);
        assert_eq!(
            plan.topology,
            ExecutionTopology::SingleWithIndependentVerifier
        );
        assert_eq!(plan.required_checks, vec!["build".to_owned()]);
        assert!(plan.required_successful_tools.contains("run_check"));
        assert_eq!(
            plan.required_check_commands.get("build"),
            Some(&vec![
                "cargo".to_owned(),
                "check".to_owned(),
                "--workspace".to_owned(),
            ])
        );
        let binding = plan.axd_result_binding.ok_or("missing result binding")?;
        assert_eq!(binding.source_revision, "source-1");
        assert_eq!(
            binding.check_kinds.get("build").map(String::as_str),
            Some("build")
        );
        assert!(!binding.check_kinds.contains_key("optional-lint"));
        Ok(())
    }

    #[test]
    fn malformed_prepared_check_plan_fails_closed() -> Result<(), Box<dyn std::error::Error>> {
        let profile = DeveloperProfile::new()?;
        let task = TaskEnvelope {
            schema_version: agent_core::PROTOCOL_VERSION,
            task_id: TaskId::parse("task-malformed-check-plan")?,
            session_id: SessionId::parse("session-malformed-check-plan")?,
            parent_task_id: None,
            profile: ProfileId::developer(),
            track: Some(TrackId::builder()),
            objective: "verify".to_owned(),
            image_task: None,
            history: Vec::new(),
            workspace: Some("workspace".to_owned()),
            skills: Vec::new(),
            context: BTreeMap::from([(
                "axd".to_owned(),
                serde_json::json!({
                    "runtime_policy":{
                        "execution_class":"verified",
                        "checks":"build"
                    }
                }),
            )]),
            budget: agent_core::TaskBudget::default(),
            effect_approval: EffectApprovalMode::Deny,
            allow_memory_write: false,
            approvals: Vec::new(),
        };
        assert!(
            profile
                .prepare(&task, &[], &ProfileContext::default())
                .is_err()
        );
        Ok(())
    }
}
