use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
};

use agent_core::{
    AgentSpec, CompositionReceipt, DataExtensionSpec, EffectApprovalMode, EffectApprovalSpec,
    EffectsSpec, MemorySpec, PluginTier, PluginTierBinding, ProfileId, ProviderBinding,
    SelectedPluginBinding, SelectedSkillBinding, SkillId, TaskEnvelope, TaskEnvelopeV3, TrackId,
};
use agent_profile_api::{CompositionPluginCatalog, ProfileContext, ProfileError, ProfileRegistry};
use agent_provider_api::ProviderPluginEnvelope;
use agent_skills::{DataExtensionDocument, SkillCatalog, load_data_extension};

/// Default linked profile catalog for the stock `agent` binary. Composition
/// and runtime receive the same registry snapshot; neither side re-derives
/// profile authority independently.
pub fn profiles() -> Result<ProfileRegistry, ProfileError> {
    stock_plugin_catalog()
        .and_then(|catalog| catalog.profiles().map_err(|error| error.to_string()))
        .map_err(ProfileError::Invalid)
}

/// Builds the explicit stock plugin snapshot used by compose, run, and the
/// review host. The catalog is linked Rust code only; no executable discovery
/// or dynamic loader is involved.
pub fn stock_plugin_catalog() -> Result<CompositionPluginCatalog, String> {
    let mut catalog = CompositionPluginCatalog::new();
    for plugin in agent_provider::linked_provider_plugins()? {
        catalog
            .register_provider(plugin)
            .map_err(|error| error.to_string())?;
    }
    for plugin in agent_profiles::linked_profile_plugins()? {
        catalog
            .register_profile(plugin)
            .map_err(|error| error.to_string())?;
    }
    catalog.finalize().map_err(|error| error.to_string())?;
    Ok(catalog)
}

/// One pure host-side resolution result. It is consumed by both `compose` and
/// `run`; neither caller is allowed to re-derive profile/tool/skill authority.
#[derive(Debug)]
pub struct ExplicitResolution {
    pub task: TaskEnvelope,
    pub receipt: CompositionReceipt,
    /// The exact pre-provider inputs used during resolution. Run reuses these
    /// values instead of rescanning a mutable skills directory after the
    /// receipt has been persisted.
    pub skill_roots: Vec<PathBuf>,
    pub skill_catalog: SkillCatalog,
    pub profiles: ProfileRegistry,
    pub plugin_catalog: CompositionPluginCatalog,
    pub provider_plugin_id: String,
    pub provider_plugin_manifest_sha256: String,
    pub profile_plugin_id: String,
    pub profile_plugin_manifest_sha256: String,
}

