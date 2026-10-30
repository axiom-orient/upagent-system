//! Durable create-only composition proof storage.
//!
//! Composition is resolved before credentials/provider activation. The run
//! path persists the exact receipt once, then every resume must present the
//! same task/spec digest. Existing bytes are never overwritten.

use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use std::collections::BTreeMap;

use agent_core::{CompositionReceipt, SelectedToolBinding, SelectedToolSideEffect, ToolDefinition};
use thiserror::Error;

const MAXIMUM_RECEIPT_BYTES: u64 = 1024 * 1024;

#[derive(Debug, Error)]
pub enum CompositionStoreError {
    #[error("composition store I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("composition store JSON failed: {0}")]
    Json(#[from] serde_json::Error),
    #[error("composition store contract failed: {0}")]
    Contract(String),
}

/// Return the exact canonical definitions for the built-in tools selected by
/// an explicit composition. Dynamic extension tools are intentionally absent:
/// composition has no executable extension loader.
pub fn canonical_tool_bindings(names: &[String]) -> Result<Vec<SelectedToolBinding>, String> {
    let mut definitions = BTreeMap::<String, ToolDefinition>::new();
    for definition in agent_effects::tool_definitions() {
        definitions.insert(definition.name.clone(), definition);
    }
    for definition in crate::memory_tools::definitions() {
        definitions.insert(definition.name.clone(), definition);
    }
    for definition in crate::image_tools::tool_definitions() {
        definitions.insert(definition.name.clone(), definition);
    }
    let semantic = crate::image_tools::semantic_tool_definition()?;
    definitions.insert(semantic.name.clone(), semantic);
    for definition in crate::imagemaker::definitions()? {
        definitions.insert(definition.name.clone(), definition);
    }
    let interaction = crate::interaction_tools::definition();
    definitions.insert(interaction.name.clone(), interaction);

    let mut bindings = Vec::with_capacity(names.len());
    for name in names {
        let definition = definitions
            .get(name)
            .cloned()
            .ok_or_else(|| format!("selected tool {name} has no canonical built-in definition"))?;
        let side_effect = if agent_core::is_external_image_tool_name(name) {
            SelectedToolSideEffect::External
        } else if agent_core::is_image_tool_name(name)
            || agent_core::IMAGE_MAKER_TOOL_NAMES.contains(&name.as_str())
            || matches!(
                name.as_str(),
                "repo_map" | "search_code" | "read_file" | "git_status" | "inspect_diff"
            )
        {
            SelectedToolSideEffect::ReadOnly
        } else {
            SelectedToolSideEffect::External
        };
        let origin = if agent_core::is_image_tool_name(name)
            || agent_core::IMAGE_MAKER_TOOL_NAMES.contains(&name.as_str())
        {
            format!("upagent.host.tools@{}", env!("CARGO_PKG_VERSION"))
        } else {
            let origin_id = crate::tool_authority::built_in_tool_authority(name)
                .map(crate::tool_authority::BuiltInToolAuthority::id)
                .ok_or_else(|| format!("selected tool {name} has no host authority"))?;
            format!("{origin_id}@{}", env!("CARGO_PKG_VERSION"))
        };
        bindings.push(
            SelectedToolBinding::create(definition, side_effect, &origin)
                .map_err(|error| error.to_string())?,
        );
    }
    bindings.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(bindings)
}

/// Create-only storage for one task's immutable composition receipt.
#[derive(Debug, Clone)]
pub struct CompositionStore {
    root: PathBuf,
}

impl CompositionStore {
    /// Construct a store without creating directories. This is used by
    /// provider-free composition inspection.
    pub fn existing(root: impl AsRef<Path>) -> Result<Self, CompositionStoreError> {
        let root = existing_directory(root.as_ref(), "composition state root")?;
        Ok(Self { root })
    }

    /// Construct a store for the run path, creating only the bounded state
    /// directories required for composition evidence.
    pub fn open(root: impl AsRef<Path>) -> Result<Self, CompositionStoreError> {
        let root = root.as_ref();
        prepare_directory(root)?;
        let root = root.canonicalize()?;
        Ok(Self { root })
    }

    /// Persist exactly one receipt. A second invocation must contain byte- and
    /// digest-equivalent content; it never replaces the original.
    pub fn create_only(
        &self,
        receipt: &CompositionReceipt,
    ) -> Result<PathBuf, CompositionStoreError> {
        receipt
            .verify()
            .map_err(|error| CompositionStoreError::Contract(error.to_string()))?;
        let directory = self
            .root
            .join("compositions")
            .join(receipt.session_id.as_str())
            .join(receipt.task_id.as_str());
        prepare_directory(&directory)?;
        let path = directory.join("composition.json");
        let bytes = serde_json::to_vec_pretty(receipt)?;
        if bytes.is_empty()
            || u64::try_from(bytes.len()).unwrap_or(u64::MAX) > MAXIMUM_RECEIPT_BYTES
        {
            return Err(CompositionStoreError::Contract(
                "composition receipt exceeds its byte bound".to_owned(),
            ));
        }
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(mut file) => {
                set_private_file(&file)?;
                file.write_all(&bytes)?;
                file.sync_all()?;
                #[cfg(unix)]
                fs::File::open(&directory)?.sync_all()?;
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                let existing = read_receipt(&path)?;
                if existing.digest != receipt.digest || existing != *receipt {
                    return Err(CompositionStoreError::Contract(
                        "composition receipt already exists with a different digest".to_owned(),
                    ));
                }
            }
            Err(error) => return Err(error.into()),
        }
        Ok(path)
    }

