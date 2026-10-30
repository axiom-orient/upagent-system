#![forbid(unsafe_code)]

use std::{
    collections::{BTreeMap, BTreeSet, btree_map::Entry},
    fmt,
    sync::Arc,
};

use agent_core::{
    ModelMessage, PluginTier, PluginTierBinding, ProfileId, ProfileManifest, ReasoningEffort,
    SkillId, TaskBudget, TaskEnvelope, TrackId,
};
use agent_plugin_kernel::{
    ContributionKind, KERNEL_API_VERSION, PluginGraph, PluginId, PluginManifest,
};
use agent_provider_api::{ProviderError, ProviderPlanes, ProviderPlugin, ProviderPluginEnvelope};
use agent_skills::SkillDocument;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ProfileError {
    #[error("profile {0} is not registered")]
    NotFound(ProfileId),
    #[error("profile {0} is already registered")]
    Duplicate(ProfileId),
    #[error("profile contract failed: {0}")]
    Invalid(String),
}

/// A linked Rust agent-policy plugin. Implementations are selected by the
/// composition root; profiles are never discovered from the filesystem.
pub trait ProfilePlugin: Send + Sync + fmt::Debug {
    fn manifest(&self) -> &PluginManifest;

    fn register_profiles(&self, registry: &mut ProfileRegistry) -> Result<(), ProfileError>;

    /// IDs owned by this plugin.  The default implementation materializes an
    /// isolated registry so a catalog can seal the selected profile's owning
    /// plugin without relying on registration order or filesystem discovery.
    fn profile_ids(&self) -> Result<Vec<ProfileId>, ProfileError> {
        let mut registry = ProfileRegistry::default();
        self.register_profiles(&mut registry)?;
        Ok(registry
            .manifests()
            .into_iter()
            .map(|manifest| manifest.id)
            .collect())
    }
}

#[derive(Debug, Error)]
pub enum CompositionPluginError {
    #[error("plugin manifest is invalid: {0}")]
    Manifest(String),
    #[error("plugin {0} does not declare the required contribution")]
    WrongContribution(PluginId),
    #[error("provider plugin {0} is not registered")]
    ProviderNotFound(String),
    #[error("profile plugin {0} is not registered")]
    ProfileNotFound(String),
    #[error("provider plugin failed: {0}")]
    Provider(#[from] ProviderError),
    #[error("profile plugin failed: {0}")]
    Profile(#[from] ProfileError),
}

/// One immutable catalog shared by composition, runtime activation, and
/// independent review. It is deliberately limited to linked Rust plugins:
/// there is no dylib, PATH, or Wasm loader at this boundary.
#[derive(Clone, Default)]
pub struct CompositionPluginCatalog {
    providers: BTreeMap<PluginId, Arc<dyn ProviderPlugin>>,
    profiles: BTreeMap<PluginId, Arc<dyn ProfilePlugin>>,
}

impl fmt::Debug for CompositionPluginCatalog {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CompositionPluginCatalog")
            .field("providers", &self.providers.keys().collect::<Vec<_>>())
            .field("profiles", &self.profiles.keys().collect::<Vec<_>>())
            .finish()
    }
}

impl CompositionPluginCatalog {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register_provider(
        &mut self,
        plugin: Arc<dyn ProviderPlugin>,
    ) -> Result<(), CompositionPluginError> {
        validate_plugin_manifest_shape(plugin.manifest(), ContributionKind::Provider)?;
        let id = plugin.manifest().id().clone();
        if self.profiles.contains_key(&id) {
            return Err(CompositionPluginError::Manifest(format!(
                "duplicate plugin id {id} crosses provider/profile catalog boundaries"
            )));
        }
        match self.providers.entry(id.clone()) {
            Entry::Vacant(entry) => {
                entry.insert(plugin);
                Ok(())
            }
            Entry::Occupied(_) => Err(CompositionPluginError::Manifest(format!(
                "duplicate provider plugin {id}"
            ))),
        }
    }

    pub fn register_profile(
        &mut self,
        plugin: Arc<dyn ProfilePlugin>,
    ) -> Result<(), CompositionPluginError> {
        validate_plugin_manifest_shape(plugin.manifest(), ContributionKind::Profiles)?;
        let id = plugin.manifest().id().clone();
        if self.providers.contains_key(&id) {
            return Err(CompositionPluginError::Manifest(format!(
                "duplicate plugin id {id} crosses provider/profile catalog boundaries"
            )));
        }
        match self.profiles.entry(id.clone()) {
            Entry::Vacant(entry) => {
                entry.insert(plugin);
                Ok(())
            }
            Entry::Occupied(_) => Err(CompositionPluginError::Manifest(format!(
                "duplicate profile plugin {id}"
            ))),
        }
    }

