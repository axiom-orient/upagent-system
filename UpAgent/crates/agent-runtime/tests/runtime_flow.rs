#![cfg(feature = "test-support")]

use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    ops::Deref,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use agent_core::{
    AgentStatus, ApprovalGrant, ArtifactId, ArtifactRef, CapabilityId, ConversationTurn,
    EffectApprovalMode, FailureCode, IMAGE_TASK_CONTRACT_SCHEMA, IMAGE_VERIFICATION_SPEC_SCHEMA,
    ImageCompareArtifactKind, ImageCompareAssertion, ImageCompareSeverity, ImageSource,
    ImageTaskContract, ImageVerificationContract, ImageVerificationSpec, KernelEvent, MessageRole,
    ModelTurn, ModelTurnResult, ProfileId, SelectedToolSideEffect, SessionId, SkillId, TaskBudget,
    TaskEnvelope, TaskId, TaskState, ToolCall, ToolCallId, ToolDefinition, ToolResult, TrackId,
};
use agent_memory::MemoryStore;
use agent_plugin_kernel::{ContributionKind, PluginId, PluginManifest, PluginTrust};
use agent_provider_api::{
    MAXIMUM_MODEL_RESPONSE_BYTES, ModelPort, ProviderError, ResolvedImageObservation,
};
use agent_runtime::{
    AgentRuntime, AgentRuntimeOptions, ApprovalStatus, ApprovalStore, ArtifactStore,
    EffectRecoveryResolution, ModuleArtifact, ModuleExecution, ModuleTool, PluginRegistrar,
    ResolvedTask, RuntimePlugin, RuntimePluginError, TaskCancellation, ToolContext, ToolModule,
    ToolModuleError, ToolSideEffect, canonical_tool_bindings, inspect_task, list_effect_recoveries,
    list_recoveries, resolve_effect_recovery,
};
use agent_skills::SkillCatalog;
use async_trait::async_trait;
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;

#[derive(Debug)]
struct SequenceModel {
    results: Mutex<VecDeque<ModelTurnResult>>,
    turns: Mutex<Vec<ModelTurn>>,
    observation_counts: Mutex<Vec<usize>>,
}

impl SequenceModel {
    fn new(results: Vec<ModelTurnResult>) -> Self {
        Self {
            results: Mutex::new(results.into()),
            turns: Mutex::new(Vec::new()),
            observation_counts: Mutex::new(Vec::new()),
        }
    }

    fn recorded_turns(&self) -> Result<Vec<ModelTurn>, std::io::Error> {
        self.turns
            .lock()
            .map(|turns| turns.clone())
            .map_err(|_| std::io::Error::other("test provider turn log was poisoned"))
    }

    fn recorded_observation_counts(&self) -> Result<Vec<usize>, std::io::Error> {
        self.observation_counts
            .lock()
            .map(|counts| counts.clone())
            .map_err(|_| std::io::Error::other("test provider observation log was poisoned"))
    }

    fn next(&self, turn: &ModelTurn) -> Result<ModelTurnResult, ProviderError> {
        self.turns
            .lock()
            .map_err(|_| ProviderError::Contract("test provider turn log was poisoned".to_owned()))?
            .push(turn.clone());
        let mut results = self.results.lock().map_err(|_| {
            ProviderError::Contract("test provider result queue was poisoned".to_owned())
        })?;
        let mut result = results.pop_front().ok_or_else(|| {
            ProviderError::Contract("test provider has no remaining result".to_owned())
        })?;
        // Only an explicitly queued test review consumes this marker. Other
        // tests do not gain implicit approval by observing a candidate.
        if result.text == "TEST_IMAGE_REVIEW_CURRENT" {
            let mut review = turn.metadata.get("imageReview").cloned().ok_or_else(|| {
                ProviderError::Contract("test review has no current binding".to_owned())
            })?;
            let criteria = review
                .get_mut("criteria")
                .and_then(Value::as_array_mut)
                .ok_or_else(|| ProviderError::Contract("test review has no criteria".to_owned()))?;
            for criterion in criteria {
                let id = criterion["criterionId"].clone();
                *criterion = json!({"criterionId":id,"decision":"PASS","evidence":"Explicit harness evidence; not a production visual review"});
            }
            result.text = format!("IMAGE_REVIEW:{review}");
        }
        Ok(result)
    }
}

#[async_trait]
impl ModelPort for SequenceModel {
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
            maximum_output_bytes: agent_provider_api::MAXIMUM_MODEL_RESPONSE_BYTES,
        }
    }

    async fn complete(&self, turn: &ModelTurn) -> Result<ModelTurnResult, ProviderError> {
        self.next(turn)
    }

    async fn complete_with_observations(
        &self,
        turn: &ModelTurn,
        observations: &[ResolvedImageObservation],
        _cancellation: &CancellationToken,
    ) -> Result<ModelTurnResult, ProviderError> {
        self.observation_counts
            .lock()
            .map_err(|_| {
                ProviderError::Contract("test provider observation log was poisoned".to_owned())
            })?
            .push(observations.len());
        self.next(turn)
    }
}

/// Records the peak number of model calls that were ever in flight together.
///
/// The runtime admits a task before it reaches the model, so overlap here is a
/// faithful proxy for two tasks executing concurrently.
#[derive(Debug)]
struct OverlapProbe {
    active: AtomicUsize,
    peak: AtomicUsize,
    hold: Duration,
}

#[derive(Debug)]
struct MutatingOverlapProbe {
    active: AtomicUsize,
    peak: AtomicUsize,
    hold: Duration,
}

impl MutatingOverlapProbe {
    fn new(hold: Duration) -> Self {
        Self {
            active: AtomicUsize::new(0),
            peak: AtomicUsize::new(0),
            hold,
        }
    }

    fn peak(&self) -> usize {
        self.peak.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl ModelPort for MutatingOverlapProbe {
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
            maximum_output_bytes: agent_provider_api::MAXIMUM_MODEL_RESPONSE_BYTES,
        }
    }

    async fn complete(&self, model_turn: &ModelTurn) -> Result<ModelTurnResult, ProviderError> {
        let in_flight = self.active.fetch_add(1, Ordering::SeqCst) + 1;
        self.peak.fetch_max(in_flight, Ordering::SeqCst);
        tokio::time::sleep(self.hold).await;
        self.active.fetch_sub(1, Ordering::SeqCst);
        let has_tool_result = model_turn
            .messages
            .iter()
            .any(|message| message.role == MessageRole::Tool);
        if has_tool_result {
            return Ok(turn("Recorded the approved write.", Vec::new()));
        }
        let path = if model_turn
            .messages
            .iter()
            .any(|message| message.content.contains("first exact grant"))
        {
            "first.txt"
        } else {
            "second.txt"
        };
        Ok(turn(
            "",
            vec![ToolCall {
                id: ToolCallId::parse(format!("call-{}", uuid::Uuid::now_v7()))
                    .map_err(|error| ProviderError::Contract(error.to_string()))?,
                name: "write_file".to_owned(),
                arguments: json!({
                    "path": path,
                    "content": "approved",
                    "createOnly": true,
                    "expectedSha256": Value::Null
                }),
            }],
        ))
    }
}

impl OverlapProbe {
    fn new(hold: Duration) -> Self {
        Self {
            active: AtomicUsize::new(0),
            peak: AtomicUsize::new(0),
            hold,
        }
    }

    fn peak(&self) -> usize {
        self.peak.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl ModelPort for OverlapProbe {
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
            maximum_output_bytes: agent_provider_api::MAXIMUM_MODEL_RESPONSE_BYTES,
        }
    }

    async fn complete(&self, _turn: &ModelTurn) -> Result<ModelTurnResult, ProviderError> {
        let in_flight = self.active.fetch_add(1, Ordering::SeqCst) + 1;
        self.peak.fetch_max(in_flight, Ordering::SeqCst);
        tokio::time::sleep(self.hold).await;
        self.active.fetch_sub(1, Ordering::SeqCst);
        Ok(turn("Recorded the workspace observation.", Vec::new()))
    }
}

#[derive(Debug)]
struct PendingModel {
    started: tokio::sync::Notify,
}

impl PendingModel {
    fn new() -> Self {
        Self {
            started: tokio::sync::Notify::new(),
        }
    }

    async fn wait_until_started(&self) {
        self.started.notified().await;
    }
}

#[async_trait]
impl ModelPort for PendingModel {
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
            maximum_output_bytes: agent_provider_api::MAXIMUM_MODEL_RESPONSE_BYTES,
        }
    }

    async fn complete(&self, _turn: &ModelTurn) -> Result<ModelTurnResult, ProviderError> {
        self.started.notify_one();
        std::future::pending::<Result<ModelTurnResult, ProviderError>>().await
    }
}

fn skill_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../skills")
}

#[derive(Clone, Debug)]
struct TestRuntime(AgentRuntime);

impl Deref for TestRuntime {
    type Target = AgentRuntime;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl TestRuntime {
    #[allow(clippy::panic)]
    fn resolve(task: TaskEnvelope) -> ResolvedTask {
        match ResolvedTask::from_internal_for_tests(task) {
            Ok(value) => value,
            Err(error) => panic!("valid test task: {error}"),
        }
    }

    async fn run(&self, task: TaskEnvelope) -> agent_core::AgentResult {
        let resolved = Self::resolve(task);
        self.0.run(resolved).await
    }

    async fn run_with_cancellation(
        &self,
        task: TaskEnvelope,
        cancellation: TaskCancellation,
    ) -> agent_core::AgentResult {
        let resolved = Self::resolve(task);
        Box::pin(self.0.run_with_cancellation(resolved, cancellation)).await
    }

    async fn run_many(&self, tasks: Vec<TaskEnvelope>) -> Vec<agent_core::AgentResult> {
        let resolved = tasks.into_iter().map(Self::resolve).collect();
        self.0.run_many(resolved).await
    }

    async fn run_many_with_cancellation(
        &self,
        tasks: Vec<TaskEnvelope>,
        cancellation: TaskCancellation,
    ) -> Vec<agent_core::AgentResult> {
        let resolved = tasks.into_iter().map(Self::resolve).collect();
        self.0
            .run_many_with_cancellation(resolved, cancellation)
            .await
    }
}

fn runtime(
    state: &Path,
    model: Arc<dyn ModelPort>,
    modules: Vec<Arc<dyn ToolModule>>,
) -> Result<TestRuntime, Box<dyn std::error::Error>> {
    let options = AgentRuntimeOptions {
        state_root: state.to_path_buf(),
        skill_roots: vec![skill_root()],
        profiles: agent_profiles::built_in_registry()?,
        model,
        maximum_parallel_tasks: 4,
        tool_modules: modules,
    };
    Ok(TestRuntime(AgentRuntime::open(options)?))
}

fn runtime_with_skill_root(
    state: &Path,
    skills: PathBuf,
    model: Arc<dyn ModelPort>,
) -> Result<TestRuntime, Box<dyn std::error::Error>> {
    let options = AgentRuntimeOptions {
        state_root: state.to_path_buf(),
        skill_roots: vec![skills],
        profiles: agent_profiles::built_in_registry()?,
        model,
        maximum_parallel_tasks: 4,
        tool_modules: Vec::new(),
    };
    Ok(TestRuntime(AgentRuntime::open(options)?))
}

fn task(profile: ProfileId, objective: &str) -> Result<TaskEnvelope, Box<dyn std::error::Error>> {
    Ok(TaskEnvelope {
        schema_version: agent_core::PROTOCOL_VERSION,
        task_id: TaskId::parse(format!("task-{}", uuid::Uuid::now_v7()))?,
        session_id: SessionId::parse(format!("session-{}", uuid::Uuid::now_v7()))?,
        parent_task_id: None,
        profile,
        track: None,
        objective: objective.to_owned(),
        image_task: None,
        history: Vec::<ConversationTurn>::new(),
        workspace: None,
        skills: Vec::new(),
        context: BTreeMap::new(),
        budget: TaskBudget::default(),
        effect_approval: EffectApprovalMode::Deny,
        allow_memory_write: false,
        approvals: Vec::new(),
    })
}

#[derive(Debug)]
struct EmptyToolPlugin {
    manifest: PluginManifest,
}

impl RuntimePlugin for EmptyToolPlugin {
    fn manifest(&self) -> &PluginManifest {
        &self.manifest
    }

