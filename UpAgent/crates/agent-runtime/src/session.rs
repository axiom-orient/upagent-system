use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, Weak},
    time::Duration,
};

use crate::plugin::assemble_registry;
use crate::tool_authority::{
    built_in_tool_authority, is_effect_tool, is_interaction_tool, is_memory_tool,
};
use crate::{
    ApprovalStore, ArtifactStore, RegisteredTool, RuntimePlugin, TaskCancellation, ToolModule,
    ToolSideEffect, memory_tools, register_modules,
};
use agent_core::{
    AgentFailure, AgentResult, AgentStatus, ApprovalPolicy, ArtifactRef, CompositionReceipt,
    EffectApprovalMode, FailureCode, KernelEvent, MemoryPolicy, ModelMessage, ModelTurn, ProfileId,
    ProviderBinding, SelectedToolBinding, SelectedToolSideEffect, SessionId, SkillId, TaskEnvelope,
    TaskId, ToolCall, ToolDefinition, ToolResult,
};
use agent_effects::{EffectPlane, tool_definitions};
use agent_memory::MemoryStore;
use agent_profile_api::{
    ExecutionTopology, ProfileContext, ProfileDefinition, ProfilePlan, ProfileRegistry,
};
use agent_provider_api::{
    MAXIMUM_MODEL_MESSAGE_SCALARS, MAXIMUM_MODEL_MESSAGES_PER_TURN, ModelPort,
};
use agent_skills::{SkillCatalog, SkillDocument};
use futures_util::{StreamExt, stream};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use thiserror::Error;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

mod approval_gate;
pub(crate) mod checkpoint;
mod durable;
mod machine;
pub(crate) use machine::event_artifact_uses;
mod run_loop;
mod tool_dispatch;
mod tool_outcome;
mod tool_output;

use durable::{DurableSession, SuspensionBinding};
use machine::{SessionMachine, undurable_failure, undurable_suspension};
use tool_dispatch::ToolExecution;
use tool_outcome::ToolProgress;

const MAXIMUM_RECALLED_MEMORIES: usize = 20;
const MAXIMUM_MEMORY_PROJECTION_BYTES: usize = 8 * 1024;
const MAXIMUM_RECALLED_MEMORY_BYTES: usize = 128 * 1024;
const MODEL_CANCELLATION_DRAIN_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, Error)]
pub enum RuntimeError {
    #[error("runtime contract failed: {0}")]
    Contract(String),
    #[error("runtime storage failed: {0}")]
    Storage(String),
}

/// A task admitted by a host-owned composition proof.
///
/// The production runtime never accepts a raw `TaskEnvelope` for execution.
/// `ReviewCampaign` has its own provider runner and does not enter this runtime.
#[derive(Debug, Clone)]
pub struct ResolvedTask {
    task: TaskEnvelope,
    receipt: Option<CompositionReceipt>,
}

impl ResolvedTask {
    pub fn new(task: TaskEnvelope, receipt: CompositionReceipt) -> Result<Self, RuntimeError> {
        receipt
            .verify()
            .map_err(|error| RuntimeError::Contract(error.to_string()))?;
        task.validate().map_err(RuntimeError::Contract)?;
        validate_provider_budget(task.budget.maximum_output_bytes, &receipt.provider)
            .map_err(RuntimeError::Contract)?;
        if task.task_id != receipt.task_id || task.session_id != receipt.session_id {
            return Err(RuntimeError::Contract(
                "resolved task identity does not match its composition receipt".to_owned(),
            ));
        }
        let original_context = task
            .context
            .iter()
            .filter(|(key, _)| !key.starts_with("_upagent_"))
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect::<serde_json::Map<_, _>>();
        let reconstructed_ingress = agent_core::TaskEnvelopeV3 {
            schema_version: agent_core::TASK_ENVELOPE_V3_VERSION,
            task_id: task.task_id.clone(),
            session_id: task.session_id.clone(),
            parent_task_id: task.parent_task_id.clone(),
            objective: task.objective.clone(),
            image_task: task.image_task.clone(),
            history: task.history.clone(),
            workspace: task.workspace.clone(),
            context: original_context,
            budget: task.budget.clone(),
            approvals: task.approvals.clone(),
        };
        let reconstructed_task_digest = reconstructed_ingress
            .digest()
            .map_err(|error| RuntimeError::Contract(error.to_string()))?;
        if reconstructed_task_digest != receipt.task_digest {
            return Err(RuntimeError::Contract(
                "resolved task ingress does not match its composition task digest".to_owned(),
            ));
        }
        if task.context.get("_upagent_task_digest")
            != Some(&Value::String(receipt.task_digest.clone()))
        {
            return Err(RuntimeError::Contract(
                "resolved task is missing the original task digest binding".to_owned(),
            ));
        }
        if task.context.get("_upagent_composition_digest")
            != Some(&Value::String(receipt.digest.clone()))
        {
            return Err(RuntimeError::Contract(
                "resolved task is missing the composition digest binding".to_owned(),
            ));
        }
        let profile = ProfileId::parse(receipt.spec.profile.clone())
            .map_err(|error| RuntimeError::Contract(error.to_string()))?;
        if task.profile != profile {
            return Err(RuntimeError::Contract(
                "resolved task profile does not match its composition receipt".to_owned(),
            ));
        }
        let expected_skills = receipt
            .spec
            .skills
            .iter()
            .cloned()
            .map(SkillId::parse)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| RuntimeError::Contract(error.to_string()))?;
        if task.skills != expected_skills {
            return Err(RuntimeError::Contract(
                "resolved task skills do not match its composition receipt".to_owned(),
            ));
        }
        let expected_tools = receipt.spec.tools.iter().cloned().collect::<BTreeSet<_>>();
        let actual_tools = task
            .context
            .get("_upagent_composition_tools")
            .and_then(Value::as_array)
            .ok_or_else(|| {
                RuntimeError::Contract(
                    "resolved task is missing the selected tool binding".to_owned(),
                )
            })?
            .iter()
            .map(|value| {
                value.as_str().map(str::to_owned).ok_or_else(|| {
                    RuntimeError::Contract(
                        "resolved task selected tools must be strings".to_owned(),
                    )
                })
            })
            .collect::<Result<BTreeSet<_>, _>>()?;
        if actual_tools != expected_tools {
            return Err(RuntimeError::Contract(
                "resolved task tools do not match its composition receipt".to_owned(),
            ));
        }
        if task.effect_approval
            != match receipt.spec.effect_approval {
                agent_core::EffectApprovalSpec::Deny => EffectApprovalMode::Deny,
                agent_core::EffectApprovalSpec::PlanOnly => EffectApprovalMode::PlanOnly,
                agent_core::EffectApprovalSpec::TaskGrant => EffectApprovalMode::TaskGrant,
            }
            || task.allow_memory_write
                != matches!(receipt.spec.memory, agent_core::MemorySpec::LongTermScoped)
        {
            return Err(RuntimeError::Contract(
                "resolved task authority does not match its composition receipt".to_owned(),
            ));
        }
        Ok(Self {
            task,
            receipt: Some(receipt),
        })
    }

    /// Test harness adapter for the existing in-process runtime flow tests.
    /// It is intentionally available only under the non-production
    /// `test-support` feature; normal builds cannot construct an unbound task.
    #[doc(hidden)]
    #[cfg(feature = "test-support")]
    pub fn from_internal_for_tests(task: TaskEnvelope) -> Result<Self, RuntimeError> {
        task.validate().map_err(RuntimeError::Contract)?;
        Ok(Self {
            task,
            receipt: None,
        })
    }

    #[must_use]
    pub fn task(&self) -> &TaskEnvelope {
        &self.task
    }

    #[must_use]
    pub fn composition_receipt(&self) -> Option<&CompositionReceipt> {
        self.receipt.as_ref()
    }
}