    /// Resolve every registered manifest and its transitive dependency graph.
    /// Registration intentionally remains atomic and order-independent; a
    /// dependency may be registered later and is checked here/finalize time.
    pub fn finalize(&self) -> Result<(), CompositionPluginError> {
        if self.providers.is_empty() && self.profiles.is_empty() {
            return Ok(());
        }
        self.graph().map(|_| ())
    }

    pub fn provider(&self, id: &str) -> Result<Arc<dyn ProviderPlugin>, CompositionPluginError> {
        if self.providers.is_empty() && self.profiles.is_empty() {
            return Err(CompositionPluginError::ProviderNotFound(id.to_owned()));
        }
        self.graph()?;
        self.providers
            .iter()
            .find(|(plugin_id, _)| plugin_id.as_str() == id)
            .map(|(_, plugin)| plugin.clone())
            .ok_or_else(|| CompositionPluginError::ProviderNotFound(id.to_owned()))
    }

    pub fn profiles(&self) -> Result<ProfileRegistry, CompositionPluginError> {
        if self.providers.is_empty() && self.profiles.is_empty() {
            return Ok(ProfileRegistry::default());
        }
        let graph = self.graph()?;
        let mut registry = ProfileRegistry::default();
        for plugin_id in graph.activation_order() {
            if let Some(plugin) = self.profiles.get(plugin_id) {
                plugin.register_profiles(&mut registry)?;
            }
        }
        Ok(registry)
    }

    /// Resolve the profile definition and seal the linked plugin that owns it.
    /// Ambiguous ownership is rejected rather than depending on map order.
    pub fn profile_plugin_binding(
        &self,
        profile_id: &ProfileId,
    ) -> Result<PluginTierBinding, CompositionPluginError> {
        if self.providers.is_empty() && self.profiles.is_empty() {
            return Err(CompositionPluginError::ProfileNotFound(
                profile_id.as_str().to_owned(),
            ));
        }
        let graph = self.graph()?;
        let mut owner = None;
        for plugin_id in graph.activation_order() {
            let Some(plugin) = self.profiles.get(plugin_id) else {
                continue;
            };
            if plugin.profile_ids()?.iter().any(|id| id == profile_id) {
                if owner.is_some() {
                    return Err(CompositionPluginError::Manifest(format!(
                        "profile {profile_id} is owned by multiple profile plugins"
                    )));
                }
                owner = Some(plugin_binding(plugin.manifest()));
            }
        }
        owner.ok_or_else(|| CompositionPluginError::ProfileNotFound(profile_id.as_str().to_owned()))
    }

    /// Verify a caller-selected provider envelope against this catalog and
    /// return the linked-Rust provider. Configuration remains opaque here and
    /// is parsed only by that plugin.
    pub fn provider_for_envelope(
        &self,
        envelope: &ProviderPluginEnvelope,
    ) -> Result<Arc<dyn ProviderPlugin>, CompositionPluginError> {
        envelope
            .validate()
            .map_err(CompositionPluginError::Manifest)?;
        let provider = self.provider(&envelope.plugin_id)?;
        if !provider
            .manifest()
            .digest()
            .eq_ignore_ascii_case(&envelope.manifest_sha256)
        {
            return Err(CompositionPluginError::Manifest(format!(
                "provider plugin {} manifest digest does not match the envelope",
                envelope.plugin_id
            )));
        }
        Ok(provider)
    }

    pub fn provider_binding_from_envelope(
        &self,
        envelope: &ProviderPluginEnvelope,
    ) -> Result<agent_core::ProviderBinding, CompositionPluginError> {
        Ok(self
            .provider_for_envelope(envelope)?
            .resolve_binding(&envelope.configuration)?)
    }

    pub async fn connect_provider_envelope(
        &self,
        envelope: &ProviderPluginEnvelope,
    ) -> Result<ProviderPlanes, CompositionPluginError> {
        Ok(self
            .provider_for_envelope(envelope)?
            .connect(&envelope.configuration)
            .await?)
    }

    /// All registered linked plugins are selected by this catalog snapshot.
    /// Their manifest digests are sealed in the composition receipt.
    pub fn plugin_bindings(&self) -> Result<Vec<PluginTierBinding>, CompositionPluginError> {
        if self.providers.is_empty() && self.profiles.is_empty() {
            return Ok(Vec::new());
        }
        self.graph()?;
        Ok(self
            .providers
            .values()
            .map(|plugin| plugin_binding(plugin.manifest()))
            .chain(
                self.profiles
                    .values()
                    .map(|plugin| plugin_binding(plugin.manifest())),
            )
            .collect())
    }