/// Resolve using one caller-selected linked provider/profile catalog. The
/// selected provider plugin owns configuration parsing and derives the binding;
/// this function validates and seals the resulting immutable facts.
pub fn resolve_explicit_with_catalog(
    task: TaskEnvelopeV3,
    spec: &AgentSpec,
    provider_selection: &ProviderPluginEnvelope,
    skill_catalog: &SkillCatalog,
    skill_roots: Vec<PathBuf>,
    plugin_catalog: CompositionPluginCatalog,
) -> Result<ExplicitResolution, String> {
    let provider_plugin = plugin_catalog
        .provider_for_envelope(provider_selection)
        .map_err(|error| error.to_string())?;
    let provider = provider_plugin
        .resolve_binding(&provider_selection.configuration)
        .map_err(|error| error.to_string())?;
    let profiles = plugin_catalog
        .profiles()
        .map_err(|error| error.to_string())?;
    task.validate().map_err(|error| error.to_string())?;
    let mut spec = spec.resolve().map_err(|error| error.to_string())?;
    spec.validate_provider(&provider)
        .map_err(|error| error.to_string())?;
    if let Some(semantic_mask) = spec.semantic_mask.as_ref() {
        spec.semantic_mask = Some(
            agent_runtime::ProcessSemanticMaskAdapter::resolve_spec(semantic_mask)
                .map_err(|error| format!("semantic-mask helper composition is invalid: {error}"))?,
        );
    }
    validate_provider_budget(task.budget.maximum_output_bytes, &provider)?;
    let profile_id = ProfileId::parse(spec.profile.clone()).map_err(|error| error.to_string())?;
    let profile = profiles
        .resolve(&profile_id)
        .map_err(|error| error.to_string())?;
    let profile_plugin_binding = plugin_catalog
        .profile_plugin_binding(&profile_id)
        .map_err(|error| error.to_string())?;
    let workflow_owned_skills =
        profile_id == ProfileId::developer() || profile_id == ProfileId::reviewer();
    let track = spec
        .workflow
        .as_deref()
        .map(|value| TrackId::parse(value.to_owned()).map_err(|error| error.to_string()))
        .transpose()?;
    if !profile.manifest().allows_track(track.as_ref()) {
        return Err(format!(
            "workflow {} is not allowed for profile {}",
            spec.workflow.as_deref().unwrap_or("<none>"),
            spec.profile
        ));
    }
    if spec.profile == ProfileId::developer().as_str() && track.is_none() {
        return Err("developer profile requires one explicit workflow".to_owned());
    }
    if spec.profile == ProfileId::twin().as_str() && spec.memory == MemorySpec::None {
        return Err("twin profile requires an explicit memory authority".to_owned());
    }

    if spec.skills.is_empty() {
        return Err(
            "AgentSpec skills must be explicitly selected; empty does not expand profile defaults"
                .to_owned(),
        );
    }
    let default_skills = profile
        .default_skills(track.as_ref())
        .map_err(|error| error.to_string())?;
    let requested_skills = spec
        .skills
        .iter()
        .map(|value| SkillId::parse(value.clone()).map_err(|error| error.to_string()))
        .collect::<Result<Vec<_>, _>>()?;
    if workflow_owned_skills
        && requested_skills.iter().cloned().collect::<BTreeSet<_>>() != default_skills
    {
        return Err(format!(
            "{profile_id} skills are selected by workflow and must match its default skill"
        ));
    }
    let mut requested_skills = requested_skills;
    requested_skills.sort();
    requested_skills.dedup();
    let mut documents = requested_skills
        .iter()
        .map(|skill| {
            skill_catalog
                .load(skill, &profile_id)
                .map_err(|error| error.to_string())
        })
        .collect::<Result<Vec<_>, _>>()?;

    // Tier-2/3 extensions are data-only packages. Resolve their bytes during
    // composition and merge the resulting immutable skill documents into the
    // catalog snapshot used by run. No executable extension loader exists.
    let mut data_extensions = Vec::<DataExtensionDocument>::new();
    for extension in &spec.extensions {
        let (id, source, approved) = match extension {
            DataExtensionSpec::Approved {
                id,
                source,
                approved_sha256,
            } => (id.as_str(), source.as_str(), Some(approved_sha256.as_str())),
            DataExtensionSpec::Dynamic { id, source } => (id.as_str(), source.as_str(), None),
        };
        let document = load_data_extension(
            std::path::Path::new(source),
            id,
            profile.manifest(),
            approved,
        )
        .map_err(|error| error.to_string())?;
        if document.skills.iter().any(|skill| {
            documents
                .iter()
                .any(|existing| existing.descriptor.manifest.id == skill.descriptor.manifest.id)
        }) {
            return Err(format!("data extension {id} duplicates a selected skill"));
        }
        for skill in &document.skills {
            if !profile
                .manifest()
                .allows_skill(&skill.descriptor.manifest.id)
                || !skill
                    .descriptor
                    .manifest
                    .required_capabilities
                    .is_subset(&profile.manifest().capabilities)
            {
                return Err(format!(
                    "data extension {id} emitted a skill outside profile authority"
                ));
            }
        }
        data_extensions.push(document);
    }
    for extension in &data_extensions {
        documents.extend(extension.skills.clone());
    }
    documents.sort_by(|left, right| {
        left.descriptor
            .manifest
            .id
            .cmp(&right.descriptor.manifest.id)
    });
    let resolved_skill_ids = documents
        .iter()
        .map(|document| document.descriptor.manifest.id.clone())
        .collect::<Vec<_>>();
    let mut resolved_catalog = skill_catalog.clone();
    for extension in &data_extensions {
        resolved_catalog
            .merge_data_documents(extension.skills.clone())
            .map_err(|error| error.to_string())?;
    }

    let mut context_values = task.context.clone().into_iter().collect::<BTreeMap<_, _>>();
    let profile_context = ProfileContext {
        recalled_memory: Vec::new(),
        values: context_values.clone(),
        available_tools: BTreeSet::new(),
    };
    let internal_task = TaskEnvelope {
        schema_version: agent_core::PROTOCOL_VERSION,
        task_id: task.task_id.clone(),
        session_id: task.session_id.clone(),
        parent_task_id: task.parent_task_id.clone(),
        profile: profile_id.clone(),
        track: track.clone(),
        objective: task.objective.clone(),
        image_task: task.image_task.clone(),
        history: task.history.clone(),
        workspace: task.workspace.clone(),
        // Extension skills are part of the same immutable composition.  The
        // runtime must receive the complete resolved set, not only the
        // workflow's base selection.
        skills: resolved_skill_ids.clone(),
        context: context_values.clone(),
        budget: task.budget.clone(),
        effect_approval: match spec.effect_approval {
            EffectApprovalSpec::Deny => EffectApprovalMode::Deny,
            EffectApprovalSpec::PlanOnly => EffectApprovalMode::PlanOnly,
            EffectApprovalSpec::TaskGrant => EffectApprovalMode::TaskGrant,
        },
        allow_memory_write: matches!(spec.memory, MemorySpec::LongTermScoped),
        approvals: task.approvals.clone(),
    };
    let plan = profile
        .prepare(&internal_task, &documents, &profile_context)
        .map_err(|error| error.to_string())?;
    if spec.tools.is_empty() && !plan.allowed_tools.is_empty() {
        return Err(
            "AgentSpec tools must be explicitly selected; empty does not expand profile defaults"
                .to_owned(),
        );
    }
    let selected_tools = spec.tools.iter().cloned().collect::<BTreeSet<_>>();
    if !selected_tools.is_subset(&plan.allowed_tools) {
        let unknown = selected_tools
            .difference(&plan.allowed_tools)
            .cloned()
            .collect::<Vec<_>>();
        return Err(format!(
            "AgentSpec selected tools outside profile authority: {unknown:?}"
        ));
    }
    validate_image_task_authority(task.image_task.as_ref(), &selected_tools)?;
    let has_effect_tool = selected_tools.iter().any(|tool| {
        matches!(
            tool.as_str(),
            "repo_map"
                | "search_code"
                | "read_file"
                | "write_file"
                | "replace_text"
                | "run_check"
                | "git_status"
                | "inspect_diff"
        )
    });
    if has_effect_tool && spec.effects != EffectsSpec::Rax {
        return Err("effect tools require AgentSpec effects=rax".to_owned());
    }
    validate_tool_authority(&selected_tools, spec.effects, spec.effect_approval)?;
    let selected_tools_vec = selected_tools.into_iter().collect::<Vec<_>>();
    spec.skills = documents
        .iter()
        .map(|value| value.descriptor.manifest.id.as_str().to_owned())
        .collect();
    spec.tools.clone_from(&selected_tools_vec);
    spec.workflow = track.as_ref().map(|value| value.as_str().to_owned());
    spec = spec.resolve().map_err(|error| error.to_string())?;
    let task_digest = task.digest().map_err(|error| error.to_string())?;
    let mut plugin_tiers = plugin_catalog
        .plugin_bindings()
        .map_err(|error| error.to_string())?;
    plugin_tiers.extend(
        data_extensions
            .iter()
            .map(|extension| PluginTierBinding {
                id: extension.id.clone(),
                tier: spec
                    .extensions
                    .iter()
                    .find_map(|candidate| match candidate {
                        DataExtensionSpec::Approved { id, .. } if id == &extension.id => {
                            Some(PluginTier::Approved)
                        }
                        DataExtensionSpec::Dynamic { id, .. } if id == &extension.id => {
                            Some(PluginTier::Dynamic)
                        }
                        _ => None,
                    })
                    .unwrap_or(PluginTier::Dynamic),
                linked_manifest_sha256: None,
                source_sha256: Some(extension.source_sha256.clone()),
            })
            .collect::<Vec<_>>(),
    );
    let selected_skills = documents
        .iter()
        .map(|document| SelectedSkillBinding {
            id: document.descriptor.manifest.id.as_str().to_owned(),
            instructions_sha256: document.instructions_sha256.clone(),
        })
        .collect::<Vec<_>>();
    let selected_tools = agent_runtime::canonical_tool_bindings(&selected_tools_vec)?;
    let selected_provider_plugin = SelectedPluginBinding {
        plugin_id: provider_selection.plugin_id.clone(),
        manifest_sha256: provider_plugin.manifest().digest(),
    };
    let selected_profile_plugin = SelectedPluginBinding {
        plugin_id: profile_plugin_binding.id,
        manifest_sha256: profile_plugin_binding
            .linked_manifest_sha256
            .ok_or_else(|| {
                "selected profile plugin is missing its linked manifest digest".to_owned()
            })?,
    };
    let receipt = CompositionReceipt::create(
        task.task_id,
        task.session_id,
        task_digest,
        provider,
        spec,
        selected_skills,
        selected_tools,
        plugin_tiers,
        selected_provider_plugin.clone(),
        selected_profile_plugin.clone(),
    )
    .map_err(|error| error.to_string())?;
    let composition_context = serde_json::Value::String(receipt.digest.clone());
    context_values.insert(
        "_upagent_composition_digest".to_owned(),
        composition_context,
    );
    context_values.insert(
        "_upagent_task_digest".to_owned(),
        serde_json::Value::String(receipt.task_digest.clone()),
    );
    context_values.insert(
        "_upagent_review".to_owned(),
        serde_json::Value::String(
            match receipt.spec.review {
                agent_core::ReviewSpec::None => "none",
                agent_core::ReviewSpec::EmitSubjectBundle => "emit_subject_bundle",
            }
            .to_owned(),
        ),
    );
    context_values.insert(
        "_upagent_composition_tools".to_owned(),
        serde_json::Value::Array(
            selected_tools_vec
                .iter()
                .map(|value| serde_json::Value::String(value.clone()))
                .collect(),
        ),
    );
    let task = TaskEnvelope {
        context: context_values,
        ..internal_task
    };
    Ok(ExplicitResolution {
        task,
        receipt,
        skill_roots,
        skill_catalog: resolved_catalog,
        profiles,
        plugin_catalog,
        provider_plugin_id: provider_selection.plugin_id.clone(),
        provider_plugin_manifest_sha256: provider_plugin.manifest().digest(),
        profile_plugin_id: selected_profile_plugin.plugin_id,
        profile_plugin_manifest_sha256: selected_profile_plugin.manifest_sha256,
    })
}