    fn contribute(&self, _registrar: &mut PluginRegistrar<'_>) -> Result<(), RuntimePluginError> {
        Ok(())
    }
}

#[test]
fn invalid_plugin_composition_does_not_create_runtime_state()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = TempDir::new()?;
    let state = temp.path().join("state");
    let options = AgentRuntimeOptions {
        state_root: state.clone(),
        skill_roots: vec![skill_root()],
        profiles: agent_profiles::built_in_registry()?,
        model: Arc::new(SequenceModel::new(Vec::new())),
        maximum_parallel_tasks: 4,
        tool_modules: Vec::new(),
    };
    let manifest = PluginManifest::linked(
        PluginId::parse("test.empty-tools")?,
        "1",
        PluginTrust::BuiltIn,
    )?
    .contributes(ContributionKind::Tools);

    let result =
        AgentRuntime::open_with_plugins(options, vec![Arc::new(EmptyToolPlugin { manifest })]);

    assert!(matches!(
        result,
        Err(agent_runtime::RuntimeError::Contract(_))
    ));
    assert!(
        !state.exists(),
        "invalid startup composition must not create runtime state"
    );
    Ok(())
}

fn turn(text: &str, tool_calls: Vec<ToolCall>) -> ModelTurnResult {
    ModelTurnResult {
        text: text.to_owned(),
        tool_calls,
        usage: None,
    }
}

fn call(name: &str, arguments: Value) -> Result<ToolCall, Box<dyn std::error::Error>> {
    Ok(ToolCall {
        id: ToolCallId::parse(format!("call-{}", uuid::Uuid::now_v7()))?,
        name: name.to_owned(),
        arguments,
    })
}

#[tokio::test]
async fn creator_direct_path_produces_a_content_addressed_artifact()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = TempDir::new()?;
    let model = Arc::new(SequenceModel::new(vec![turn("# Result", Vec::new())]));
    let runtime = runtime(&temp.path().join("state"), model.clone(), Vec::new())?;
    let result = runtime
        .run(task(ProfileId::creator(), "Write a heading")?)
        .await;
    assert_eq!(result.status, AgentStatus::Succeeded);
    assert_eq!(result.output.as_deref(), Some("# Result"));
    assert_eq!(result.artifacts.len(), 3);
    assert!(result.artifacts.iter().any(|artifact| {
        artifact.media_type == "application/vnd.upagent.model-turn+json; charset=utf-8"
    }));
    assert!(result.artifacts.iter().any(|artifact| {
        artifact.media_type == "application/vnd.upagent.model-result+json; charset=utf-8"
    }));
    assert!(
        result
            .artifacts
            .iter()
            .any(|artifact| { artifact.media_type == "text/markdown; charset=utf-8" })
    );
    let turns = model.recorded_turns()?;
    assert_eq!(turns.len(), 1);
    assert_eq!(
        turns[0].maximum_response_bytes, MAXIMUM_MODEL_RESPONSE_BYTES,
        "default creator budget must be accepted by the active provider"
    );
    assert!(result.failure.is_none());
    Ok(())
}

#[tokio::test]
async fn active_image_task_cannot_succeed_without_candidate()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = TempDir::new()?;
    let model = Arc::new(SequenceModel::new(vec![
        turn("done", Vec::new()),
        turn("still done", Vec::new()),
        turn("final done", Vec::new()),
    ]));
    let runtime = runtime(&temp.path().join("state"), model.clone(), Vec::new())?;
    let mut request = task(ProfileId::creator(), "Edit the provided image")?;
    request.image_task = Some(ImageTaskContract {
        schema: IMAGE_TASK_CONTRACT_SCHEMA.to_owned(),
        goal: "Edit the provided image without replacing the source".to_owned(),
        source: ImageSource::Provided {
            artifact: fixture_image_artifact('0')?,
        },
        maximum_attempts: 3,
        preserve: vec!["preserve all unspecified regions".to_owned()],
        visual_criteria: vec!["visually review the current candidate".to_owned()],
        verification: ImageVerificationContract {
            visual_review: true,
            deterministic: None,
        },
    });

    let result = runtime.run(request).await;

    assert_eq!(result.status, AgentStatus::Failed, "{result:?}");
    assert_eq!(
        result.failure.as_ref().map(|failure| failure.code),
        Some(FailureCode::VerificationFailed)
    );
    assert!(result.failure.as_ref().is_some_and(|failure| {
        failure.message == "image director could not accept the final candidate"
    }));
    assert_eq!(model.recorded_turns()?.len(), 3);
    assert_eq!(model.recorded_observation_counts()?, vec![0, 0, 0]);
    Ok(())
}

#[derive(Debug)]
struct ExactImageFlowModule {
    candidate: Vec<u8>,
    specification: ImageVerificationSpec,
}

#[async_trait]
impl ToolModule for ExactImageFlowModule {
    #[allow(clippy::expect_used)]
    fn tools(&self) -> Vec<ModuleTool> {
        canonical_tool_bindings(&["image_render".to_owned(), "image_compare".to_owned()])
            .expect("canonical image tool bindings")
            .into_iter()
            .map(|binding| ModuleTool {
                definition: binding.definition,
                side_effect: match binding.side_effect {
                    SelectedToolSideEffect::ReadOnly => ToolSideEffect::ReadOnly,
                    SelectedToolSideEffect::External => ToolSideEffect::External,
                },
                allowed_profiles: BTreeSet::from([ProfileId::creator()]),
                required_capabilities: BTreeSet::new(),
                activation_skills: BTreeSet::new(),
            })
            .collect()
    }

    async fn execute(
        &self,
        _context: ToolContext,
        call: ToolCall,
    ) -> Result<ModuleExecution, ToolModuleError> {
        match call.name.as_str() {
            "image_render" => Ok(ModuleExecution {
                result: ToolResult {
                    call_id: call.id,
                    name: call.name,
                    value: json!({"operation":"render","artifactRoles":["render"]}),
                    is_error: false,
                    failure: None,
                },
                receipts: Vec::new(),
                artifacts: vec![ModuleArtifact {
                    bytes: self.candidate.clone(),
                    media_type: "image/png".to_owned(),
                    extension: "png".to_owned(),
                }],
            }),
            "image_compare" => Ok(ModuleExecution {
                result: ToolResult {
                    call_id: call.id,
                    name: call.name,
                    value: json!({
                        "operation":"compare",
                        "allRequiredPassed":true,
                        "verificationSpecDigest":self
                            .specification
                            .digest()
                            .map_err(ToolModuleError::Contract)?,
                        "assertions":self.specification.assertions.iter().map(|assertion| json!({
                            "id":assertion.id(),
                            "assertionType":assertion.assertion_type(),
                            "severity":assertion.severity(),
                            "passed":true,
                            "observed":true,
                            "message":"test deterministic evaluation"
                        })).collect::<Vec<_>>(),
                        "artifactRoles":[]
                    }),
                    is_error: false,
                    failure: None,
                },
                receipts: Vec::new(),
                artifacts: Vec::new(),
            }),
            _ => Err(ToolModuleError::Contract(
                "unexpected exact image flow tool".to_owned(),
            )),
        }
    }
}

fn fixture_image_artifact(seed: char) -> Result<ArtifactRef, Box<dyn std::error::Error>> {
    let digest = seed.to_string().repeat(64);
    Ok(ArtifactRef {
        id: ArtifactId::parse(format!("art-{digest}"))?,
        sha256: digest.clone(),
        media_type: "image/png".to_owned(),
        byte_length: 1,
        relative_path: format!("{}/{}.png", &digest[..2], digest),
    })
}

fn one_pixel_png() -> Vec<u8> {
    vec![
        0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x04, 0x00, 0x00, 0x00, 0xb5,
        0x1c, 0x0c, 0x02, 0x00, 0x00, 0x00, 0x0b, 0x49, 0x44, 0x41, 0x54, 0x78, 0xda, 0x63, 0x64,
        0xf8, 0x0f, 0x00, 0x01, 0x05, 0x01, 0x01, 0x27, 0x18, 0xe3, 0x66, 0x00, 0x00, 0x00, 0x00,
        0x49, 0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
    ]
}

#[tokio::test]
async fn exact_image_contract_closes_render_compare_observation_loop()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = TempDir::new()?;
    let state = temp.path().join("state");
    std::fs::create_dir_all(&state)?;
    let bytes = one_pixel_png();
    let store = ArtifactStore::open(state.join("artifacts"))?;
    let baseline = store.put(&bytes, "image/png", "png")?;
    let specification = ImageVerificationSpec {
        schema: IMAGE_VERIFICATION_SPEC_SCHEMA.to_owned(),
        before_artifact: baseline.clone(),
        after_kind: ImageCompareArtifactKind::Raster,
        assertions: vec![ImageCompareAssertion::ExactEqual {
            id: "preserve".to_owned(),
            severity: ImageCompareSeverity::Required,
        }],
    };
    let render = call(
        "image_render",
        json!({
            "canvas":{"width":1,"height":1,"background":[0,0,0,0]},
            "nodes":[{
                "id":"source",
                "artifact":baseline,
                "kind":"raster",
                "transform":{"matrix":[1.0,0.0,0.0,0.0,1.0,0.0,0.0,1.0]},
                "opacity":255
            }]
        }),
    )?;
    let compare = call(
        "image_compare",
        json!({
            "beforeArtifact":baseline,
            "afterArtifact":baseline,
            "afterKind":"raster",
            "assertions":specification.assertions,
            "preview":{"difference":false,"maskOverlay":false,"maximumEdge":256}
        }),
    )?;
    let model = Arc::new(SequenceModel::new(vec![
        turn("", vec![render]),
        turn("", vec![compare]),
        turn("TEST_IMAGE_REVIEW_CURRENT", Vec::new()),
        turn("Completed the exact image task.", Vec::new()),
    ]));
    let runtime = runtime(
        &state,
        model.clone(),
        vec![Arc::new(ExactImageFlowModule {
            candidate: bytes,
            specification: specification.clone(),
        })],
    )?;
    let plugin_snapshot = runtime.plugin_snapshot().clone();
    let canonical_compare = canonical_tool_bindings(&["image_compare".to_owned()])?;
    let mut request = task(ProfileId::creator(), "Preserve the provided image exactly")?;
    request.image_task = Some(ImageTaskContract {
        schema: IMAGE_TASK_CONTRACT_SCHEMA.to_owned(),
        goal: "Preserve the provided image exactly".to_owned(),
        source: ImageSource::Provided {
            artifact: baseline.clone(),
        },
        maximum_attempts: 2,
        preserve: vec!["all pixels".to_owned()],
        visual_criteria: vec!["observe the current render".to_owned()],
        verification: ImageVerificationContract {
            visual_review: true,
            deterministic: Some(specification.clone()),
        },
    });

    let result = runtime.run(request).await;

    assert_eq!(result.status, AgentStatus::Succeeded, "{result:?}");
    assert_eq!(result.metrics.get("toolCalls"), Some(&json!(2)));
    assert_eq!(
        result.metrics["imageDirector"]["verifiedSpecDigest"],
        specification.digest()?
    );
    let turns = model.recorded_turns()?;
    assert_eq!(turns.len(), 4);
    let canonical_branches =
        &canonical_compare[0].definition.input_schema["properties"]["assertions"]["items"]["oneOf"];
    let canonical_branches = canonical_branches
        .as_array()
        .ok_or("canonical comparison schema has no assertion branches")?;
    assert!(canonical_branches.len() > 1);
    let exact_branch = canonical_branches
        .iter()
        .find(|branch| branch["properties"]["type"]["const"] == "exact_equal")
        .ok_or("canonical comparison schema has no exact_equal branch")?;
    for turn in &turns {
        let compare = turn
            .tools
            .iter()
            .find(|tool| tool.name == "image_compare")
            .ok_or("model turn has no comparison tool")?;
        assert_eq!(
            compare.input_schema["properties"]["assertions"]["items"]["oneOf"],
            json!([exact_branch])
        );
    }
    let request_artifacts = result
        .artifacts
        .iter()
        .filter(|artifact| {
            artifact.media_type == "application/vnd.upagent.model-turn+json; charset=utf-8"
        })
        .collect::<Vec<_>>();
    assert_eq!(request_artifacts.len(), turns.len());
    for artifact in request_artifacts {
        let persisted: ModelTurn = serde_json::from_slice(&store.read(artifact)?)?;
        assert_eq!(persisted.tools, turns[0].tools);
    }
    assert_eq!(runtime.plugin_snapshot(), &plugin_snapshot);
    assert_eq!(
        canonical_tool_bindings(&["image_compare".to_owned()])?,
        canonical_compare
    );
    assert_eq!(
        result.metrics["imageDirector"]["visualReview"]["criteria"][0]["decision"],
        "PASS"
    );
    assert!(!format!("{:?}", result.output).contains("IMAGE_REVIEW:"));
    Ok(())
}

