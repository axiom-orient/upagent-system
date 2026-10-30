//! Typed linked-Rust extensions hosted by `AgentRuntime`.
//!
//! Profile, provider, and skill authorities are constructor inputs, not
//! extension points. Owner-linked extensions may add only tool modules or
//! completion verifiers. Startup is transactional because the frozen registry
//! is published only after every selected extension has contributed
//! successfully.

use std::{
    collections::{BTreeMap, BTreeSet, btree_map::Entry},
    error::Error,
    fmt,
    sync::Arc,
};

use agent_plugin_kernel::{
    ContributionKind, PluginGraph, PluginGraphError, PluginId, PluginLifecycleEvent,
    PluginManifest, PluginTrust,
};
use agent_profile_api::ProfileRegistry;
use agent_provider_api::ModelPort;
use agent_skills::SkillCatalog;

use crate::extension::ToolModule;

mod evidence;
mod provenance;

pub use evidence::{CompletionEvidence, CompletionVerifier};
pub use provenance::{PluginBindingError, PluginExecutionBinding, PluginOrigin, PluginSnapshot};

use evidence::HostCompletionVerifier;

/// One linked plugin that contributes typed runtime capabilities.
pub trait RuntimePlugin: Send + Sync + fmt::Debug {
    /// Immutable graph manifest.
    fn manifest(&self) -> &PluginManifest;

    /// Contributes to the staged typed registry.
    ///
    /// This phase is registration-only: it must not perform external I/O or
    /// mutate durable state. The registry becomes visible only after all
    /// contributions validate successfully.
    fn contribute(&self, registrar: &mut PluginRegistrar<'_>) -> Result<(), RuntimePluginError>;
}

/// Registrar scoped to the plugin currently being activated.
pub struct PluginRegistrar<'a> {
    builder: &'a mut RegistryBuilder,
    origin: PluginOrigin,
    contributed: BTreeSet<ContributionKind>,
}

impl fmt::Debug for PluginRegistrar<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PluginRegistrar")
            .field("origin", &self.origin)
            .field("contributed", &self.contributed)
            .finish_non_exhaustive()
    }
}

impl<'a> PluginRegistrar<'a> {
    fn new(builder: &'a mut RegistryBuilder, origin: PluginOrigin) -> Self {
        Self {
            builder,
            origin,
            contributed: BTreeSet::new(),
        }
    }

    /// Registers one trusted linked tool module.
    pub fn register_tool_module(
        &mut self,
        module: Arc<dyn ToolModule>,
    ) -> Result<(), RuntimePluginError> {
        self.builder.tool_modules.push(PluginToolModule {
            origin: self.origin.clone(),
            module,
        });
        self.contributed.insert(ContributionKind::Tools);
        Ok(())
    }

    /// Registers one model-independent completion verifier.
    pub fn register_verifier(
        &mut self,
        verifier: Arc<dyn CompletionVerifier>,
    ) -> Result<(), RuntimePluginError> {
        let id = verifier.id().to_owned();
        if id.trim().is_empty() || id.len() > 128 {
            return Err(RuntimePluginError::InvalidVerifierId(id));
        }
        let Entry::Vacant(entry) = self.builder.verifiers.entry(id.clone()) else {
            return Err(RuntimePluginError::DuplicateVerifier(id));
        };
        entry.insert(verifier);
        self.builder
            .verifier_origins
            .insert(id, self.origin.clone());
        self.contributed.insert(ContributionKind::Verifiers);
        Ok(())
    }

    fn finish(self, manifest: &PluginManifest) -> Result<(), RuntimePluginError> {
        if &self.contributed != manifest.contributions() {
            return Err(RuntimePluginError::ContributionMismatch {
                plugin: manifest.id().clone(),
                declared: manifest.contributions().clone(),
                actual: self.contributed,
            });
        }
        Ok(())
    }
}

/// Static plugin contributing trusted tool modules.
#[derive(Clone, Debug)]
pub struct StaticToolsPlugin {
    manifest: PluginManifest,
    modules: Vec<Arc<dyn ToolModule>>,
}

