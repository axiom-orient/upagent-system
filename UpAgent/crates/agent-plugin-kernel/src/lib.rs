//! Deterministic plugin graph contracts for `UpAgent`.
//!
//! This crate owns no I/O and no runtime service locator. It validates a
//! host-selected extension graph and produces a stable activation order and digest.
//! `agent-runtime` remains the kernel host and publishes typed registries only
//! after every selected plugin contribution has succeeded.

#![forbid(unsafe_code)]

use std::{
    collections::{BTreeMap, BTreeSet},
    error::Error,
    fmt,
};

use sha2::{Digest, Sha256};

/// Plugin API implemented by this kernel revision.
pub const KERNEL_API_VERSION: u32 = 1;

const MAXIMUM_CONFIGURATION_ENTRIES: usize = 64;
const MAXIMUM_CONFIGURATION_KEY_BYTES: usize = 128;
const MAXIMUM_CONFIGURATION_VALUE_BYTES: usize = 16 * 1024;

/// Stable plugin identifier.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct PluginId(String);

impl PluginId {
    /// Parses a bounded identifier.
    pub fn parse(value: impl Into<String>) -> Result<Self, PluginGraphError> {
        let value = value.into();
        let valid = !value.is_empty()
            && value.len() <= 128
            && value.bytes().all(|byte| {
                byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_' | b'/')
            });
        if valid {
            Ok(Self(value))
        } else {
            Err(PluginGraphError::InvalidIdentifier(value))
        }
    }

    /// Returns the identifier text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for PluginId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// Trust classification for linked Rust extensions.
///
/// `UpAgent` intentionally exposes no generic process/Wasm plugin host. External
/// executables use their own explicit, versioned adapter boundaries.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum PluginTrust {
    /// UpAgent-owned code compiled into the product.
    BuiltIn,
    /// Owner-approved linked Rust extension.
    Trusted,
}

impl PluginTrust {
    /// Stable wire label used by diagnostics and composition fingerprints.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::BuiltIn => "built_in",
            Self::Trusted => "trusted",
        }
    }
}

/// Typed registry area contributed by a plugin.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum ContributionKind {
    Tools,
    Verifiers,
    Provider,
    Profiles,
}

impl ContributionKind {
    /// Stable wire label used by diagnostics and composition fingerprints.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Tools => "tools",
            Self::Verifiers => "verifiers",
            Self::Provider => "provider",
            Self::Profiles => "profiles",
        }
    }
}

/// Immutable linked-Rust plugin contract.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PluginManifest {
    id: PluginId,
    version: String,
    kernel_api: u32,
    trust: PluginTrust,
    dependencies: BTreeSet<PluginId>,
    contributions: BTreeSet<ContributionKind>,
    configuration: BTreeMap<String, String>,
}

impl PluginManifest {
    /// Creates a linked Rust manifest targeting the current kernel API.
    pub fn linked(
        id: PluginId,
        version: impl Into<String>,
        trust: PluginTrust,
    ) -> Result<Self, PluginGraphError> {
        let version = version.into();
        if version.trim().is_empty() || version.len() > 64 || version.chars().any(char::is_control)
        {
            return Err(PluginGraphError::InvalidVersion(version));
        }
        Ok(Self {
            id,
            version,
            kernel_api: KERNEL_API_VERSION,
            trust,
            dependencies: BTreeSet::new(),
            contributions: BTreeSet::new(),
            configuration: BTreeMap::new(),
        })
    }

    #[cfg(test)]
    fn linked_for_kernel_api(
        id: PluginId,
        version: impl Into<String>,
        kernel_api: u32,
        trust: PluginTrust,
    ) -> Result<Self, PluginGraphError> {
        let mut manifest = Self::linked(id, version, trust)?;
        manifest.kernel_api = kernel_api;
        Ok(manifest)
    }

    /// Adds a dependency. Dependencies determine activation order.
    #[must_use]
    pub fn depends_on(mut self, dependency: PluginId) -> Self {
        self.dependencies.insert(dependency);
        self
    }

    /// Declares one typed contribution area.
    #[must_use]
    pub fn contributes(mut self, kind: ContributionKind) -> Self {
        self.contributions.insert(kind);
        self
    }

