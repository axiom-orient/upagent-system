use std::{collections::BTreeMap, fmt};

use agent_plugin_kernel::{PluginId, PluginLifecycleEvent, PluginManifest};
use agent_profile_api::ProfilePlan;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::tool_authority::built_in_tool_authority;

/// Provenance attached to one linked-Rust runtime extension.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PluginOrigin {
    id: PluginId,
    version: String,
    manifest_digest: String,
}

impl PluginOrigin {
    pub(super) fn from_manifest(manifest: &PluginManifest) -> Self {
        Self {
            id: manifest.id().clone(),
            version: manifest.version().to_owned(),
            manifest_digest: manifest.digest(),
        }
    }

    #[must_use]
    pub fn id(&self) -> &PluginId {
        &self.id
    }

    #[must_use]
    pub fn version(&self) -> &str {
        &self.version
    }

    #[must_use]
    pub fn manifest_digest(&self) -> &str {
        &self.manifest_digest
    }

    #[must_use]
    pub(crate) fn as_json(&self) -> Value {
        json!({
            "id": self.id.as_str(),
            "version": self.version.as_str(),
            "manifestDigest": self.manifest_digest.as_str(),
        })
    }
}

/// Frozen active extension graph and contribution provenance.
///
/// Profiles, provider, and skills are constructor/composition authorities and
/// intentionally do not appear here. This snapshot describes only actual
/// runtime extension variation: tools and completion verifiers.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PluginSnapshot {
    pub(super) fingerprint: String,
    pub(super) activation_order: Vec<PluginOrigin>,
    pub(super) lifecycle: Vec<PluginLifecycleEvent>,
    pub(super) tool_origins: BTreeMap<String, PluginOrigin>,
    pub(super) verifier_origins: BTreeMap<String, PluginOrigin>,
}

/// Task-specific projection of the extension set actually used by a plan.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PluginExecutionBinding {
    fingerprint: String,
    plugin_set_fingerprint: String,
    tool_origins: BTreeMap<String, SelectedToolOrigin>,
    verifier_origins: BTreeMap<String, PluginOrigin>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum SelectedToolOrigin {
    Plugin(PluginOrigin),
    PrivilegedHost(&'static str),
}

impl SelectedToolOrigin {
    fn as_json(&self) -> Value {
        match self {
            Self::Plugin(origin) => json!({"kind":"plugin", "plugin":origin.as_json()}),
            Self::PrivilegedHost(id) => json!({
                "kind":"privileged_host",
                "id":id,
                "version":env!("CARGO_PKG_VERSION"),
            }),
        }
    }

    fn digest_identity(&self) -> String {
        match self {
            Self::Plugin(origin) => format!("plugin:{}", origin.manifest_digest()),
            Self::PrivilegedHost(id) => {
                format!("privileged-host:{id}@{}", env!("CARGO_PKG_VERSION"))
            }
        }
    }
}

impl PluginExecutionBinding {
    #[must_use]
    pub fn fingerprint(&self) -> &str {
        &self.fingerprint
    }

    #[must_use]
    pub fn plugin_set_fingerprint(&self) -> &str {
        &self.plugin_set_fingerprint
    }

    #[must_use]
    pub fn as_json(&self) -> Value {
        json!({
            "fingerprint":self.fingerprint.as_str(),
            "pluginSetDigest":self.plugin_set_fingerprint.as_str(),
            "selectedTools":self.tool_origins.iter().map(|(tool, origin)| {
                json!({"tool":tool, "origin":origin.as_json()})
            }).collect::<Vec<_>>(),
            "verifiers":self.verifier_origins.iter().map(|(id, origin)| {
                json!({"verifier":id, "plugin":origin.as_json()})
            }).collect::<Vec<_>>(),
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PluginBindingError {
    MissingToolOrigin(String),
}

impl fmt::Display for PluginBindingError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingToolOrigin(tool) => write!(
                formatter,
                "active tool {tool} has no extension or privileged-host authority"
            ),
        }
    }
}

impl std::error::Error for PluginBindingError {}

impl PluginSnapshot {
    #[must_use]
    pub fn fingerprint(&self) -> &str {
        &self.fingerprint
    }

    #[must_use]
    pub fn activation_order(&self) -> &[PluginOrigin] {
        &self.activation_order
    }

    #[must_use]
    pub fn lifecycle(&self) -> &[PluginLifecycleEvent] {
        &self.lifecycle
    }

    #[must_use]
    pub fn tool_origin(&self, tool: &str) -> Option<&PluginOrigin> {
        self.tool_origins.get(tool)
    }

    #[must_use]
    pub fn verifier_origins(&self) -> &BTreeMap<String, PluginOrigin> {
        &self.verifier_origins
    }

    #[must_use]
    pub fn as_json(&self) -> Value {
        json!({
            "fingerprint":self.fingerprint.as_str(),
            "activationOrder":self.activation_order.iter().map(PluginOrigin::as_json).collect::<Vec<_>>(),
            "tools":self.tool_origins.iter().map(|(tool, origin)| {
                json!({"tool":tool, "plugin":origin.as_json()})
            }).collect::<Vec<_>>(),
            "verifiers":self.verifier_origins.iter().map(|(id, origin)| {
                json!({"verifier":id, "plugin":origin.as_json()})
            }).collect::<Vec<_>>(),
        })
    }

    pub(crate) fn bind_execution(
        &self,
        plan: &ProfilePlan,
    ) -> Result<PluginExecutionBinding, PluginBindingError> {
        let tool_origins = plan
            .allowed_tools
            .iter()
            .map(|tool| {
                let origin = if let Some(origin) = self.tool_origins.get(tool) {
                    SelectedToolOrigin::Plugin(origin.clone())
                } else if let Some(authority) = built_in_tool_authority(tool) {
                    SelectedToolOrigin::PrivilegedHost(authority.id())
                } else {
                    return Err(PluginBindingError::MissingToolOrigin(tool.clone()));
                };
                Ok((tool.clone(), origin))
            })
            .collect::<Result<BTreeMap<_, _>, PluginBindingError>>()?;

        let mut hasher = Sha256::new();
        hash_field(
            &mut hasher,
            b"schema",
            b"upagent-plugin-execution-binding-v2",
        );
        hash_field(&mut hasher, b"plugin-set", self.fingerprint.as_bytes());
        for (tool, origin) in &tool_origins {
            hash_field(&mut hasher, b"selected-tool", tool.as_bytes());
            let digest = origin.digest_identity();
            hash_field(&mut hasher, b"tool-origin", digest.as_bytes());
        }
        for (id, origin) in &self.verifier_origins {
            hash_field(&mut hasher, b"verifier-id", id.as_bytes());
            hash_field(
                &mut hasher,
                b"verifier-origin",
                origin.manifest_digest().as_bytes(),
            );
        }
        Ok(PluginExecutionBinding {
            fingerprint: format!("{:x}", hasher.finalize()),
            plugin_set_fingerprint: self.fingerprint.clone(),
            tool_origins,
            verifier_origins: self.verifier_origins.clone(),
        })
    }

    pub(crate) fn replace_tool_origins(&mut self, tools: &BTreeMap<String, crate::RegisteredTool>) {
        self.tool_origins = tools
            .iter()
            .map(|(name, tool)| (name.clone(), tool.origin.clone()))
            .collect();
    }
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