#[tokio::test]
async fn oversized_active_provider_budget_is_rejected_during_preparation()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = TempDir::new()?;
    let model = Arc::new(SequenceModel::new(vec![turn("must not run", Vec::new())]));
    let runtime = runtime(&temp.path().join("state"), model.clone(), Vec::new())?;
    let mut request = task(ProfileId::creator(), "Reject an unsupported response bound")?;
    request.budget.maximum_output_bytes = MAXIMUM_MODEL_RESPONSE_BYTES + 1;

    let result = runtime.run(request).await;

    assert_eq!(result.status, AgentStatus::Failed, "{result:?}");
    assert_eq!(
        result.failure.as_ref().map(|failure| failure.code),
        Some(FailureCode::InvalidRequest)
    );
    assert!(
        result
            .failure
            .as_ref()
            .is_some_and(|failure| { failure.message.contains("exceeds provider test bound") })
    );
    assert!(
        model.recorded_turns()?.is_empty(),
        "unsupported budgets must fail before provider dispatch"
    );
    Ok(())
}

#[tokio::test]
async fn terminal_result_projection_reuses_the_exact_child_without_model_call()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = TempDir::new()?;
    let model = Arc::new(SequenceModel::new(vec![turn("# Cached", Vec::new())]));
    let runtime = runtime(&temp.path().join("state"), model.clone(), Vec::new())?;
    let request = task(ProfileId::creator(), "Cache this child")?;

    let first = runtime.run(request.clone()).await;
    assert_eq!(first.status, AgentStatus::Succeeded, "{first:?}");
    let cached = runtime
        .read_terminal_result(&request)?
        .ok_or("terminal result projection returned no result")?;
    assert_eq!(cached.output.as_deref(), Some("# Cached"));

    let second = runtime
        .read_terminal_result(&request)?
        .ok_or("second terminal result projection returned no result")?;
    assert_eq!(second.output.as_deref(), Some("# Cached"));
    assert_eq!(model.recorded_turns()?.len(), 1);

    let mut changed = request.clone();
    changed.objective = "Different child".to_owned();
    assert!(runtime.read_terminal_result(&changed).is_err());

    let task_directory = temp
        .path()
        .join("state")
        .join("sessions")
        .join(request.session_id.as_str())
        .join(request.task_id.as_str());
    let result_path = task_directory.join("result.json");
    let mut forged = cached;
    let mut status_mismatch = forged.clone();
    status_mismatch.status = AgentStatus::Failed;
    std::fs::write(&result_path, serde_json::to_vec_pretty(&status_mismatch)?)?;
    assert!(runtime.read_terminal_result(&request).is_err());
    std::fs::write(&result_path, serde_json::to_vec_pretty(&forged)?)?;
    forged.state = TaskState::Suspended;
    forged.status = AgentStatus::Suspended;
    std::fs::write(result_path, serde_json::to_vec_pretty(&forged)?)?;
    assert!(runtime.read_terminal_result(&request).is_err());
    Ok(())
}

#[tokio::test]
async fn developer_loads_only_the_selected_track_skill() -> Result<(), Box<dyn std::error::Error>> {
    let temp = TempDir::new()?;
    let workspace = temp.path().join("workspace");
    std::fs::create_dir(&workspace)?;
    let model = Arc::new(SequenceModel::new(vec![turn(
        "Prototype observed.",
        Vec::new(),
    )]));
    let runtime = runtime(&temp.path().join("state"), model.clone(), Vec::new())?;
    let mut request = task(ProfileId::developer(), "Test one product hypothesis")?;
    request.track = Some(TrackId::prototyper());
    request.workspace = Some(workspace.to_string_lossy().into_owned());

    let result = runtime.run(request).await;

    assert_eq!(result.status, AgentStatus::Succeeded, "{result:?}");
    let turns = model.recorded_turns()?;
    let messages = &turns.first().ok_or("provider received no turn")?.messages;
    let joined = messages
        .iter()
        .map(|message| message.content.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(joined.contains("# Prototyper"));
    for excluded in ["# Builder", "# Grower", "# Sweeper", "# Maintainer"] {
        assert!(!joined.contains(excluded), "unexpected skill: {excluded}");
    }
    Ok(())
}

#[tokio::test]
async fn task_inspection_reconstructs_track_and_selected_skill_evidence()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = TempDir::new()?;
    let workspace = temp.path().join("workspace");
    std::fs::create_dir(&workspace)?;
    let model = Arc::new(SequenceModel::new(vec![turn(
        "Prototype observed.",
        Vec::new(),
    )]));
    let runtime = runtime(&temp.path().join("state"), model, Vec::new())?;
    let mut request = task(ProfileId::developer(), "Inspect this prototype")?;
    request.track = Some(TrackId::prototyper());
    request.workspace = Some(workspace.canonicalize()?.to_string_lossy().into_owned());
    let result = runtime.run(request.clone()).await;
    assert_eq!(result.status, AgentStatus::Succeeded, "{result:?}");

    let task_directory = temp
        .path()
        .join("state")
        .join("sessions")
        .join(request.session_id.as_str())
        .join(request.task_id.as_str());
    let before_request = std::fs::read(task_directory.join("request.json"))?;
    let before_events = std::fs::read(task_directory.join("events.ndjson"))?;
    let before_result = std::fs::read(task_directory.join("result.json"))?;

    let inspection = inspect_task(
        temp.path().join("state"),
        request.session_id.as_str(),
        request.task_id.as_str(),
    )?;
    assert_eq!(inspection.task, request);
    assert_eq!(
        inspection.events.last().map(|event| event.next_state),
        Some(TaskState::Succeeded)
    );
    assert_eq!(
        inspection.result.as_ref().map(|value| value.status),
        Some(AgentStatus::Succeeded)
    );
    let validated = inspection
        .events
        .iter()
        .find(|record| record.event == KernelEvent::Validated)
        .ok_or("validated event missing")?;
    assert_eq!(validated.payload["track"], "prototyper");
    assert_eq!(
        validated.payload["selectedSkills"]
            .as_array()
            .and_then(|skills| skills.first())
            .and_then(|skill| skill.get("id")),
        Some(&serde_json::json!("developer/prototyper"))
    );
    assert_eq!(
        std::fs::read(task_directory.join("request.json"))?,
        before_request
    );
    assert_eq!(
        std::fs::read(task_directory.join("events.ndjson"))?,
        before_events
    );
    assert_eq!(
        std::fs::read(task_directory.join("result.json"))?,
        before_result
    );
    Ok(())
}

#[tokio::test]
async fn task_inspection_rejects_tampered_event_and_result()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = TempDir::new()?;
    let model = Arc::new(SequenceModel::new(vec![turn("Done", Vec::new())]));
    let runtime = runtime(&temp.path().join("state"), model, Vec::new())?;
    let request = task(ProfileId::creator(), "Inspect tamper evidence")?;
    runtime.run(request.clone()).await;
    let task_directory = temp
        .path()
        .join("state")
        .join("sessions")
        .join(request.session_id.as_str())
        .join(request.task_id.as_str());
    let events_path = task_directory.join("events.ndjson");
    let original_events = std::fs::read(&events_path)?;
    let mut event: serde_json::Value = serde_json::from_slice(
        std::fs::read_to_string(&events_path)?
            .lines()
            .next()
            .ok_or("event missing")?
            .as_bytes(),
    )?;
    event["sequence"] = serde_json::json!(99);
    let mut tampered_events = serde_json::to_vec(&event)?;
    tampered_events.push(b'\n');
    tampered_events.extend_from_slice(
        &original_events[original_events
            .iter()
            .position(|byte| *byte == b'\n')
            .ok_or("newline missing")?
            + 1..],
    );
    std::fs::write(&events_path, tampered_events)?;
    assert!(
        inspect_task(
            temp.path().join("state"),
            request.session_id.as_str(),
            request.task_id.as_str()
        )
        .is_err()
    );
    std::fs::write(&events_path, &original_events)?;

    let result_path = task_directory.join("result.json");
    let original_result = std::fs::read(&result_path)?;
    let mut result: serde_json::Value = serde_json::from_slice(&original_result)?;
    result["state"] = serde_json::json!("failed");
    std::fs::write(&result_path, serde_json::to_vec_pretty(&result)?)?;
    assert!(
        inspect_task(
            temp.path().join("state"),
            request.session_id.as_str(),
            request.task_id.as_str()
        )
        .is_err()
    );
    std::fs::write(&result_path, original_result)?;
    Ok(())
}

#[tokio::test]
async fn task_inspection_rejects_an_active_task() -> Result<(), Box<dyn std::error::Error>> {
    let temp = TempDir::new()?;
    let model = Arc::new(PendingModel::new());
    let runtime = runtime(&temp.path().join("state"), model.clone(), Vec::new())?;
    let request = task(ProfileId::creator(), "Inspect active task")?;
    let cancellation = TaskCancellation::new();
    let runtime_for_task = runtime.clone();
    let cancellation_for_task = cancellation.clone();
    let request_for_task = request.clone();
    let running = tokio::spawn(async move {
        runtime_for_task
            .run_with_cancellation(request_for_task, cancellation_for_task)
            .await
    });
    model.wait_until_started().await;

    assert!(
        inspect_task(
            temp.path().join("state"),
            request.session_id.as_str(),
            request.task_id.as_str()
        )
        .is_err()
    );
    cancellation.cancel();
    let result = running.await?;
    assert_eq!(result.status, AgentStatus::Cancelled);
    Ok(())
}

#[tokio::test]
async fn developer_rejects_cross_track_skill_injection() -> Result<(), Box<dyn std::error::Error>> {
    let temp = TempDir::new()?;
    let workspace = temp.path().join("workspace");
    std::fs::create_dir(&workspace)?;
    let model = Arc::new(SequenceModel::new(vec![turn("unused", Vec::new())]));
    let runtime = runtime(&temp.path().join("state"), model.clone(), Vec::new())?;
    let mut request = task(ProfileId::developer(), "Prototype only")?;
    request.track = Some(TrackId::prototyper());
    request.workspace = Some(workspace.to_string_lossy().into_owned());
    request.skills.push(SkillId::parse("developer/builder")?);

    let result = runtime.run(request).await;

    assert_eq!(result.status, AgentStatus::Failed);
    assert_eq!(
        result.failure.as_ref().map(|failure| failure.code),
        Some(FailureCode::InvalidProfile)
    );
    assert_eq!(model.recorded_turns()?, [] as [ModelTurn; 0]);
    Ok(())
}

#[tokio::test]
async fn deadline_suspends_and_the_same_task_resumes_without_losing_work()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = TempDir::new()?;
    let state = temp.path().join("state");
    let initial_runtime = runtime(&state, Arc::new(PendingModel::new()), Vec::new())?;
    let mut request = task(ProfileId::creator(), "Never complete")?;
    request.budget.timeout_ms = 1_000;
    let task_directory = state
        .join("sessions")
        .join(request.session_id.as_str())
        .join(request.task_id.as_str());

    let suspended = initial_runtime.run(request.clone()).await;

    assert_eq!(suspended.status, AgentStatus::Suspended);
    assert_eq!(suspended.state, TaskState::Suspended);
    assert_eq!(
        suspended.failure.as_ref().map(|failure| failure.code),
        Some(FailureCode::BudgetExceeded)
    );
    assert!(
        suspended
            .failure
            .as_ref()
            .is_some_and(|failure| failure.details.contains_key("providerDrain"))
    );
    assert!(!task_directory.join("result.json").exists());
    let recoveries = list_recoveries(&state)?;
    assert_eq!(recoveries.len(), 1);
    assert!(recoveries[0].resumable);
    drop(initial_runtime);

    let model = Arc::new(SequenceModel::new(vec![turn("# Resumed", Vec::new())]));
    let resumed_runtime = runtime(&state, model.clone(), Vec::new())?;
    let resumed = resumed_runtime.run(request).await;

    assert_eq!(resumed.status, AgentStatus::Succeeded, "{resumed:?}");
    assert_eq!(resumed.output.as_deref(), Some("# Resumed"));
    assert_eq!(resumed.metrics.get("modelCalls"), Some(&json!(2)));
    assert!(resumed.artifacts.iter().any(|artifact| {
        artifact
            .media_type
            .starts_with("application/vnd.upagent.session-checkpoint")
    }));
    assert!(task_directory.join("result.json").is_file());
    let events = std::fs::read_to_string(task_directory.join("events.ndjson"))?;
    assert!(events.contains("\"event\":{\"kind\":\"suspended\"}"));
    assert!(events.contains("\"event\":{\"kind\":\"resumed\"}"));
    let inspection = inspect_task(
        &state,
        resumed.session_id.as_str(),
        resumed.task_id.as_str(),
    )?;
    let checkpoint_refs = inspection
        .events
        .iter()
        .filter(|event| matches!(event.event, KernelEvent::Suspended | KernelEvent::Resumed))
        .map(|event| event.payload.get("checkpoint").cloned())
        .collect::<Vec<_>>();
    assert_eq!(checkpoint_refs.len(), 2);
    assert_eq!(checkpoint_refs[0], checkpoint_refs[1]);
    let checkpoint = checkpoint_refs[0].as_ref().ok_or("checkpoint missing")?;
    let checkpoint: agent_core::ArtifactRef = serde_json::from_value(checkpoint.clone())?;
    assert!(resumed.artifacts.contains(&checkpoint));
    assert!(
        inspection
            .result
            .as_ref()
            .is_some_and(|result| result.artifacts.contains(&checkpoint))
    );
    assert_eq!(model.recorded_turns()?.len(), 1);
    Ok(())
}