impl StaticToolsPlugin {
    /// Creates a linked tool plugin.
    pub fn new(
        manifest: PluginManifest,
        modules: Vec<Arc<dyn ToolModule>>,
    ) -> Result<Self, RuntimePluginError> {
        validate_linked_manifest(&manifest, ContributionKind::Tools)?;
        if modules.is_empty() {
            return Err(RuntimePluginError::EmptyTools(manifest.id().clone()));
        }
        Ok(Self { manifest, modules })
    }
}

impl RuntimePlugin for StaticToolsPlugin {
    fn manifest(&self) -> &PluginManifest {
        &self.manifest
    }

    fn contribute(&self, registrar: &mut PluginRegistrar<'_>) -> Result<(), RuntimePluginError> {
        for module in &self.modules {
            registrar.register_tool_module(Arc::clone(module))?;
        }
        Ok(())
    }
}

fn validate_linked_manifest(
    manifest: &PluginManifest,
    expected: ContributionKind,
) -> Result<(), RuntimePluginError> {
    if manifest.contributions() != &BTreeSet::from([expected]) {
        return Err(RuntimePluginError::InvalidStaticManifest {
            plugin: manifest.id().clone(),
            expected,
        });
    }
    Ok(())
}

#[derive(Debug)]
pub(crate) struct PluginToolModule {
    pub(crate) origin: PluginOrigin,
    pub(crate) module: Arc<dyn ToolModule>,
}

pub(crate) struct RuntimeRegistry {
    pub(crate) profiles: ProfileRegistry,
    pub(crate) tool_modules: Vec<PluginToolModule>,
    pub(crate) model: Arc<dyn ModelPort>,
    pub(crate) skills: SkillCatalog,
    pub(crate) verifiers: BTreeMap<String, Arc<dyn CompletionVerifier>>,
    pub(crate) snapshot: PluginSnapshot,
}

#[derive(Default)]
struct RegistryBuilder {
    tool_modules: Vec<PluginToolModule>,
    verifiers: BTreeMap<String, Arc<dyn CompletionVerifier>>,
    verifier_origins: BTreeMap<String, PluginOrigin>,
}

#[derive(Debug)]
struct StaticVerifierPlugin {
    manifest: PluginManifest,
    verifier: Arc<dyn CompletionVerifier>,
}

impl StaticVerifierPlugin {
    fn new(
        manifest: PluginManifest,
        verifier: Arc<dyn CompletionVerifier>,
    ) -> Result<Self, RuntimePluginError> {
        validate_linked_manifest(&manifest, ContributionKind::Verifiers)?;
        Ok(Self { manifest, verifier })
    }
}

impl RuntimePlugin for StaticVerifierPlugin {
    fn manifest(&self) -> &PluginManifest {
        &self.manifest
    }

    fn contribute(&self, registrar: &mut PluginRegistrar<'_>) -> Result<(), RuntimePluginError> {
        registrar.register_verifier(Arc::clone(&self.verifier))
    }
}