    /// Adds deterministic configuration to the composition identity.
    #[must_use]
    pub fn configured(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.configuration.insert(key.into(), value.into());
        self
    }

    /// Plugin identifier.
    #[must_use]
    pub fn id(&self) -> &PluginId {
        &self.id
    }

    /// Plugin version.
    #[must_use]
    pub fn version(&self) -> &str {
        &self.version
    }

    /// Required kernel API.
    #[must_use]
    pub const fn kernel_api(&self) -> u32 {
        self.kernel_api
    }

    /// Trust class.
    #[must_use]
    pub const fn trust(&self) -> PluginTrust {
        self.trust
    }

    /// Declared dependencies.
    #[must_use]
    pub fn dependencies(&self) -> &BTreeSet<PluginId> {
        &self.dependencies
    }

    /// Typed contribution areas.
    #[must_use]
    pub fn contributions(&self) -> &BTreeSet<ContributionKind> {
        &self.contributions
    }

    /// Deterministic configuration.
    #[must_use]
    pub fn configuration(&self) -> &BTreeMap<String, String> {
        &self.configuration
    }

    /// Stable, delimiter-safe digest of this manifest.
    ///
    /// Every field is length-prefixed before hashing. This prevents a plugin
    /// configuration value containing newlines or separator characters from
    /// producing the same identity as a different field layout.
    #[must_use]
    pub fn digest(&self) -> String {
        let mut hasher = Sha256::new();
        hash_field(&mut hasher, b"schema", b"upagent-plugin-manifest-v2");
        hash_field(&mut hasher, b"id", self.id.as_str().as_bytes());
        hash_field(&mut hasher, b"version", self.version.as_bytes());
        hash_field(&mut hasher, b"kernel-api", &self.kernel_api.to_be_bytes());
        hash_field(&mut hasher, b"trust", self.trust.as_str().as_bytes());
        for dependency in &self.dependencies {
            hash_field(&mut hasher, b"dependency", dependency.as_str().as_bytes());
        }
        for contribution in &self.contributions {
            hash_field(
                &mut hasher,
                b"contribution",
                contribution.as_str().as_bytes(),
            );
        }
        for (key, value) in &self.configuration {
            hash_field(&mut hasher, b"configuration-key", key.as_bytes());
            hash_field(&mut hasher, b"configuration-value", value.as_bytes());
        }
        format!("{:x}", hasher.finalize())
    }

    /// Validates manifest-local bounds without resolving dependencies.
    pub fn validate(&self) -> Result<(), PluginGraphError> {
        if self.configuration.len() > MAXIMUM_CONFIGURATION_ENTRIES {
            return Err(PluginGraphError::TooManyConfigurationEntries {
                plugin: self.id.clone(),
                actual: self.configuration.len(),
                maximum: MAXIMUM_CONFIGURATION_ENTRIES,
            });
        }
        for (key, value) in &self.configuration {
            let key_valid = !key.is_empty()
                && key.len() <= MAXIMUM_CONFIGURATION_KEY_BYTES
                && key.bytes().all(|byte| {
                    byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_' | b'/')
                });
            if !key_valid {
                return Err(PluginGraphError::InvalidConfigurationKey {
                    plugin: self.id.clone(),
                    key: key.clone(),
                });
            }
            if value.len() > MAXIMUM_CONFIGURATION_VALUE_BYTES || value.contains('\0') {
                return Err(PluginGraphError::InvalidConfigurationValue {
                    plugin: self.id.clone(),
                    key: key.clone(),
                    actual_bytes: value.len(),
                    maximum_bytes: MAXIMUM_CONFIGURATION_VALUE_BYTES,
                });
            }
        }
        Ok(())
    }
}

/// Observable startup lifecycle. Durable promotion remains a runtime decision.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PluginLifecycleEvent {
    Registered { plugin: PluginId },
    ResolutionStarted,
    Resolved { plugin: PluginId },
    ContributionStarted { plugin: PluginId },
    Active { plugin: PluginId },
    Ready { fingerprint: String },
}

/// Resolved deterministic graph.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PluginGraph {
    activation_order: Vec<PluginId>,
    manifests: BTreeMap<PluginId, PluginManifest>,
    fingerprint: String,
    events: Vec<PluginLifecycleEvent>,
}