#[derive(Clone)]
pub struct AgentRuntime {
    inner: Arc<RuntimeInner>,
}

impl std::fmt::Debug for AgentRuntime {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AgentRuntime")
            .field("state_root", &self.inner.state_root)
            .field("profiles", &self.inner.profiles)
            .field("skills", &self.inner.skills.descriptors().len())
            .field("maximum_parallel_tasks", &self.inner.maximum_parallel_tasks)
            .field(
                "plugin_fingerprint",
                &self.inner.plugin_snapshot.fingerprint(),
            )
            .finish_non_exhaustive()
    }
}

struct RuntimeInner {
    state_root: PathBuf,
    profiles: ProfileRegistry,
    skills: SkillCatalog,
    /// The memory store is opened only when the host-selected composition
    /// grants memory authority. Runtime paths that receive memory tools fail
    /// closed when this is `None`; a non-memory profile therefore does not
    /// create a memory directory as an incidental side effect.
    memory: Option<MemoryStore>,
    artifacts: ArtifactStore,
    approvals: ApprovalStore,
    model: Arc<dyn ModelPort>,
    task_slots: Arc<Semaphore>,
    maximum_parallel_tasks: usize,
    writer_leases: tokio::sync::Mutex<BTreeMap<PathBuf, Weak<Semaphore>>>,
    effect_planes: Mutex<BTreeMap<PathBuf, Weak<EffectPlane>>>,
    extension_tools: BTreeMap<String, RegisteredTool>,
    completion_verifiers: BTreeMap<String, Arc<dyn crate::CompletionVerifier>>,
    plugin_snapshot: crate::PluginSnapshot,
}

#[derive(Debug)]
pub struct AgentRuntimeOptions {
    pub state_root: PathBuf,
    pub skill_roots: Vec<PathBuf>,
    pub profiles: ProfileRegistry,
    pub model: Arc<dyn ModelPort>,
    pub maximum_parallel_tasks: usize,
    pub tool_modules: Vec<Arc<dyn ToolModule>>,
}

impl AgentRuntime {
    pub fn open(options: AgentRuntimeOptions) -> Result<Self, RuntimeError> {
        Self::open_with_plugins(options, Vec::new())
    }

    /// Opens the runtime after transactionally staging every linked plugin
    /// contribution. No typed registry is published when graph resolution or a
    /// contribution fails.
    pub fn open_with_plugins(
        options: AgentRuntimeOptions,
        plugins: Vec<Arc<dyn RuntimePlugin>>,
    ) -> Result<Self, RuntimeError> {
        Self::open_with_plugins_and_catalog(options, plugins, None)
    }

    /// Opens the runtime with a skill catalog that was validated by the host
    /// before provider activation. Passing the catalog avoids a second scan of
    /// the same local skill roots while preserving the runtime's own authority
    /// checks during plugin assembly.
    pub fn open_with_plugins_and_catalog(
        options: AgentRuntimeOptions,
        plugins: Vec<Arc<dyn RuntimePlugin>>,
        preloaded_skills: Option<SkillCatalog>,
    ) -> Result<Self, RuntimeError> {
        Self::open_with_plugins_and_catalog_and_memory(options, plugins, preloaded_skills, true)
    }

    /// Variant used by an explicit `AgentSpec` composition. `memory_enabled`
    /// is a resolved host decision, not a task-controlled flag. The default
    /// API above is used by the independent review runner and in-process
    /// harness tests; ordinary CLI execution supplies an immutable receipt.
    pub fn open_with_plugins_and_catalog_and_memory(
        options: AgentRuntimeOptions,
        plugins: Vec<Arc<dyn RuntimePlugin>>,
        preloaded_skills: Option<SkillCatalog>,
        memory_enabled: bool,
    ) -> Result<Self, RuntimeError> {
        let AgentRuntimeOptions {
            state_root,
            skill_roots,
            profiles,
            model,
            maximum_parallel_tasks,
            tool_modules,
        } = options;
        model
            .binding()
            .validate()
            .map_err(|error| RuntimeError::Contract(error.to_string()))?;
        if maximum_parallel_tasks == 0 || maximum_parallel_tasks > 64 {
            return Err(RuntimeError::Contract(
                "maximum parallel tasks must be in 1..=64".to_owned(),
            ));
        }
        // Discover and validate the complete immutable composition before
        // creating runtime state. A graph, contribution, or tool-catalog
        // failure must not leave a partially initialized state directory.
        let skills = match preloaded_skills {
            Some(value) => value,
            None => SkillCatalog::discover(skill_roots)
                .map_err(|error| RuntimeError::Contract(error.to_string()))?,
        };
        let mut registry = assemble_registry(profiles, tool_modules, model, skills, plugins)
            .map_err(|error| RuntimeError::Contract(error.to_string()))?;
        let extension_tools =
            register_modules(registry.tool_modules).map_err(RuntimeError::Contract)?;
        registry.snapshot.replace_tool_origins(&extension_tools);
        for name in extension_tools.keys() {
            if is_effect_tool(name) || is_memory_tool(name) || is_interaction_tool(name) {
                return Err(RuntimeError::Contract(format!(
                    "extension tool {name} collides with a built-in tool"
                )));
            }
        }
        validate_extension_catalog(&registry.profiles, &registry.skills, &extension_tools)?;

        prepare_private_state_root(&state_root)?;
        let state_root = state_root
            .canonicalize()
            .map_err(|error| RuntimeError::Storage(error.to_string()))?;
        let memory = if memory_enabled {
            Some(
                MemoryStore::open(state_root.join("memory"))
                    .map_err(|error| RuntimeError::Storage(error.to_string()))?,
            )
        } else {
            None
        };
        let artifacts = ArtifactStore::open(state_root.join("artifacts"))
            .map_err(|error| RuntimeError::Storage(error.to_string()))?;
        let approvals = ApprovalStore::open(state_root.join("approvals"))
            .map_err(|error| RuntimeError::Storage(error.to_string()))?;
        Ok(Self {
            inner: Arc::new(RuntimeInner {
                state_root,
                profiles: registry.profiles,
                skills: registry.skills,
                memory,
                artifacts,
                approvals,
                model: registry.model,
                task_slots: Arc::new(Semaphore::new(maximum_parallel_tasks)),
                maximum_parallel_tasks,
                writer_leases: tokio::sync::Mutex::new(BTreeMap::new()),
                effect_planes: Mutex::new(BTreeMap::new()),
                extension_tools,
                completion_verifiers: registry.verifiers,
                plugin_snapshot: registry.snapshot,
            }),
        })
    }

    #[must_use]
    pub fn profiles(&self) -> Vec<agent_core::ProfileManifest> {
        self.inner.profiles.manifests()
    }

    #[must_use]
    pub fn skills(&self) -> Vec<agent_skills::SkillDescriptor> {
        self.inner.skills.descriptors()
    }

    #[must_use]
    pub fn plugin_snapshot(&self) -> &crate::PluginSnapshot {
        &self.inner.plugin_snapshot
    }

    pub fn recoveries(&self) -> Result<Vec<crate::journal::RecoveryEntry>, RuntimeError> {
        crate::journal::list_incomplete(&self.inner.state_root)
            .map_err(|error| RuntimeError::Storage(error.to_string()))
    }

    /// Reads a completed task result only when its durable request is an exact
    /// match. Review orchestration uses this projection to reconcile a child
    /// that completed before its parent recorded the lane evidence, without
    /// issuing another provider call.
    pub fn read_terminal_result(
        &self,
        task: &TaskEnvelope,
    ) -> Result<Option<AgentResult>, RuntimeError> {
        crate::journal::read_terminal_result(&self.inner.state_root, task)
            .map_err(|error| RuntimeError::Storage(error.to_string()))
    }