#[tokio::test]
async fn corrupted_resume_checkpoint_is_rejected() -> Result<(), Box<dyn std::error::Error>> {
    let temp = TempDir::new()?;
    let state = temp.path().join("state");
    let initial = runtime(&state, Arc::new(PendingModel::new()), Vec::new())?;
    let mut request = task(ProfileId::creator(), "Suspend before corruption")?;
    request.budget.timeout_ms = 1_000;

    let suspended = initial.run(request.clone()).await;
    assert_eq!(suspended.status, AgentStatus::Suspended);
    let checkpoint = suspended
        .artifacts
        .iter()
        .find(|artifact| {
            artifact
                .media_type
                .starts_with("application/vnd.upagent.session-checkpoint")
        })
        .ok_or("suspended result has no checkpoint artifact")?;
    let checkpoint_path = state.join("artifacts").join(&checkpoint.relative_path);
    let mut bytes = std::fs::read(&checkpoint_path)?;
    bytes[0] ^= 1;
    std::fs::write(&checkpoint_path, bytes)?;
    drop(initial);

    let resumed_runtime = runtime(
        &state,
        Arc::new(SequenceModel::new(vec![turn("unused", Vec::new())])),
        Vec::new(),
    )?;
    let rejected = resumed_runtime.run(request).await;

    assert_eq!(rejected.status, AgentStatus::Suspended);
    assert_eq!(
        rejected.failure.as_ref().map(|failure| failure.code),
        Some(FailureCode::StorageFailed)
    );
    let recoveries = list_recoveries(&state)?;
    assert_eq!(recoveries.len(), 1);
    assert!(!recoveries[0].resumable);
    Ok(())
}

#[tokio::test]
async fn changed_track_or_skill_preparation_cannot_resume() -> Result<(), Box<dyn std::error::Error>>
{
    let temp = TempDir::new()?;
    let state = temp.path().join("state");
    let skills = temp.path().join("skills");
    let creator = skills.join("creator/chat");
    std::fs::create_dir_all(&creator)?;
    std::fs::write(
        creator.join("skill.json"),
        r#"{"id":"creator/chat","version":"1.0.0","description":"test","profiles":["creator"]}"#,
    )?;
    std::fs::write(creator.join("SKILL.md"), "# First preparation")?;
    let initial = runtime_with_skill_root(&state, skills.clone(), Arc::new(PendingModel::new()))?;
    let mut request = task(ProfileId::creator(), "Suspend before skill drift")?;
    request.budget.timeout_ms = 1_000;

    let suspended = initial.run(request.clone()).await;
    assert_eq!(suspended.status, AgentStatus::Suspended, "{suspended:?}");
    drop(initial);
    std::fs::write(creator.join("SKILL.md"), "# Changed preparation")?;

    let resumed_runtime = runtime_with_skill_root(
        &state,
        skills.clone(),
        Arc::new(SequenceModel::new(vec![turn("unused", Vec::new())])),
    )?;
    let rejected = resumed_runtime.run(request.clone()).await;

    assert_eq!(rejected.status, AgentStatus::Suspended);
    assert_eq!(
        rejected.failure.as_ref().map(|failure| failure.code),
        Some(FailureCode::InvalidProfile)
    );
    let task_directory = state
        .join("sessions")
        .join(request.session_id.as_str())
        .join(request.task_id.as_str());
    assert!(!task_directory.join("result.json").exists());
    drop(resumed_runtime);
    std::fs::write(creator.join("SKILL.md"), "# First preparation")?;
    let restored = runtime_with_skill_root(
        &state,
        skills,
        Arc::new(SequenceModel::new(vec![turn("restored", Vec::new())])),
    )?;
    let completed = restored.run(request).await;
    assert_eq!(completed.status, AgentStatus::Succeeded, "{completed:?}");
    Ok(())
}

#[tokio::test]
async fn transition_budget_exhaustion_records_a_terminal_failure()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = TempDir::new()?;
    let state = temp.path().join("state");
    let runtime = runtime(&state, Arc::new(PendingModel::new()), Vec::new())?;
    let mut request = task(ProfileId::creator(), "Stop at the control boundary")?;
    request.budget.maximum_transitions = 5;
    request.budget.timeout_ms = 1_000;
    let task_directory = state
        .join("sessions")
        .join(request.session_id.as_str())
        .join(request.task_id.as_str());

    let result = runtime.run(request).await;

    assert_eq!(result.status, AgentStatus::Failed, "{result:?}");
    assert!(task_directory.join("result.json").is_file());
    let events = std::fs::read_to_string(task_directory.join("events.ndjson"))?;
    assert!(events.contains("\"kind\":\"failed\""));
    assert!(!events.contains("\"kind\":\"suspended\""));
    Ok(())
}

#[tokio::test]
async fn model_call_exhaustion_still_fails_instead_of_resuming()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = TempDir::new()?;
    let workspace = temp.path().join("workspace");
    std::fs::create_dir(&workspace)?;
    std::fs::write(workspace.join("input.txt"), "evidence")?;
    let read = call("read_file", json!({"path":"input.txt","maxBytes":4096}))?;
    let runtime = runtime(
        &temp.path().join("state"),
        Arc::new(SequenceModel::new(vec![turn("", vec![read])])),
        Vec::new(),
    )?;
    let mut request = task(ProfileId::creator(), "Read once, then stop")?;
    request.workspace = Some(workspace.to_string_lossy().into_owned());
    request.budget.maximum_model_calls = 1;

    let result = runtime.run(request).await;

    assert_eq!(result.status, AgentStatus::Failed);
    assert_eq!(
        result.failure.as_ref().map(|failure| failure.code),
        Some(FailureCode::BudgetExceeded)
    );
    assert!(
        result
            .failure
            .as_ref()
            .is_some_and(|failure| failure.message.contains("model call budget"))
    );
    Ok(())
}

#[tokio::test]
async fn explicit_task_cancellation_stops_a_pending_model_call()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = TempDir::new()?;
    let model = Arc::new(PendingModel::new());
    let runtime = runtime(&temp.path().join("state"), model.clone(), Vec::new())?;
    let request = task(ProfileId::creator(), "Wait indefinitely")?;
    let cancellation = TaskCancellation::new();
    let execution = {
        let runtime = runtime.clone();
        let cancellation = cancellation.clone();
        tokio::spawn(async move { runtime.run_with_cancellation(request, cancellation).await })
    };

    tokio::time::timeout(Duration::from_secs(2), model.wait_until_started()).await?;
    cancellation.cancel();
    let result = tokio::time::timeout(Duration::from_secs(2), execution).await??;

    assert_eq!(result.status, AgentStatus::Cancelled);
    assert_eq!(
        result.failure.as_ref().map(|failure| failure.code),
        Some(FailureCode::Cancelled)
    );
    assert!(
        result
            .failure
            .as_ref()
            .is_some_and(|failure| failure.details.contains_key("providerDrain"))
    );
    Ok(())
}

#[tokio::test]
async fn pre_cancelled_tasks_are_classified_without_starting_the_model()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = TempDir::new()?;
    let model = Arc::new(SequenceModel::new(vec![turn("unused", Vec::new())]));
    let runtime = runtime(&temp.path().join("state"), model.clone(), Vec::new())?;
    let cancellation = TaskCancellation::new();
    cancellation.cancel();

    let result = runtime
        .run_with_cancellation(task(ProfileId::creator(), "Do not start")?, cancellation)
        .await;

    assert_eq!(result.status, AgentStatus::Cancelled);
    assert_eq!(
        result.failure.as_ref().map(|failure| failure.code),
        Some(FailureCode::Cancelled)
    );
    assert_eq!(model.recorded_turns()?, [] as [ModelTurn; 0]);
    Ok(())
}

#[tokio::test]
async fn developer_uses_the_real_rax_read_boundary_before_finishing()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = TempDir::new()?;
    let workspace = temp.path().join("workspace");
    std::fs::create_dir_all(&workspace)?;
    std::fs::write(workspace.join("README.md"), "evidence")?;
    let read = call("read_file", json!({"path":"README.md","maxBytes":4096}))?;
    let model = Arc::new(SequenceModel::new(vec![
        turn("", vec![read]),
        turn("Observed the repository evidence.", Vec::new()),
    ]));
    let runtime = runtime(&temp.path().join("state"), model, Vec::new())?;
    let mut request = task(ProfileId::developer(), "Inspect README")?;
    request.track = Some(TrackId::builder());
    request.workspace = Some(workspace.to_string_lossy().into_owned());
    let result = runtime.run(request).await;
    assert_eq!(result.status, AgentStatus::Succeeded);
    assert_eq!(result.metrics.get("toolCalls"), Some(&json!(1)));
    Ok(())
}

#[tokio::test]
async fn direct_execution_changes_the_actual_model_and_tool_surface()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = TempDir::new()?;
    let workspace = temp.path().join("workspace");
    std::fs::create_dir_all(&workspace)?;
    let model = Arc::new(SequenceModel::new(vec![turn(
        "The requested bounded observation is complete.",
        Vec::new(),
    )]));
    let runtime = runtime(&temp.path().join("state"), model.clone(), Vec::new())?;
    let mut request = task(ProfileId::developer(), "Inspect without changing files")?;
    request.track = Some(TrackId::builder());
    request.workspace = Some(workspace.to_string_lossy().into_owned());
    request.context.insert(
        "axd".to_owned(),
        json!({
            "execution_level":"direct",
            "required_tools":["workspace.read"],
            "max_workers":1,
            "output_contract":"structured_result_with_evidence"
        }),
    );

    let result = runtime.run(request).await;

    assert_eq!(result.status, AgentStatus::Succeeded, "{result:?}");
    let turns = model.recorded_turns()?;
    assert_eq!(turns.len(), 1);
    assert_eq!(turns[0].reasoning, agent_core::ReasoningEffort::Low);
    let tool_names = turns[0]
        .tools
        .iter()
        .map(|tool| tool.name.as_str())
        .collect::<BTreeSet<_>>();
    assert!(tool_names.contains("read_file"));
    assert!(!tool_names.contains("write_file"));
    assert!(!tool_names.contains("run_check"));
    assert_eq!(
        turns[0].metadata.get("executionClass"),
        Some(&json!("direct"))
    );
    assert_eq!(
        turns[0]
            .metadata
            .get("pluginSetDigest")
            .and_then(Value::as_str)
            .map(str::len),
        Some(64)
    );
    Ok(())
}

#[tokio::test]
async fn verified_execution_rejects_a_final_answer_without_check_evidence()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = TempDir::new()?;
    let workspace = temp.path().join("workspace");
    std::fs::create_dir_all(&workspace)?;
    let model = Arc::new(SequenceModel::new(vec![turn(
        "Implemented and verified.",
        Vec::new(),
    )]));
    let runtime = runtime(&temp.path().join("state"), model.clone(), Vec::new())?;
    let mut request = task(ProfileId::developer(), "Implement a verified change")?;
    request.track = Some(TrackId::builder());
    request.workspace = Some(workspace.to_string_lossy().into_owned());
    request.context.insert(
        "axd".to_owned(),
        json!({
            "execution_level":"verified",
            "required_tools":["check.run"],
            "required_checks":["build"],
            "max_workers":1,
            "output_contract":"structured_result_with_evidence"
        }),
    );

    let result = runtime.run(request).await;

    assert_eq!(result.status, AgentStatus::Failed);
    assert_eq!(
        result.failure.as_ref().map(|failure| failure.code),
        Some(FailureCode::VerificationFailed)
    );
    let turns = model.recorded_turns()?;
    assert_eq!(turns[0].reasoning, agent_core::ReasoningEffort::High);
    assert!(turns[0].tools.iter().any(|tool| tool.name == "run_check"));
    Ok(())
}

