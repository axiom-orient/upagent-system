#![forbid(unsafe_code)]
#![recursion_limit = "256"]

use sha2::{Digest, Sha256};

mod approval;
mod artifact;
mod cancellation;
mod composition;
mod extension;
mod image_tools;
mod imagemaker;
mod interaction_tools;
mod journal;
mod memory_tools;
mod plugin;
mod promptgen;
mod semantic_mask;
mod session;
mod tool_authority;

pub use approval::{
    ApprovalError, ApprovalGrantIssued, ApprovalRecord, ApprovalStatus, ApprovalStore,
};
pub use artifact::*;
pub use cancellation::*;
pub use composition::*;
pub use extension::*;
pub use image_tools::*;
pub use imagemaker::ImageMakerToolModule;
pub use journal::{
    JournalRecord, RecoveryEntry, TaskInspection, list_incomplete as list_recoveries,
    resolve_incomplete as resolve_recovery,
};
pub use promptgen::PromptGenAdapter;
pub use semantic_mask::*;

/// Read and verify one stopped task's durable request, event chain, and
/// optional terminal result without creating or changing filesystem evidence.
pub fn inspect_task(
    state_root: impl AsRef<std::path::Path>,
    session_id: &str,
    task_id: &str,
) -> Result<TaskInspection, RuntimeError> {
    journal::inspect_task(state_root.as_ref(), session_id, task_id)
        .map_err(|error| RuntimeError::Storage(error.to_string()))
}
pub use plugin::{
    CompletionEvidence, CompletionVerifier, PluginBindingError, PluginExecutionBinding,
    PluginOrigin, PluginRegistrar, PluginSnapshot, RuntimePlugin, RuntimePluginError,
    StaticToolsPlugin,
};
pub use session::*;

pub use agent_effects::{EffectRecoveryEntry, EffectRecoveryRecord, EffectRecoveryResolution};

pub fn list_effect_recoveries(
    workspace: impl AsRef<std::path::Path>,
    state_root: impl AsRef<std::path::Path>,
) -> Result<Vec<EffectRecoveryEntry>, RuntimeError> {
    open_effect_plane(workspace, state_root)?
        .list_recovery()
        .map_err(|error| RuntimeError::Storage(error.to_string()))
}

pub fn resolve_effect_recovery(
    workspace: impl AsRef<std::path::Path>,
    state_root: impl AsRef<std::path::Path>,
    run_id: &str,
    resolution: EffectRecoveryResolution,
    note: Option<String>,
) -> Result<EffectRecoveryRecord, RuntimeError> {
    open_effect_plane(workspace, state_root)?
        .resolve_recovery(run_id, resolution, note)
        .map_err(|error| RuntimeError::Storage(error.to_string()))
}

fn open_effect_plane(
    workspace: impl AsRef<std::path::Path>,
    state_root: impl AsRef<std::path::Path>,
) -> Result<agent_effects::EffectPlane, RuntimeError> {
    let workspace = workspace
        .as_ref()
        .canonicalize()
        .map_err(|error| RuntimeError::Storage(error.to_string()))?;
    if !workspace.is_dir() {
        return Err(RuntimeError::Contract(
            "effect workspace must be an existing directory".to_owned(),
        ));
    }
    let state_root = canonicalize_effect_path(state_root.as_ref())?;
    if state_root.starts_with(&workspace) || workspace.starts_with(&state_root) {
        return Err(RuntimeError::Contract(
            "effect state root and workspace must not overlap".to_owned(),
        ));
    }
    let workspace_text = workspace
        .to_str()
        .ok_or_else(|| RuntimeError::Contract("workspace path must be UTF-8".to_owned()))?;
    let digest = format!("{:x}", Sha256::digest(workspace_text.as_bytes()));
    let state_dir = state_root.join("effects").join(digest);
    agent_effects::EffectPlane::open(workspace, state_dir)
        .map_err(|error| RuntimeError::Storage(error.to_string()))
}

fn canonicalize_effect_path(path: &std::path::Path) -> Result<std::path::PathBuf, RuntimeError> {
    if !path.is_absolute() {
        return Err(RuntimeError::Contract(
            "effect state root must be an absolute path".to_owned(),
        ));
    }
    if let Ok(metadata) = std::fs::symlink_metadata(path) {
        if metadata.file_type().is_symlink() {
            return Err(RuntimeError::Contract(
                "effect state root must not be a symbolic link".to_owned(),
            ));
        }
        if !metadata.is_dir() {
            return Err(RuntimeError::Contract(
                "effect state root must be a directory".to_owned(),
            ));
        }
    }
    let mut cursor = path;
    let mut missing = Vec::new();
    loop {
        match cursor.canonicalize() {
            Ok(mut resolved) => {
                for component in missing.iter().rev() {
                    resolved.push(component);
                }
                return Ok(resolved);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let leaf = cursor.file_name().ok_or_else(|| {
                    RuntimeError::Storage(format!(
                        "failed to resolve effect state root ancestor {}",
                        path.display()
                    ))
                })?;
                missing.push(leaf.to_owned());
                cursor = cursor.parent().ok_or_else(|| {
                    RuntimeError::Storage("effect state root has no existing ancestor".to_owned())
                })?;
            }
            Err(error) => {
                return Err(RuntimeError::Storage(format!(
                    "failed to resolve effect state root: {error}"
                )));
            }
        }
    }
}