    pub fn resolve_recovery(
        &self,
        session_id: &str,
        task_id: &str,
        resolution: &str,
    ) -> Result<crate::journal::RecoveryEntry, RuntimeError> {
        let session = SessionId::parse(session_id.to_owned())
            .map_err(|error| RuntimeError::Contract(error.to_string()))?;
        let task = TaskId::parse(task_id.to_owned())
            .map_err(|error| RuntimeError::Contract(error.to_string()))?;
        crate::journal::resolve_incomplete(
            &self.inner.state_root,
            session.as_str(),
            task.as_str(),
            resolution,
        )
        .map_err(|error| RuntimeError::Storage(error.to_string()))
    }

    pub async fn run(&self, task: ResolvedTask) -> AgentResult {
        Box::pin(self.run_with_cancellation(task, TaskCancellation::new())).await
    }

    pub async fn run_with_cancellation(
        &self,
        resolved: ResolvedTask,
        cancellation: TaskCancellation,
    ) -> AgentResult {
        let composition = resolved.receipt.clone();
        let mut task = resolved.task;
        if let Err(failure) = normalize_task(&mut task) {
            return undurable_failure(&task, failure);
        }
        if cancellation.is_cancelled() {
            return undurable_failure(
                &task,
                cancelled_failure("task was cancelled before it started"),
            );
        }
        let started_at = tokio::time::Instant::now();
        let deadline = started_at + Duration::from_millis(task.budget.timeout_ms);
        let permit = tokio::select! {
            biased;
            () = cancellation.cancelled() => {
                return undurable_failure(
                    &task,
                    cancelled_failure("task was cancelled while waiting for a runtime slot"),
                );
            }
            result = self.inner.task_slots.clone().acquire_owned() => {
                match result {
                    Ok(value) => value,
                    Err(_) => {
                        return undurable_failure(
                            &task,
                            AgentFailure::new(
                                FailureCode::InternalInvariant,
                                "runtime task supervisor is shut down",
                            ),
                        );
                    }
                }
            }
            () = tokio::time::sleep_until(deadline) => {
                return undurable_failure(
                    &task,
                    AgentFailure::new(
                        FailureCode::BudgetExceeded,
                        "task deadline expired while waiting for a runtime slot",
                    ),
                );
            }
        };
        let writer = if task.effect_approval == EffectApprovalMode::TaskGrant
            || !task.approvals.is_empty()
        {
            let writer = self.acquire_workspace_writer(&task);
            tokio::pin!(writer);
            let acquired = tokio::select! {
                biased;
                () = cancellation.cancelled() => {
                    return undurable_failure(
                        &task,
                        cancelled_failure(
                            "task was cancelled while waiting for workspace mutation authority",
                        ),
                    );
                }
                result = &mut writer => result,
                () = tokio::time::sleep_until(deadline) => {
                    return undurable_failure(
                        &task,
                        AgentFailure::new(
                            FailureCode::BudgetExceeded,
                            "task deadline expired while waiting for workspace mutation authority",
                        ),
                    );
                }
            };
            match acquired {
                Ok(value) => value,
                Err(failure) => return undurable_failure(&task, failure),
            }
        } else {
            None
        };
        let result = Box::pin(self.execute(task, composition, &cancellation)).await;
        drop(writer);
        drop(permit);
        result
    }

    pub async fn run_many(&self, tasks: Vec<ResolvedTask>) -> Vec<AgentResult> {
        self.run_many_with_cancellation(tasks, TaskCancellation::new())
            .await
    }

    pub async fn run_many_with_cancellation(
        &self,
        tasks: Vec<ResolvedTask>,
        cancellation: TaskCancellation,
    ) -> Vec<AgentResult> {
        let maximum = self.inner.maximum_parallel_tasks;
        let runtime = self.clone();
        let mut results = stream::iter(tasks.into_iter().enumerate())
            .map(move |(index, task)| {
                let runtime = runtime.clone();
                let cancellation = cancellation.clone();
                async move {
                    (
                        index,
                        Box::pin(runtime.run_with_cancellation(task, cancellation)).await,
                    )
                }
            })
            .buffer_unordered(maximum)
            .collect::<Vec<_>>()
            .await;
        results.sort_by_key(|(index, _)| *index);
        results.into_iter().map(|(_, result)| result).collect()
    }

    async fn execute(
        &self,
        task: TaskEnvelope,
        composition: Option<CompositionReceipt>,
        cancellation: &TaskCancellation,
    ) -> AgentResult {
        let mut session = match DurableSession::open(
            self.inner.state_root.clone(),
            self.inner.artifacts.clone(),
            &task,
        )
        .await
        {
            Ok(value) => value,
            Err(failure)
                if failure
                    .details
                    .get("suspendedCheckpoint")
                    .and_then(Value::as_bool)
                    == Some(true) =>
            {
                return undurable_suspension(&task, failure);
            }
            Err(failure) => return undurable_failure(&task, failure),
        };
        if cancellation.is_cancelled() {
            return session
                .fail(
                    &task,
                    cancelled_failure("task was cancelled before preflight"),
                )
                .await;
        }
        let resume = session.take_resume();
        let restored_approvals = resume
            .as_ref()
            .map(|checkpoint| checkpoint.value.approved_subjects.clone());
        let preflight = self
            .preflight(&task, composition.as_ref(), restored_approvals)
            .await;
        let prepared = match preflight {
            Ok(value) => value,
            Err(failure) => return session.fail(&task, failure).await,
        };
        if cancellation.is_cancelled() {
            return session
                .fail(
                    &task,
                    cancelled_failure("task was cancelled during preflight"),
                )
                .await;
        }
        let messages = if let Some(checkpoint) = resume {
            if checkpoint.value.plugin_set_digest
                != prepared.plugin_binding.plugin_set_fingerprint()
                || checkpoint.value.plugin_execution_digest != prepared.plugin_binding.fingerprint()
                || checkpoint.value.preparation_digest != prepared.preparation_digest
                || checkpoint.value.composition_digest != prepared.composition_digest
            {
                return session.blocked(
                    &task,
                    AgentFailure::new(
                        FailureCode::InvalidProfile,
                        "suspended task preparation changed; restore the previous environment before resuming",
                    )
                    .retryable(true),
                );
            }
            if let Err(failure) = session
                .machine()
                .and_then(|machine| machine.constrain_budget(&prepared.plan.effective_budget))
            {
                return session.fail(&task, failure).await;
            }
            if let Err(failure) = session
                .record_control(
                    KernelEvent::Resumed,
                    json!({"checkpoint": checkpoint.artifact}),
                )
                .await
            {
                return session.fail(&task, failure).await;
            }
            checkpoint.value.messages
        } else {
            if let Err(failure) = session
                .machine()
                .and_then(|machine| machine.constrain_budget(&prepared.plan.effective_budget))
            {
                return session.fail(&task, failure).await;
            }
            let execution_metadata = self.execution_metadata(&task, &prepared);
            if let Err(failure) = session
                .record(KernelEvent::Validated, execution_metadata)
                .await
            {
                return session.fail(&task, failure).await;
            }
            if let Err(failure) = session
                .record(
                    KernelEvent::Started,
                    json!({"pluginGraph": self.inner.plugin_snapshot.as_json()}),
                )
                .await
            {
                return session.fail(&task, failure).await;
            }
            prepared.plan.messages.clone()
        };
        let deadline = tokio::time::Instant::now()
            + Duration::from_millis(prepared.plan.effective_budget.timeout_ms);
        self.run_loop(task, prepared, session, messages, deadline, cancellation)
            .await
    }