impl PluginGraph {
    /// Resolves the roots and all transitive dependencies.
    pub fn resolve(
        manifests: impl IntoIterator<Item = PluginManifest>,
        roots: impl IntoIterator<Item = PluginId>,
    ) -> Result<Self, PluginGraphError> {
        let mut catalog = BTreeMap::new();
        let mut events = Vec::new();
        for manifest in manifests {
            manifest.validate()?;
            if manifest.kernel_api() != KERNEL_API_VERSION {
                return Err(PluginGraphError::KernelApiMismatch {
                    plugin: manifest.id().clone(),
                    required: manifest.kernel_api(),
                    supported: KERNEL_API_VERSION,
                });
            }
            let id = manifest.id().clone();
            if catalog.insert(id.clone(), manifest).is_some() {
                return Err(PluginGraphError::DuplicatePlugin(id));
            }
            events.push(PluginLifecycleEvent::Registered { plugin: id });
        }
        let roots = roots.into_iter().collect::<BTreeSet<_>>();
        if roots.is_empty() {
            return Err(PluginGraphError::EmptySelection);
        }
        events.push(PluginLifecycleEvent::ResolutionStarted);

        let selected = select_transitive(&catalog, &roots)?;
        let activation_order = topological_order(&catalog, &selected)?;
        for plugin in &activation_order {
            events.push(PluginLifecycleEvent::Resolved {
                plugin: plugin.clone(),
            });
        }
        let fingerprint = graph_fingerprint(&catalog, &activation_order);
        Ok(Self {
            activation_order,
            manifests: catalog,
            fingerprint,
            events,
        })
    }

    /// Deterministic dependency-first activation order.
    #[must_use]
    pub fn activation_order(&self) -> &[PluginId] {
        &self.activation_order
    }

    /// Looks up one selected or unselected catalog manifest.
    #[must_use]
    pub fn manifest(&self, id: &PluginId) -> Option<&PluginManifest> {
        self.manifests.get(id)
    }

    /// Stable graph digest.
    #[must_use]
    pub fn fingerprint(&self) -> &str {
        &self.fingerprint
    }

    /// Resolution lifecycle events.
    #[must_use]
    pub fn events(&self) -> &[PluginLifecycleEvent] {
        &self.events
    }
}

fn select_transitive(
    catalog: &BTreeMap<PluginId, PluginManifest>,
    roots: &BTreeSet<PluginId>,
) -> Result<BTreeSet<PluginId>, PluginGraphError> {
    let mut selected = BTreeSet::new();
    let mut pending = roots.iter().cloned().collect::<Vec<_>>();
    while let Some(plugin) = pending.pop() {
        if !selected.insert(plugin.clone()) {
            continue;
        }
        let manifest = catalog
            .get(&plugin)
            .ok_or_else(|| PluginGraphError::MissingPlugin(plugin.clone()))?;
        pending.extend(manifest.dependencies().iter().cloned());
    }
    Ok(selected)
}

fn topological_order(
    catalog: &BTreeMap<PluginId, PluginManifest>,
    selected: &BTreeSet<PluginId>,
) -> Result<Vec<PluginId>, PluginGraphError> {
    let mut indegree = selected
        .iter()
        .cloned()
        .map(|plugin| (plugin, 0_usize))
        .collect::<BTreeMap<_, _>>();
    let mut dependants = BTreeMap::<PluginId, BTreeSet<PluginId>>::new();
    for plugin in selected {
        let manifest = catalog
            .get(plugin)
            .ok_or_else(|| PluginGraphError::MissingPlugin(plugin.clone()))?;
        for dependency in manifest.dependencies() {
            if !selected.contains(dependency) {
                return Err(PluginGraphError::MissingPlugin(dependency.clone()));
            }
            if let Some(value) = indegree.get_mut(plugin) {
                *value = value.saturating_add(1);
            }
            dependants
                .entry(dependency.clone())
                .or_default()
                .insert(plugin.clone());
        }
    }

    let mut ready = indegree
        .iter()
        .filter_map(|(plugin, degree)| (*degree == 0).then_some(plugin.clone()))
        .collect::<BTreeSet<_>>();
    let mut order = Vec::with_capacity(selected.len());
    while let Some(plugin) = ready.pop_first() {
        order.push(plugin.clone());
        if let Some(children) = dependants.get(&plugin) {
            for child in children {
                if let Some(degree) = indegree.get_mut(child) {
                    *degree = degree.saturating_sub(1);
                    if *degree == 0 {
                        ready.insert(child.clone());
                    }
                }
            }
        }
    }
    if order.len() != selected.len() {
        let resolved = order.iter().cloned().collect::<BTreeSet<_>>();
        let cycle = selected.difference(&resolved).cloned().collect();
        return Err(PluginGraphError::DependencyCycle(cycle));
    }
    Ok(order)
}