/// Reject an oversized request while composing so the immutable receipt is
/// never created for a task the selected provider cannot accept; no
/// caller-visible budget is silently clamped.
fn validate_provider_budget(
    maximum_output_bytes: usize,
    provider: &ProviderBinding,
) -> Result<(), String> {
    if maximum_output_bytes > provider.maximum_output_bytes {
        return Err(format!(
            "TaskEnvelope maximumOutputBytes {maximum_output_bytes} exceeds provider {} bound {}",
            provider.id, provider.maximum_output_bytes
        ));
    }
    Ok(())
}

fn validate_image_task_authority(
    image_task: Option<&agent_core::ImageTaskContract>,
    selected_tools: &BTreeSet<String>,
) -> Result<(), String> {
    let candidate_tools_selected = selected_tools
        .iter()
        .any(|tool| agent_core::is_candidate_image_tool_name(tool));
    if candidate_tools_selected && image_task.is_none() {
        return Err("candidate-producing image tools require TaskEnvelopeV3.imageTask".to_owned());
    }
    let Some(image_task) = image_task else {
        return Ok(());
    };
    if !candidate_tools_selected {
        return Err(
            "TaskEnvelopeV3.imageTask requires at least one candidate-producing image tool"
                .to_owned(),
        );
    }
    if image_task.source.is_provided() && selected_tools.contains(agent_core::IMAGE_GENERATE_TOOL) {
        return Err("provided-source image tasks cannot select image_generate".to_owned());
    }
    if image_task.source.is_generate_new()
        && !selected_tools.contains(agent_core::IMAGE_GENERATE_TOOL)
    {
        return Err("generate_new image tasks require image_generate".to_owned());
    }
    if image_task.requires_deterministic_compare()
        && !selected_tools.contains(agent_core::IMAGE_COMPARE_TOOL)
    {
        return Err(
            "exact deterministic verification tasks require image_compare in AgentSpec.tools"
                .to_owned(),
        );
    }
    Ok(())
}