    /// Read and verify one task receipt without creating state.
    pub fn read(
        &self,
        session_id: &str,
        task_id: &str,
    ) -> Result<CompositionReceipt, CompositionStoreError> {
        let path = self
            .root
            .join("compositions")
            .join(session_id)
            .join(task_id)
            .join("composition.json");
        let value = read_receipt(&path)?;
        value
            .verify()
            .map_err(|error| CompositionStoreError::Contract(error.to_string()))?;
        Ok(value)
    }

    /// Read a receipt when present. Missing evidence is represented as `None`
    /// so task inspections remain read-only and non-failing; a present
    /// but malformed receipt still fails closed.
    pub fn read_if_exists(
        &self,
        session_id: &str,
        task_id: &str,
    ) -> Result<Option<CompositionReceipt>, CompositionStoreError> {
        let path = self
            .root
            .join("compositions")
            .join(session_id)
            .join(task_id)
            .join("composition.json");
        match fs::symlink_metadata(&path) {
            Ok(_) => self.read(session_id, task_id).map(Some),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error.into()),
        }
    }
}

fn read_receipt(path: &Path) -> Result<CompositionReceipt, CompositionStoreError> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(CompositionStoreError::Contract(
            "composition.json must be a regular file".to_owned(),
        ));
    }
    if metadata.len() == 0 || metadata.len() > MAXIMUM_RECEIPT_BYTES {
        return Err(CompositionStoreError::Contract(
            "composition.json is outside its byte bound".to_owned(),
        ));
    }
    let file = OpenOptions::new().read(true).open(path)?;
    let mut bytes = Vec::new();
    file.take(MAXIMUM_RECEIPT_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > MAXIMUM_RECEIPT_BYTES {
        return Err(CompositionStoreError::Contract(
            "composition.json exceeds its byte bound".to_owned(),
        ));
    }
    Ok(serde_json::from_slice(&bytes)?)
}

fn prepare_directory(path: &Path) -> Result<(), CompositionStoreError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                return Err(CompositionStoreError::Contract(format!(
                    "{} must be a real directory",
                    path.display()
                )));
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            fs::create_dir_all(path)?;
        }
        Err(error) => return Err(error.into()),
    }
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(CompositionStoreError::Contract(format!(
            "{} must be a real directory",
            path.display()
        )));
    }
    set_private_directory(path)?;
    Ok(())
}

fn existing_directory(path: &Path, label: &str) -> Result<PathBuf, CompositionStoreError> {
    let metadata = fs::symlink_metadata(path).map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            CompositionStoreError::Contract(format!("{label} does not exist"))
        } else {
            CompositionStoreError::Io(error)
        }
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(CompositionStoreError::Contract(format!(
            "{label} must be a real directory"
        )));
    }
    Ok(path.to_path_buf())
}

fn set_private_directory(path: &Path) -> Result<(), CompositionStoreError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

fn set_private_file(file: &std::fs::File) -> Result<(), CompositionStoreError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_core::{
        AGENT_SPEC_SCHEMA_VERSION, AgentSpec, CompositionReceipt, EffectApprovalSpec, EffectsSpec,
        IsolationSpec, MemorySpec, PROVIDER_CAP_TEXT, PluginTier, PluginTierBinding,
        ProviderBinding, ReviewSpec, SelectedPluginBinding, SessionId, TASK_ENVELOPE_V3_VERSION,
        TaskBudget, TaskEnvelopeV3, TaskId,
    };

    fn receipt() -> CompositionReceipt {
        let task = TaskEnvelopeV3 {
            schema_version: TASK_ENVELOPE_V3_VERSION,
            task_id: TaskId::parse("task-store").expect("id"),
            session_id: SessionId::parse("session-store").expect("id"),
            parent_task_id: None,
            objective: "store".to_owned(),
            image_task: None,
            history: Vec::new(),
            workspace: None,
            context: serde_json::Map::new(),
            budget: TaskBudget::default(),
            approvals: Vec::new(),
        };
        let spec = AgentSpec {
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
        };
        CompositionReceipt::create(
            task.task_id.clone(),
            task.session_id.clone(),
            task.digest().expect("task digest"),
            ProviderBinding {
                id: "test".to_owned(),
                model: "test-model".to_owned(),
                route_sha256: "f".repeat(64),
                capabilities: std::collections::BTreeSet::from([PROVIDER_CAP_TEXT.to_owned()]),
                maximum_output_bytes: 8 * 1024 * 1024,
            },
            spec,
            Vec::new(),
            Vec::new(),
            vec![
                PluginTierBinding {
                    id: "upagent.kernel".to_owned(),
                    tier: PluginTier::BuiltIn,
                    linked_manifest_sha256: Some("a".repeat(64)),
                    source_sha256: None,
                },
                PluginTierBinding {
                    id: "upagent.provider.test".to_owned(),
                    tier: PluginTier::BuiltIn,
                    linked_manifest_sha256: Some("b".repeat(64)),
                    source_sha256: None,
                },
                PluginTierBinding {
                    id: "upagent.profile.test".to_owned(),
                    tier: PluginTier::BuiltIn,
                    linked_manifest_sha256: Some("c".repeat(64)),
                    source_sha256: None,
                },
            ],
            SelectedPluginBinding {
                plugin_id: "upagent.provider.test".to_owned(),
                manifest_sha256: "b".repeat(64),
            },
            SelectedPluginBinding {
                plugin_id: "upagent.profile.test".to_owned(),
                manifest_sha256: "c".repeat(64),
            },
        )
        .expect("receipt")
    }

    #[test]
    fn create_only_rejects_drift_and_round_trips() -> Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let store = CompositionStore::open(temp.path())?;
        let value = receipt();
        store.create_only(&value)?;
        assert_eq!(store.read("session-store", "task-store")?, value);
        let mut changed = value;
        changed.spec.profile = "developer".to_owned();
        assert!(store.create_only(&changed).is_err());
        Ok(())
    }
}