#[tokio::test]
async fn verified_v1_check_without_host_argv_cannot_execute_or_close_evidence()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = TempDir::new()?;
    let workspace = temp.path().join("workspace");
    std::fs::create_dir_all(&workspace)?;
    let attempted = call(
        "run_check",
        json!({
            "requirementId":"build",
            "program":"/usr/bin/touch",
            "args":["must-not-exist"],
            "cwd":null
        }),
    )?;
    let model = Arc::new(SequenceModel::new(vec![turn("", vec![attempted])]));
    let runtime = runtime(&temp.path().join("state"), model, Vec::new())?;
    let mut request = task(ProfileId::developer(), "Do not forge an unbound v1 check")?;
    request.track = Some(TrackId::builder());
    request.workspace = Some(workspace.to_string_lossy().into_owned());
    request.context.insert(
        "axd".to_owned(),
        json!({
            "execution_level":"verified",
            "required_tools":["check.run"],
            "required_checks":["build"],
            "max_workers":1,
            "output_contract":"structured_result_with_evidence"
        }),
    );

    let result = runtime.run(request).await;

    assert_eq!(result.status, AgentStatus::Failed);
    assert_eq!(
        result.failure.as_ref().map(|failure| failure.code),
        Some(FailureCode::VerificationFailed)
    );
    assert!(!workspace.join("must-not-exist").exists());
    Ok(())
}

#[tokio::test]
async fn prepared_v2_check_cannot_be_satisfied_by_a_different_command()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = TempDir::new()?;
    let workspace = temp.path().join("workspace");
    std::fs::create_dir_all(&workspace)?;
    let forged = call(
        "run_check",
        json!({
            "requirementId":"build",
            "program":"true",
            "args":[],
            "cwd":null
        }),
    )?;
    let model = Arc::new(SequenceModel::new(vec![turn("", vec![forged])]));
    let runtime = runtime(&temp.path().join("state"), model, Vec::new())?;
    let mut request = task(ProfileId::developer(), "Run the authorized build check")?;
    request.track = Some(TrackId::builder());
    request.workspace = Some(workspace.to_string_lossy().into_owned());
    request.context.insert(
        "axd".to_owned(),
        json!({
            "contract_digest":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "work_item_id":"work-build",
            "source_revision":"source-1",
            "contract_revision":1,
            "execution_class":"verified",
            "reasoning_effort":"high",
            "maximum_workers":1,
            "topology":"single_with_independent_verifier",
            "required_tools":["test"],
            "checks":[{
                "requirement_id":"build",
                "kind":"build",
                "required":true,
                "argv":["cargo","check","--workspace"]
            }],
            "output_schema":"axd.agent-result/2"
        }),
    );

    let result = runtime.run(request).await;

    assert_eq!(result.status, AgentStatus::Failed);
    assert_eq!(
        result.failure.as_ref().map(|failure| failure.code),
        Some(FailureCode::VerificationFailed)
    );
    Ok(())
}

#[tokio::test]
async fn prepared_v2_check_requires_a_known_requirement_before_process_execution()
-> Result<(), Box<dyn std::error::Error>> {
    for requirement_id in [Value::Null, json!("unknown")] {
        let temp = TempDir::new()?;
        let workspace = temp.path().join("workspace");
        std::fs::create_dir_all(&workspace)?;
        let attempted = call(
            "run_check",
            json!({
                "requirementId":requirement_id,
                "program":"/usr/bin/touch",
                "args":["must-not-exist"],
                "cwd":null
            }),
        )?;
        let model = Arc::new(SequenceModel::new(vec![turn("", vec![attempted])]));
        let runtime = runtime(&temp.path().join("state"), model, Vec::new())?;
        let mut request = task(ProfileId::developer(), "Run only a bound check")?;
        request.track = Some(TrackId::prototyper());
        request.workspace = Some(workspace.to_string_lossy().into_owned());
        request.context.insert(
            "axd".to_owned(),
            json!({
                "contract_digest":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                "work_item_id":"work-build",
                "source_revision":"source-1",
                "contract_revision":1,
                "execution_class":"verified",
                "reasoning_effort":"high",
                "maximum_workers":1,
                "topology":"single_with_independent_verifier",
                "required_tools":["test"],
                "checks":[{
                    "requirement_id":"build",
                    "kind":"build",
                    "required":true,
                    "argv":["true"]
                }],
                "output_schema":"axd.agent-result/2"
            }),
        );

        let result = runtime.run(request).await;

        assert_eq!(result.status, AgentStatus::Failed);
        assert_eq!(
            result.failure.as_ref().map(|failure| failure.code),
            Some(FailureCode::VerificationFailed)
        );
        assert!(!workspace.join("must-not-exist").exists());
    }
    Ok(())
}

#[tokio::test]
async fn prepared_v2_check_closes_evidence_only_after_the_authorized_command_passes()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = TempDir::new()?;
    let workspace = temp.path().join("workspace");
    std::fs::create_dir_all(&workspace)?;
    let authorized = call(
        "run_check",
        json!({
            "requirementId":"build",
            "program":"true",
            "args":[],
            "cwd":null,
            "timeoutMs":5_000,
            "maxOutputBytes":4_096
        }),
    )?;
    let model = Arc::new(SequenceModel::new(vec![
        turn("", vec![authorized]),
        turn("The host-authorized build check passed.", Vec::new()),
    ]));
    let runtime = runtime(&temp.path().join("state"), model, Vec::new())?;
    let mut request = task(ProfileId::developer(), "Run the authorized build check")?;
    request.track = Some(TrackId::builder());
    request.workspace = Some(workspace.to_string_lossy().into_owned());
    request.effect_approval = EffectApprovalMode::TaskGrant;
    request.context.insert(
        "axd".to_owned(),
        json!({
            "contract_digest":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "work_item_id":"work-build",
            "source_revision":"source-1",
            "contract_revision":1,
            "execution_class":"verified",
            "reasoning_effort":"high",
            "maximum_workers":1,
            "topology":"single_with_independent_verifier",
            "required_tools":["test"],
            "checks":[{
                "requirement_id":"build",
                "kind":"build",
                "required":true,
                "argv":["true"]
            }],
            "output_schema":"axd.agent-result/2"
        }),
    );

    let result = runtime.run(request).await;

    assert_eq!(result.status, AgentStatus::Succeeded, "{result:?}");
    assert_eq!(
        result.metrics.get("successfulChecks"),
        Some(&json!(["build"]))
    );
    assert!(result.artifacts.len() >= 2);
    Ok(())
}

#[tokio::test]
async fn twin_memory_write_is_revisioned_and_recallable() -> Result<(), Box<dyn std::error::Error>>
{
    let temp = TempDir::new()?;
    let remember = call(
        "remember",
        json!({
            "kind":"preference",
            "content":"Prefers small explicit systems",
            "confidenceBasisPoints":10000,
            "retention":"persistent"
        }),
    )?;
    let model = Arc::new(SequenceModel::new(vec![
        turn("", vec![remember]),
        turn("Stored with explicit approval.", Vec::new()),
    ]));
    let state = temp.path().join("state");
    let runtime = runtime(&state, model, Vec::new())?;
    let mut request = task(ProfileId::twin(), "Remember this preference")?;
    request
        .context
        .insert("memoryScope".to_owned(), json!("user:test"));
    request.allow_memory_write = true;
    let result = runtime.run(request).await;
    assert_eq!(result.status, AgentStatus::Succeeded);
    assert_eq!(result.receipts.len(), 1);
    let memory = MemoryStore::open(state.join("memory"))?;
    let recalled = memory.recall("user:test", "small explicit", 10)?;
    assert_eq!(recalled.len(), 1);
    Ok(())
}

#[tokio::test]
async fn profile_can_pause_for_material_user_input() -> Result<(), Box<dyn std::error::Error>> {
    let temp = TempDir::new()?;
    let state = temp.path().join("state");
    let request_input = call(
        "request_input",
        json!({"question":"Which format?","options":["Markdown","JSON"]}),
    )?;
    let model = Arc::new(SequenceModel::new(vec![turn("", vec![request_input])]));
    let runtime = runtime(&state, model, Vec::new())?;
    let request = task(ProfileId::creator(), "Create the artifact")?;
    let result = runtime.run(request.clone()).await;
    assert_eq!(result.status, AgentStatus::AwaitingInput);
    assert!(
        result
            .output
            .as_deref()
            .is_some_and(|value| value.contains("Markdown"))
    );
    let inspection = inspect_task(
        &state,
        request.session_id.as_str(),
        request.task_id.as_str(),
    )?;
    let persisted = inspection
        .result
        .as_ref()
        .ok_or("awaiting-input result missing")?;
    assert_eq!(persisted.state, TaskState::AwaitingInput);
    assert_eq!(persisted.status, AgentStatus::AwaitingInput);
    Ok(())
}

#[derive(Debug)]
struct WordCountModule {
    activation_skill: SkillId,
}

#[async_trait]
impl ToolModule for WordCountModule {
    fn tools(&self) -> Vec<ModuleTool> {
        vec![ModuleTool {
            definition: ToolDefinition {
                name: "word_count".to_owned(),
                description: "Count whitespace-separated words.".to_owned(),
                input_schema: json!({
                    "type":"object",
                    "properties":{"text":{"type":"string"}},
                    "required":["text"],
                    "additionalProperties":false
                }),
                strict: true,
            },
            side_effect: ToolSideEffect::ReadOnly,
            allowed_profiles: BTreeSet::from([ProfileId::creator()]),
            required_capabilities: BTreeSet::<CapabilityId>::new(),
            activation_skills: BTreeSet::from([self.activation_skill.clone()]),
        }]
    }

    async fn execute(
        &self,
        _context: ToolContext,
        call: ToolCall,
    ) -> Result<ModuleExecution, ToolModuleError> {
        let text = call
            .arguments
            .get("text")
            .and_then(Value::as_str)
            .ok_or_else(|| ToolModuleError::Contract("word_count text is required".to_owned()))?;
        Ok(ModuleExecution {
            result: ToolResult {
                call_id: call.id,
                name: call.name,
                value: json!({"words":text.split_whitespace().count()}),
                is_error: false,
                failure: None,
            },
            receipts: Vec::new(),
            artifacts: Vec::new(),
        })
    }
}

#[tokio::test]
async fn trusted_extension_is_profile_and_skill_scoped() -> Result<(), Box<dyn std::error::Error>> {
    let temp = TempDir::new()?;
    let word_count = call("word_count", json!({"text":"one two three"}))?;
    let model = Arc::new(SequenceModel::new(vec![
        turn("", vec![word_count]),
        turn("Three words.", Vec::new()),
    ]));
    let runtime = runtime(
        &temp.path().join("state"),
        model,
        vec![Arc::new(WordCountModule {
            activation_skill: SkillId::parse("creator/document")?,
        })],
    )?;
    let mut request = task(ProfileId::creator(), "Count the words")?;
    request.skills.push(SkillId::parse("creator/document")?);
    let result = runtime.run(request).await;
    assert_eq!(result.status, AgentStatus::Succeeded);
    assert_eq!(result.metrics.get("toolCalls"), Some(&json!(1)));
    Ok(())
}

#[derive(Debug)]
struct PendingReadOnlyModule {
    activation_skill: SkillId,
    started: Arc<tokio::sync::Notify>,
    stopped: Arc<tokio::sync::Notify>,
}

#[async_trait]
impl ToolModule for PendingReadOnlyModule {
    fn tools(&self) -> Vec<ModuleTool> {
        vec![ModuleTool {
            definition: ToolDefinition {
                name: "pending_read".to_owned(),
                description: "Wait until the task is cancelled.".to_owned(),
                input_schema: json!({
                    "type":"object",
                    "properties":{},
                    "additionalProperties":false
                }),
                strict: true,
            },
            side_effect: ToolSideEffect::ReadOnly,
            allowed_profiles: BTreeSet::from([ProfileId::creator()]),
            required_capabilities: BTreeSet::<CapabilityId>::new(),
            activation_skills: BTreeSet::from([self.activation_skill.clone()]),
        }]
    }