/// Transactionally assembles the immutable runtime registry.
///
/// Profiles, provider, and skills are authoritative constructor inputs and are
/// never projected back into the extension graph. `RuntimePlugin` represents
/// only actual linked-Rust extension variation: tools and verifiers.
/// The staged builder is dropped on any failure and no partially assembled
/// registry is published.
pub(crate) fn assemble_registry(
    profiles: ProfileRegistry,
    base_modules: Vec<Arc<dyn ToolModule>>,
    model: Arc<dyn ModelPort>,
    skills: SkillCatalog,
    plugins: Vec<Arc<dyn RuntimePlugin>>,
) -> Result<RuntimeRegistry, RuntimePluginError> {
    if profiles.is_empty() {
        return Err(RuntimePluginError::NoProfiles);
    }

    let mut pending = BTreeMap::<PluginId, Arc<dyn RuntimePlugin>>::new();
    if !base_modules.is_empty() {
        insert_plugin(
            &mut pending,
            Arc::new(StaticToolsPlugin::new(
                host_manifest("upagent.host.tools", ContributionKind::Tools)?,
                base_modules,
            )?),
        )?;
    }
    insert_plugin(
        &mut pending,
        Arc::new(StaticVerifierPlugin::new(
            host_manifest("upagent.host.verifier", ContributionKind::Verifiers)?,
            Arc::new(HostCompletionVerifier),
        )?),
    )?;
    for plugin in plugins {
        validate_extension_manifest(plugin.manifest())?;
        insert_plugin(&mut pending, plugin)?;
    }

    let manifests = pending
        .values()
        .map(|plugin| plugin.manifest().clone())
        .collect::<Vec<_>>();
    let roots = pending.keys().cloned().collect::<Vec<_>>();
    let graph = PluginGraph::resolve(manifests, roots)?;
    let mut builder = RegistryBuilder::default();
    let mut lifecycle = graph.events().to_vec();
    let mut activation_order = Vec::new();

    for plugin_id in graph.activation_order() {
        let plugin = pending
            .remove(plugin_id)
            .ok_or_else(|| RuntimePluginError::MissingContribution(plugin_id.clone()))?;
        let manifest = plugin.manifest();
        let origin = PluginOrigin::from_manifest(manifest);
        lifecycle.push(PluginLifecycleEvent::ContributionStarted {
            plugin: plugin_id.clone(),
        });
        let mut registrar = PluginRegistrar::new(&mut builder, origin.clone());
        plugin.contribute(&mut registrar).map_err(|source| {
            RuntimePluginError::ContributionFailed {
                plugin: plugin_id.clone(),
                source: Box::new(source),
            }
        })?;
        registrar
            .finish(manifest)
            .map_err(|source| RuntimePluginError::ContributionFailed {
                plugin: plugin_id.clone(),
                source: Box::new(source),
            })?;
        lifecycle.push(PluginLifecycleEvent::Active {
            plugin: plugin_id.clone(),
        });
        activation_order.push(origin);
    }

    freeze_registry(
        builder,
        profiles,
        model,
        skills,
        graph.fingerprint(),
        lifecycle,
        activation_order,
    )
}

fn validate_extension_manifest(manifest: &PluginManifest) -> Result<(), RuntimePluginError> {
    if manifest.contributions().is_empty() {
        return Err(RuntimePluginError::EmptyExtensionContributions(
            manifest.id().clone(),
        ));
    }
    Ok(())
}

fn host_manifest(
    id: &str,
    contribution: ContributionKind,
) -> Result<PluginManifest, RuntimePluginError> {
    Ok(PluginManifest::linked(
        PluginId::parse(id)?,
        env!("CARGO_PKG_VERSION"),
        PluginTrust::BuiltIn,
    )?
    .contributes(contribution))
}

fn insert_plugin(
    pending: &mut BTreeMap<PluginId, Arc<dyn RuntimePlugin>>,
    plugin: Arc<dyn RuntimePlugin>,
) -> Result<(), RuntimePluginError> {
    let id = plugin.manifest().id().clone();
    if pending.insert(id.clone(), plugin).is_some() {
        return Err(RuntimePluginError::DuplicatePlugin(id));
    }
    Ok(())
}

fn freeze_registry(
    builder: RegistryBuilder,
    profiles: ProfileRegistry,
    model: Arc<dyn ModelPort>,
    skills: SkillCatalog,
    fingerprint: &str,
    mut lifecycle: Vec<PluginLifecycleEvent>,
    activation_order: Vec<PluginOrigin>,
) -> Result<RuntimeRegistry, RuntimePluginError> {
    if builder.verifiers.is_empty() {
        return Err(RuntimePluginError::NoVerifier);
    }

    let fingerprint = fingerprint.to_owned();
    lifecycle.push(PluginLifecycleEvent::Ready {
        fingerprint: fingerprint.clone(),
    });
    Ok(RuntimeRegistry {
        profiles,
        tool_modules: builder.tool_modules,
        model,
        skills,
        verifiers: builder.verifiers,
        snapshot: PluginSnapshot {
            fingerprint,
            activation_order,
            lifecycle,
            tool_origins: BTreeMap::new(),
            verifier_origins: builder.verifier_origins,
        },
    })
}

/// Runtime plugin integration failure.
#[derive(Debug)]
pub enum RuntimePluginError {
    Graph(PluginGraphError),
    DuplicatePlugin(PluginId),
    MissingContribution(PluginId),
    InvalidStaticManifest {
        plugin: PluginId,
        expected: ContributionKind,
    },
    ContributionFailed {
        plugin: PluginId,
        source: Box<RuntimePluginError>,
    },
    ContributionMismatch {
        plugin: PluginId,
        declared: BTreeSet<ContributionKind>,
        actual: BTreeSet<ContributionKind>,
    },
    EmptyExtensionContributions(PluginId),
    EmptyTools(PluginId),
    InvalidVerifierId(String),
    DuplicateVerifier(String),
    NoProfiles,
    NoVerifier,
}