    async fn preflight(
        &self,
        task: &TaskEnvelope,
        composition: Option<&CompositionReceipt>,
        restored_approvals: Option<BTreeSet<String>>,
    ) -> Result<PreparedTask, AgentFailure> {
        let active_provider = self.inner.model.binding();
        active_provider
            .validate()
            .map_err(|error| AgentFailure::new(FailureCode::InvalidRequest, error.to_string()))?;
        if let Some(receipt) = composition
            && receipt.provider != active_provider
        {
            return Err(AgentFailure::new(
                FailureCode::InternalInvariant,
                "active provider binding differs from the composition receipt",
            ));
        }
        validate_provider_budget(task.budget.maximum_output_bytes, &active_provider)
            .map_err(|message| AgentFailure::new(FailureCode::InvalidRequest, message))?;
        let approved_subjects = match restored_approvals {
            Some(value) => value,
            None => self.redeem_approvals(task).await?,
        };
        let profile = self
            .inner
            .profiles
            .resolve(&task.profile)
            .map_err(|error| AgentFailure::new(FailureCode::InvalidProfile, error.to_string()))?;
        validate_profile_authority(profile.manifest(), task)?;
        let skills = load_skills(&self.inner.skills, profile.as_ref(), task, composition)?;
        let profile_values = task
            .context
            .iter()
            .filter(|(key, _)| !key.starts_with("_upagent_"))
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect::<BTreeMap<_, _>>();
        let mut context = ProfileContext {
            recalled_memory: Vec::new(),
            values: profile_values,
            available_tools: BTreeSet::new(),
        };
        if task.profile == ProfileId::twin() {
            let scope = context
                .values
                .get("memoryScope")
                .and_then(Value::as_str)
                .filter(|value| !value.trim().is_empty())
                .ok_or_else(|| {
                    AgentFailure::new(
                        FailureCode::InvalidRequest,
                        "twin profile requires context.memoryScope",
                    )
                })?;
            // Recall takes the scope lock and reads the durable snapshot, so it
            // never runs on a runtime worker.
            let memory = self.inner.memory.clone().ok_or_else(|| {
                AgentFailure::new(
                    FailureCode::MemoryFailed,
                    "memory store is not enabled by the selected composition",
                )
            })?;
            let scope = scope.to_owned();
            let objective = task.objective.clone();
            let recalled = tokio::task::spawn_blocking(move || {
                memory.recall(&scope, &objective, MAXIMUM_RECALLED_MEMORIES)
            })
            .await
            .map_err(|error| {
                AgentFailure::new(FailureCode::MemoryFailed, "memory recall did not complete")
                    .with_detail("cause", error.to_string())
            })?
            .map_err(|error| AgentFailure::new(FailureCode::MemoryFailed, error.to_string()))?;
            let mut projected = Vec::new();
            let mut projected_bytes = 0_usize;
            for item in recalled {
                let line = format!(
                    "id={} kind={:?} confidence={} provenance={} content={}",
                    item.entry.id,
                    item.entry.kind,
                    item.entry.confidence_basis_points,
                    bounded_text(&item.entry.provenance, 2 * 1024),
                    bounded_text(&item.entry.content, MAXIMUM_MEMORY_PROJECTION_BYTES),
                );
                let next = projected_bytes.checked_add(line.len()).ok_or_else(|| {
                    AgentFailure::new(
                        FailureCode::MemoryFailed,
                        "recalled memory projection size overflowed",
                    )
                })?;
                if next > MAXIMUM_RECALLED_MEMORY_BYTES {
                    break;
                }
                projected_bytes = next;
                projected.push(line);
            }
            context.recalled_memory = projected;
        }
        let loaded_skill_ids = skills
            .iter()
            .map(|skill| skill.descriptor.manifest.id.clone())
            .collect::<BTreeSet<_>>();
        let selected_skills = skills
            .iter()
            .map(|skill| agent_core::SelectedSkillBinding {
                id: skill.descriptor.manifest.id.as_str().to_owned(),
                instructions_sha256: skill.instructions_sha256.clone(),
            })
            .collect::<Vec<_>>();
        context.available_tools = self
            .inner
            .extension_tools
            .iter()
            .filter(|(_, tool)| tool.active_for(profile.manifest(), &loaded_skill_ids))
            .map(|(name, _)| name.clone())
            .collect();
        let plan = profile
            .prepare(task, &skills, &context)
            .map_err(|error| AgentFailure::new(FailureCode::InvalidProfile, error.to_string()))?;
        validate_provider_budget(plan.effective_budget.maximum_output_bytes, &active_provider)
            .map_err(|message| AgentFailure::new(FailureCode::InvalidRequest, message))?;
        let tool_definitions = self.resolve_tools(task, &plan, &loaded_skill_ids, composition)?;
        if let Some(receipt) = composition {
            // The composition receipt also seals provider/profile catalog
            // manifests. Those are constructor-owned authorities and are
            // intentionally absent from `RuntimePlugin`'s tool/verifier-only
            // graph. Compare a receipt binding only when the same ID belongs
            // to this runtime extension graph; provider/profile bindings are
            // already selected through the host's immutable catalog before
            // this runtime is opened.
            let active_plugins = self
                .inner
                .plugin_snapshot
                .activation_order()
                .iter()
                .map(|origin| {
                    (
                        origin.id().as_str().to_owned(),
                        origin.manifest_digest().to_owned(),
                    )
                })
                .collect::<Vec<_>>();
            for binding in receipt.plugin_tiers.iter().filter_map(|binding| {
                binding
                    .linked_manifest_sha256
                    .as_ref()
                    .map(|digest| (binding.id.as_str(), digest.as_str()))
            }) {
                if let Some((_, active_digest)) = active_plugins
                    .iter()
                    .find(|(active_id, _)| active_id == binding.0)
                    && active_digest != binding.1
                {
                    return Err(AgentFailure::new(
                        FailureCode::InternalInvariant,
                        "a runtime plugin manifest differs from the composition receipt",
                    ));
                }
            }
            let selected =
                self.actual_tool_bindings(&tool_definitions, &plan, &loaded_skill_ids)?;
            if selected != receipt.selected_tools {
                return Err(AgentFailure::new(
                    FailureCode::InternalInvariant,
                    "active tool definitions or side effects differ from the composition receipt",
                ));
            }
            let mut expected_skills = receipt.selected_skills.clone();
            let mut actual_skills = selected_skills.clone();
            expected_skills.sort_by(|left, right| left.id.cmp(&right.id));
            actual_skills.sort_by(|left, right| left.id.cmp(&right.id));
            if expected_skills != actual_skills {
                return Err(AgentFailure::new(
                    FailureCode::InvalidProfile,
                    "active skill content differs from the composition receipt",
                ));
            }
        }
        let mut binding_plan = plan.clone();
        binding_plan.allowed_tools = tool_definitions
            .iter()
            .map(|tool| tool.name.clone())
            .collect();
        let plugin_binding = self
            .inner
            .plugin_snapshot
            .bind_execution(&binding_plan)
            .map_err(|error| {
                AgentFailure::new(
                    FailureCode::InternalInvariant,
                    "prepared task could not bind its plugin execution composition",
                )
                .with_detail("cause", error.to_string())
            })?;
        let preparation_digest = preparation_digest(
            &plan,
            &tool_definitions,
            plugin_binding.plugin_set_fingerprint(),
            plugin_binding.fingerprint(),
            &approved_subjects,
        )?;
        let effect_plane = if tool_definitions
            .iter()
            .any(|tool| is_effect_tool(&tool.name))
        {
            let workspace = task.workspace.as_deref().ok_or_else(|| {
                AgentFailure::new(
                    FailureCode::InvalidRequest,
                    "selected profile tools require an explicit workspace",
                )
            })?;
            Some(self.effect_plane(Path::new(workspace))?)
        } else {
            None
        };
        Ok(PreparedTask {
            profile,
            plan,
            tools: tool_definitions,
            selected_skills,
            plugin_binding,
            effect_plane,
            approved_subjects,
            preparation_digest,
            composition_digest: composition.map(|receipt| receipt.digest.clone()),
            composition: composition.cloned(),
        })
    }