    async fn execute(
        &self,
        context: ToolContext,
        _call: ToolCall,
    ) -> Result<ModuleExecution, ToolModuleError> {
        self.started.notify_one();
        context.cancellation.cancelled().await;
        self.stopped.notify_one();
        Err(ToolModuleError::Cancelled(agent_core::AgentFailure::new(
            FailureCode::Cancelled,
            "pending read stopped and cleaned up",
        )))
    }
}

#[tokio::test]
async fn explicit_task_cancellation_stops_a_read_only_extension()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = TempDir::new()?;
    let pending_call = call("pending_read", json!({}))?;
    let model = Arc::new(SequenceModel::new(vec![turn("", vec![pending_call])]));
    let started = Arc::new(tokio::sync::Notify::new());
    let stopped = Arc::new(tokio::sync::Notify::new());
    let runtime = runtime(
        &temp.path().join("state"),
        model,
        vec![Arc::new(PendingReadOnlyModule {
            activation_skill: SkillId::parse("creator/document")?,
            started: Arc::clone(&started),
            stopped: Arc::clone(&stopped),
        })],
    )?;
    let mut request = task(ProfileId::creator(), "Wait in the extension")?;
    request.skills.push(SkillId::parse("creator/document")?);
    let cancellation = TaskCancellation::new();
    let execution = {
        let runtime = runtime.clone();
        let cancellation = cancellation.clone();
        tokio::spawn(async move { runtime.run_with_cancellation(request, cancellation).await })
    };

    tokio::time::timeout(Duration::from_secs(2), started.notified()).await?;
    cancellation.cancel();
    let result = tokio::time::timeout(Duration::from_secs(2), execution).await??;
    tokio::time::timeout(Duration::from_secs(2), stopped.notified()).await?;

    assert_eq!(result.status, AgentStatus::Cancelled);
    assert_eq!(
        result.failure.as_ref().map(|failure| failure.code),
        Some(FailureCode::Cancelled)
    );
    Ok(())
}

#[derive(Debug)]
struct PendingExternalModule {
    activation_skill: SkillId,
    started: Arc<tokio::sync::Notify>,
    stopped: Arc<tokio::sync::Notify>,
}

#[async_trait]
impl ToolModule for PendingExternalModule {
    fn tools(&self) -> Vec<ModuleTool> {
        vec![ModuleTool {
            definition: ToolDefinition {
                name: "pending_external".to_owned(),
                description: "Represent an external operation interrupted by cancellation."
                    .to_owned(),
                input_schema: json!({
                    "type":"object",
                    "properties":{},
                    "additionalProperties":false
                }),
                strict: true,
            },
            side_effect: ToolSideEffect::External,
            allowed_profiles: BTreeSet::from([ProfileId::creator()]),
            required_capabilities: BTreeSet::<CapabilityId>::new(),
            activation_skills: BTreeSet::from([self.activation_skill.clone()]),
        }]
    }

    async fn execute(
        &self,
        context: ToolContext,
        _call: ToolCall,
    ) -> Result<ModuleExecution, ToolModuleError> {
        self.started.notify_one();
        context.cancellation.cancelled().await;
        self.stopped.notify_one();
        Err(ToolModuleError::Cancelled(agent_core::AgentFailure::new(
            FailureCode::Cancelled,
            "pending external operation stopped and cleaned up",
        )))
    }
}

#[tokio::test]
async fn cancelling_an_external_extension_requires_explicit_resolution()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = TempDir::new()?;
    let pending_call = call("pending_external", json!({}))?;
    let model = Arc::new(SequenceModel::new(vec![turn("", vec![pending_call])]));
    let started = Arc::new(tokio::sync::Notify::new());
    let stopped = Arc::new(tokio::sync::Notify::new());
    let runtime = runtime(
        &temp.path().join("state"),
        model,
        vec![Arc::new(PendingExternalModule {
            activation_skill: SkillId::parse("creator/document")?,
            started: Arc::clone(&started),
            stopped: Arc::clone(&stopped),
        })],
    )?;
    let mut request = task(ProfileId::creator(), "Wait in the external extension")?;
    request.skills.push(SkillId::parse("creator/document")?);
    request.effect_approval = EffectApprovalMode::TaskGrant;
    let cancellation = TaskCancellation::new();
    let execution = {
        let runtime = runtime.clone();
        let cancellation = cancellation.clone();
        tokio::spawn(async move { runtime.run_with_cancellation(request, cancellation).await })
    };

    tokio::time::timeout(Duration::from_secs(2), started.notified()).await?;
    cancellation.cancel();
    let result = tokio::time::timeout(Duration::from_secs(2), execution).await??;
    tokio::time::timeout(Duration::from_secs(2), stopped.notified()).await?;

    assert_eq!(result.status, AgentStatus::ResolutionRequired);
    assert_eq!(
        result.failure.as_ref().map(|failure| failure.code),
        Some(FailureCode::ResolutionRequired)
    );
    Ok(())
}

#[derive(Debug)]
struct LargeResultModule {
    activation_skill: SkillId,
}

#[async_trait]
impl ToolModule for LargeResultModule {
    fn tools(&self) -> Vec<ModuleTool> {
        vec![ModuleTool {
            definition: ToolDefinition {
                name: "large_result".to_owned(),
                description: "Return a bounded but provider-oversized JSON result.".to_owned(),
                input_schema: json!({
                    "type":"object",
                    "properties":{},
                    "additionalProperties":false
                }),
                strict: true,
            },
            side_effect: ToolSideEffect::ReadOnly,
            allowed_profiles: BTreeSet::from([ProfileId::creator()]),
            required_capabilities: BTreeSet::<CapabilityId>::new(),
            activation_skills: BTreeSet::from([self.activation_skill.clone()]),
        }]
    }

    async fn execute(
        &self,
        _context: ToolContext,
        call: ToolCall,
    ) -> Result<ModuleExecution, ToolModuleError> {
        Ok(ModuleExecution {
            result: ToolResult {
                call_id: call.id,
                name: call.name,
                value: json!({"content":"x".repeat(256 * 1024)}),
                is_error: false,
                failure: None,
            },
            receipts: Vec::new(),
            artifacts: Vec::new(),
        })
    }
}

#[tokio::test]
async fn oversized_tool_results_are_artifactized_before_the_next_model_turn()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = TempDir::new()?;
    let large = call("large_result", json!({}))?;
    let model = Arc::new(SequenceModel::new(vec![
        turn("", vec![large]),
        turn("Used the bounded artifact projection.", Vec::new()),
    ]));
    let runtime = runtime(
        &temp.path().join("state"),
        model.clone(),
        vec![Arc::new(LargeResultModule {
            activation_skill: SkillId::parse("creator/document")?,
        })],
    )?;
    let mut request = task(ProfileId::creator(), "Process the large result")?;
    request.skills.push(SkillId::parse("creator/document")?);
    let result = runtime.run(request).await;
    assert_eq!(result.status, AgentStatus::Succeeded);
    assert_eq!(result.artifacts.len(), 6);
    assert_eq!(
        result
            .artifacts
            .iter()
            .filter(|artifact| {
                artifact.media_type == "application/vnd.upagent.model-turn+json; charset=utf-8"
            })
            .count(),
        2
    );
    assert_eq!(
        result
            .artifacts
            .iter()
            .filter(|artifact| {
                artifact.media_type == "application/vnd.upagent.model-result+json; charset=utf-8"
            })
            .count(),
        2
    );
    assert!(
        result
            .artifacts
            .iter()
            .any(|artifact| artifact.media_type == "application/json")
    );
    assert!(
        result
            .artifacts
            .iter()
            .any(|artifact| artifact.media_type == "text/markdown; charset=utf-8")
    );

    let turns = model.recorded_turns()?;
    assert_eq!(turns.len(), 2);
    let tool_message = turns[1]
        .messages
        .iter()
        .find(|message| message.role == MessageRole::Tool)
        .ok_or_else(|| std::io::Error::other("second turn has no tool result"))?;
    let projection: Value = serde_json::from_str(&tool_message.content)?;
    assert_eq!(projection.get("oversized"), Some(&json!(true)));
    assert!(projection.get("artifact").is_some());
    assert!(tool_message.content.len() < 32 * 1024);
    Ok(())
}

#[tokio::test]
async fn task_inspection_rejects_a_removed_tool_artifact_field()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = TempDir::new()?;
    let large = call("large_result", json!({}))?;
    let model = Arc::new(SequenceModel::new(vec![
        turn("", vec![large]),
        turn("Done", Vec::new()),
    ]));
    let state = temp.path().join("state");
    let runtime = runtime(
        &state,
        model,
        vec![Arc::new(LargeResultModule {
            activation_skill: SkillId::parse("creator/document")?,
        })],
    )?;
    let mut request = task(ProfileId::creator(), "Inspect removed tool evidence")?;
    request.skills.push(SkillId::parse("creator/document")?);
    assert_eq!(
        runtime.run(request.clone()).await.status,
        AgentStatus::Succeeded
    );

    let task_directory = state
        .join("sessions")
        .join(request.session_id.as_str())
        .join(request.task_id.as_str());
    let events_path = task_directory.join("events.ndjson");
    let mut changed = false;
    let mut rewritten = String::new();
    for line in std::fs::read_to_string(&events_path)?.lines() {
        let mut record: Value = serde_json::from_str(line)?;
        if record
            .get("event")
            .and_then(|value| value.get("kind"))
            .and_then(Value::as_str)
            == Some("tool_completed")
        {
            record
                .get_mut("payload")
                .and_then(Value::as_object_mut)
                .ok_or("tool completion payload is not an object")?
                .remove("artifacts");
            changed = true;
        }
        rewritten.push_str(&serde_json::to_string(&record)?);
        rewritten.push('\n');
    }
    assert!(changed, "the fixture must contain a tool completion event");
    std::fs::write(&events_path, rewritten)?;
    assert!(
        inspect_task(
            &state,
            request.session_id.as_str(),
            request.task_id.as_str()
        )
        .is_err()
    );
    Ok(())
}

#[tokio::test]
async fn provider_tool_call_identifiers_cannot_be_reused_across_turns()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = TempDir::new()?;
    let duplicate_id = ToolCallId::parse("call-duplicate")?;
    let first = ToolCall {
        id: duplicate_id.clone(),
        name: "word_count".to_owned(),
        arguments: json!({"text":"one"}),
    };
    let second = ToolCall {
        id: duplicate_id,
        name: "word_count".to_owned(),
        arguments: json!({"text":"two"}),
    };
    let model = Arc::new(SequenceModel::new(vec![
        turn("", vec![first]),
        turn("", vec![second]),
    ]));
    let runtime = runtime(
        &temp.path().join("state"),
        model,
        vec![Arc::new(WordCountModule {
            activation_skill: SkillId::parse("creator/document")?,
        })],
    )?;
    let mut request = task(ProfileId::creator(), "Count twice")?;
    request.skills.push(SkillId::parse("creator/document")?);
    let result = runtime.run(request).await;
    assert_eq!(result.status, AgentStatus::Failed);
    assert_eq!(
        result.failure.as_ref().map(|failure| failure.code),
        Some(FailureCode::ProviderFailed)
    );
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn canonical_workspace_paths_must_remain_lossless_utf8()
-> Result<(), Box<dyn std::error::Error>> {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;
    use std::os::unix::fs::symlink;

    let temp = TempDir::new()?;
    let workspace = temp
        .path()
        .join(OsString::from_vec(b"workspace-\xff".to_vec()));
    if let Err(error) = std::fs::create_dir_all(&workspace) {
        // macOS filesystems reject non-UTF-8 path components before the
        // runtime can validate and preserve the canonical path.
        if cfg!(target_os = "macos") && matches!(error.raw_os_error(), Some(1 | 92)) {
            return Ok(());
        }
        return Err(error.into());
    }
    let link = temp.path().join("workspace-link");
    symlink(&workspace, &link)?;
    let model = Arc::new(SequenceModel::new(vec![turn("unused", Vec::new())]));
    let runtime = runtime(&temp.path().join("state"), model, Vec::new())?;
    let mut request = task(ProfileId::developer(), "Inspect the repository")?;
    request.track = Some(TrackId::builder());
    request.workspace = Some(link.to_string_lossy().into_owned());

    let result = runtime.run(request).await;

    assert_eq!(result.status, AgentStatus::Failed);
    assert_eq!(
        result.failure.as_ref().map(|failure| failure.code),
        Some(FailureCode::InvalidRequest)
    );
    Ok(())
}