fn validate_tool_authority(
    selected_tools: &BTreeSet<String>,
    effects: EffectsSpec,
    effect_approval: EffectApprovalSpec,
) -> Result<(), String> {
    if selected_tools
        .iter()
        .any(|tool| agent_core::is_external_image_tool_name(tool))
        && effect_approval == EffectApprovalSpec::Deny
    {
        return Err(
            "external image tools require AgentSpec effectApproval=plan_only or task_grant"
                .to_owned(),
        );
    }
    let has_mutating_tool = selected_tools.iter().any(|tool| {
        matches!(
            tool.as_str(),
            "write_file" | "replace_text" | "apply_patch" | "run_check" | "process"
        )
    });
    if has_mutating_tool
        && (effects != EffectsSpec::Rax || effect_approval == EffectApprovalSpec::Deny)
    {
        return Err(
            "mutating tools require AgentSpec effects=rax and non-deny effectApproval".to_owned(),
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn image_artifact() -> agent_core::ArtifactRef {
        let digest = "0".repeat(64);
        agent_core::ArtifactRef {
            id: agent_core::ArtifactId::parse(format!("art-{digest}")).expect("artifact id"),
            sha256: digest.clone(),
            media_type: "image/png".to_owned(),
            byte_length: 1,
            relative_path: format!("00/{digest}.png"),
        }
    }

    fn image_task(
        source: agent_core::ImageSource,
        compare_required: bool,
    ) -> agent_core::ImageTaskContract {
        let before_artifact = image_artifact();
        agent_core::ImageTaskContract {
            schema: agent_core::IMAGE_TASK_CONTRACT_SCHEMA.to_owned(),
            goal: "edit the image".to_owned(),
            source,
            maximum_attempts: 4,
            preserve: Vec::new(),
            visual_criteria: vec!["review the final candidate".to_owned()],
            verification: agent_core::ImageVerificationContract {
                visual_review: true,
                deterministic: compare_required.then(|| agent_core::ImageVerificationSpec {
                    schema: agent_core::IMAGE_VERIFICATION_SPEC_SCHEMA.to_owned(),
                    before_artifact,
                    after_kind: agent_core::ImageCompareArtifactKind::Raster,
                    assertions: vec![agent_core::ImageCompareAssertion::ExactEqual {
                        id: "preserve".to_owned(),
                        severity: agent_core::ImageCompareSeverity::Required,
                    }],
                }),
            },
        }
    }

    #[test]
    fn image_generation_requires_non_deny_effect_approval() {
        let selected = BTreeSet::from(["image_generate".to_owned()]);
        assert!(
            validate_tool_authority(&selected, EffectsSpec::None, EffectApprovalSpec::Deny)
                .is_err()
        );
        assert!(
            validate_tool_authority(&selected, EffectsSpec::None, EffectApprovalSpec::PlanOnly)
                .is_ok()
        );
    }

    #[test]
    fn deterministic_image_tools_are_allowed_under_denied_effects() {
        let selected = BTreeSet::from([
            "image_inspect".to_owned(),
            "image_extract".to_owned(),
            "image_render".to_owned(),
        ]);
        assert!(
            validate_tool_authority(&selected, EffectsSpec::None, EffectApprovalSpec::Deny).is_ok()
        );
    }

    #[test]
    fn candidate_tools_require_explicit_image_task_contract() {
        let selected = BTreeSet::from(["image_render".to_owned()]);
        assert!(validate_image_task_authority(None, &selected).is_err());
        assert!(
            validate_image_task_authority(
                Some(&image_task(
                    agent_core::ImageSource::Provided {
                        artifact: image_artifact()
                    },
                    false
                )),
                &selected,
            )
            .is_ok()
        );
    }

    #[test]
    fn image_task_source_and_compare_obligations_bind_tool_authority() {
        let provided_with_generate =
            BTreeSet::from(["image_generate".to_owned(), "image_render".to_owned()]);
        assert!(
            validate_image_task_authority(
                Some(&image_task(
                    agent_core::ImageSource::Provided {
                        artifact: image_artifact()
                    },
                    false
                )),
                &provided_with_generate,
            )
            .is_err()
        );

        let generated_without_generate = BTreeSet::from(["image_render".to_owned()]);
        assert!(
            validate_image_task_authority(
                Some(&image_task(agent_core::ImageSource::GenerateNew, false)),
                &generated_without_generate,
            )
            .is_err()
        );

        let required_compare_missing = BTreeSet::from(["image_render".to_owned()]);
        assert!(
            validate_image_task_authority(
                Some(&image_task(
                    agent_core::ImageSource::Provided {
                        artifact: image_artifact()
                    },
                    true
                )),
                &required_compare_missing,
            )
            .is_err()
        );
    }

    #[test]
    fn oversized_task_budget_is_rejected_at_provider_binding_boundary() {
        let provider = ProviderBinding {
            id: "test-provider".to_owned(),
            model: "test-model".to_owned(),
            route_sha256: "a".repeat(64),
            capabilities: BTreeSet::from([agent_core::PROVIDER_CAP_TEXT.to_owned()]),
            maximum_output_bytes: 4096,
        };
        assert!(validate_provider_budget(4096, &provider).is_ok());
        let error = validate_provider_budget(4097, &provider)
            .expect_err("provider binding must reject an oversized model budget");
        assert!(error.contains("maximumOutputBytes"));
        assert!(error.contains("test-provider"));
    }

    #[test]
    fn stock_catalog_seals_provider_and_profile_plugin_identities() {
        let catalog = stock_plugin_catalog().expect("stock plugin catalog");
        let provider_ids = catalog.provider_plugin_ids();
        let profile_ids = catalog.profile_plugin_ids();
        assert_ne!(provider_ids, [] as [String; 0]);
        assert_ne!(profile_ids, [] as [String; 0]);
        assert!(provider_ids.iter().all(|id| !id.is_empty()));
        assert!(profile_ids.iter().all(|id| !id.is_empty()));
        let bindings = catalog.plugin_bindings().expect("catalog graph");
        assert_eq!(bindings.len(), 2);
        assert!(bindings.iter().all(|binding| {
            binding.tier == PluginTier::BuiltIn
                && binding
                    .linked_manifest_sha256
                    .as_ref()
                    .is_some_and(|digest| digest.len() == 64)
                && binding.source_sha256.is_none()
        }));
    }
}