    #[must_use]
    pub fn provider_plugin_ids(&self) -> Vec<String> {
        self.providers
            .keys()
            .map(|id| id.as_str().to_owned())
            .collect()
    }

    #[must_use]
    pub fn profile_plugin_ids(&self) -> Vec<String> {
        self.profiles
            .keys()
            .map(|id| id.as_str().to_owned())
            .collect()
    }

    fn graph(&self) -> Result<PluginGraph, CompositionPluginError> {
        let manifests = self
            .providers
            .values()
            .map(|plugin| plugin.manifest().clone())
            .chain(
                self.profiles
                    .values()
                    .map(|plugin| plugin.manifest().clone()),
            )
            .collect::<Vec<_>>();
        let roots = manifests
            .iter()
            .map(|manifest| manifest.id().clone())
            .collect::<Vec<_>>();
        PluginGraph::resolve(manifests, roots)
            .map_err(|error| CompositionPluginError::Manifest(error.to_string()))
    }
}

fn validate_plugin_manifest_shape(
    manifest: &PluginManifest,
    contribution: ContributionKind,
) -> Result<(), CompositionPluginError> {
    if !manifest.contributions().contains(&contribution) {
        return Err(CompositionPluginError::WrongContribution(
            manifest.id().clone(),
        ));
    }
    if manifest.kernel_api() != KERNEL_API_VERSION {
        return Err(CompositionPluginError::Manifest(format!(
            "plugin {} requires unsupported kernel API {}",
            manifest.id(),
            manifest.kernel_api()
        )));
    }
    manifest
        .validate()
        .map_err(|error| CompositionPluginError::Manifest(error.to_string()))
}

fn plugin_binding(manifest: &PluginManifest) -> PluginTierBinding {
    PluginTierBinding {
        id: manifest.id().as_str().to_owned(),
        tier: PluginTier::BuiltIn,
        linked_manifest_sha256: Some(manifest.digest()),
        source_sha256: None,
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ProfileContext {
    #[serde(default)]
    pub recalled_memory: Vec<String>,
    #[serde(default)]
    pub values: BTreeMap<String, Value>,
    /// Runtime-activated extension tools available to this profile.
    ///
    /// This field is host-owned and is deliberately excluded from serialized
    /// task context. It lets profile policy resolve exact extension tool names
    /// without exposing composition metadata to the model.
    #[serde(skip)]
    pub available_tools: BTreeSet<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionClass {
    Direct,
    Guided,
    #[default]
    Grounded,
    Verified,
}

impl ExecutionClass {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Direct => "direct",
            Self::Guided => "guided",
            Self::Grounded => "grounded",
            Self::Verified => "verified",
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionTopology {
    #[default]
    Single,
    ParallelRead,
    SingleWithIndependentVerifier,
}

impl ExecutionTopology {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Single => "single",
            Self::ParallelRead => "parallel_read",
            Self::SingleWithIndependentVerifier => "single_with_independent_verifier",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ProfilePlan {
    pub manifest: ProfileManifest,
    pub messages: Vec<ModelMessage>,
    pub track: Option<TrackId>,
    pub allowed_tools: BTreeSet<String>,
    pub reasoning: ReasoningEffort,
    pub memory_scope: Option<String>,
    pub execution_class: ExecutionClass,
    pub required_successful_tools: BTreeSet<String>,
    pub required_checks: Vec<String>,
    /// Host-authorized argv for checks prepared from an AXD v2 contract.
    pub required_check_commands: BTreeMap<String, Vec<String>>,
    /// Evidence tools required only when a source mutation succeeds.
    pub required_successful_tools_after_mutation: BTreeSet<String>,
    pub maximum_workers: u16,
    pub topology: ExecutionTopology,
    pub output_contract: Option<String>,
    pub axd_result_binding: Option<AxdResultBinding>,
    /// Effective task budget after intersecting caller authority and policy.
    pub effective_budget: TaskBudget,
    /// Optional count ceiling. It may narrow caller authority, never widen it.
    pub policy_budget_cap: Option<TaskBudget>,
}

/// Fields required to materialize an `axd.agent-result/2` artifact after the
/// runtime has closed the evidence contract.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct AxdResultBinding {
    pub contract_digest: String,
    pub work_item_id: String,
    pub source_revision: String,
    pub contract_revision: u64,
    pub check_kinds: BTreeMap<String, String>,
}

/// Replaceable agent-policy boundary.
///
/// Implementations are pure policy: they resolve a task plus activated skills
/// and host-owned context into an immutable execution plan. I/O belongs to the
/// runtime/effect/provider boundaries, not profile implementations.
pub trait ProfileDefinition: Send + Sync + fmt::Debug {
    fn manifest(&self) -> &ProfileManifest;

    fn default_skills(&self, _track: Option<&TrackId>) -> Result<BTreeSet<SkillId>, ProfileError> {
        Ok(self.manifest().default_skills.clone())
    }

    fn prepare(
        &self,
        task: &TaskEnvelope,
        skills: &[SkillDocument],
        context: &ProfileContext,
    ) -> Result<ProfilePlan, ProfileError>;

    fn validate_output(&self, output: &str, maximum_bytes: usize) -> Result<(), ProfileError> {
        if output.trim().is_empty() {
            return Err(ProfileError::Invalid(
                "profile output must not be empty".to_owned(),
            ));
        }
        if output.len() > maximum_bytes {
            return Err(ProfileError::Invalid(format!(
                "profile output exceeds the {maximum_bytes} byte task bound"
            )));
        }
        Ok(())
    }
}

/// Immutable-by-convention host catalog of profile policy definitions.
///
/// A composition root constructs exactly one catalog snapshot and passes that
/// same value through composition and runtime admission. The runtime never
/// discovers or replaces profiles dynamically.
#[derive(Clone, Default)]
pub struct ProfileRegistry {
    entries: BTreeMap<ProfileId, Arc<dyn ProfileDefinition>>,
}

impl fmt::Debug for ProfileRegistry {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProfileRegistry")
            .field("profile_ids", &self.entries.keys().collect::<Vec<_>>())
            .finish()
    }
}

impl ProfileRegistry {
    pub fn register(&mut self, profile: Arc<dyn ProfileDefinition>) -> Result<(), ProfileError> {
        profile
            .manifest()
            .validate()
            .map_err(ProfileError::Invalid)?;
        let id = profile.manifest().id.clone();
        match self.entries.entry(id.clone()) {
            Entry::Vacant(entry) => {
                entry.insert(profile);
                Ok(())
            }
            Entry::Occupied(_) => Err(ProfileError::Duplicate(id)),
        }
    }

    pub fn resolve(&self, id: &ProfileId) -> Result<Arc<dyn ProfileDefinition>, ProfileError> {
        self.entries
            .get(id)
            .cloned()
            .ok_or_else(|| ProfileError::NotFound(id.clone()))
    }

    #[must_use]
    pub fn manifests(&self) -> Vec<ProfileManifest> {
        self.entries
            .values()
            .map(|profile| profile.manifest().clone())
            .collect()
    }

    #[must_use]
    pub fn definitions(&self) -> Vec<Arc<dyn ProfileDefinition>> {
        self.entries.values().cloned().collect()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_core::ProviderBinding;
    use agent_plugin_kernel::PluginTrust;
    use agent_provider_api::{ProviderError, ProviderPlanes};
    use async_trait::async_trait;

    #[derive(Debug)]
    struct TestProvider {
        manifest: PluginManifest,
    }

    #[async_trait]
    impl ProviderPlugin for TestProvider {
        fn manifest(&self) -> &PluginManifest {
            &self.manifest
        }

        fn resolve_binding(
            &self,
            _configuration: &Value,
        ) -> Result<ProviderBinding, ProviderError> {
            Err(ProviderError::Contract("test provider".to_owned()))
        }

        async fn connect(&self, _configuration: &Value) -> Result<ProviderPlanes, ProviderError> {
            Err(ProviderError::Contract("test provider".to_owned()))
        }
    }

    #[derive(Debug)]
    struct TestProfile {
        manifest: PluginManifest,
    }

    impl ProfilePlugin for TestProfile {
        fn manifest(&self) -> &PluginManifest {
            &self.manifest
        }

        fn register_profiles(&self, _registry: &mut ProfileRegistry) -> Result<(), ProfileError> {
            Ok(())
        }
    }

    fn manifest(
        id: &str,
        contribution: ContributionKind,
        dependency: Option<&str>,
    ) -> PluginManifest {
        let id = PluginId::parse(id.to_owned()).expect("test plugin id");
        let manifest = PluginManifest::linked(id, "1.0.0", PluginTrust::Trusted)
            .expect("test manifest")
            .contributes(contribution);
        if let Some(dependency) = dependency {
            manifest.depends_on(PluginId::parse(dependency.to_owned()).expect("dependency id"))
        } else {
            manifest
        }
    }

    #[test]
    fn dependency_resolution_waits_for_full_catalog_and_is_order_independent() {
        let profile_id = "test.profile.dependency";
        let provider_id = "test.provider.consumer";
        let mut catalog = CompositionPluginCatalog::new();
        catalog
            .register_provider(Arc::new(TestProvider {
                manifest: manifest(provider_id, ContributionKind::Provider, Some(profile_id)),
            }))
            .expect("provider registration does not depend on registration order");
        assert!(
            catalog.finalize().is_err(),
            "missing dependency is reported at finalization"
        );
        catalog
            .register_profile(Arc::new(TestProfile {
                manifest: manifest(profile_id, ContributionKind::Profiles, None),
            }))
            .expect("late dependency registration");
        catalog.finalize().expect("complete catalog graph");
        let graph = catalog.graph().expect("complete graph");
        let profile_position = graph
            .activation_order()
            .iter()
            .position(|id| id.as_str() == profile_id)
            .expect("profile activation");
        let provider_position = graph
            .activation_order()
            .iter()
            .position(|id| id.as_str() == provider_id)
            .expect("provider activation");
        assert!(profile_position < provider_position);
    }

    #[test]
    fn duplicate_provider_and_profile_ids_are_rejected_at_registration() {
        let id = "test.shared.plugin";
        let mut provider_first = CompositionPluginCatalog::new();
        provider_first
            .register_provider(Arc::new(TestProvider {
                manifest: manifest(id, ContributionKind::Provider, None),
            }))
            .expect("provider registration");
        let error = provider_first
            .register_profile(Arc::new(TestProfile {
                manifest: manifest(id, ContributionKind::Profiles, None),
            }))
            .expect_err("cross-boundary duplicate must fail");
        assert!(error.to_string().contains("crosses provider/profile"));

        let mut profile_first = CompositionPluginCatalog::new();
        profile_first
            .register_profile(Arc::new(TestProfile {
                manifest: manifest(id, ContributionKind::Profiles, None),
            }))
            .expect("profile registration");
        let error = profile_first
            .register_provider(Arc::new(TestProvider {
                manifest: manifest(id, ContributionKind::Provider, None),
            }))
            .expect_err("cross-boundary duplicate must fail in reverse order");
        assert!(error.to_string().contains("crosses provider/profile"));
    }

    #[test]
    fn provider_envelope_requires_catalog_identity_and_matching_manifest_digest() {
        let provider_id = "test.provider.envelope";
        let provider_manifest = manifest(provider_id, ContributionKind::Provider, None);
        let provider_digest = provider_manifest.digest();
        let mut catalog = CompositionPluginCatalog::new();
        catalog
            .register_provider(Arc::new(TestProvider {
                manifest: provider_manifest,
            }))
            .expect("provider registration");
        catalog.finalize().expect("complete graph");

        let envelope = ProviderPluginEnvelope {
            plugin_id: provider_id.to_owned(),
            manifest_sha256: provider_digest,
            configuration: serde_json::json!({"owned": true}),
        };
        assert!(catalog.provider_for_envelope(&envelope).is_ok());

        let mut wrong_digest = envelope.clone();
        wrong_digest.manifest_sha256 = "a".repeat(64);
        let error = catalog
            .provider_for_envelope(&wrong_digest)
            .expect_err("digest drift must fail before plugin config parsing");
        assert!(error.to_string().contains("manifest digest"));

        let mut unknown = envelope;
        unknown.plugin_id = "test.provider.unknown".to_owned();
        unknown.manifest_sha256 = "b".repeat(64);
        let error = catalog
            .provider_for_envelope(&unknown)
            .expect_err("unknown plugin must not silently fall back");
        assert!(error.to_string().contains("not registered"));
    }

    #[test]
    fn empty_catalog_has_no_selected_plugins_without_fabricating_a_graph() {
        let catalog = CompositionPluginCatalog::new();
        catalog
            .finalize()
            .expect("empty catalog is a valid snapshot");
        assert_eq!(
            catalog.plugin_bindings().expect("empty bindings"),
            [] as [agent_core::PluginTierBinding; 0]
        );
        assert!(
            catalog
                .profiles()
                .expect("empty profile registry")
                .is_empty()
        );
        let provider_error = catalog
            .provider("missing.provider")
            .expect_err("missing provider remains a not-found error");
        assert!(matches!(
            provider_error,
            CompositionPluginError::ProviderNotFound(id) if id == "missing.provider"
        ));
    }
}