#[tokio::test]
async fn runtime_state_and_effect_workspace_must_not_overlap()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = TempDir::new()?;
    let state = temp.path().join("state");
    std::fs::create_dir_all(&state)?;
    let workspace = state.join("workspace");
    std::fs::create_dir_all(&workspace)?;
    let model = Arc::new(SequenceModel::new(vec![turn("unused", Vec::new())]));
    let runtime = runtime(&state, model, Vec::new())?;
    let mut request = task(ProfileId::developer(), "Inspect the repository")?;
    request.track = Some(TrackId::builder());
    request.workspace = Some(workspace.to_string_lossy().into_owned());
    let result = runtime.run(request).await;
    assert_eq!(result.status, AgentStatus::Failed);
    assert_eq!(
        result.failure.as_ref().map(|failure| failure.code),
        Some(FailureCode::InvalidRequest)
    );
    Ok(())
}

/// Build a task-grant Developer task bound to `workspace`.
fn granted_developer_task(
    workspace: &Path,
    objective: &str,
) -> Result<TaskEnvelope, Box<dyn std::error::Error>> {
    let mut request = task(ProfileId::developer(), objective)?;
    request.track = Some(TrackId::builder());
    request.workspace = Some(workspace.to_string_lossy().into_owned());
    request.effect_approval = EffectApprovalMode::TaskGrant;
    Ok(request)
}

#[tokio::test]
async fn one_shared_workspace_admits_one_mutating_task_at_a_time()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = TempDir::new()?;
    let workspace = temp.path().join("workspace");
    std::fs::create_dir_all(&workspace)?;
    let probe = Arc::new(OverlapProbe::new(Duration::from_millis(250)));
    let runtime = runtime(&temp.path().join("state"), probe.clone(), Vec::new())?;

    let results = runtime
        .run_many(vec![
            granted_developer_task(&workspace, "First mutation")?,
            granted_developer_task(&workspace, "Second mutation")?,
        ])
        .await;

    assert!(
        results
            .iter()
            .all(|result| result.status == AgentStatus::Succeeded),
        "both tasks must complete: {results:?}"
    );
    assert_eq!(
        probe.peak(),
        1,
        "the workspace writer lease must serialize task-grant tasks on one workspace"
    );
    Ok(())
}

#[tokio::test]
async fn exact_mutation_grants_share_the_workspace_writer_lease()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = TempDir::new()?;
    let state = temp.path().join("state");
    let workspace = temp.path().join("workspace");
    std::fs::create_dir_all(&workspace)?;

    let first_request = {
        let mut request = task(ProfileId::developer(), "first exact grant")?;
        request.track = Some(TrackId::prototyper());
        request.workspace = Some(workspace.to_string_lossy().into_owned());
        request.effect_approval = EffectApprovalMode::PlanOnly;
        request
    };
    let second_request = {
        let mut request = task(ProfileId::developer(), "second exact grant")?;
        request.track = Some(TrackId::prototyper());
        request.workspace = Some(workspace.to_string_lossy().into_owned());
        request.effect_approval = EffectApprovalMode::PlanOnly;
        request
    };
    let planning = runtime(
        &state,
        Arc::new(MutatingOverlapProbe::new(Duration::from_millis(1))),
        Vec::new(),
    )?;
    let first_pending = planning.run(first_request.clone()).await;
    let second_pending = planning.run(second_request.clone()).await;
    let first_subject = first_pending
        .approval
        .as_ref()
        .ok_or("first approval")?
        .subject
        .clone();
    let second_subject = second_pending
        .approval
        .as_ref()
        .ok_or("second approval")?
        .subject
        .clone();
    let store = ApprovalStore::open(state.join("approvals"))?;
    let first_grant = store.approve(&first_subject)?;
    let second_grant = store.approve(&second_subject)?;

    let probe = Arc::new(MutatingOverlapProbe::new(Duration::from_millis(150)));
    let executing = runtime(&state, probe.clone(), Vec::new())?;
    let mut first = first_request;
    first.task_id = TaskId::parse(format!("task-{}", uuid::Uuid::now_v7()))?;
    first.approvals = vec![ApprovalGrant {
        subject: first_grant.subject,
        token: first_grant.token,
    }];
    let mut second = second_request;
    second.task_id = TaskId::parse(format!("task-{}", uuid::Uuid::now_v7()))?;
    second.approvals = vec![ApprovalGrant {
        subject: second_grant.subject,
        token: second_grant.token,
    }];
    let results = executing.run_many(vec![first, second]).await;

    assert!(
        results
            .iter()
            .all(|result| result.status == AgentStatus::Succeeded),
        "{results:?}"
    );
    assert_eq!(
        probe.peak(),
        1,
        "exact mutation grants must serialize one workspace"
    );
    assert_eq!(
        std::fs::read_to_string(workspace.join("first.txt"))?,
        "approved"
    );
    assert_eq!(
        std::fs::read_to_string(workspace.join("second.txt"))?,
        "approved"
    );
    Ok(())
}

#[test]
fn public_effect_recovery_helpers_reject_overlapping_roots_before_state_creation()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = TempDir::new()?;
    let workspace = temp.path().join("workspace");
    std::fs::create_dir_all(&workspace)?;
    let nested_state = workspace.join("state");
    let listed = list_effect_recoveries(&workspace, &nested_state)
        .expect_err("state below workspace must be rejected");
    assert!(listed.to_string().contains("overlap"));
    assert!(!nested_state.exists());

    let outer_state = temp.path().join("outer-state");
    let nested_workspace = outer_state.join("workspace");
    std::fs::create_dir_all(&nested_workspace)?;
    let resolved = resolve_effect_recovery(
        &nested_workspace,
        &outer_state,
        "run-overlap",
        EffectRecoveryResolution::ManualReviewRequired,
        None,
    )
    .expect_err("state above workspace must be rejected");
    assert!(resolved.to_string().contains("overlap"));
    assert!(!outer_state.join("effects").exists());
    Ok(())
}

#[tokio::test]
async fn the_writer_lease_is_scoped_to_one_workspace() -> Result<(), Box<dyn std::error::Error>> {
    let temp = TempDir::new()?;
    let first = temp.path().join("first");
    let second = temp.path().join("second");
    std::fs::create_dir_all(&first)?;
    std::fs::create_dir_all(&second)?;
    let probe = Arc::new(OverlapProbe::new(Duration::from_millis(250)));
    let runtime = runtime(&temp.path().join("state"), probe.clone(), Vec::new())?;

    let results = runtime
        .run_many(vec![
            granted_developer_task(&first, "Mutate the first workspace")?,
            granted_developer_task(&second, "Mutate the second workspace")?,
        ])
        .await;

    assert!(
        results
            .iter()
            .all(|result| result.status == AgentStatus::Succeeded),
        "both tasks must complete: {results:?}"
    );
    assert_eq!(
        probe.peak(),
        2,
        "distinct workspaces must not contend for one another's writer lease"
    );
    Ok(())
}

#[tokio::test]
async fn a_distinct_workspace_path_spelling_resolves_to_the_same_lease()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = TempDir::new()?;
    let workspace = temp.path().join("workspace");
    std::fs::create_dir_all(workspace.join("nested"))?;
    let probe = Arc::new(OverlapProbe::new(Duration::from_millis(250)));
    let runtime = runtime(&temp.path().join("state"), probe.clone(), Vec::new())?;

    // The lease is keyed on the canonical path, so an equivalent spelling of the
    // same directory must not hand out a second mutation authority.
    let results = runtime
        .run_many(vec![
            granted_developer_task(&workspace, "Canonical spelling")?,
            granted_developer_task(&workspace.join("nested/.."), "Equivalent spelling")?,
        ])
        .await;

    assert!(
        results
            .iter()
            .all(|result| result.status == AgentStatus::Succeeded),
        "both tasks must complete: {results:?}"
    );
    assert_eq!(
        probe.peak(),
        1,
        "an equivalent path spelling must resolve to the same writer lease"
    );
    Ok(())
}

#[tokio::test]
async fn batch_results_are_returned_in_request_order() -> Result<(), Box<dyn std::error::Error>> {
    let temp = TempDir::new()?;
    // Distinct hold times make the completion order differ from the input order.
    let probe = Arc::new(OverlapProbe::new(Duration::from_millis(50)));
    let runtime = runtime(&temp.path().join("state"), probe, Vec::new())?;

    let requests = (0..4)
        .map(|index| task(ProfileId::creator(), &format!("Explain topic {index}")))
        .collect::<Result<Vec<_>, _>>()?;
    let expected = requests
        .iter()
        .map(|request| request.task_id.clone())
        .collect::<Vec<_>>();

    let results = runtime.run_many(requests).await;

    assert_eq!(results.len(), expected.len());
    assert_eq!(
        results
            .iter()
            .map(|result| result.task_id.clone())
            .collect::<Vec<_>>(),
        expected,
        "batch results must be collected back into request order"
    );
    assert!(
        results
            .iter()
            .all(|result| result.status == AgentStatus::Succeeded),
        "every batch task must succeed: {:?}",
        results
            .iter()
            .map(|result| (result.status, result.failure.clone()))
            .collect::<Vec<_>>()
    );
    Ok(())
}

#[tokio::test]
async fn batch_cancellation_reaches_every_pending_task() -> Result<(), Box<dyn std::error::Error>> {
    let temp = TempDir::new()?;
    let runtime = runtime(
        &temp.path().join("state"),
        Arc::new(PendingModel::new()),
        Vec::new(),
    )?;
    let cancellation = TaskCancellation::new();

    let requests = (0..3)
        .map(|index| task(ProfileId::creator(), &format!("Explain topic {index}")))
        .collect::<Result<Vec<_>, _>>()?;
    let execution = runtime.run_many_with_cancellation(requests, cancellation.clone());
    tokio::pin!(execution);

    let results = tokio::select! {
        results = &mut execution => results,
        () = tokio::time::sleep(Duration::from_millis(100)) => {
            cancellation.cancel();
            execution.await
        }
    };

    assert_eq!(results.len(), 3);
    for result in &results {
        assert_eq!(result.status, AgentStatus::Cancelled);
        assert_eq!(
            result.failure.as_ref().map(|failure| failure.code),
            Some(FailureCode::Cancelled)
        );
    }
    Ok(())
}

#[tokio::test]
async fn a_plan_only_effect_becomes_executable_only_after_an_operator_decision()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = TempDir::new()?;
    let state = temp.path().join("state");
    let workspace = temp.path().join("workspace");
    std::fs::create_dir_all(&workspace)?;
    let write = || {
        call(
            "write_file",
            json!({
                "path":"created.txt",
                "content":"after",
                "createOnly":true,
                "expectedSha256":Value::Null
            }),
        )
    };
    let plan_only_model = Arc::new(SequenceModel::new(vec![turn("", vec![write()?])]));
    let planning = runtime(&state, plan_only_model, Vec::new())?;

    let mut request = task(ProfileId::developer(), "Record the decision")?;
    request.track = Some(TrackId::prototyper());
    request.workspace = Some(workspace.to_string_lossy().into_owned());
    request.effect_approval = EffectApprovalMode::PlanOnly;
    let pending = planning.run(request.clone()).await;

    assert_eq!(pending.status, AgentStatus::AwaitingApproval);
    let pending_inspection = inspect_task(
        &state,
        request.session_id.as_str(),
        request.task_id.as_str(),
    )?;
    let persisted = pending_inspection
        .result
        .as_ref()
        .ok_or("awaiting-approval result missing")?;
    assert_eq!(persisted.state, TaskState::AwaitingApproval);
    assert_eq!(persisted.status, AgentStatus::AwaitingApproval);
    let approval = pending.approval.as_ref().expect("an approval request");
    assert!(approval.effects.contains(&"write_workspace".to_owned()));
    assert!(
        !workspace.join("created.txt").exists(),
        "a plan-only run must not mutate the workspace"
    );

    // The decision is recorded, so an operator has something to act on.
    let store = ApprovalStore::open(state.join("approvals"))?;
    let recorded = store.list()?;
    assert_eq!(recorded.len(), 1);
    assert_eq!(recorded[0].subject, approval.subject);
    assert_eq!(recorded[0].status, ApprovalStatus::Pending);

    let grant = store.approve(&approval.subject)?;

    // A later run that requests the same operation redeems the decision.
    let approved_model = Arc::new(SequenceModel::new(vec![
        turn("", vec![write()?]),
        turn("Applied the approved write.", Vec::new()),
    ]));
    let redeemer = runtime(&state, approved_model, Vec::new())?;
    let mut redeeming = request.clone();
    redeeming.task_id = TaskId::parse(format!("task-{}", uuid::Uuid::now_v7()))?;
    redeeming.approvals = vec![ApprovalGrant {
        subject: grant.subject.clone(),
        token: grant.token.clone(),
    }];
    let applied = redeemer.run(redeeming.clone()).await;

    assert_eq!(applied.status, AgentStatus::Succeeded, "{applied:?}");
    assert_eq!(
        std::fs::read_to_string(workspace.join("created.txt"))?,
        "after"
    );

    // The same token cannot authorise a second run.
    let replay_model = Arc::new(SequenceModel::new(vec![turn("", vec![write()?])]));
    let replayer = runtime(&state, replay_model, Vec::new())?;
    let mut replay = redeeming;
    replay.task_id = TaskId::parse(format!("task-{}", uuid::Uuid::now_v7()))?;
    let refused = replayer.run(replay).await;

    assert_eq!(refused.status, AgentStatus::Failed);
    assert_eq!(
        refused.failure.as_ref().map(|failure| failure.code),
        Some(FailureCode::CapabilityDenied)
    );
    Ok(())
}