impl fmt::Display for RuntimePluginError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Graph(error) => write!(formatter, "plugin graph failed: {error}"),
            Self::DuplicatePlugin(plugin) => write!(formatter, "duplicate runtime plugin {plugin}"),
            Self::MissingContribution(plugin) => {
                write!(formatter, "resolved plugin {plugin} has no contribution")
            }
            Self::InvalidStaticManifest { plugin, expected } => write!(
                formatter,
                "static plugin {plugin} must declare only {expected:?}"
            ),
            Self::ContributionFailed { plugin, source } => {
                write!(formatter, "plugin {plugin} contribution failed: {source}")
            }
            Self::ContributionMismatch {
                plugin,
                declared,
                actual,
            } => write!(
                formatter,
                "plugin {plugin} contribution mismatch: declared={declared:?}, actual={actual:?}"
            ),
            Self::EmptyExtensionContributions(plugin) => {
                write!(
                    formatter,
                    "runtime extension {plugin} declares no contributions"
                )
            }
            Self::EmptyTools(plugin) => {
                write!(formatter, "tool plugin {plugin} contributes no modules")
            }
            Self::InvalidVerifierId(id) => write!(formatter, "invalid verifier identifier {id}"),
            Self::DuplicateVerifier(id) => write!(formatter, "duplicate verifier {id}"),
            Self::NoProfiles => formatter.write_str("host profile catalog is empty"),
            Self::NoVerifier => {
                formatter.write_str("runtime plugin set contributes no completion verifier")
            }
        }
    }
}

impl Error for RuntimePluginError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Graph(error) => Some(error),
            Self::ContributionFailed { source, .. } => Some(source.as_ref()),
            _ => None,
        }
    }
}