    /// Redeem every operator decision this run carries, exactly once.
    ///
    /// Consumption happens before any tool runs so a token can never authorise
    /// two operations, and an unusable grant fails the task explicitly instead
    /// of silently degrading to an approval request the caller already answered.
    async fn redeem_approvals(
        &self,
        task: &TaskEnvelope,
    ) -> Result<BTreeSet<String>, AgentFailure> {
        if task.approvals.is_empty() {
            return Ok(BTreeSet::new());
        }
        if task.effect_approval == EffectApprovalMode::Deny {
            return Err(AgentFailure::new(
                FailureCode::CapabilityDenied,
                "deny effect approval cannot be combined with redeemed approvals",
            ));
        }
        let approvals = self.inner.approvals.clone();
        let grants = task.approvals.clone();
        tokio::task::spawn_blocking(move || {
            let mut subjects = BTreeSet::new();
            for grant in grants {
                approvals
                    .consume(&grant.subject, &grant.token)
                    .map_err(|error| {
                        AgentFailure::new(
                            FailureCode::CapabilityDenied,
                            "a supplied approval could not be redeemed",
                        )
                        .with_detail("subject", grant.subject.clone())
                        .with_detail("cause", error.to_string())
                    })?;
                subjects.insert(grant.subject);
            }
            Ok(subjects)
        })
        .await
        .map_err(|error| {
            AgentFailure::new(
                FailureCode::InternalInvariant,
                "approval redemption did not complete",
            )
            .with_detail("cause", error.to_string())
        })?
    }

    fn resolve_tools(
        &self,
        task: &TaskEnvelope,
        plan: &ProfilePlan,
        loaded_skills: &BTreeSet<agent_core::SkillId>,
        composition: Option<&CompositionReceipt>,
    ) -> Result<Vec<ToolDefinition>, AgentFailure> {
        let mut available = BTreeMap::<String, ToolDefinition>::new();
        if task.workspace.is_some() {
            for tool in tool_definitions() {
                available.insert(tool.name.clone(), tool);
            }
        }
        for tool in memory_tools::definitions() {
            available.insert(tool.name.clone(), tool);
        }
        let interaction = crate::interaction_tools::definition();
        available.insert(interaction.name.clone(), interaction);
        for (name, tool) in &self.inner.extension_tools {
            if tool.active_for(&plan.manifest, loaded_skills) {
                available.insert(name.clone(), tool.definition.clone());
            }
        }
        let selected_names = match task.context.get("_upagent_composition_tools") {
            Some(Value::Array(values)) => values
                .iter()
                .map(|value| {
                    value.as_str().ok_or_else(|| {
                        AgentFailure::new(
                            FailureCode::InvalidRequest,
                            "composition tool selection must contain strings",
                        )
                    })
                })
                .collect::<Result<BTreeSet<_>, _>>()?,
            Some(_) => {
                return Err(AgentFailure::new(
                    FailureCode::InvalidRequest,
                    "composition tool selection must be an array",
                ));
            }
            None if composition.is_some() => {
                return Err(AgentFailure::new(
                    FailureCode::InvalidRequest,
                    "composition selected tools are missing",
                ));
            }
            None => plan
                .allowed_tools
                .iter()
                .filter(|name| available.contains_key(*name))
                .map(String::as_str)
                .collect(),
        };
        if !selected_names
            .iter()
            .all(|name| plan.allowed_tools.contains(*name))
        {
            return Err(AgentFailure::new(
                FailureCode::CapabilityDenied,
                "composition selected a tool outside profile authority",
            ));
        }
        let mut selected = Vec::new();
        for name in &selected_names {
            let definition = available.get(*name).ok_or_else(|| {
                AgentFailure::new(
                    FailureCode::InternalInvariant,
                    format!("profile selected unavailable tool {name}"),
                )
            })?;
            selected.push(definition.clone());
        }
        selected.sort_by(|left, right| left.name.cmp(&right.name));
        Ok(selected)
    }

    fn actual_tool_bindings(
        &self,
        definitions: &[ToolDefinition],
        plan: &ProfilePlan,
        loaded_skills: &BTreeSet<agent_core::SkillId>,
    ) -> Result<Vec<SelectedToolBinding>, AgentFailure> {
        let mut bindings = Vec::with_capacity(definitions.len());
        for definition in definitions {
            if let Some(registered) = self.inner.extension_tools.get(&definition.name) {
                if !registered.active_for(&plan.manifest, loaded_skills) {
                    return Err(AgentFailure::new(
                        FailureCode::InternalInvariant,
                        format!("selected extension tool {} is not active", definition.name),
                    ));
                }
                // Re-read the module's immutable contract at preparation time
                // so a stateful/mutable module cannot drift after registration.
                let current = registered
                    .module
                    .tools()
                    .into_iter()
                    .find(|tool| tool.definition.name == definition.name)
                    .ok_or_else(|| {
                        AgentFailure::new(
                            FailureCode::InternalInvariant,
                            format!(
                                "selected extension tool {} disappeared from its module",
                                definition.name
                            ),
                        )
                    })?;
                if current.definition != registered.definition
                    || current.side_effect != registered.side_effect
                    || current.allowed_profiles != registered.allowed_profiles
                    || current.required_capabilities != registered.required_capabilities
                    || current.activation_skills != registered.activation_skills
                {
                    return Err(AgentFailure::new(
                        FailureCode::InternalInvariant,
                        format!(
                            "active extension tool {} drifted after registration",
                            definition.name
                        ),
                    ));
                }
                let side_effect = match registered.side_effect {
                    ToolSideEffect::ReadOnly => SelectedToolSideEffect::ReadOnly,
                    ToolSideEffect::External => SelectedToolSideEffect::External,
                };
                let origin = format!(
                    "{}@{}",
                    registered.origin.id().as_str(),
                    registered.origin.version()
                );
                bindings.push(
                    SelectedToolBinding::create(
                        registered.definition.clone(),
                        side_effect,
                        &origin,
                    )
                    .map_err(|error| {
                        AgentFailure::new(FailureCode::InternalInvariant, error.to_string())
                    })?,
                );
            } else {
                let name = vec![definition.name.clone()];
                let canonical = crate::canonical_tool_bindings(&name)
                    .map_err(|error| AgentFailure::new(FailureCode::InternalInvariant, error))?;
                let binding = canonical.into_iter().next().ok_or_else(|| {
                    AgentFailure::new(
                        FailureCode::InternalInvariant,
                        format!("selected built-in tool {} has no binding", definition.name),
                    )
                })?;
                if binding.definition != *definition {
                    return Err(AgentFailure::new(
                        FailureCode::InternalInvariant,
                        format!("selected built-in tool {} drifted", definition.name),
                    ));
                }
                bindings.push(binding);
            }
        }
        bindings.sort_by(|left, right| left.name.cmp(&right.name));
        Ok(bindings)
    }