#[tokio::test]
async fn deny_dominates_an_exact_grant_without_consuming_or_mutating()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = TempDir::new()?;
    let state = temp.path().join("state");
    let workspace = temp.path().join("workspace");
    std::fs::create_dir_all(&workspace)?;
    let write = || {
        call(
            "write_file",
            json!({
                "path":"denied.txt",
                "content":"approved later",
                "createOnly":true,
                "expectedSha256":Value::Null
            }),
        )
    };
    let mut request = task(ProfileId::developer(), "Deny dominates")?;
    request.track = Some(TrackId::prototyper());
    request.workspace = Some(workspace.to_string_lossy().into_owned());
    request.effect_approval = EffectApprovalMode::PlanOnly;
    let planning = runtime(
        &state,
        Arc::new(SequenceModel::new(vec![turn("", vec![write()?])])),
        Vec::new(),
    )?;
    let pending = planning.run(request.clone()).await;
    let subject = pending
        .approval
        .as_ref()
        .ok_or("missing approval")?
        .subject
        .clone();
    let store = ApprovalStore::open(state.join("approvals"))?;
    let grant = store.approve(&subject)?;

    let denied_runtime = runtime(
        &state,
        Arc::new(SequenceModel::new(vec![turn("", vec![write()?])])),
        Vec::new(),
    )?;
    let mut denied = request.clone();
    denied.task_id = TaskId::parse(format!("task-{}", uuid::Uuid::now_v7()))?;
    denied.effect_approval = EffectApprovalMode::Deny;
    denied.approvals = vec![ApprovalGrant {
        subject: grant.subject.clone(),
        token: grant.token.clone(),
    }];
    let refused = denied_runtime.run(denied).await;
    assert_eq!(refused.status, AgentStatus::Failed);
    assert_eq!(
        refused.failure.as_ref().map(|failure| failure.code),
        Some(FailureCode::CapabilityDenied)
    );
    assert!(!workspace.join("denied.txt").exists());
    assert_eq!(store.list()?[0].status, ApprovalStatus::Approved);

    let approved_runtime = runtime(
        &state,
        Arc::new(SequenceModel::new(vec![
            turn("", vec![write()?]),
            turn("Applied after the explicit grant.", Vec::new()),
        ])),
        Vec::new(),
    )?;
    let mut approved = request;
    approved.task_id = TaskId::parse(format!("task-{}", uuid::Uuid::now_v7()))?;
    approved.effect_approval = EffectApprovalMode::TaskGrant;
    approved.approvals = vec![ApprovalGrant {
        subject: grant.subject,
        token: grant.token,
    }];
    let applied = approved_runtime.run(approved).await;
    assert_eq!(applied.status, AgentStatus::Succeeded, "{applied:?}");
    assert_eq!(
        std::fs::read_to_string(workspace.join("denied.txt"))?,
        "approved later"
    );
    Ok(())
}

#[tokio::test]
async fn an_approval_does_not_carry_to_a_different_operation()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = TempDir::new()?;
    let state = temp.path().join("state");
    let workspace = temp.path().join("workspace");
    std::fs::create_dir_all(&workspace)?;
    let approved_write = call(
        "write_file",
        json!({
            "path":"created.txt",
            "content":"approved",
            "createOnly":true,
            "expectedSha256":Value::Null
        }),
    )?;
    let planning = runtime(
        &state,
        Arc::new(SequenceModel::new(vec![turn("", vec![approved_write])])),
        Vec::new(),
    )?;
    let mut request = task(ProfileId::developer(), "Record the decision")?;
    request.track = Some(TrackId::builder());
    request.workspace = Some(workspace.to_string_lossy().into_owned());
    request.effect_approval = EffectApprovalMode::PlanOnly;
    let pending = planning.run(request.clone()).await;
    let subject = pending
        .approval
        .as_ref()
        .expect("an approval request")
        .subject
        .clone();

    let store = ApprovalStore::open(state.join("approvals"))?;
    let grant = store.approve(&subject)?;

    // The model now asks for a different write. The grant is bound to the
    // operation the operator saw, so it must not authorise this one.
    let other_write = call(
        "write_file",
        json!({
            "path":"created.txt",
            "content":"something else entirely",
            "createOnly":true,
            "expectedSha256":Value::Null
        }),
    )?;
    let redeemer = runtime(
        &state,
        Arc::new(SequenceModel::new(vec![turn("", vec![other_write])])),
        Vec::new(),
    )?;
    let mut redeeming = request;
    redeeming.task_id = TaskId::parse(format!("task-{}", uuid::Uuid::now_v7()))?;
    redeeming.approvals = vec![ApprovalGrant {
        subject: grant.subject,
        token: grant.token,
    }];
    let result = redeemer.run(redeeming).await;

    assert_eq!(result.status, AgentStatus::AwaitingApproval);
    assert_ne!(
        result.approval.as_ref().expect("approval").subject,
        subject,
        "a different operation must produce a different decision subject"
    );
    assert!(
        !workspace.join("created.txt").exists(),
        "an unapproved operation must not mutate the workspace"
    );
    Ok(())
}

/// A side-effecting extension that records how often it actually ran.
#[derive(Debug)]
struct ExternalCounterModule {
    activation_skill: SkillId,
    runs: Arc<AtomicUsize>,
}

#[async_trait]
impl ToolModule for ExternalCounterModule {
    fn tools(&self) -> Vec<ModuleTool> {
        vec![ModuleTool {
            definition: ToolDefinition {
                name: "publish_note".to_owned(),
                description: "Publish a note to an external service.".to_owned(),
                input_schema: json!({
                    "type":"object",
                    "properties":{"text":{"type":"string"}},
                    "required":["text"],
                    "additionalProperties":false
                }),
                strict: true,
            },
            side_effect: ToolSideEffect::External,
            allowed_profiles: BTreeSet::from([ProfileId::creator()]),
            required_capabilities: BTreeSet::<CapabilityId>::new(),
            activation_skills: BTreeSet::from([self.activation_skill.clone()]),
        }]
    }

    async fn execute(
        &self,
        _context: ToolContext,
        call: ToolCall,
    ) -> Result<ModuleExecution, ToolModuleError> {
        self.runs.fetch_add(1, Ordering::SeqCst);
        Ok(ModuleExecution {
            result: ToolResult {
                call_id: call.id,
                name: call.name,
                value: json!({"published":true}),
                is_error: false,
                failure: None,
            },
            receipts: vec![agent_core::EffectReceiptRef {
                id: agent_core::ReceiptId::parse("receipt-publish")
                    .map_err(|error| ToolModuleError::Contract(error.to_string()))?,
                digest: "a".repeat(64),
                operation: "publish_note".to_owned(),
                verified: true,
            }],
            artifacts: Vec::new(),
        })
    }
}

#[tokio::test]
async fn a_side_effecting_extension_runs_only_after_its_decision_is_redeemed()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = TempDir::new()?;
    let state = temp.path().join("state");
    let skill = SkillId::parse("creator/document")?;
    let runs = Arc::new(AtomicUsize::new(0));

    let publish = || call("publish_note", json!({"text":"hello"}));
    let module = |runs: &Arc<AtomicUsize>| {
        Arc::new(ExternalCounterModule {
            activation_skill: skill.clone(),
            runs: Arc::clone(runs),
        }) as Arc<dyn ToolModule>
    };

    let planning = runtime(
        &state,
        Arc::new(SequenceModel::new(vec![turn("", vec![publish()?])])),
        vec![module(&runs)],
    )?;
    let mut request = task(ProfileId::creator(), "Publish the note")?;
    request.skills.push(skill.clone());
    request.effect_approval = EffectApprovalMode::PlanOnly;
    let pending = planning.run(request.clone()).await;

    assert_eq!(pending.status, AgentStatus::AwaitingApproval);
    assert_eq!(
        runs.load(Ordering::SeqCst),
        0,
        "a plan-only run must not reach the external service"
    );
    let subject = pending
        .approval
        .as_ref()
        .expect("an approval request")
        .subject
        .clone();

    let store = ApprovalStore::open(state.join("approvals"))?;
    assert_eq!(store.list()?.len(), 1);
    let grant = store.approve(&subject)?;

    let denied_runtime = runtime(
        &state,
        Arc::new(SequenceModel::new(vec![turn("", vec![publish()?])])),
        vec![module(&runs)],
    )?;
    let mut denied = request.clone();
    denied.task_id = TaskId::parse(format!("task-{}", uuid::Uuid::now_v7()))?;
    denied.effect_approval = EffectApprovalMode::Deny;
    denied.approvals = vec![ApprovalGrant {
        subject: grant.subject.clone(),
        token: grant.token.clone(),
    }];
    let refused = denied_runtime.run(denied).await;
    assert_eq!(refused.status, AgentStatus::Failed);
    assert_eq!(
        refused.failure.as_ref().map(|failure| failure.code),
        Some(FailureCode::CapabilityDenied)
    );
    assert_eq!(runs.load(Ordering::SeqCst), 0);
    assert_eq!(store.list()?[0].status, ApprovalStatus::Approved);

    let denied_gate_runtime = runtime(
        &state,
        Arc::new(SequenceModel::new(vec![
            turn("", vec![publish()?]),
            turn("The external call was denied.", Vec::new()),
        ])),
        vec![module(&runs)],
    )?;
    let mut gate_denied = request.clone();
    gate_denied.task_id = TaskId::parse(format!("task-{}", uuid::Uuid::now_v7()))?;
    gate_denied.effect_approval = EffectApprovalMode::Deny;
    let gate_result = denied_gate_runtime.run(gate_denied).await;
    assert_eq!(
        gate_result.status,
        AgentStatus::Succeeded,
        "{gate_result:?}"
    );
    assert_eq!(runs.load(Ordering::SeqCst), 0);

    let redeemer = runtime(
        &state,
        Arc::new(SequenceModel::new(vec![
            turn("", vec![publish()?]),
            turn("Published the approved note.", Vec::new()),
        ])),
        vec![module(&runs)],
    )?;
    let mut redeeming = request;
    redeeming.task_id = TaskId::parse(format!("task-{}", uuid::Uuid::now_v7()))?;
    redeeming.approvals = vec![ApprovalGrant {
        subject: grant.subject,
        token: grant.token,
    }];
    let applied = redeemer.run(redeeming).await;

    assert_eq!(applied.status, AgentStatus::Succeeded, "{applied:?}");
    assert_eq!(runs.load(Ordering::SeqCst), 1);
    Ok(())
}

#[test]
fn explicit_memory_boundary_does_not_open_unselected_store()
-> Result<(), Box<dyn std::error::Error>> {
    let state = TempDir::new()?;
    let skill_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("skills");
    let skills = SkillCatalog::discover(vec![skill_root.clone()])?;
    let options = AgentRuntimeOptions {
        state_root: state.path().to_path_buf(),
        skill_roots: vec![skill_root],
        profiles: agent_profiles::built_in_registry()?,
        model: Arc::new(SequenceModel::new(Vec::new())),
        maximum_parallel_tasks: 1,
        tool_modules: Vec::new(),
    };
    let _runtime = AgentRuntime::open_with_plugins_and_catalog_and_memory(
        options,
        Vec::new(),
        Some(skills),
        false,
    )?;
    assert!(!state.path().join("memory").exists());
    assert!(state.path().join("artifacts").is_dir());
    assert!(state.path().join("approvals").is_dir());
    Ok(())
}