impl From<PluginGraphError> for RuntimePluginError {
    fn from(value: PluginGraphError) -> Self {
        Self::Graph(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_core::{
        AgentFailure, FailureCode, ModelTurn, ModelTurnResult, ProfileId, ReasoningEffort,
        TaskBudget, ToolCall, ToolDefinition,
    };
    use agent_plugin_kernel::{ContributionKind, PluginId, PluginManifest, PluginTrust};
    use agent_profile_api::{ExecutionClass, ExecutionTopology, ProfilePlan};
    use agent_provider_api::{MAXIMUM_MODEL_RESPONSE_BYTES, ProviderError};
    use serde_json::json;

    use crate::{ModuleExecution, ModuleTool, ToolContext, ToolModuleError, ToolSideEffect};

    #[derive(Debug)]
    struct TestToolModule;

    #[async_trait::async_trait]
    impl ToolModule for TestToolModule {
        fn tools(&self) -> Vec<ModuleTool> {
            vec![ModuleTool {
                definition: ToolDefinition {
                    name: "test_observe".to_owned(),
                    description: "test-only read-only observation tool".to_owned(),
                    input_schema: json!({
                        "type": "object",
                        "additionalProperties": false,
                        "properties": {}
                    }),
                    strict: true,
                },
                side_effect: ToolSideEffect::ReadOnly,
                allowed_profiles: BTreeSet::from([ProfileId::creator()]),
                required_capabilities: BTreeSet::new(),
                activation_skills: BTreeSet::new(),
            }]
        }

        async fn execute(
            &self,
            _context: ToolContext,
            _call: ToolCall,
        ) -> Result<ModuleExecution, ToolModuleError> {
            Err(ToolModuleError::Contract(
                "test observation tool must not execute".to_owned(),
            ))
        }
    }

    #[derive(Debug)]
    struct TestModel;

    #[async_trait::async_trait]
    impl ModelPort for TestModel {
        fn binding(&self) -> agent_core::ProviderBinding {
            agent_core::ProviderBinding {
                id: "test".to_owned(),
                model: "test-model".to_owned(),
                route_sha256: "0".repeat(64),
                capabilities: std::collections::BTreeSet::from([
                    agent_core::PROVIDER_CAP_TEXT.to_owned(),
                    agent_core::PROVIDER_CAP_IMAGE_OBSERVATION.to_owned(),
                    agent_core::PROVIDER_CAP_IMAGE_GENERATION.to_owned(),
                ]),
                maximum_output_bytes: MAXIMUM_MODEL_RESPONSE_BYTES,
            }
        }

        async fn complete(&self, _turn: &ModelTurn) -> Result<ModelTurnResult, ProviderError> {
            Ok(ModelTurnResult {
                text: "unused".to_owned(),
                tool_calls: Vec::new(),
                usage: None,
            })
        }
    }

    #[test]
    fn constructor_authorities_are_not_projected_into_the_extension_graph()
    -> Result<(), Box<dyn Error>> {
        let registry = assemble_registry(
            agent_profiles::built_in_registry()?,
            Vec::new(),
            Arc::new(TestModel),
            SkillCatalog::default(),
            Vec::new(),
        )?;
        assert_eq!(registry.profiles.manifests().len(), 4);
        assert_eq!(registry.snapshot.activation_order().len(), 1);
        assert_eq!(
            registry.snapshot.activation_order()[0].id().as_str(),
            "upagent.host.verifier"
        );
        assert!(registry.snapshot.tool_origin("repo_map").is_none());
        assert_eq!(registry.snapshot.verifier_origins().len(), 1);
        assert_eq!(registry.snapshot.fingerprint().len(), 64);
        Ok(())
    }

    #[test]
    fn staged_tool_plugin_preserves_the_real_module_and_origin() -> Result<(), Box<dyn Error>> {
        let manifest =
            PluginManifest::linked(PluginId::parse("test.tools")?, "1", PluginTrust::BuiltIn)?
                .contributes(ContributionKind::Tools);
        let plugin = StaticToolsPlugin::new(manifest, vec![Arc::new(TestToolModule)])?;
        let registry = assemble_registry(
            agent_profiles::built_in_registry()?,
            Vec::new(),
            Arc::new(TestModel),
            SkillCatalog::default(),
            vec![Arc::new(plugin)],
        )?;
        assert_eq!(registry.tool_modules.len(), 1);
        assert_eq!(registry.tool_modules[0].origin.id().as_str(), "test.tools");
        assert_eq!(
            registry.tool_modules[0].module.tools()[0].definition.name,
            "test_observe"
        );
        Ok(())
    }

    #[test]
    fn host_verifier_closes_required_tool_and_check_evidence() {
        let verifier = HostCompletionVerifier;
        let evidence = CompletionEvidence {
            execution_class: ExecutionClass::Verified,
            topology: ExecutionTopology::SingleWithIndependentVerifier,
            required_successful_tools: BTreeSet::from([
                "run_check".to_owned(),
                "inspect_diff".to_owned(),
            ]),
            successful_tools: BTreeSet::from(["run_check".to_owned(), "inspect_diff".to_owned()]),
            required_checks: BTreeSet::from(["build".to_owned()]),
            successful_checks: BTreeSet::from(["build".to_owned()]),
            required_successful_tools_after_mutation: BTreeSet::from(["inspect_diff".to_owned()]),
            successful_tools_after_mutation: BTreeSet::from(["inspect_diff".to_owned()]),
            successful_checks_after_mutation: BTreeSet::from(["build".to_owned()]),
            successful_source_mutation: true,
        };
        assert!(verifier.verify(&evidence).is_ok());
    }

    #[test]
    fn host_verifier_rejects_model_claims_without_observed_check_evidence() {
        let verifier = HostCompletionVerifier;
        let evidence = CompletionEvidence {
            execution_class: ExecutionClass::Verified,
            topology: ExecutionTopology::SingleWithIndependentVerifier,
            required_successful_tools: BTreeSet::from(["run_check".to_owned()]),
            successful_tools: BTreeSet::from(["run_check".to_owned()]),
            required_checks: BTreeSet::from(["build".to_owned()]),
            successful_checks: BTreeSet::new(),
            required_successful_tools_after_mutation: BTreeSet::new(),
            successful_tools_after_mutation: BTreeSet::new(),
            successful_checks_after_mutation: BTreeSet::new(),
            successful_source_mutation: false,
        };
        let failure = verifier.verify(&evidence);
        assert!(matches!(
            failure,
            Err(AgentFailure {
                code: FailureCode::VerificationFailed,
                ..
            })
        ));
    }

    #[test]
    fn host_verifier_rejects_evidence_observed_before_a_later_mutation() {
        let verifier = HostCompletionVerifier;
        let evidence = CompletionEvidence {
            execution_class: ExecutionClass::Verified,
            topology: ExecutionTopology::SingleWithIndependentVerifier,
            required_successful_tools: BTreeSet::from(["write_file".to_owned()]),
            successful_tools: BTreeSet::from(["write_file".to_owned(), "inspect_diff".to_owned()]),
            required_checks: BTreeSet::from(["build".to_owned()]),
            successful_checks: BTreeSet::from(["build".to_owned()]),
            required_successful_tools_after_mutation: BTreeSet::from(["inspect_diff".to_owned()]),
            successful_tools_after_mutation: BTreeSet::new(),
            successful_checks_after_mutation: BTreeSet::new(),
            successful_source_mutation: true,
        };
        assert!(matches!(
            verifier.verify(&evidence),
            Err(AgentFailure {
                code: FailureCode::VerificationFailed,
                ..
            })
        ));
    }

    #[test]
    fn host_verifier_accepts_required_evidence_observed_after_mutation() {
        let verifier = HostCompletionVerifier;
        let evidence = CompletionEvidence {
            execution_class: ExecutionClass::Verified,
            topology: ExecutionTopology::SingleWithIndependentVerifier,
            required_successful_tools: BTreeSet::from(["write_file".to_owned()]),
            successful_tools: BTreeSet::from(["write_file".to_owned(), "inspect_diff".to_owned()]),
            required_checks: BTreeSet::from(["build".to_owned()]),
            successful_checks: BTreeSet::from(["build".to_owned()]),
            required_successful_tools_after_mutation: BTreeSet::from(["inspect_diff".to_owned()]),
            successful_tools_after_mutation: BTreeSet::from(["inspect_diff".to_owned()]),
            successful_checks_after_mutation: BTreeSet::from(["build".to_owned()]),
            successful_source_mutation: true,
        };
        assert!(verifier.verify(&evidence).is_ok());
    }

    #[test]
    fn host_verifier_rejects_post_mutation_evidence_invalidated_by_another_mutation() {
        let verifier = HostCompletionVerifier;
        let evidence = CompletionEvidence {
            execution_class: ExecutionClass::Verified,
            topology: ExecutionTopology::SingleWithIndependentVerifier,
            required_successful_tools: BTreeSet::from(["write_file".to_owned()]),
            successful_tools: BTreeSet::from([
                "write_file".to_owned(),
                "inspect_diff".to_owned(),
                "apply_patch".to_owned(),
            ]),
            required_checks: BTreeSet::from(["build".to_owned()]),
            successful_checks: BTreeSet::from(["build".to_owned()]),
            required_successful_tools_after_mutation: BTreeSet::from(["inspect_diff".to_owned()]),
            successful_tools_after_mutation: BTreeSet::new(),
            successful_checks_after_mutation: BTreeSet::new(),
            successful_source_mutation: true,
        };
        assert!(matches!(
            verifier.verify(&evidence),
            Err(AgentFailure {
                code: FailureCode::VerificationFailed,
                ..
            })
        ));
    }
    #[test]
    fn execution_binding_changes_with_the_selected_extension_surface() -> Result<(), Box<dyn Error>>
    {
        let registry = assemble_registry(
            agent_profiles::built_in_registry()?,
            Vec::new(),
            Arc::new(TestModel),
            SkillCatalog::default(),
            Vec::new(),
        )?;
        let profile = registry.profiles.resolve(&ProfileId::developer())?;
        let base = ProfilePlan {
            manifest: profile.manifest().clone(),
            messages: Vec::new(),
            track: Some(agent_core::TrackId::builder()),
            allowed_tools: BTreeSet::from(["repo_map".to_owned()]),
            reasoning: ReasoningEffort::Low,
            memory_scope: None,
            execution_class: ExecutionClass::Direct,
            required_successful_tools: BTreeSet::new(),
            required_checks: Vec::new(),
            required_check_commands: BTreeMap::new(),
            required_successful_tools_after_mutation: BTreeSet::new(),
            maximum_workers: 1,
            topology: ExecutionTopology::Single,
            output_contract: None,
            axd_result_binding: None,
            effective_budget: TaskBudget::default(),
            policy_budget_cap: None,
        };
        let direct = registry.snapshot.bind_execution(&base)?;
        let mut verified_plan = base.clone();
        verified_plan.execution_class = ExecutionClass::Verified;
        verified_plan.reasoning = ReasoningEffort::High;
        verified_plan.topology = ExecutionTopology::SingleWithIndependentVerifier;
        verified_plan.allowed_tools.extend([
            "inspect_diff".to_owned(),
            "run_check".to_owned(),
            "write_file".to_owned(),
        ]);
        let verified = registry.snapshot.bind_execution(&verified_plan)?;
        assert_eq!(
            direct.plugin_set_fingerprint(),
            verified.plugin_set_fingerprint()
        );
        assert_ne!(direct.fingerprint(), verified.fingerprint());
        assert_eq!(direct.fingerprint().len(), 64);
        assert_eq!(verified.fingerprint().len(), 64);

        let mut invalid_plan = base;
        invalid_plan.allowed_tools.insert("missing_tool".to_owned());
        assert!(matches!(
            registry.snapshot.bind_execution(&invalid_plan),
            Err(PluginBindingError::MissingToolOrigin(tool)) if tool == "missing_tool"
        ));
        Ok(())
    }

    #[test]
    fn contribution_failure_never_publishes_a_partial_registry() -> Result<(), Box<dyn Error>> {
        #[derive(Debug)]
        struct EmptyToolsPlugin {
            manifest: PluginManifest,
        }

        impl RuntimePlugin for EmptyToolsPlugin {
            fn manifest(&self) -> &PluginManifest {
                &self.manifest
            }

            fn contribute(
                &self,
                _registrar: &mut PluginRegistrar<'_>,
            ) -> Result<(), RuntimePluginError> {
                Ok(())
            }
        }

        let manifest = PluginManifest::linked(
            PluginId::parse("test.empty-tools")?,
            "1",
            PluginTrust::Trusted,
        )?
        .contributes(ContributionKind::Tools);
        let result = assemble_registry(
            agent_profiles::built_in_registry()?,
            Vec::new(),
            Arc::new(TestModel),
            SkillCatalog::default(),
            vec![Arc::new(EmptyToolsPlugin { manifest })],
        );
        assert!(matches!(
            result,
            Err(RuntimePluginError::ContributionFailed { plugin, source })
                if plugin.as_str() == "test.empty-tools"
                    && matches!(
                        source.as_ref(),
                        RuntimePluginError::ContributionMismatch { .. }
                    )
        ));
        Ok(())
    }

    #[test]
    fn duplicate_verifier_preserves_the_registered_value_and_origin() -> Result<(), Box<dyn Error>>
    {
        let original_manifest =
            host_manifest("test.original-verifier", ContributionKind::Verifiers)?;
        let original_origin = PluginOrigin::from_manifest(&original_manifest);
        let duplicate_manifest =
            host_manifest("test.duplicate-verifier", ContributionKind::Verifiers)?;
        let duplicate_origin = PluginOrigin::from_manifest(&duplicate_manifest);
        let original: Arc<dyn CompletionVerifier> = Arc::new(HostCompletionVerifier);
        let duplicate: Arc<dyn CompletionVerifier> = Arc::new(HostCompletionVerifier);
        let mut builder = RegistryBuilder::default();
        let mut registrar = PluginRegistrar::new(&mut builder, original_origin.clone());
        registrar.register_verifier(Arc::clone(&original))?;
        registrar.finish(&original_manifest)?;

        let mut registrar = PluginRegistrar::new(&mut builder, duplicate_origin);
        assert!(matches!(
            registrar.register_verifier(duplicate),
            Err(RuntimePluginError::DuplicateVerifier(id)) if id == original.id()
        ));
        assert!(registrar.contributed.is_empty());
        assert!(Arc::ptr_eq(
            registrar
                .builder
                .verifiers
                .get(original.id())
                .expect("registered verifier"),
            &original,
        ));
        assert_eq!(
            registrar.builder.verifier_origins.get(original.id()),
            Some(&original_origin),
        );
        Ok(())
    }
}