    fn execution_metadata(&self, task: &TaskEnvelope, prepared: &PreparedTask) -> Value {
        json!({
            "compositionDigest": prepared.composition_digest,
            "isolation": prepared.composition.as_ref().map(|receipt| match &receipt.spec.isolation {
                agent_core::IsolationSpec::Native => json!({"kind":"native"}),
                agent_core::IsolationSpec::HostAdapter { adapter, .. } => json!({
                    "kind":"host_adapter",
                    "adapter": adapter,
                    "evidence": "operator_declared_host_recipe",
                }),
            }),
            "profile": task.profile.as_str(),
            "track": task.track.as_ref().map(agent_core::TrackId::as_str),
            "selectedSkills": &prepared.selected_skills,
            "pluginSetDigest": self.inner.plugin_snapshot.fingerprint(),
            "pluginExecutionDigest": prepared.plugin_binding.fingerprint(),
            "preparationDigest": prepared.preparation_digest,
            "pluginExecution": prepared.plugin_binding.as_json(),
            "executionClass": prepared.plan.execution_class.as_str(),
            "reasoning": prepared.plan.reasoning,
            "maximumWorkers": prepared.plan.maximum_workers,
            "effectiveBudget": &prepared.plan.effective_budget,
            "topology": prepared.plan.topology.as_str(),
            "allowedTools": prepared
                .tools
                .iter()
                .map(|tool| tool.name.as_str())
                .collect::<Vec<_>>(),
            "requiredSuccessfulTools": &prepared.plan.required_successful_tools,
            "requiredSuccessfulToolsAfterMutation": &prepared
                .plan
                .required_successful_tools_after_mutation,
            "requiredChecks": &prepared.plan.required_checks,
            "requiredCheckCommands": &prepared.plan.required_check_commands,
            "outputContract": &prepared.plan.output_contract,
            "completionVerifiers": self
                .inner
                .completion_verifiers
                .keys()
                .cloned()
                .collect::<Vec<_>>(),
        })
    }

    fn verify_completion_contract(
        &self,
        plan: &ProfilePlan,
        machine: &SessionMachine,
    ) -> Result<(), AgentFailure> {
        if let Some(blocker) = machine.image_director.completion_blocker() {
            return Err(AgentFailure::new(FailureCode::VerificationFailed, blocker)
                .with_detail("authority", "upagent.image-director/1"));
        }
        if plan.topology == ExecutionTopology::SingleWithIndependentVerifier
            && self.inner.completion_verifiers.is_empty()
        {
            return Err(AgentFailure::new(
                FailureCode::VerificationFailed,
                "execution topology requires an independent completion verifier",
            ));
        }
        let evidence = crate::CompletionEvidence {
            execution_class: plan.execution_class,
            topology: plan.topology,
            required_successful_tools: plan.required_successful_tools.clone(),
            successful_tools: machine.successful_tools.clone(),
            required_checks: plan.required_checks.iter().cloned().collect(),
            successful_checks: machine.successful_checks.clone(),
            required_successful_tools_after_mutation: plan
                .required_successful_tools_after_mutation
                .clone(),
            successful_tools_after_mutation: machine.successful_tools_after_mutation.clone(),
            successful_checks_after_mutation: machine.successful_checks_after_mutation.clone(),
            successful_source_mutation: machine.successful_source_mutation,
        };
        for verifier in self.inner.completion_verifiers.values() {
            verifier.verify(&evidence)?;
        }
        Ok(())
    }

    fn tool_origin(&self, name: &str) -> Value {
        if let Some(origin) = self.inner.plugin_snapshot.tool_origin(name) {
            return json!({"kind":"plugin","plugin":origin.as_json()});
        }
        let host = built_in_tool_authority(name)
            .map_or("upagent.kernel.unknown", |authority| authority.id());
        json!({
            "kind":"privileged_host",
            "id":host,
            "version":env!("CARGO_PKG_VERSION"),
        })
    }

    fn can_parallelize_read(&self, prepared: &PreparedTask, call: &ToolCall) -> bool {
        if !prepared.plan.allowed_tools.contains(&call.name) {
            return false;
        }
        if call.name == "image_extract"
            && call
                .arguments
                .get("selector")
                .and_then(Value::as_object)
                .and_then(|selector| selector.get("type"))
                .and_then(Value::as_str)
                == Some("semantic")
        {
            // ImageDirector owns one durable semantic operation at a time;
            // admitting a batch in parallel would make the second request
            // race the first pending identity.
            return false;
        }
        if matches!(
            call.name.as_str(),
            "repo_map" | "search_code" | "read_file" | "git_status" | "inspect_diff"
        ) {
            return true;
        }
        self.inner
            .extension_tools
            .get(&call.name)
            .is_some_and(|tool| tool.side_effect == ToolSideEffect::ReadOnly)
    }

    async fn admit_tool_call(
        &self,
        session: &mut DurableSession,
        prepared: &PreparedTask,
        call: &ToolCall,
    ) -> Result<(), AgentFailure> {
        session.machine()?.register_tool_call(&call.id)?;
        session.machine()?.admit_image_tool(call)?;
        session
            .record(
                KernelEvent::ToolRequested,
                json!({
                    "callId":call.id.as_str(),
                    "name":call.name.as_str(),
                    "origin":self.tool_origin(&call.name),
                    "pluginSetDigest":self.inner.plugin_snapshot.fingerprint(),
                    "pluginExecutionDigest":prepared.plugin_binding.fingerprint(),
                }),
            )
            .await?;
        let machine = session.machine()?;
        machine.tool_calls = machine.tool_calls.saturating_add(1);
        Ok(())
    }

    fn effect_plane(&self, workspace: &Path) -> Result<Arc<EffectPlane>, AgentFailure> {
        let workspace = workspace.canonicalize().map_err(|error| {
            AgentFailure::new(
                FailureCode::InvalidRequest,
                "workspace cannot be canonicalized",
            )
            .with_detail("cause", error.to_string())
        })?;
        if self.inner.state_root.starts_with(&workspace)
            || workspace.starts_with(&self.inner.state_root)
        {
            return Err(AgentFailure::new(
                FailureCode::InvalidRequest,
                "runtime state root and effect workspace must not overlap",
            ));
        }
        let workspace_text = workspace.to_str().ok_or_else(|| {
            AgentFailure::new(
                FailureCode::InvalidRequest,
                "workspace canonical path must be valid UTF-8",
            )
        })?;
        let mut cache = self.inner.effect_planes.lock().map_err(|_| {
            AgentFailure::new(
                FailureCode::InternalInvariant,
                "effect plane cache lock was poisoned",
            )
        })?;
        cache.retain(|_, value| value.strong_count() > 0);
        if let Some(value) = cache.get(&workspace).and_then(Weak::upgrade) {
            return Ok(value);
        }
        let digest = format!("{:x}", Sha256::digest(workspace_text.as_bytes()));
        let state_dir = self.inner.state_root.join("effects").join(digest);
        let plane =
            Arc::new(EffectPlane::open(&workspace, state_dir).map_err(|error| {
                AgentFailure::new(FailureCode::EffectFailed, error.to_string())
            })?);
        cache.insert(workspace, Arc::downgrade(&plane));
        Ok(plane)
    }

    async fn acquire_workspace_writer(
        &self,
        task: &TaskEnvelope,
    ) -> Result<Option<OwnedSemaphorePermit>, AgentFailure> {
        let Some(workspace) = task.workspace.as_deref() else {
            return Ok(None);
        };
        let key = PathBuf::from(workspace);
        let semaphore = {
            let mut leases = self.inner.writer_leases.lock().await;
            leases.retain(|_, value| value.strong_count() > 0);
            if let Some(existing) = leases.get(&key).and_then(Weak::upgrade) {
                existing
            } else {
                let value = Arc::new(Semaphore::new(1));
                leases.insert(key, Arc::downgrade(&value));
                value
            }
        };
        semaphore.acquire_owned().await.map(Some).map_err(|_| {
            AgentFailure::new(
                FailureCode::InternalInvariant,
                "workspace writer authority is closed",
            )
        })
    }
}

struct PreparedTask {
    profile: Arc<dyn ProfileDefinition>,
    plan: ProfilePlan,
    tools: Vec<ToolDefinition>,
    selected_skills: Vec<agent_core::SelectedSkillBinding>,
    plugin_binding: crate::PluginExecutionBinding,
    effect_plane: Option<Arc<EffectPlane>>,
    /// Decisions already redeemed for this run.
    approved_subjects: BTreeSet<String>,
    preparation_digest: String,
    composition_digest: Option<String>,
    composition: Option<CompositionReceipt>,
}