fn graph_fingerprint(catalog: &BTreeMap<PluginId, PluginManifest>, order: &[PluginId]) -> String {
    let mut hasher = Sha256::new();
    hash_field(&mut hasher, b"schema", b"upagent-plugin-graph-v1");
    for plugin in order {
        hash_field(&mut hasher, b"plugin-id", plugin.as_str().as_bytes());
        if let Some(manifest) = catalog.get(plugin) {
            hash_field(&mut hasher, b"plugin-digest", manifest.digest().as_bytes());
        }
    }
    format!("{:x}", hasher.finalize())
}

fn hash_field(hasher: &mut Sha256, label: &[u8], value: &[u8]) {
    hash_bytes(hasher, label);
    hash_bytes(hasher, value);
}

fn hash_bytes(hasher: &mut Sha256, value: &[u8]) {
    let length = u64::try_from(value.len()).unwrap_or(u64::MAX);
    hasher.update(length.to_be_bytes());
    hasher.update(value);
}

/// Graph validation failure.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PluginGraphError {
    InvalidIdentifier(String),
    InvalidVersion(String),
    TooManyConfigurationEntries {
        plugin: PluginId,
        actual: usize,
        maximum: usize,
    },
    InvalidConfigurationKey {
        plugin: PluginId,
        key: String,
    },
    InvalidConfigurationValue {
        plugin: PluginId,
        key: String,
        actual_bytes: usize,
        maximum_bytes: usize,
    },
    DuplicatePlugin(PluginId),
    MissingPlugin(PluginId),
    EmptySelection,
    KernelApiMismatch {
        plugin: PluginId,
        required: u32,
        supported: u32,
    },
    DependencyCycle(BTreeSet<PluginId>),
}

impl fmt::Display for PluginGraphError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidIdentifier(value) => {
                write!(formatter, "invalid plugin identifier: {value}")
            }
            Self::InvalidVersion(value) => write!(formatter, "invalid plugin version: {value}"),
            Self::TooManyConfigurationEntries {
                plugin,
                actual,
                maximum,
            } => write!(
                formatter,
                "plugin {plugin} has {actual} configuration entries; maximum is {maximum}"
            ),
            Self::InvalidConfigurationKey { plugin, key } => {
                write!(
                    formatter,
                    "plugin {plugin} has invalid configuration key {key:?}"
                )
            }
            Self::InvalidConfigurationValue {
                plugin,
                key,
                actual_bytes,
                maximum_bytes,
            } => write!(
                formatter,
                "plugin {plugin} configuration {key:?} is {actual_bytes} bytes; maximum is {maximum_bytes}"
            ),
            Self::DuplicatePlugin(plugin) => write!(formatter, "duplicate plugin: {plugin}"),
            Self::MissingPlugin(plugin) => write!(formatter, "missing plugin: {plugin}"),
            Self::EmptySelection => formatter.write_str("plugin selection is empty"),
            Self::KernelApiMismatch {
                plugin,
                required,
                supported,
            } => write!(
                formatter,
                "plugin {plugin} requires kernel API {required}; host supports {supported}"
            ),
            Self::DependencyCycle(plugins) => {
                write!(formatter, "plugin dependency cycle: {plugins:?}")
            }
        }
    }
}