fn build_output_contract_payload(
    task: &TaskEnvelope,
    plan: &ProfilePlan,
    machine: &SessionMachine,
    output_artifact: &ArtifactRef,
    plugin_set_digest: &str,
    plugin_execution_digest: &str,
) -> Result<Option<Value>, AgentFailure> {
    let Some(contract) = plan.output_contract.as_deref() else {
        return Ok(None);
    };
    let output = json!({
        "textArtifact": output_artifact,
        "executionClass": plan.execution_class.as_str(),
        "pluginSetDigest": plugin_set_digest,
        "pluginExecutionDigest": plugin_execution_digest,
        "successfulTools": &machine.successful_tools,
        "successfulChecks": &machine.successful_checks,
        "requiredCheckCommands": &plan.required_check_commands,
        "receipts": &machine.receipts,
    });
    match contract {
        "structured_result_with_evidence" => Ok(Some(json!({
            "schema": contract,
            "taskId": task.task_id.as_str(),
            "sessionId": task.session_id.as_str(),
            "status": "succeeded",
            "output": output,
        }))),
        "axd.agent-result/2" => {
            let binding = plan.axd_result_binding.as_ref().ok_or_else(|| {
                AgentFailure::new(
                    FailureCode::InvalidProfile,
                    "axd.agent-result/2 has no prepared result binding",
                )
            })?;
            let evidence = plan
                .required_checks
                .iter()
                .map(|requirement| {
                    json!({
                        "requirement_id": requirement,
                        "kind": binding
                            .check_kinds
                            .get(requirement)
                            .map_or("other", String::as_str),
                        "status": "pass",
                        "source_revision": binding.source_revision.as_str(),
                        "contract_revision": binding.contract_revision,
                        "summary": "UpAgent run_check completed successfully for the required check",
                        "artifact_digest": Value::Null,
                    })
                })
                .collect::<Vec<_>>();
            Ok(Some(json!({
                "schema": contract,
                "contract_digest": binding.contract_digest.as_str(),
                "work_item_id": binding.work_item_id.as_str(),
                "source_revision": binding.source_revision.as_str(),
                "contract_revision": binding.contract_revision,
                "status": "succeeded",
                "evidence": evidence,
                "output": output,
            })))
        }
        other => Err(AgentFailure::new(
            FailureCode::InvalidProfile,
            format!("output contract {other} is not implemented"),
        )),
    }
}

fn load_skills(
    catalog: &SkillCatalog,
    profile: &dyn ProfileDefinition,
    task: &TaskEnvelope,
    composition: Option<&CompositionReceipt>,
) -> Result<Vec<SkillDocument>, AgentFailure> {
    let manifest = profile.manifest();
    if composition.is_none() && manifest.id == ProfileId::developer() && !task.skills.is_empty() {
        return Err(AgentFailure::new(
            FailureCode::InvalidProfile,
            "developer track skills are selected by track and cannot be injected explicitly",
        ));
    }
    let selected = if composition.is_some() {
        task.skills.iter().cloned().collect::<BTreeSet<_>>()
    } else {
        profile
            .default_skills(task.track.as_ref())
            .map_err(|error| AgentFailure::new(FailureCode::InvalidProfile, error.to_string()))?
            .into_iter()
            .chain(task.skills.iter().cloned())
            .collect::<BTreeSet<_>>()
    };
    if composition.is_some() && selected.is_empty() {
        return Err(AgentFailure::new(
            FailureCode::InvalidProfile,
            "composition selected no skills",
        ));
    }
    let mut documents = Vec::with_capacity(selected.len());
    for id in selected {
        if !manifest.allows_skill(&id) {
            return Err(AgentFailure::new(
                FailureCode::CapabilityDenied,
                format!("skill {id} is outside profile {}", manifest.id),
            ));
        }
        documents.push(
            catalog.load(&id, &manifest.id).map_err(|error| {
                AgentFailure::new(FailureCode::InvalidProfile, error.to_string())
            })?,
        );
    }
    Ok(documents)
}

fn preparation_digest(
    plan: &ProfilePlan,
    tools: &[ToolDefinition],
    plugin_set_digest: &str,
    plugin_execution_digest: &str,
    approved_subjects: &BTreeSet<String>,
) -> Result<String, AgentFailure> {
    let projection = json!({
        "messages": &plan.messages,
        "tools": tools,
        "track": &plan.track,
        "reasoning": plan.reasoning,
        "executionClass": plan.execution_class,
        "topology": plan.topology,
        "maximumWorkers": plan.maximum_workers,
        "allowedTools": &plan.allowed_tools,
        "requiredSuccessfulTools": &plan.required_successful_tools,
        "requiredSuccessfulToolsAfterMutation": &plan.required_successful_tools_after_mutation,
        "requiredChecks": &plan.required_checks,
        "requiredCheckCommands": &plan.required_check_commands,
        "outputContract": &plan.output_contract,
        "effectiveBudget": &plan.effective_budget,
        "pluginSetDigest": plugin_set_digest,
        "pluginExecutionDigest": plugin_execution_digest,
        "approvedSubjects": approved_subjects,
    });
    let bytes = serde_json::to_vec(&projection).map_err(|error| {
        AgentFailure::new(
            FailureCode::InternalInvariant,
            "prepared task could not be hashed for resume",
        )
        .with_detail("cause", error.to_string())
    })?;
    Ok(format!("{:x}", Sha256::digest(bytes)))
}

fn validate_extension_catalog(
    profiles: &ProfileRegistry,
    skills: &SkillCatalog,
    tools: &BTreeMap<String, RegisteredTool>,
) -> Result<(), RuntimeError> {
    let skill_descriptors = skills
        .descriptors()
        .into_iter()
        .map(|descriptor| (descriptor.manifest.id.clone(), descriptor))
        .collect::<BTreeMap<_, _>>();

    for (name, tool) in tools {
        for skill in &tool.activation_skills {
            if !skill_descriptors.contains_key(skill) {
                return Err(RuntimeError::Contract(format!(
                    "extension tool {name} references unknown activation skill {skill}"
                )));
            }
        }

        for profile_id in &tool.allowed_profiles {
            let profile = profiles.resolve(profile_id).map_err(|error| {
                RuntimeError::Contract(format!(
                    "extension tool {name} references an unavailable profile: {error}"
                ))
            })?;
            let manifest = profile.manifest();
            if !tool.required_capabilities.is_subset(&manifest.capabilities) {
                return Err(RuntimeError::Contract(format!(
                    "extension tool {name} requires capabilities unavailable to profile {profile_id}"
                )));
            }
            if !tool.activation_skills.is_empty()
                && !tool.activation_skills.iter().any(|skill| {
                    manifest.allows_skill(skill)
                        && skill_descriptors.get(skill).is_some_and(|descriptor| {
                            descriptor.manifest.profiles.contains(profile_id)
                        })
                })
            {
                return Err(RuntimeError::Contract(format!(
                    "extension tool {name} has no loadable activation skill for profile {profile_id}"
                )));
            }
        }
    }
    Ok(())
}

fn validate_profile_authority(
    manifest: &agent_core::ProfileManifest,
    task: &TaskEnvelope,
) -> Result<(), AgentFailure> {
    match manifest.approval_policy {
        ApprovalPolicy::DenyEffects if task.effect_approval != EffectApprovalMode::Deny => {
            return Err(AgentFailure::new(
                FailureCode::CapabilityDenied,
                "profile denies effect authorization",
            ));
        }
        ApprovalPolicy::RequireExplicit
            if task.effect_approval == EffectApprovalMode::TaskGrant =>
        {
            return Err(AgentFailure::new(
                FailureCode::CapabilityDenied,
                "profile permits planning but not task-wide effect grants",
            ));
        }
        ApprovalPolicy::DenyEffects
        | ApprovalPolicy::RequireExplicit
        | ApprovalPolicy::AllowTaskGrant => {}
    }
    if task.allow_memory_write && manifest.memory_policy != MemoryPolicy::LongTermScoped {
        return Err(AgentFailure::new(
            FailureCode::CapabilityDenied,
            "profile does not own durable long-term memory",
        ));
    }
    Ok(())
}

fn normalize_task(task: &mut TaskEnvelope) -> Result<(), AgentFailure> {
    task.validate().map_err(invalid_request)?;
    if task.effect_approval == EffectApprovalMode::Deny && !task.approvals.is_empty() {
        return Err(AgentFailure::new(
            FailureCode::CapabilityDenied,
            "deny effect approval cannot be combined with approvals",
        ));
    }
    task.objective = task.objective.trim().to_owned();
    let mut unique = BTreeSet::new();
    for skill in &task.skills {
        if !unique.insert(skill.clone()) {
            return Err(AgentFailure::new(
                FailureCode::InvalidRequest,
                format!("task contains duplicate skill {skill}"),
            ));
        }
    }
    if let Some(workspace) = task.workspace.as_deref() {
        let path = Path::new(workspace);
        if !path.is_absolute() {
            return Err(AgentFailure::new(
                FailureCode::InvalidRequest,
                "workspace path must be absolute",
            ));
        }
        let canonical = path.canonicalize().map_err(|error| {
            AgentFailure::new(FailureCode::InvalidRequest, "workspace does not exist")
                .with_detail("cause", error.to_string())
        })?;
        if !canonical.is_dir() {
            return Err(AgentFailure::new(
                FailureCode::InvalidRequest,
                "workspace must be a directory",
            ));
        }
        let canonical = canonical.to_str().ok_or_else(|| {
            AgentFailure::new(
                FailureCode::InvalidRequest,
                "workspace canonical path must be valid UTF-8",
            )
        })?;
        task.workspace = Some(canonical.to_owned());
    }
    Ok(())
}

fn failed_tool(call: &ToolCall, failure: AgentFailure) -> ToolResult {
    ToolResult {
        call_id: call.id.clone(),
        name: call.name.clone(),
        value: Value::Null,
        is_error: true,
        failure: Some(failure),
    }
}

fn denied_tool(call: &ToolCall, message: &str) -> ToolResult {
    ToolResult {
        call_id: call.id.clone(),
        name: call.name.clone(),
        value: Value::Null,
        is_error: true,
        failure: Some(AgentFailure::new(FailureCode::CapabilityDenied, message)),
    }
}

fn bounded_text(value: &str, maximum_bytes: usize) -> String {
    if value.len() <= maximum_bytes {
        return value.to_owned();
    }
    let mut end = maximum_bytes.saturating_sub(3).min(value.len());
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}...", &value[..end])
}

fn cancelled_failure(message: impl Into<String>) -> AgentFailure {
    AgentFailure::new(FailureCode::Cancelled, message)
}

fn budget_exceeded(message: impl Into<String>) -> AgentFailure {
    AgentFailure::new(FailureCode::BudgetExceeded, message)
}

fn succeeded_result(task: &TaskEnvelope, machine: &SessionMachine, text: String) -> AgentResult {
    AgentResult {
        schema_version: agent_core::PROTOCOL_VERSION,
        task_id: task.task_id.clone(),
        session_id: task.session_id.clone(),
        profile: task.profile.clone(),
        track: task.track.clone(),
        state: machine.state,
        status: AgentStatus::Succeeded,
        output: Some(text),
        artifacts: machine.artifacts(),
        receipts: machine.receipts.clone(),
        composition_digest: task
            .context
            .get("_upagent_composition_digest")
            .and_then(Value::as_str)
            .map(str::to_owned),
        review_subject_bundle: None,
        approval: None,
        failure: None,
        metrics: machine.metrics(),
    }
}

fn invalid_request(error: impl std::fmt::Display) -> AgentFailure {
    AgentFailure::new(FailureCode::InvalidRequest, error.to_string())
}

/// Keep the provider response bound adapter-neutral at runtime. The concrete
/// provider advertises this immutable limit through `ModelPort::binding`; a
/// larger task budget is rejected, never silently narrowed.
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

fn prepare_private_state_root(path: &Path) -> Result<(), RuntimeError> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                return Err(RuntimeError::Contract(
                    "runtime state root must be a real directory".to_owned(),
                ));
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            std::fs::create_dir_all(path)
                .map_err(|error| RuntimeError::Storage(error.to_string()))?;
        }
        Err(error) => return Err(RuntimeError::Storage(error.to_string())),
    }
    let metadata = std::fs::symlink_metadata(path)
        .map_err(|error| RuntimeError::Storage(error.to_string()))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(RuntimeError::Contract(
            "runtime state root must be a real directory".to_owned(),
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
            .map_err(|error| RuntimeError::Storage(error.to_string()))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_core::{ModelTurn, ModelTurnResult, ReasoningEffort, TaskBudget, ToolDefinition};
    use agent_profile_api::{ExecutionClass, ExecutionTopology, ProfilePlan};
    use agent_provider_api::{MAXIMUM_MODEL_RESPONSE_BYTES, ProviderError};
    use async_trait::async_trait;
    use serde_json::json;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use crate::{ModuleExecution, ModuleTool, ToolContext, ToolModuleError};

    #[derive(Debug)]
    struct NoopModel;

    #[async_trait]
    impl ModelPort for NoopModel {
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
            Err(ProviderError::Contract(
                "noop model must not be called".to_owned(),
            ))
        }
    }

    #[derive(Debug)]
    struct DriftingToolModule {
        calls: AtomicUsize,
    }

    #[async_trait]
    impl ToolModule for DriftingToolModule {
        fn tools(&self) -> Vec<ModuleTool> {
            let drifted = self.calls.fetch_add(1, Ordering::SeqCst) > 0;
            vec![ModuleTool {
                definition: ToolDefinition {
                    name: "drift_tool".to_owned(),
                    description: if drifted {
                        "changed after registration".to_owned()
                    } else {
                        "stable registered definition".to_owned()
                    },
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
                "drift test tool must not execute".to_owned(),
            ))
        }
    }

    #[test]
    fn preparation_rejects_extension_definition_drift_after_registration()
    -> Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let runtime = AgentRuntime::open(AgentRuntimeOptions {
            state_root: temp.path().join("state"),
            skill_roots: Vec::new(),
            profiles: agent_profiles::built_in_registry()?,
            model: Arc::new(NoopModel),
            maximum_parallel_tasks: 1,
            tool_modules: vec![Arc::new(DriftingToolModule {
                calls: AtomicUsize::new(0),
            })],
        })?;
        let creator = runtime.inner.profiles.resolve(&ProfileId::creator())?;
        let plan = ProfilePlan {
            manifest: creator.manifest().clone(),
            messages: Vec::new(),
            track: None,
            allowed_tools: BTreeSet::from(["drift_tool".to_owned()]),
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
            effective_budget: TaskBudget::default(),
            policy_budget_cap: None,
        };
        let definition = runtime
            .inner
            .extension_tools
            .get("drift_tool")
            .ok_or("registered drift tool missing")?
            .definition
            .clone();
        let error = runtime
            .actual_tool_bindings(&[definition], &plan, &BTreeSet::new())
            .expect_err("drifted module must not satisfy a frozen tool binding");
        assert!(error.to_string().contains("drifted after registration"));
        Ok(())
    }
}