impl Error for PluginGraphError {}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(value: &str) -> Result<PluginId, PluginGraphError> {
        PluginId::parse(value)
    }

    fn manifest(value: &str) -> Result<PluginManifest, PluginGraphError> {
        PluginManifest::linked(id(value)?, "1.0.0", PluginTrust::BuiltIn)
    }

    #[test]
    fn resolves_dependencies_before_consumers() -> Result<(), PluginGraphError> {
        let base = manifest("base")?.contributes(ContributionKind::Verifiers);
        let leaf = manifest("leaf")?
            .depends_on(id("base")?)
            .contributes(ContributionKind::Tools);
        let graph = PluginGraph::resolve([leaf, base], [id("leaf")?])?;
        assert_eq!(graph.activation_order(), &[id("base")?, id("leaf")?]);
        Ok(())
    }

    #[test]
    fn rejects_cycles() -> Result<(), PluginGraphError> {
        let left = manifest("left")?.depends_on(id("right")?);
        let right = manifest("right")?.depends_on(id("left")?);
        assert!(matches!(
            PluginGraph::resolve([left, right], [id("left")?]),
            Err(PluginGraphError::DependencyCycle(_))
        ));
        Ok(())
    }

    #[test]
    fn rejects_missing_dependency() -> Result<(), PluginGraphError> {
        let missing = id("missing")?;
        let root = manifest("root")?.depends_on(missing.clone());
        assert!(matches!(
            PluginGraph::resolve([root], [id("root")?]),
            Err(PluginGraphError::MissingPlugin(plugin)) if plugin == missing
        ));
        Ok(())
    }

    #[test]
    fn rejects_incompatible_kernel_api() -> Result<(), PluginGraphError> {
        let plugin = PluginManifest::linked_for_kernel_api(
            id("future")?,
            "1.0.0",
            KERNEL_API_VERSION.saturating_add(1),
            PluginTrust::Trusted,
        )?;
        assert!(matches!(
            PluginGraph::resolve([plugin], [id("future")?]),
            Err(PluginGraphError::KernelApiMismatch { .. })
        ));
        Ok(())
    }

    #[test]
    fn fingerprint_is_order_independent_and_configuration_sensitive() -> Result<(), PluginGraphError>
    {
        let first = manifest("first")?
            .configured("mode", "a")
            .contributes(ContributionKind::Verifiers);
        let second = manifest("second")?
            .depends_on(id("first")?)
            .contributes(ContributionKind::Tools);
        let left = PluginGraph::resolve([first.clone(), second.clone()], [id("second")?])?;
        let right = PluginGraph::resolve([second, first], [id("second")?])?;
        assert_eq!(left.fingerprint(), right.fingerprint());

        let changed = PluginGraph::resolve(
            [
                manifest("first")?
                    .configured("mode", "b")
                    .contributes(ContributionKind::Verifiers),
                manifest("second")?
                    .depends_on(id("first")?)
                    .contributes(ContributionKind::Tools),
            ],
            [id("second")?],
        )?;
        assert_ne!(left.fingerprint(), changed.fingerprint());
        Ok(())
    }

    #[test]
    fn manifest_digest_is_delimiter_safe() -> Result<(), PluginGraphError> {
        let embedded = manifest("same")?.configured("a", "b\nconfiguration.c=d");
        let split = manifest("same")?.configured("a", "b").configured("c", "d");
        assert_ne!(embedded.digest(), split.digest());
        Ok(())
    }

    #[test]
    fn rejects_oversized_or_nul_configuration() -> Result<(), PluginGraphError> {
        let mut too_many = manifest("too-many")?;
        for index in 0..=MAXIMUM_CONFIGURATION_ENTRIES {
            too_many = too_many.configured(format!("key-{index}"), "value");
        }
        assert!(matches!(
            PluginGraph::resolve([too_many], [id("too-many")?]),
            Err(PluginGraphError::TooManyConfigurationEntries { .. })
        ));

        let nul = manifest("nul")?.configured("key", "before\0after");
        assert!(matches!(
            PluginGraph::resolve([nul], [id("nul")?]),
            Err(PluginGraphError::InvalidConfigurationValue { .. })
        ));
        Ok(())
    }

    #[test]
    fn rejects_invalid_configuration_before_resolution() -> Result<(), PluginGraphError> {
        let plugin = manifest("configured")?.configured("bad key", "value");
        assert!(matches!(
            PluginGraph::resolve([plugin], [id("configured")?]),
            Err(PluginGraphError::InvalidConfigurationKey { .. })
        ));
        Ok(())
    }
}
