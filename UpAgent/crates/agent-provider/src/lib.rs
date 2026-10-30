#![forbid(unsafe_code)]

//! Vergerail process transport for the active `UpAgent` provider.
//!
//! The provider crate contains no credential material. Each request is sent to
//! one explicitly configured Vergerail executable; the child is killed and
//! drained on cancellation or timeout and the outcome is never retried.

use agent_core::{
    AgentFailure, FailureCode, MessageRole, ModelContentPart, ModelMessage, ModelTurn,
    ModelTurnResult, PROVIDER_CAP_IMAGE_GENERATION, PROVIDER_CAP_IMAGE_OBSERVATION,
    PROVIDER_CAP_TEXT, ProviderBinding, ReasoningEffort, ToolCall,
};
use agent_plugin_kernel::{ContributionKind, PluginId, PluginManifest, PluginTrust};
use agent_provider_api::{
    ImageBackgroundRequirement, ImageGenerationMismatch, ImageGenerationRequirements, ImagePort,
    MAXIMUM_IMAGE_BYTES, MAXIMUM_MODEL_MESSAGE_BYTES, MAXIMUM_MODEL_MESSAGES_PER_TURN,
    MAXIMUM_MODEL_RESPONSE_BYTES, MAXIMUM_MODEL_TOOLS_PER_TURN, MAXIMUM_TOOL_ARGUMENT_BYTES,
    MAXIMUM_TOOL_ARGUMENTS_BYTES, MAXIMUM_TOOL_CALL_ID_BYTES, MAXIMUM_TOOL_CALLS_PER_TURN,
    MAXIMUM_TOOL_NAME_BYTES, ModelPort, ProviderError, ProviderImage, ProviderImageResult,
    ProviderImageStatus, ResolvedImageObservation,
};
use agent_provider_api::{ProviderPlanes, ProviderPlugin};
use async_trait::async_trait;
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::Digest as _;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt as _, AsyncWriteExt as _};
use tokio::process::Command;
use tokio::task::{JoinHandle, JoinSet};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

pub use agent_contract::limits::{
    MAXIMUM_IMAGE_FRAME_BYTES, MAXIMUM_MODEL_FRAME_BYTES, MAXIMUM_PROVIDER_REQUEST_BYTES,
    MAXIMUM_PROVIDER_TIMEOUT_MS,
};
/// The sole UpAgent↔Vergerail wire contract. Text, multimodal turns, and image
/// generation all use this envelope; capability selection belongs to the
/// provider binding, not to parallel protocol names.
pub use agent_contract::provider::PROVIDER_PROTOCOL_SCHEMA;
pub const MAXIMUM_PROVIDER_FRAME_BYTES: usize = MAXIMUM_PROVIDER_REQUEST_BYTES;
pub const MAXIMUM_IMAGE_BASE64_BYTES: usize = 4 * ((MAXIMUM_IMAGE_BYTES.saturating_add(2)) / 3);
pub const VERGERAIL_PLUGIN_ID: &str = "upagent.provider.vergerail";
const PROCESS_CLEANUP_TIMEOUT: Duration = Duration::from_millis(250);
const PROCESS_DRAIN_TIMEOUT: Duration = Duration::from_millis(250);
const STAGING_ROOT_PREFIX: &str = "vergerail-upagent-request-";
const JSON_STRING_MAX_EXPANSION: usize = 6;
const MODEL_RESPONSE_ENVELOPE_HEADROOM_BYTES: usize = 256 * 1024;
const MODEL_TOOL_CALL_WRAPPER_HEADROOM_BYTES: usize = 128;
const MODEL_WORST_CASE_ENCODED_BYTES: usize = JSON_STRING_MAX_EXPANSION
    * (MAXIMUM_MODEL_RESPONSE_BYTES + MAXIMUM_TOOL_ARGUMENTS_BYTES)
    + MAXIMUM_TOOL_CALLS_PER_TURN
        * (JSON_STRING_MAX_EXPANSION * (MAXIMUM_TOOL_CALL_ID_BYTES + MAXIMUM_TOOL_NAME_BYTES)
            + MODEL_TOOL_CALL_WRAPPER_HEADROOM_BYTES)
    + MODEL_RESPONSE_ENVELOPE_HEADROOM_BYTES;
const IMAGE_WORST_CASE_ENCODED_BYTES: usize =
    MAXIMUM_IMAGE_BASE64_BYTES + MODEL_RESPONSE_ENVELOPE_HEADROOM_BYTES;

const _: () = assert!(MAXIMUM_MODEL_FRAME_BYTES >= MODEL_WORST_CASE_ENCODED_BYTES);
const _: () = assert!(MAXIMUM_IMAGE_FRAME_BYTES >= IMAGE_WORST_CASE_ENCODED_BYTES);

type HomeLock = tokio::sync::Mutex<()>;
type HomeLockRegistry = Mutex<BTreeMap<PathBuf, Weak<HomeLock>>>;

static HOME_LOCKS: OnceLock<HomeLockRegistry> = OnceLock::new();

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct VergerailConfig {
    /// Versioned JSONL contract expected from the executable.
    pub protocol_schema: String,
    /// Explicit Vergerail executable/process entrypoint.
    pub executable: PathBuf,
    /// SHA-256 identity of the executable, bound by the composition receipt.
    pub executable_sha256: String,
    /// Explicit audited Codex runtime package root.
    pub runtime_package: PathBuf,
    /// Dedicated managed Codex home; this value is never opened by `UpAgent`.
    pub managed_home: PathBuf,
    /// Explicit model selected inside the managed home.
    pub model: String,
    /// Read-only workspace passed to Vergerail.
    pub workspace: PathBuf,
}

impl fmt::Debug for VergerailConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("VergerailConfig")
            .field("protocol_schema", &self.protocol_schema)
            .field("executable", &self.executable)
            .field("executable_sha256", &self.executable_sha256)
            .field("runtime_package", &self.runtime_package)
            .field("managed_home", &self.managed_home)
            .field("model", &self.model)
            .field("workspace", &self.workspace)
            .finish()
    }
}

#[derive(Clone)]
pub struct VergerailProvider {
    config: VergerailConfig,
    binding: ProviderBinding,
    /// Vergerail holds an exclusive lock for each managed home. Serialize
    /// one-shot children sharing this configured home so concurrent callers
    /// cannot race that ownership boundary.
    home_lock: Arc<tokio::sync::Mutex<()>>,
}

/// Stock linked-Rust provider plugin. The plugin owns strict Vergerail
/// configuration parsing and is the only authority used to resolve/connect
/// the stock provider from a composition catalog.
#[derive(Clone)]
pub struct VergerailPlugin {
    manifest: PluginManifest,
}

impl fmt::Debug for VergerailPlugin {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("VergerailPlugin")
            .field("id", &self.manifest.id())
            .field("manifestSha256", &self.manifest.digest())
            .finish()
    }
}

impl VergerailPlugin {
    pub fn new() -> Result<Self, String> {
        let id =
            PluginId::parse(VERGERAIL_PLUGIN_ID.to_owned()).map_err(|error| error.to_string())?;
        let manifest = PluginManifest::linked(id, env!("CARGO_PKG_VERSION"), PluginTrust::BuiltIn)
            .map_err(|error| error.to_string())?
            .contributes(ContributionKind::Provider);
        Ok(Self { manifest })
    }
}

/// Linked provider plugins exported by this package.  The composition roots
/// consume this registry generically; adding another trusted Rust provider is
/// a provider-package change, not a CLI/core type switch.
pub fn linked_provider_plugins() -> Result<Vec<Arc<dyn ProviderPlugin>>, String> {
    Ok(vec![Arc::new(VergerailPlugin::new()?)])
}

#[async_trait]
impl ProviderPlugin for VergerailPlugin {
    fn manifest(&self) -> &PluginManifest {
        &self.manifest
    }

    fn resolve_binding(&self, configuration: &Value) -> Result<ProviderBinding, ProviderError> {
        let config: VergerailConfig = serde_json::from_value(configuration.clone())
            .map_err(|error| contract(format!("Vergerail configuration is invalid: {error}")))?;
        resolve_binding(&config)
    }

    async fn connect(&self, configuration: &Value) -> Result<ProviderPlanes, ProviderError> {
        let config: VergerailConfig = serde_json::from_value(configuration.clone())
            .map_err(|error| contract(format!("Vergerail configuration is invalid: {error}")))?;
        let provider = VergerailProvider::connect(config).await?;
        let provider = Arc::new(provider);
        Ok(ProviderPlanes {
            model: provider.clone(),
            image: provider,
        })
    }
}

fn home_lock_for(path: &Path) -> Arc<HomeLock> {
    let key = normalized_managed_home(path);
    let registry = HOME_LOCKS.get_or_init(|| Mutex::new(BTreeMap::new()));
    let mut locks = registry
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    locks.retain(|_, lock| lock.strong_count() != 0);
    if let Some(lock) = locks.get(&key).and_then(Weak::upgrade) {
        return lock;
    }
    let lock = Arc::new(HomeLock::new(()));
    locks.insert(key, Arc::downgrade(&lock));
    lock
}

/// Resolve an identity for a managed home even when its final directory has
/// not been created yet. Canonicalizing the nearest existing ancestor makes
/// symlink and platform aliases share the same process lock without opening
/// or copying any managed-home contents.
fn normalized_managed_home(path: &Path) -> PathBuf {
    let lexical = path
        .components()
        .filter(|component| !matches!(component, Component::CurDir))
        .fold(PathBuf::new(), |mut normalized, component| {
            normalized.push(component.as_os_str());
            normalized
        });
    let mut candidate = lexical.clone();
    let mut unresolved = Vec::<PathBuf>::new();
    loop {
        if let Ok(mut canonical) = fs::canonicalize(&candidate) {
            for component in unresolved.iter().rev() {
                canonical.push(component);
            }
            return canonical;
        }
        let Some(name) = candidate.file_name() else {
            return lexical;
        };
        unresolved.push(PathBuf::from(name));
        let Some(parent) = candidate.parent() else {
            return lexical;
        };
        if parent == candidate {
            return lexical;
        }
        candidate = parent.to_path_buf();
    }
}

/// Resolve the declared Vergerail route into the provider-neutral facts used
/// by composition and review preflight. This validates only the explicit
/// route contract; it does not inspect the executable or credential state.
pub fn declared_binding(config: &VergerailConfig) -> Result<ProviderBinding, ProviderError> {
    validate_config(config)?;
    let route_bytes = serde_json::to_vec(config)
        .map_err(|error| contract(format!("provider route could not be encoded: {error}")))?;
    let route_sha256 = format!("{:x}", sha2::Sha256::digest(&route_bytes));
    let capabilities = BTreeSet::from([
        PROVIDER_CAP_TEXT.to_owned(),
        PROVIDER_CAP_IMAGE_OBSERVATION.to_owned(),
        PROVIDER_CAP_IMAGE_GENERATION.to_owned(),
    ]);
    let binding = ProviderBinding {
        id: "vergerail".to_owned(),
        model: config.model.clone(),
        route_sha256,
        capabilities,
        maximum_output_bytes: MAXIMUM_MODEL_RESPONSE_BYTES,
    };
    binding
        .validate()
        .map_err(|error| contract(format!("provider binding is invalid: {error}")))?;
    Ok(binding)
}

/// Resolve the concrete Vergerail route and verify the executable identity.
/// This is the admission used immediately before a provider is activated. It
/// performs no provider request and reads no credential material.
pub fn resolve_binding(config: &VergerailConfig) -> Result<ProviderBinding, ProviderError> {
    let binding = declared_binding(config)?;
    validate_executable_identity(config)?;
    Ok(binding)
}

impl fmt::Debug for VergerailProvider {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("VergerailProvider")
            .field("config", &self.config)
            .field("binding", &self.binding)
            .finish_non_exhaustive()
    }
}

impl VergerailProvider {
    // Preserve the public async API and defer admission until its future is polled.
    #[allow(clippy::unused_async, clippy::unused_async_trait_impl)]
    pub async fn connect(config: VergerailConfig) -> Result<Self, ProviderError> {
        let binding = resolve_binding(&config)?;
        Ok(Self {
            home_lock: home_lock_for(&config.managed_home),
            config,
            binding,
        })
    }

    #[must_use]
    pub fn config(&self) -> &VergerailConfig {
        &self.config
    }

    fn verify_executable_identity(&self) -> Result<(), ProviderError> {
        validate_executable_identity(&self.config)
    }

    async fn complete_turn(
        &self,
        turn: &ModelTurn,
        cancellation: &CancellationToken,
    ) -> Result<ModelTurnResult, ProviderError> {
        validate_turn(turn)?;
        if turn_has_image_observations(turn) {
            return Err(failure(
                FailureCode::CapabilityDenied,
                "logical image observations require resolved vergerail.upagent/2 sidecars",
            ));
        }
        if cancellation.is_cancelled() {
            return Err(cancelled_provider_execution());
        }
        let request_id = format!("upagent-{}", Uuid::now_v7());
        let request = serialize_model_request(turn, &request_id)?;
        let output = self
            .execute_process(
                request,
                turn.timeout_ms,
                cancellation,
                false,
                MAXIMUM_MODEL_FRAME_BYTES,
                None,
            )
            .await?;
        let response: ModelResponse =
            decode_success(&output, WireOperation::ModelTurn, &request_id)?;
        model_result_from_response(response, turn)
    }

    async fn complete_multimodal_turn(
        &self,
        turn: &ModelTurn,
        observations: &[ResolvedImageObservation],
        cancellation: &CancellationToken,
    ) -> Result<ModelTurnResult, ProviderError> {
        validate_turn(turn)?;
        validate_resolved_observations(turn, observations)?;
        if observations.is_empty() {
            return self.complete_turn(turn, cancellation).await;
        }
        if cancellation.is_cancelled() {
            return Err(cancelled_provider_execution());
        }
        let request_id = format!("upagent-{}", Uuid::now_v7());
        let wire_observations = observations
            .iter()
            .map(wire_image_observation)
            .collect::<Result<Vec<_>, ProviderError>>()?;
        let staging_root = create_request_staging_root()?;
        let request = match serialize_multimodal_request(
            turn,
            &request_id,
            &wire_observations,
            Some(&staging_root),
        ) {
            Ok(request) => request,
            Err(error) => return finish_staging_root(Err(error), Some(staging_root)),
        };
        let output = self
            .execute_process(
                request,
                turn.timeout_ms,
                cancellation,
                false,
                MAXIMUM_MODEL_FRAME_BYTES,
                Some(staging_root),
            )
            .await?;
        let response: ModelResponse =
            decode_success(&output, WireOperation::ModelTurn, &request_id)?;
        model_result_from_response(response, turn)
    }

    async fn generate_image_turn(
        &self,
        prompt: &str,
        requirements: &ImageGenerationRequirements,
        reasoning: ReasoningEffort,
        timeout_ms: u64,
        maximum_bytes: usize,
        cancellation: &CancellationToken,
    ) -> Result<ProviderImageResult, ProviderError> {
        if prompt.trim().is_empty() || prompt.len() > MAXIMUM_MODEL_MESSAGE_BYTES {
            return Err(contract("image prompt must be non-empty and bounded"));
        }
        if !(1..=MAXIMUM_PROVIDER_TIMEOUT_MS).contains(&timeout_ms) {
            return Err(contract(
                "image timeout is outside the bounded provider range",
            ));
        }
        if maximum_bytes == 0 || maximum_bytes > MAXIMUM_IMAGE_BYTES {
            return Err(contract(
                "image byte bound is outside the bounded provider range",
            ));
        }
        if cancellation.is_cancelled() {
            return Err(cancelled_provider_execution());
        }
        let request_id = format!("upagent-image-{}", Uuid::now_v7());
        let request = serialize_provider_request(&ProviderRequest {
            schema_version: 2,
            request_id: request_id.clone(),
            operation: WireOperation::ImageGenerate,
            messages: std::borrow::Cow::Borrowed(&[]),
            tools: std::borrow::Cow::Borrowed(&[]),
            observations: std::borrow::Cow::Borrowed(&[]),
            staging_root: None,
            reasoning,
            timeout_ms,
            maximum_response_bytes: maximum_bytes,
            prompt: Some(prompt.to_owned()),
            image_options: Some(*requirements),
        })
        .map_err(|error| contract(error.to_string()))?;
        let output = self
            .execute_process(
                request,
                timeout_ms,
                cancellation,
                true,
                MAXIMUM_IMAGE_FRAME_BYTES,
                None,
            )
            .await?;
        let response: ImageResponse =
            decode_success(&output, WireOperation::ImageGenerate, &request_id)
                .map_err(image_outcome_error)?;
        if response.image.media_type != "image/png"
            || response.image.width == 0
            || response.image.height == 0
        {
            return Err(image_outcome_error(failure(
                FailureCode::ProviderFailed,
                "provider image media type or dimensions are invalid",
            )));
        }
        if response.image.byte_length > maximum_bytes
            || response.image.base64.len() > MAXIMUM_IMAGE_BASE64_BYTES
        {
            return Err(image_outcome_error(failure(
                FailureCode::ProviderFailed,
                "provider image exceeded its encoded or decoded byte bound",
            )));
        }
        let bytes = BASE64_STANDARD
            .decode(response.image.base64.as_bytes())
            .map_err(|error| {
                image_outcome_error(contract(format!(
                    "provider image base64 is invalid: {error}"
                )))
            })?;
        if bytes.len() != response.image.byte_length
            || bytes.len() > maximum_bytes
            || bytes.len() > MAXIMUM_IMAGE_BYTES
        {
            return Err(image_outcome_error(failure(
                FailureCode::ProviderFailed,
                "provider image exceeded its decoded byte bound",
            )));
        }
        let (width, height) = validate_png(&bytes).map_err(image_outcome_error)?;
        if response.image.width != width || response.image.height != height {
            return Err(image_outcome_error(failure(
                FailureCode::ProviderFailed,
                "provider image dimensions did not match its PNG payload",
            )));
        }
        let observed_transparent_background = response.image.transparent_background;
        let mismatches = image_generation_mismatches(
            *requirements,
            response.image.width,
            response.image.height,
            observed_transparent_background,
        );
        // The provider reports only raw image metadata.  The host owns the
        // canonical PerfectPixel inspection and derives advisory mismatch
        // evidence without discarding otherwise valid bytes.
        let status = if mismatches.is_empty() {
            ProviderImageStatus::Generated
        } else {
            ProviderImageStatus::GeneratedNeedsRepair
        };
        let result = ProviderImageResult {
            image: ProviderImage {
                bytes,
                media_type: response.image.media_type,
                width,
                height,
            },
            status,
            observed_transparent_background,
            mismatches,
        };
        result.validate().map_err(image_outcome_error)?;
        Ok(result)
    }

    async fn execute_process(
        &self,
        request: Vec<u8>,
        timeout_ms: u64,
        cancellation: &CancellationToken,
        external: bool,
        frame_bytes: usize,
        staging_root: Option<PathBuf>,
    ) -> Result<Vec<u8>, ProviderError> {
        let result = self
            .execute_process_inner(request, timeout_ms, cancellation, external, frame_bytes)
            .await;
        finish_staging_root(result, staging_root)
    }

    async fn execute_process_inner(
        &self,
        request: Vec<u8>,
        timeout_ms: u64,
        cancellation: &CancellationToken,
        external: bool,
        frame_bytes: usize,
    ) -> Result<Vec<u8>, ProviderError> {
        admit_provider_request(&request)?;
        let deadline = tokio::time::Instant::now() + Duration::from_millis(timeout_ms);
        let _home_guard = tokio::select! {
            biased;
            () = cancellation.cancelled() => return Err(cancelled_provider_execution()),
            result = tokio::time::timeout_at(deadline, self.home_lock.lock()) => {
                match result {
                    Ok(guard) => guard,
                    Err(_) => {
                        return Err(failure(
                            FailureCode::ProviderFailed,
                            "managed Vergerail home lock timed out before dispatch",
                        ));
                    }
                }
            }
        };
        if cancellation.is_cancelled() {
            return Err(cancelled_provider_execution());
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(failure(
                FailureCode::ProviderFailed,
                "provider deadline elapsed before dispatch",
            ));
        }
        // Re-check the receipt-bound executable only after acquiring the
        // managed-home lock and immediately before spawn. This closes the
        // TOCTOU window without hashing the same binary twice per request.
        self.verify_executable_identity()?;
        let mut command = Command::new(&self.config.executable);
        command
            .kill_on_drop(true)
            .env_clear()
            // CODEX_HOME is the upstream Codex account/configuration
            // boundary. Keep it authoritative and explicit: Vergerail's
            // app-server inherits this value when it starts Codex.
            .env("CODEX_HOME", &self.config.managed_home)
            .env("VERGERAIL_CODEX_PACKAGE", &self.config.runtime_package)
            .env("VERGERAIL_MODEL", &self.config.model)
            .env("VERGERAIL_WORKSPACE", &self.config.workspace)
            .env("NO_COLOR", "1")
            .env("PATH", "/usr/bin:/bin");
        configure_process_group(&mut command);
        let mut child = command
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|error| {
                failure(
                    FailureCode::ProviderUnavailable,
                    format!("could not start Vergerail executable: {error}"),
                )
            })?;
        // Keep this identifier even after the leader is reaped. If an
        // inherited pipe remains open, the process group is the only bounded
        // cleanup boundary that can remove a descendant holding that pipe.
        let process_group = child.id();
        let Some(mut stdin) = child.stdin.take() else {
            stop_child(&mut child, process_group).await;
            return Err(failure(
                FailureCode::ProviderUnavailable,
                "Vergerail process did not expose stdin",
            ));
        };
        let Some(stdout) = child.stdout.take() else {
            stop_child(&mut child, process_group).await;
            return Err(failure(
                FailureCode::ProviderUnavailable,
                "Vergerail process did not expose stdout",
            ));
        };
        let Some(stderr) = child.stderr.take() else {
            stop_child(&mut child, process_group).await;
            return Err(failure(
                FailureCode::ProviderUnavailable,
                "Vergerail process did not expose stderr",
            ));
        };
        let stdout_task = tokio::spawn(read_bounded(stdout, frame_bytes + 1));
        let stderr_task = tokio::spawn(read_bounded(stderr, 64 * 1024));
        let write_result = tokio::select! {
            biased;
            () = cancellation.cancelled() => None,
            result = tokio::time::timeout_at(deadline, async {
                stdin
                    .write_all(&request)
                    .await
                    .map_err(|error| error.to_string())?;
                stdin.shutdown().await.map_err(|error| error.to_string())
            }) => Some(result),
        };
        drop(stdin);
        if write_result.is_none() {
            stop_child(&mut child, process_group).await;
            let _ = collect_reader_tasks(process_group, stdout_task, stderr_task).await;
            return Err(cancelled_provider_execution());
        }
        let write_failed = !matches!(write_result, Some(Ok(Ok(()))));
        if cancellation.is_cancelled() || write_failed {
            stop_child(&mut child, process_group).await;
            let _ = collect_reader_tasks(process_group, stdout_task, stderr_task).await;
            return Err(if external {
                resolution_required(if cancellation.is_cancelled() {
                    "image generation was cancelled after dispatch"
                } else {
                    "image generation dispatch timed out; outcome is unknown"
                })
            } else if cancellation.is_cancelled() {
                cancelled_provider_execution()
            } else {
                failure(
                    FailureCode::ProviderFailed,
                    "provider request write timed out",
                )
            });
        }
        let mut wait = Box::pin(child.wait());
        let wait_outcome = tokio::select! {
            biased;
            result = &mut wait => ProcessWaitOutcome::Exited(result),
            () = cancellation.cancelled() => ProcessWaitOutcome::Cancelled,
            () = tokio::time::sleep_until(deadline) => ProcessWaitOutcome::TimedOut,
        };
        drop(wait);
        let status = match wait_outcome {
            ProcessWaitOutcome::Exited(Ok(status)) => status,
            ProcessWaitOutcome::Exited(Err(error)) => {
                stop_child(&mut child, process_group).await;
                let _ = collect_reader_tasks(process_group, stdout_task, stderr_task).await;
                return Err(if external {
                    resolution_required(format!(
                        "Vergerail process wait failed after image dispatch: {error}"
                    ))
                } else {
                    failure(
                        FailureCode::ProviderUnavailable,
                        format!("Vergerail process wait failed: {error}"),
                    )
                });
            }
            ProcessWaitOutcome::Cancelled => {
                stop_child(&mut child, process_group).await;
                let _ = collect_reader_tasks(process_group, stdout_task, stderr_task).await;
                return Err(if external {
                    resolution_required("image generation was cancelled after dispatch")
                } else {
                    cancelled_provider_execution()
                });
            }
            ProcessWaitOutcome::TimedOut => {
                stop_child(&mut child, process_group).await;
                let _ = collect_reader_tasks(process_group, stdout_task, stderr_task).await;
                return Err(if external {
                    resolution_required("image generation timed out; outcome is unknown")
                } else {
                    failure(FailureCode::ProviderFailed, "provider request timed out")
                });
            }
        };
        let (stdout_result, stderr_result) =
            collect_reader_tasks(process_group, stdout_task, stderr_task)
                .await
                .map_err(|error| {
                    if external {
                        resolution_required(format!(
                            "Vergerail output drain failed after image dispatch: {error}"
                        ))
                    } else {
                        failure(FailureCode::ProviderFailed, error)
                    }
                })?;
        let stdout = stdout_result.map_err(|error| {
            if external {
                resolution_required(format!(
                    "Vergerail stdout failed after image dispatch: {error}"
                ))
            } else {
                failure(FailureCode::ProviderFailed, error)
            }
        })?;
        let stderr = stderr_result.map_err(|error| {
            if external {
                resolution_required(format!(
                    "Vergerail stderr failed after image dispatch: {error}"
                ))
            } else {
                failure(FailureCode::ProviderFailed, error)
            }
        })?;
        if !status.success() {
            return Err(if external {
                resolution_required(format!(
                    "Vergerail process exited unsuccessfully after image dispatch: status={} stderr={}",
                    status,
                    bounded_stderr(&stderr)
                ))
            } else {
                failure(
                    FailureCode::ProviderFailed,
                    format!(
                        "Vergerail process exited unsuccessfully: status={} stderr={}",
                        status,
                        bounded_stderr(&stderr)
                    ),
                )
            });
        }
        if stdout.len() > frame_bytes {
            return Err(if external {
                resolution_required(
                    "Vergerail response exceeded the JSONL frame bound after image dispatch",
                )
            } else {
                failure(
                    FailureCode::ProviderFailed,
                    "Vergerail response exceeded the JSONL frame bound",
                )
            });
        }
        if stdout.is_empty() {
            return Err(resolution_required_if(
                external,
                "Vergerail process exited without a response",
                "Vergerail process exited without a response",
            ));
        }
        Ok(stdout)
    }
}

#[derive(Debug)]
enum ProcessWaitOutcome {
    Exited(Result<std::process::ExitStatus, std::io::Error>),
    Cancelled,
    TimedOut,
}

fn configure_process_group(command: &mut Command) {
    #[cfg(unix)]
    {
        command.process_group(0);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;

        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        command.creation_flags(CREATE_NEW_PROCESS_GROUP);
    }
}

#[allow(clippy::unused_async)]
async fn terminate_process_group(pid: Option<u32>) {
    let Some(pid) = pid else {
        return;
    };

    #[cfg(unix)]
    {
        use rustix::process::{Pid, Signal, kill_process_group};

        let Ok(raw_pid) = i32::try_from(pid) else {
            return;
        };
        let Some(process_group) = Pid::from_raw(raw_pid) else {
            return;
        };
        let _ = kill_process_group(process_group, Signal::KILL);
    }
    #[cfg(windows)]
    {
        let Ok(mut taskkill) = tokio::process::Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/T", "/F"])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true)
            .spawn()
        else {
            return;
        };
        if tokio::time::timeout(PROCESS_CLEANUP_TIMEOUT, taskkill.wait())
            .await
            .is_err()
        {
            let _ = taskkill.start_kill();
            let _ = tokio::time::timeout(PROCESS_CLEANUP_TIMEOUT, taskkill.wait()).await;
        }
    }
}

/// Stop a child and its descendants without allowing process cleanup to hold
/// the managed-home lock indefinitely. The process group is signalled while
/// the child identity is still owned, then the direct child is reaped within a
/// fixed grace period.
async fn stop_child(child: &mut tokio::process::Child, process_group: Option<u32>) {
    terminate_process_group(process_group).await;
    let _ = child.start_kill();
    let _ = tokio::time::timeout(PROCESS_CLEANUP_TIMEOUT, child.wait()).await;
}

async fn collect_reader_tasks(
    process_group: Option<u32>,
    stdout_task: JoinHandle<Result<Vec<u8>, String>>,
    stderr_task: JoinHandle<Result<Vec<u8>, String>>,
) -> Result<(Result<Vec<u8>, String>, Result<Vec<u8>, String>), String> {
    let mut reader_tasks = JoinSet::new();
    reader_tasks.spawn(async move {
        let result = match stdout_task.await {
            Ok(result) => result,
            Err(error) => Err(format!("stdout reader task failed: {error}")),
        };
        ("stdout", result)
    });
    reader_tasks.spawn(async move {
        let result = match stderr_task.await {
            Ok(result) => result,
            Err(error) => Err(format!("stderr reader task failed: {error}")),
        };
        ("stderr", result)
    });

    let joined = tokio::time::timeout(PROCESS_DRAIN_TIMEOUT, async {
        let mut stdout = None;
        let mut stderr = None;
        while stdout.is_none() || stderr.is_none() {
            match reader_tasks.join_next().await {
                Some(Ok(("stdout", result))) => stdout = Some(result),
                Some(Ok(("stderr", result))) => stderr = Some(result),
                Some(Ok((stream, _))) => {
                    return Err(format!("unexpected {stream} reader task"));
                }
                Some(Err(error)) => return Err(format!("reader task failed: {error}")),
                None => {
                    return Err("reader tasks ended before both streams were collected".to_owned());
                }
            }
        }
        match (stdout, stderr) {
            (Some(stdout), Some(stderr)) => Ok((stdout, stderr)),
            _ => Err("reader tasks ended before both streams were collected".to_owned()),
        }
    })
    .await;

    if let Ok(result) = joined {
        result
    } else {
        // This path also runs after a normal leader exit. Keep the process
        // group id from spawn so a descendant that retained a pipe is killed
        // before the canonical home lock is released. JoinSet removes each
        // completed task exactly once, so a task that completed just before
        // the timeout is never polled again during cleanup.
        terminate_process_group(process_group).await;
        reader_tasks.abort_all();
        let _ = tokio::time::timeout(PROCESS_CLEANUP_TIMEOUT, async {
            while reader_tasks.join_next().await.is_some() {}
        })
        .await;
        Err("child output drain exceeded the bounded cleanup timeout".to_owned())
    }
}

async fn read_bounded<R>(reader: R, limit: usize) -> Result<Vec<u8>, String>
where
    R: AsyncRead + Unpin,
{
    let mut output = Vec::new();
    reader
        .take(limit as u64)
        .read_to_end(&mut output)
        .await
        .map_err(|error| error.to_string())?;
    if output.len() > limit.saturating_sub(1) {
        return Err("child output exceeded the bounded transport limit".to_owned());
    }
    Ok(output)
}

fn bounded_stderr(stderr: &[u8]) -> String {
    String::from_utf8_lossy(stderr).chars().take(512).collect()
}

use agent_contract::provider::{
    FailureResponse as WireFailureResponse, ImageResponse, ModelResponse,
    ProviderFailure as WireFailure, ProviderObservation as WireImageObservation,
    ProviderOperation as WireOperation, ProviderRequest,
};

fn serialize_model_request(turn: &ModelTurn, request_id: &str) -> Result<Vec<u8>, ProviderError> {
    serialize_provider_request(&ProviderRequest {
        schema_version: 2,
        request_id: request_id.to_owned(),
        operation: WireOperation::ModelTurn,
        observations: std::borrow::Cow::Borrowed(&[]),
        staging_root: None,
        messages: std::borrow::Cow::Borrowed(&turn.messages),
        tools: std::borrow::Cow::Borrowed(&turn.tools),
        reasoning: turn.reasoning,
        timeout_ms: turn.timeout_ms,
        maximum_response_bytes: turn.maximum_response_bytes,
        prompt: None,
        image_options: None,
    })
}

fn serialize_multimodal_request(
    turn: &ModelTurn,
    request_id: &str,
    observations: &[WireImageObservation],
    staging_root: Option<&Path>,
) -> Result<Vec<u8>, ProviderError> {
    serialize_provider_request(&ProviderRequest {
        schema_version: 2,
        request_id: request_id.to_owned(),
        operation: WireOperation::ModelTurn,
        messages: std::borrow::Cow::Borrowed(&turn.messages),
        observations: std::borrow::Cow::Borrowed(observations),
        staging_root: staging_root.map(Path::to_path_buf),
        tools: std::borrow::Cow::Borrowed(&turn.tools),
        reasoning: turn.reasoning,
        timeout_ms: turn.timeout_ms,
        maximum_response_bytes: turn.maximum_response_bytes,
        prompt: None,
        image_options: None,
    })
}

fn serialize_provider_request(request: &ProviderRequest<'_>) -> Result<Vec<u8>, ProviderError> {
    request
        .validate_wire()
        .map_err(|error| contract(error.message))?;
    serde_json::to_vec(request).map_err(|error| contract(error.to_string()))
}

fn admit_provider_request(request: &[u8]) -> Result<(), ProviderError> {
    if request.len() > MAXIMUM_PROVIDER_REQUEST_BYTES {
        return Err(contract(format!(
            "provider request exceeds the {} MiB JSONL frame bound",
            MAXIMUM_PROVIDER_REQUEST_BYTES / (1024 * 1024)
        )));
    }
    Ok(())
}

fn finish_staging_root<T>(
    result: Result<T, ProviderError>,
    staging_root: Option<PathBuf>,
) -> Result<T, ProviderError> {
    let Some(staging_root) = staging_root else {
        return result;
    };
    let cleanup = remove_request_staging_root(&staging_root);
    match (result, cleanup) {
        (Ok(value), Ok(())) => Ok(value),
        (Err(error), Ok(())) => Err(error),
        (Ok(_), Err(cleanup_error)) => Err(cleanup_error),
        (Err(error), Err(cleanup_error)) => {
            let code = match &error {
                ProviderError::Failure(failure) => failure.code,
                ProviderError::Contract(_) => FailureCode::ProviderFailed,
            };
            Err(failure(
                code,
                format!("{error}; request staging cleanup also failed: {cleanup_error}"),
            ))
        }
    }
}

fn create_request_staging_root() -> Result<PathBuf, ProviderError> {
    let parent = std::env::temp_dir();
    for attempt in 0..16_u32 {
        let root = parent.join(format!("{STAGING_ROOT_PREFIX}{}-{attempt}", Uuid::now_v7()));
        match fs::create_dir(&root) {
            Ok(()) => {
                #[cfg(unix)]
                let permission_result = {
                    use std::os::unix::fs::PermissionsExt as _;
                    fs::set_permissions(&root, fs::Permissions::from_mode(0o700))
                };
                #[cfg(not(unix))]
                let permission_result: std::io::Result<()> = Ok(());
                if let Err(error) = permission_result {
                    let cleanup = fs::remove_dir(&root);
                    return Err(match cleanup {
                        Ok(()) => failure(
                            FailureCode::ProviderUnavailable,
                            format!("could not secure request staging root: {error}"),
                        ),
                        Err(cleanup_error)
                            if cleanup_error.kind() == std::io::ErrorKind::NotFound =>
                        {
                            failure(
                                FailureCode::ProviderUnavailable,
                                format!("could not secure request staging root: {error}"),
                            )
                        }
                        Err(cleanup_error) => failure(
                            FailureCode::ProviderUnavailable,
                            format!(
                                "could not secure request staging root: {error}; cleanup also failed: {cleanup_error}"
                            ),
                        ),
                    });
                }
                return Ok(root);
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => {
                return Err(failure(
                    FailureCode::ProviderUnavailable,
                    format!("could not create request staging root: {error}"),
                ));
            }
        }
    }
    Err(failure(
        FailureCode::ProviderUnavailable,
        "could not allocate a unique request staging root",
    ))
}

fn remove_request_staging_root(path: &Path) -> Result<(), ProviderError> {
    let expected_parent = std::env::temp_dir();
    let valid_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.starts_with(STAGING_ROOT_PREFIX));
    if !path.is_absolute()
        || path.parent() != Some(expected_parent.as_path())
        || !valid_name
        || path
            .components()
            .any(|component| component == Component::ParentDir)
    {
        return Err(contract(
            "request staging cleanup refused a path outside the exact private root",
        ));
    }
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
            return Err(failure(
                FailureCode::ProviderFailed,
                "request staging root is not a private directory",
            ));
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(failure(
                FailureCode::ProviderFailed,
                format!("could not inspect request staging root: {error}"),
            ));
        }
    }
    match fs::remove_dir_all(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(failure(
            FailureCode::ProviderFailed,
            format!("could not remove request staging root: {error}"),
        )),
    }
}

fn wire_image_observation(
    value: &ResolvedImageObservation,
) -> Result<WireImageObservation, ProviderError> {
    if value.bytes.is_empty() || value.bytes.len() > MAXIMUM_IMAGE_BYTES {
        return Err(contract(
            "resolved image observation is outside the byte bound",
        ));
    }
    Ok(WireImageObservation {
        message_index: value.message_index,
        part_index: value.part_index,
        media_type: value.media_type.clone(),
        sha256: value.sha256.clone(),
        width: value.width,
        height: value.height,
        role: value.role,
        base64: BASE64_STANDARD.encode(&value.bytes),
    })
}

fn decode_success<T>(
    bytes: &[u8],
    operation: WireOperation,
    request_id: &str,
) -> Result<T, ProviderError>
where
    T: for<'de> Deserialize<'de> + OperationResponse,
{
    if bytes.len() > frame_limit(operation) {
        return Err(contract(format!(
            "provider {operation:?} response exceeded its bounded frame"
        )));
    }
    if let Ok(error) = serde_json::from_slice::<WireFailureResponse>(bytes) {
        if error.schema_version != 2 || error.ok || error.request_id != request_id {
            return Err(contract("provider failure response had invalid envelope"));
        }
        return Err(map_wire_failure(error.error));
    }
    let response = serde_json::from_slice::<T>(bytes)
        .map_err(|error| contract(format!("provider response was not strict JSON: {error}")))?;
    if response.schema_version() != 2
        || response.request_id() != request_id
        || response.operation() != operation
    {
        return Err(contract(
            "provider response envelope did not match the request",
        ));
    }
    Ok(response)
}

fn frame_limit(operation: WireOperation) -> usize {
    match operation {
        WireOperation::ModelTurn => MAXIMUM_MODEL_FRAME_BYTES,
        WireOperation::ImageGenerate => MAXIMUM_IMAGE_FRAME_BYTES,
    }
}

trait OperationResponse {
    fn schema_version(&self) -> u32;
    fn request_id(&self) -> &str;
    fn operation(&self) -> WireOperation;
}

impl OperationResponse for ModelResponse {
    fn schema_version(&self) -> u32 {
        self.schema_version
    }

    fn request_id(&self) -> &str {
        &self.request_id
    }

    fn operation(&self) -> WireOperation {
        self.operation
    }
}

impl OperationResponse for ImageResponse {
    fn schema_version(&self) -> u32 {
        self.schema_version
    }

    fn request_id(&self) -> &str {
        &self.request_id
    }

    fn operation(&self) -> WireOperation {
        self.operation
    }
}

fn validate_config(config: &VergerailConfig) -> Result<(), ProviderError> {
    if config.protocol_schema != PROVIDER_PROTOCOL_SCHEMA {
        return Err(contract(format!(
            "protocolSchema must be {PROVIDER_PROTOCOL_SCHEMA}"
        )));
    }
    for (name, path) in [
        ("executable", &config.executable),
        ("runtimePackage", &config.runtime_package),
        ("managedHome", &config.managed_home),
        ("workspace", &config.workspace),
    ] {
        validate_absolute_path(name, path)?;
    }
    validate_text(&config.model, "model", 160)?;
    if config.executable_sha256.len() != 64
        || !config
            .executable_sha256
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(contract(
            "executableSha256 must be a 64-character hexadecimal digest",
        ));
    }
    Ok(())
}

fn validate_executable_identity(config: &VergerailConfig) -> Result<(), ProviderError> {
    let metadata = fs::symlink_metadata(&config.executable).map_err(|error| {
        failure(
            FailureCode::ProviderUnavailable,
            format!("cannot inspect Vergerail executable: {error}"),
        )
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(failure(
            FailureCode::ProviderUnavailable,
            "Vergerail executable must be a regular non-symlink file",
        ));
    }
    #[cfg(unix)]
    if std::os::unix::fs::PermissionsExt::mode(&metadata.permissions()) & 0o111 == 0 {
        return Err(failure(
            FailureCode::ProviderUnavailable,
            "Vergerail executable is not executable",
        ));
    }
    let bytes = fs::read(&config.executable).map_err(|error| {
        failure(
            FailureCode::ProviderUnavailable,
            format!("cannot read Vergerail executable identity: {error}"),
        )
    })?;
    if bytes.is_empty() || bytes.len() > 128 * 1024 * 1024 {
        return Err(failure(
            FailureCode::ProviderUnavailable,
            "Vergerail executable size is outside the identity bound",
        ));
    }
    let observed = format!("{:x}", sha2::Sha256::digest(&bytes));
    if !observed.eq_ignore_ascii_case(&config.executable_sha256) {
        return Err(failure(
            FailureCode::ProviderUnavailable,
            format!(
                "Vergerail executable digest mismatch: expected {}, observed {observed}",
                config.executable_sha256
            ),
        ));
    }
    Ok(())
}

fn validate_absolute_path(name: &str, path: &Path) -> Result<(), ProviderError> {
    if !path.is_absolute() || path.as_os_str().is_empty() || path.to_string_lossy().contains('\0') {
        return Err(contract(format!(
            "{name} must be an absolute path without NUL"
        )));
    }
    if path
        .components()
        .any(|component| component == Component::ParentDir)
    {
        return Err(contract(format!(
            "{name} must not contain parent traversal"
        )));
    }
    Ok(())
}

fn validate_text(value: &str, label: &str, maximum: usize) -> Result<(), ProviderError> {
    if value.trim().is_empty() || value.len() > maximum || value.contains('\0') {
        return Err(contract(format!(
            "{label} must be a bounded non-empty value"
        )));
    }
    Ok(())
}

fn validate_turn(turn: &ModelTurn) -> Result<(), ProviderError> {
    if turn.messages.is_empty() {
        return Err(contract("model turn must contain at least one message"));
    }
    if turn.messages.len() > MAXIMUM_MODEL_MESSAGES_PER_TURN {
        return Err(contract("model turn contains too many messages"));
    }
    if turn.timeout_ms == 0 || turn.timeout_ms > MAXIMUM_PROVIDER_TIMEOUT_MS {
        return Err(contract(
            "model timeout is outside the bounded provider range",
        ));
    }
    if turn.maximum_response_bytes == 0
        || turn.maximum_response_bytes > MAXIMUM_MODEL_RESPONSE_BYTES
    {
        return Err(contract(
            "model response byte bound is outside the bounded range",
        ));
    }
    if turn.tools.len() > MAXIMUM_MODEL_TOOLS_PER_TURN {
        return Err(contract("model turn contains too many tools"));
    }
    for tool in &turn.tools {
        if tool.name.is_empty() || tool.name.len() > MAXIMUM_TOOL_NAME_BYTES {
            return Err(contract(
                "advertised tool name is outside the bounded provider range",
            ));
        }
    }
    for message in &turn.messages {
        validate_message(message)?;
    }
    Ok(())
}

fn model_result_from_response(
    response: ModelResponse,
    turn: &ModelTurn,
) -> Result<ModelTurnResult, ProviderError> {
    if response.text.len() > turn.maximum_response_bytes {
        return Err(failure(
            FailureCode::ProviderFailed,
            "provider response exceeded maximumResponseBytes",
        ));
    }
    if response.tool_calls.len() > MAXIMUM_TOOL_CALLS_PER_TURN {
        return Err(contract("provider returned too many tool calls"));
    }
    let allowed = turn
        .tools
        .iter()
        .map(|tool| tool.name.as_str())
        .collect::<BTreeSet<_>>();
    let mut ids = BTreeSet::new();
    let mut argument_bytes = 0_usize;
    let tool_calls = response
        .tool_calls
        .into_iter()
        .map(|call| {
            if call.id.len() > MAXIMUM_TOOL_CALL_ID_BYTES {
                return Err(contract(
                    "provider returned a tool-call identifier outside the bounded range",
                ));
            }
            if call.name.len() > MAXIMUM_TOOL_NAME_BYTES {
                return Err(contract(
                    "provider returned a tool name outside the bounded range",
                ));
            }
            if !allowed.contains(call.name.as_str()) {
                return Err(failure(
                    FailureCode::ProviderFailed,
                    "provider returned a tool outside the advertised set",
                ));
            }
            if !ids.insert(call.id.clone()) {
                return Err(failure(
                    FailureCode::ProviderFailed,
                    "provider returned a duplicate tool call identifier",
                ));
            }
            let argument_length = serde_json::to_vec(&call.arguments)
                .map_err(|error| {
                    contract(format!("tool call arguments are not serializable: {error}"))
                })?
                .len();
            if argument_length > MAXIMUM_TOOL_ARGUMENT_BYTES {
                return Err(contract(
                    "provider returned a tool-call argument outside the bounded range",
                ));
            }
            argument_bytes = argument_bytes
                .checked_add(argument_length)
                .ok_or_else(|| contract("provider tool-call argument size overflowed"))?;
            if argument_bytes > MAXIMUM_TOOL_ARGUMENTS_BYTES {
                return Err(contract(
                    "provider returned aggregate tool-call arguments outside the bounded range",
                ));
            }
            Ok(ToolCall {
                id: call.id,
                name: call.name,
                arguments: call.arguments,
            })
        })
        .collect::<Result<Vec<_>, ProviderError>>()?;
    Ok(ModelTurnResult {
        text: response.text,
        tool_calls,
        usage: response.usage,
    })
}

fn validate_message(message: &ModelMessage) -> Result<(), ProviderError> {
    if message.content.len() > MAXIMUM_MODEL_MESSAGE_BYTES {
        return Err(contract("model message exceeds the bounded byte limit"));
    }
    if message.content_parts.len() > 8 {
        return Err(contract("model message contains too many content parts"));
    }
    let has_parts = !message.content_parts.is_empty();
    if message.role == MessageRole::Assistant {
        if message.content.is_empty() && message.tool_calls.is_empty() && !has_parts {
            return Err(contract(
                "assistant messages require content, content parts, or at least one tool call",
            ));
        }
    } else if message.content.is_empty() && !has_parts {
        return Err(contract(
            "non-assistant messages require non-empty content or content parts",
        ));
    }
    if message.role == MessageRole::Tool {
        if message.tool_call_id.is_none() || message.tool_name.is_none() {
            return Err(contract("tool messages require toolCallId and toolName"));
        }
        if !message.tool_calls.is_empty() {
            return Err(contract("tool messages cannot contain tool calls"));
        }
    } else {
        if message.tool_call_id.is_some() || message.tool_name.is_some() {
            return Err(contract(
                "only tool messages may contain toolCallId and toolName",
            ));
        }
        if message.role != MessageRole::Assistant && !message.tool_calls.is_empty() {
            return Err(contract("only assistant messages may contain tool calls"));
        }
    }
    for call in &message.tool_calls {
        if !call.arguments.is_object() {
            return Err(contract("tool call arguments must be a JSON object"));
        }
    }
    for part in &message.content_parts {
        match part {
            ModelContentPart::Text { text } => {
                if text.trim().is_empty() || text.len() > MAXIMUM_MODEL_MESSAGE_BYTES {
                    return Err(contract("text content part is outside the bounded range"));
                }
            }
            ModelContentPart::ImageObservation { image } => {
                if image.artifact.media_type != "image/png"
                    || image.artifact.byte_length == 0
                    || image.artifact.byte_length > MAXIMUM_IMAGE_BYTES as u64
                {
                    return Err(contract(
                        "image observation requires a bounded image/png artifact",
                    ));
                }
                if let Some(caption) = &image.caption
                    && (caption.trim().is_empty() || caption.len() > 1024)
                {
                    return Err(contract(
                        "image observation caption is outside the bounded range",
                    ));
                }
            }
        }
    }
    Ok(())
}

fn validate_resolved_observations(
    turn: &ModelTurn,
    observations: &[ResolvedImageObservation],
) -> Result<(), ProviderError> {
    if observations.len() > 16 {
        return Err(contract("model turn contains too many image observations"));
    }
    let mut aggregate_bytes = 0_usize;
    let mut identities = BTreeSet::new();
    for observation in observations {
        let message = turn
            .messages
            .get(observation.message_index)
            .ok_or_else(|| contract("image observation message index is invalid"))?;
        let part = message
            .content_parts
            .get(observation.part_index)
            .ok_or_else(|| contract("image observation part index is invalid"))?;
        let ModelContentPart::ImageObservation { image } = part else {
            return Err(contract(
                "resolved observation does not reference an image content part",
            ));
        };
        if image.artifact.sha256 != observation.sha256
            || image.artifact.media_type != observation.media_type
            || image.role != observation.role
            || observation.media_type != "image/png"
        {
            return Err(contract(
                "resolved observation does not match its artifact reference",
            ));
        }
        let byte_length = usize::try_from(image.artifact.byte_length)
            .map_err(|_| contract("image observation byte length overflowed"))?;
        if byte_length != observation.bytes.len()
            || byte_length == 0
            || byte_length > MAXIMUM_IMAGE_BYTES
        {
            return Err(contract("resolved observation byte length is invalid"));
        }
        let observed_sha = format!("{:x}", sha2::Sha256::digest(&observation.bytes));
        if observed_sha != observation.sha256 {
            return Err(contract(
                "resolved observation bytes do not match the content address",
            ));
        }
        let (width, height) = validate_png(&observation.bytes)?;
        if width != observation.width || height != observation.height {
            return Err(contract(
                "resolved observation dimensions do not match the PNG payload",
            ));
        }
        if !identities.insert((observation.message_index, observation.part_index)) {
            return Err(contract("duplicate resolved image observation"));
        }
        aggregate_bytes = aggregate_bytes
            .checked_add(byte_length)
            .ok_or_else(|| contract("image observation aggregate byte count overflowed"))?;
        if aggregate_bytes > 32 * 1024 * 1024 {
            return Err(contract(
                "image observations exceed the aggregate byte bound",
            ));
        }
    }
    let expected = turn
        .messages
        .iter()
        .flat_map(|message| message.content_parts.iter())
        .filter(|part| matches!(part, ModelContentPart::ImageObservation { .. }))
        .count();
    if expected != observations.len() {
        return Err(contract("not every image content part was resolved"));
    }
    Ok(())
}

fn turn_has_image_observations(turn: &ModelTurn) -> bool {
    turn.messages.iter().any(|message| {
        message
            .content_parts
            .iter()
            .any(|part| matches!(part, ModelContentPart::ImageObservation { .. }))
    })
}

fn validate_png(bytes: &[u8]) -> Result<(u32, u32), ProviderError> {
    if bytes.len() < 33 || !bytes.starts_with(b"\x89PNG\r\n\x1a\n") || &bytes[12..16] != b"IHDR" {
        return Err(failure(
            FailureCode::ProviderFailed,
            "provider image is not a PNG raster",
        ));
    }
    let width_bytes: [u8; 4] = bytes[16..20].try_into().map_err(|_| {
        failure(
            FailureCode::ProviderFailed,
            "provider PNG width header is invalid",
        )
    })?;
    let height_bytes: [u8; 4] = bytes[20..24].try_into().map_err(|_| {
        failure(
            FailureCode::ProviderFailed,
            "provider PNG height header is invalid",
        )
    })?;
    let width = u32::from_be_bytes(width_bytes);
    let height = u32::from_be_bytes(height_bytes);
    if width == 0 || height == 0 || width > 8_192 || height > 8_192 {
        return Err(failure(
            FailureCode::ProviderFailed,
            "provider image dimensions exceed the bounded raster limit",
        ));
    }
    Ok((width, height))
}

fn image_generation_mismatches(
    requirements: ImageGenerationRequirements,
    width: u32,
    height: u32,
    observed_transparent_background: Option<bool>,
) -> Vec<ImageGenerationMismatch> {
    let mut mismatches = Vec::new();
    if let Some(requested) = requirements.size
        && let Some((expected_width, expected_height)) = requested.dimensions()
        && (width, height) != (expected_width, expected_height)
    {
        mismatches.push(ImageGenerationMismatch::Size {
            requested,
            observed_width: width,
            observed_height: height,
        });
    }
    if let Some(
        requested @ (ImageBackgroundRequirement::Transparent | ImageBackgroundRequirement::Opaque),
    ) = requirements.background
    {
        let expected_transparent = matches!(requested, ImageBackgroundRequirement::Transparent);
        if observed_transparent_background != Some(expected_transparent) {
            mismatches.push(ImageGenerationMismatch::Background {
                requested,
                observed_transparent: observed_transparent_background,
            });
        }
    }
    mismatches
}

fn map_wire_failure(error: WireFailure) -> ProviderError {
    let code = match error.code.as_str() {
        "invalid_request" | "invalid_configuration" => FailureCode::InvalidRequest,
        "resource_limit" | "timeout" => FailureCode::BudgetExceeded,
        "authentication_required" | "runtime_verification" => FailureCode::ProviderUnavailable,
        "resolution_required" => FailureCode::ResolutionRequired,
        _ => FailureCode::ProviderFailed,
    };
    ProviderError::Failure(
        AgentFailure::new(code, error.message)
            .retryable(error.retryable)
            .with_detail("providerCode", error.code),
    )
}

pub fn validate_supported_provider_id(value: &str) -> Result<(), String> {
    if value == "vergerail" {
        Ok(())
    } else {
        Err(format!("unsupported provider: {value}; expected vergerail"))
    }
}

fn contract(message: impl Into<String>) -> ProviderError {
    ProviderError::Contract(message.into())
}

fn failure(code: FailureCode, message: impl Into<String>) -> ProviderError {
    ProviderError::Failure(AgentFailure::new(code, message))
}

/// Once an image request has been written to the Vergerail process, any
/// transport or malformed-result failure leaves the external outcome
/// unobservable. Preserve only explicit terminal request/authority failures;
/// callers must resolve every other image outcome instead of retrying and
/// risking duplicate billed generations.
fn image_outcome_error(error: ProviderError) -> ProviderError {
    match error {
        ProviderError::Failure(failure)
            if matches!(
                failure.code,
                FailureCode::InvalidRequest | FailureCode::CapabilityDenied
            ) =>
        {
            ProviderError::Failure(failure)
        }
        ProviderError::Failure(failure)
            if failure
                .details
                .get("providerCode")
                .and_then(Value::as_str)
                .is_some_and(|code| {
                    matches!(
                        code,
                        "invalid_request"
                            | "invalid_configuration"
                            | "authentication_required"
                            | "runtime_verification"
                            | "capability_denied"
                    )
                }) =>
        {
            ProviderError::Failure(failure)
        }
        ProviderError::Failure(failure) if failure.code == FailureCode::ResolutionRequired => {
            ProviderError::Failure(failure)
        }
        other => {
            let detail = other.to_string();
            ProviderError::Failure(
                AgentFailure::new(
                    FailureCode::ResolutionRequired,
                    "image generation outcome is unknown after dispatch",
                )
                .with_detail("cause", detail),
            )
        }
    }
}

fn cancelled_provider_execution() -> ProviderError {
    failure(FailureCode::Cancelled, "provider execution was cancelled")
}

fn resolution_required(message: impl Into<String>) -> ProviderError {
    failure(FailureCode::ResolutionRequired, message)
}

fn resolution_required_if(
    external: bool,
    external_message: &str,
    local_message: &str,
) -> ProviderError {
    if external {
        resolution_required(external_message)
    } else {
        failure(FailureCode::ProviderFailed, local_message)
    }
}

#[async_trait]
impl ModelPort for VergerailProvider {
    fn binding(&self) -> ProviderBinding {
        self.binding.clone()
    }

    async fn complete(&self, turn: &ModelTurn) -> Result<ModelTurnResult, ProviderError> {
        self.complete_turn(turn, &CancellationToken::new()).await
    }

    async fn complete_with_cancellation(
        &self,
        turn: &ModelTurn,
        cancellation: &CancellationToken,
    ) -> Result<ModelTurnResult, ProviderError> {
        self.complete_turn(turn, cancellation).await
    }

    async fn complete_with_observations(
        &self,
        turn: &ModelTurn,
        observations: &[ResolvedImageObservation],
        cancellation: &CancellationToken,
    ) -> Result<ModelTurnResult, ProviderError> {
        self.complete_multimodal_turn(turn, observations, cancellation)
            .await
    }
}

#[async_trait]
impl ImagePort for VergerailProvider {
    fn binding(&self) -> ProviderBinding {
        self.binding.clone()
    }

    async fn generate_image(
        &self,
        prompt: &str,
        requirements: &ImageGenerationRequirements,
        reasoning: ReasoningEffort,
        timeout_ms: u64,
        maximum_bytes: usize,
    ) -> Result<ProviderImageResult, ProviderError> {
        self.generate_image_turn(
            prompt,
            requirements,
            reasoning,
            timeout_ms,
            maximum_bytes,
            &CancellationToken::new(),
        )
        .await
    }

    async fn generate_image_with_cancellation(
        &self,
        prompt: &str,
        requirements: &ImageGenerationRequirements,
        reasoning: ReasoningEffort,
        timeout_ms: u64,
        maximum_bytes: usize,
        cancellation: &CancellationToken,
    ) -> Result<ProviderImageResult, ProviderError> {
        self.generate_image_turn(
            prompt,
            requirements,
            reasoning,
            timeout_ms,
            maximum_bytes,
            cancellation,
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_core::{
        ArtifactId, ArtifactRef, ImageObservation, ImageObservationDetail, ImageObservationRole,
        MessageRole, ModelContentPart, ModelMessage, TaskBudget,
    };
    use agent_core::{ToolCallId, ToolDefinition};
    use agent_provider_api::ProviderPlugin;
    use std::collections::BTreeMap;
    use std::fs;
    use std::io::Cursor;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn wire_reasoning_maps_every_core_effort_without_downgrade() {
        let cases = [
            (ReasoningEffort::Off, "off"),
            (ReasoningEffort::Low, "low"),
            (ReasoningEffort::Medium, "medium"),
            (ReasoningEffort::High, "high"),
            (ReasoningEffort::XHigh, "xhigh"),
            (ReasoningEffort::Max, "max"),
        ];
        for (effort, expected) in cases {
            assert_eq!(
                serde_json::to_value(effort).expect("serialize reasoning"),
                serde_json::json!(expected)
            );
        }
    }

    #[test]
    fn vergerail_provider_plugin_has_a_linked_manifest() {
        let plugin = VergerailPlugin::new().expect("vergerail plugin");
        assert_eq!(plugin.manifest().id().as_str(), VERGERAIL_PLUGIN_ID);
        assert!(
            plugin
                .manifest()
                .contributions()
                .contains(&ContributionKind::Provider)
        );
        assert_eq!(plugin.manifest().digest().len(), 64);
    }

    #[tokio::test]
    async fn reader_drain_timeout_does_not_repoll_completed_task() {
        let stdout = tokio::spawn(async { Ok::<Vec<u8>, String>(Vec::new()) });
        let stderr =
            tokio::spawn(async { std::future::pending::<Result<Vec<u8>, String>>().await });
        let result = collect_reader_tasks(None, stdout, stderr).await;
        assert!(matches!(
            result,
            Err(message) if message == "child output drain exceeded the bounded cleanup timeout"
        ));
    }

    use tempfile::TempDir;

    fn config() -> VergerailConfig {
        VergerailConfig {
            protocol_schema: PROVIDER_PROTOCOL_SCHEMA.to_owned(),
            executable: PathBuf::from("/tmp/vergerail-provider"),
            executable_sha256: "a".repeat(64),
            runtime_package: PathBuf::from("/tmp/vergerail-runtime"),
            managed_home: PathBuf::from("/tmp/vergerail-home"),
            model: "gpt-5.6-luna".to_owned(),
            workspace: PathBuf::from("/tmp/vergerail-workspace"),
        }
    }

    fn turn() -> ModelTurn {
        ModelTurn {
            messages: vec![ModelMessage::text(MessageRole::User, "hello")],
            tools: Vec::new(),
            reasoning: ReasoningEffort::Medium,
            timeout_ms: 60_000,
            maximum_response_bytes: 1024 * 1024,
            metadata: BTreeMap::new(),
        }
    }

    #[test]
    fn config_has_only_explicit_vergerail_route_fields() {
        assert!(validate_config(&config()).is_ok());
        assert!(validate_supported_provider_id("vergerail").is_ok());
        assert!(validate_supported_provider_id("openai").is_err());
        let mut invalid = config();
        invalid.workspace = PathBuf::from("relative");
        assert!(validate_config(&invalid).is_err());
        let mut retired = serde_json::to_value(config()).expect("serialize config");
        assert!(retired.get("owner").is_none());
        retired["owner"] = serde_json::json!("upagent");
        assert!(serde_json::from_value::<VergerailConfig>(retired).is_err());
    }

    #[test]
    fn canonical_protocol_is_required_for_all_provider_operations() {
        assert!(validate_config(&config()).is_ok());
        let mut retired = config();
        retired.protocol_schema = "vergerail.provider/1".to_owned();
        assert!(validate_config(&retired).is_err());
    }

    #[test]
    fn declared_binding_is_pure_route_contract_while_resolution_checks_executable_identity() {
        let mut config = config();
        config.executable = PathBuf::from("/definitely-not-existing-upagent-vergerail-provider");
        let binding = declared_binding(&config).expect("declared provider binding");
        assert_eq!(binding.id, "vergerail");
        assert_eq!(binding.model, config.model);
        assert!(binding.supports(PROVIDER_CAP_TEXT));
        assert!(resolve_binding(&config).is_err());
    }

    #[test]
    fn model_turn_bounds_and_role_validation_fail_closed() {
        assert!(validate_turn(&turn()).is_ok());
        let mut invalid = turn();
        invalid.messages.clear();
        assert!(validate_turn(&invalid).is_err());
        invalid = turn();
        invalid.messages[0].content = "x".repeat(MAXIMUM_MODEL_MESSAGE_BYTES + 1);
        assert!(validate_turn(&invalid).is_err());
        invalid = turn();
        invalid.messages[0].role = MessageRole::User;
        invalid.messages[0].tool_calls.push(ToolCall {
            id: ToolCallId::parse("call-1").unwrap(),
            name: "tool".to_owned(),
            arguments: serde_json::json!({}),
        });
        assert!(validate_turn(&invalid).is_err());
    }

    #[test]
    fn model_response_bound_matches_default_budget_and_has_an_explicit_edge() {
        assert_eq!(
            TaskBudget::default().maximum_output_bytes,
            MAXIMUM_MODEL_RESPONSE_BYTES
        );

        let mut exact = turn();
        exact.maximum_response_bytes = MAXIMUM_MODEL_RESPONSE_BYTES;
        assert!(validate_turn(&exact).is_ok());

        let mut over = turn();
        over.maximum_response_bytes = MAXIMUM_MODEL_RESPONSE_BYTES + 1;
        assert!(validate_turn(&over).is_err());
    }

    #[test]
    fn strict_failure_and_success_envelopes_reject_drift() {
        let failure_json = br#"{"schemaVersion":2,"requestId":"x","ok":false,"error":{"code":"resolution_required","message":"unknown","retryable":false}}"#;
        assert!(matches!(
            decode_success::<ModelResponse>(failure_json, WireOperation::ModelTurn, "x"),
            Err(ProviderError::Failure(failure)) if failure.code == FailureCode::ResolutionRequired
        ));
        let wrong_failure_id = br#"{"schemaVersion":2,"requestId":"other","ok":false,"error":{"code":"resolution_required","message":"unknown","retryable":false}}"#;
        assert!(
            decode_success::<ModelResponse>(wrong_failure_id, WireOperation::ModelTurn, "x")
                .is_err()
        );
        let unknown = br#"{"schemaVersion":2,"requestId":"x","operation":"model_turn","text":"","toolCalls":[],"usage":null,"extra":1}"#;
        assert!(decode_success::<ModelResponse>(unknown, WireOperation::ModelTurn, "x").is_err());
    }

    fn turn_with_tool() -> ModelTurn {
        let mut value = turn();
        value.tools.push(ToolDefinition {
            name: "fake_tool".to_owned(),
            description: "a bounded fake tool".to_owned(),
            input_schema: serde_json::json!({"type":"object"}),
            strict: true,
        });
        value
    }

    #[tokio::test]
    async fn exact_decoded_text_bound_survives_json_escaping_and_bounded_tool_calls() {
        let raw = serde_json::to_vec(&serde_json::json!({
            "schemaVersion": 2,
            "requestId": "exact-bound",
            "operation": "model_turn",
            "text": "\0".repeat(MAXIMUM_MODEL_RESPONSE_BYTES),
            "toolCalls": [{
                "id": "call-bounded",
                "name": "fake_tool",
                "arguments": {"value": "bounded"}
            }],
            "usage": null
        }))
        .expect("encode exact-bound response");
        assert!(raw.len() > JSON_STRING_MAX_EXPANSION * MAXIMUM_MODEL_RESPONSE_BYTES);
        assert!(raw.len() <= MAXIMUM_MODEL_FRAME_BYTES);

        let bounded = read_bounded(Cursor::new(raw), MAXIMUM_MODEL_FRAME_BYTES + 1)
            .await
            .expect("escaped response must fit the model frame");
        let response: ModelResponse =
            decode_success(&bounded, WireOperation::ModelTurn, "exact-bound")
                .expect("strict response must decode");
        let mut request = turn_with_tool();
        request.maximum_response_bytes = MAXIMUM_MODEL_RESPONSE_BYTES;
        let result = model_result_from_response(response, &request)
            .expect("decoded text and bounded tool call must pass validation");
        assert_eq!(result.text.len(), MAXIMUM_MODEL_RESPONSE_BYTES);
        assert_eq!(result.tool_calls.len(), 1);
    }

    #[tokio::test]
    async fn model_frame_reader_rejects_one_byte_over_the_hard_bound() {
        let error = read_bounded(tokio::io::repeat(b'x'), MAXIMUM_MODEL_FRAME_BYTES + 1)
            .await
            .expect_err("the frame reader must reject one byte over its hard bound");
        assert!(error.contains("transport limit"));
    }

    #[tokio::test]
    async fn image_frame_reader_rejects_one_byte_over_its_hard_bound() {
        let error = read_bounded(tokio::io::repeat(b'x'), MAXIMUM_IMAGE_FRAME_BYTES + 1)
            .await
            .expect_err("the image frame reader must reject one byte over its hard bound");
        assert!(error.contains("transport limit"));
    }

    fn png_chunk(kind: [u8; 4], data: &[u8]) -> Vec<u8> {
        let mut chunk = Vec::with_capacity(12 + data.len());
        chunk.extend_from_slice(
            &u32::try_from(data.len())
                .expect("PNG fixture chunk fits u32")
                .to_be_bytes(),
        );
        chunk.extend_from_slice(&kind);
        chunk.extend_from_slice(data);
        let mut crc = 0xffff_ffffu32;
        for byte in chunk[4..].iter().copied() {
            crc ^= u32::from(byte);
            for _ in 0..8 {
                crc = if crc & 1 != 0 {
                    (crc >> 1) ^ 0xedb8_8320
                } else {
                    crc >> 1
                };
            }
        }
        chunk.extend_from_slice(&(!crc).to_be_bytes());
        chunk
    }

    fn valid_png_bytes(size: usize) -> Vec<u8> {
        let base = BASE64_STANDARD
            .decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII=")
            .expect("valid PNG fixture");
        let idat_type = base
            .windows(4)
            .position(|window| window == b"IDAT")
            .expect("IDAT chunk");
        let chunk_start = idat_type - 4;
        assert!(
            size >= base.len() + 16,
            "padded PNG fixture must fit ancillary chunk"
        );
        let padding_data_len = size - base.len() - 12;
        let mut padding = b"pad\0".to_vec();
        padding.resize(padding_data_len, b'x');
        let mut bytes = base[..chunk_start].to_vec();
        bytes.extend_from_slice(&png_chunk(*b"tEXt", &padding));
        bytes.extend_from_slice(&base[chunk_start..]);
        assert_eq!(bytes.len(), size);
        bytes
    }

    fn turn_with_observation(size: usize) -> (ModelTurn, Vec<ResolvedImageObservation>) {
        let bytes = valid_png_bytes(size);
        let digest = format!("{:x}", sha2::Sha256::digest(&bytes));
        let artifact = ArtifactRef {
            id: ArtifactId::parse(format!("art-{digest}-{}", "b".repeat(64))).expect("artifact id"),
            sha256: digest.clone(),
            media_type: "image/png".to_owned(),
            byte_length: size as u64,
            relative_path: format!("{}/{}-{}.png", &digest[..2], digest, "b".repeat(64)),
        };
        let mut value = turn();
        value.messages[0] =
            ModelMessage::text(MessageRole::User, "observe").with_content_parts(vec![
                ModelContentPart::ImageObservation {
                    image: ImageObservation {
                        artifact,
                        role: ImageObservationRole::Full,
                        detail: ImageObservationDetail::Auto,
                        caption: None,
                    },
                },
            ]);
        let observations = vec![ResolvedImageObservation {
            message_index: 0,
            part_index: 0,
            media_type: "image/png".to_owned(),
            sha256: digest,
            width: 1,
            height: 1,
            role: ImageObservationRole::Full,
            bytes,
        }];
        (value, observations)
    }

    fn turn_with_observations(sizes: &[usize]) -> (ModelTurn, Vec<ResolvedImageObservation>) {
        let mut value = turn();
        value.messages[0].content_parts.clear();
        let mut observations = Vec::with_capacity(sizes.len());
        for (part_index, size) in sizes.iter().copied().enumerate() {
            let (extra_turn, mut extra) = turn_with_observation(size);
            value.messages[0]
                .content_parts
                .extend(extra_turn.messages[0].content_parts.clone());
            extra[0].part_index = part_index;
            observations.extend(extra);
        }
        (value, observations)
    }

    #[cfg(unix)]
    fn tiny_observation_turn() -> (ModelTurn, Vec<ResolvedImageObservation>) {
        let bytes = BASE64_STANDARD
            .decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII=")
            .expect("valid PNG");
        let digest = format!("{:x}", sha2::Sha256::digest(&bytes));
        let artifact = ArtifactRef {
            id: ArtifactId::parse(format!("art-{digest}-{}", "b".repeat(64))).expect("artifact id"),
            sha256: digest.clone(),
            media_type: "image/png".to_owned(),
            byte_length: bytes.len() as u64,
            relative_path: format!("{}/{}-{}.png", &digest[..2], digest, "b".repeat(64)),
        };
        let mut value = turn();
        value.messages[0] =
            ModelMessage::text(MessageRole::User, "observe").with_content_parts(vec![
                ModelContentPart::ImageObservation {
                    image: ImageObservation {
                        artifact,
                        role: ImageObservationRole::Full,
                        detail: ImageObservationDetail::Auto,
                        caption: None,
                    },
                },
            ]);
        let observations = vec![ResolvedImageObservation {
            message_index: 0,
            part_index: 0,
            media_type: "image/png".to_owned(),
            sha256: digest,
            width: 1,
            height: 1,
            role: ImageObservationRole::Full,
            bytes,
        }];
        (value, observations)
    }

    #[test]
    fn v2_observation_request_base64_sizes_fit_the_64_mib_admission_bound() {
        for size in [512 * 1024, 1024 * 1024, 8 * 1024 * 1024, 32 * 1024 * 1024] {
            let (turn, observations) = if size == 32 * 1024 * 1024 {
                turn_with_observations(&[8 * 1024 * 1024; 4])
            } else {
                turn_with_observation(size)
            };
            validate_resolved_observations(&turn, &observations).expect("valid PNG observation");
            let wire = observations
                .iter()
                .map(wire_image_observation)
                .collect::<Result<Vec<_>, ProviderError>>()
                .expect("wire observation");
            let request = serialize_multimodal_request(&turn, "size-regression", &wire, None)
                .expect("serialize v2 request");
            admit_provider_request(&request).expect("request fits the provider frame");
            assert_eq!(
                serde_json::from_slice::<Value>(&request)
                    .expect("strict JSON")
                    .get("schemaVersion"),
                Some(&Value::from(2))
            );
            if size == 512 * 1024 {
                assert!(
                    request.len() > 512 * 1024,
                    "old request cap must not regress"
                );
            }
        }
    }

    #[test]
    fn provider_request_admission_rejects_one_byte_over_and_image_aggregate_overage() {
        let over_frame = vec![b'x'; MAXIMUM_PROVIDER_REQUEST_BYTES + 1];
        assert_eq!(
            admit_provider_request(&over_frame)
                .expect_err("one byte over the provider frame")
                .to_string(),
            "provider contract failed: provider request exceeds the 64 MiB JSONL frame bound"
        );

        let (aggregate_turn, observations) = turn_with_observations(&[8 * 1024 * 1024; 5]);
        let error = validate_resolved_observations(&aggregate_turn, &observations)
            .expect_err("aggregate image bound");
        assert!(error.to_string().contains("aggregate byte bound"));
    }

    #[test]
    fn tool_call_argument_bounds_reject_individual_and_aggregate_overages() {
        let individual = ModelResponse {
            schema_version: 2,
            request_id: "individual".to_owned(),
            operation: WireOperation::ModelTurn,
            text: String::new(),
            tool_calls: vec![ToolCall {
                id: ToolCallId::parse("call-individual").expect("call id"),
                name: "fake_tool".to_owned(),
                arguments: serde_json::json!({
                    "value": "x".repeat(MAXIMUM_TOOL_ARGUMENT_BYTES)
                }),
            }],
            usage: None,
        };
        let error = model_result_from_response(individual, &turn_with_tool())
            .expect_err("one over-sized tool argument must be rejected");
        assert!(matches!(
            error,
            ProviderError::Contract(message)
                if message.contains("tool-call argument outside")
        ));

        let arguments = serde_json::json!({
            "value": "x".repeat(MAXIMUM_TOOL_ARGUMENT_BYTES - 32)
        });
        let aggregate = ModelResponse {
            schema_version: 2,
            request_id: "aggregate".to_owned(),
            operation: WireOperation::ModelTurn,
            text: String::new(),
            tool_calls: (0..5)
                .map(|index| ToolCall {
                    id: ToolCallId::parse(format!("call-aggregate-{index}")).expect("call id"),
                    name: "fake_tool".to_owned(),
                    arguments: arguments.clone(),
                })
                .collect(),
            usage: None,
        };
        let error = model_result_from_response(aggregate, &turn_with_tool())
            .expect_err("aggregate tool arguments must be bounded");
        assert!(matches!(
            error,
            ProviderError::Contract(message)
                if message.contains("aggregate tool-call arguments outside")
        ));
    }

    #[cfg(unix)]
    fn fake_process_config() -> (TempDir, VergerailConfig) {
        let directory = TempDir::new().expect("temporary provider directory");
        let executable = directory.path().join("fake-vergerail-provider.sh");
        let script = r#"#!/bin/sh
mkdir -p "$CODEX_HOME"
if [ -z "${CODEX_HOME:-}" ]; then
  exit 43
fi
if [ "$VERGERAIL_MODEL" != "fake-serialization-barrier" ]; then
  if ! mkdir "$CODEX_HOME/inflight" 2>/dev/null; then
    exit 42
  fi
  trap 'rmdir "$CODEX_HOME/inflight" 2>/dev/null || true' EXIT
fi
IFS= read -r request
request_id=$(printf '%s' "$request" | sed -n 's/.*"requestId":"\([^"]*\)".*/\1/p')
operation=$(printf '%s' "$request" | sed -n 's/.*"operation":"\([^"]*\)".*/\1/p')
staging_root=$(printf '%s' "$request" | sed -n 's/.*"stagingRoot":"\([^\"]*\)".*/\1/p')
if [ "$VERGERAIL_MODEL" = "fake-staging-cancel" ]; then
  if [ -z "$staging_root" ]; then
    exit 44
  fi
  printf '%s' "$staging_root" > "$CODEX_HOME/staging-path"
  touch "$staging_root/provider-created"
  touch "$CODEX_HOME/staging-started"
  (sleep 0.4; touch "$staging_root/provider-survived"; sleep 30) &
  wait
fi
if [ "$VERGERAIL_MODEL" = "fake-serialization-barrier" ]; then
  if ! mkdir "$CODEX_HOME/inflight" 2>/dev/null; then
    touch "$CODEX_HOME/serialization-overlap"
    exit 42
  fi
  trap 'rmdir "$CODEX_HOME/inflight" 2>/dev/null || true' EXIT
  touch "$CODEX_HOME/serialization-started"
  while [ ! -f "$CODEX_HOME/serialization-release" ]; do
    sleep 0.01
  done
fi
sleep 0.05
if [ "$VERGERAIL_MODEL" = "fake-leader-exits-first" ]; then
  touch "$CODEX_HOME/leader-exits-first-started"
  (sleep 0.4; touch "$CODEX_HOME/leader-exits-first-survived"; sleep 30) &
  exit 0
elif [ "$VERGERAIL_MODEL" = "fake-descendant" ]; then
  touch "$CODEX_HOME/descendant-started"
  (sleep 0.4; touch "$CODEX_HOME/descendant-survived"; sleep 30) &
  wait
elif [ "$operation" = "image_generate" ]; then
  printf '%s\n' "{\"schemaVersion\":2,\"requestId\":\"$request_id\",\"operation\":\"image_generate\",\"image\":{\"mediaType\":\"image/png\",\"base64\":\"iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII=\",\"byteLength\":68,\"width\":1,\"height\":1,\"transparentBackground\":false}}"
elif printf '%s' "$request" | grep -q '"name":"fake_tool"'; then
  printf '%s\n' "{\"schemaVersion\":2,\"requestId\":\"$request_id\",\"operation\":\"model_turn\",\"text\":\"\",\"toolCalls\":[{\"id\":\"call-fake\",\"name\":\"fake_tool\",\"arguments\":{\"value\":1}}],\"usage\":{\"inputTokens\":2,\"outputTokens\":3,\"totalTokens\":5}}"
else
  printf '%s\n' "{\"schemaVersion\":2,\"requestId\":\"$request_id\",\"operation\":\"model_turn\",\"text\":\"fake model\",\"toolCalls\":[],\"usage\":{\"inputTokens\":2,\"outputTokens\":3,\"totalTokens\":5}}"
fi
"#;
        fs::write(&executable, script).expect("write fake provider");
        let mut permissions = fs::metadata(&executable)
            .expect("fake provider metadata")
            .permissions();
        permissions.set_mode(0o700);
        fs::set_permissions(&executable, permissions).expect("make fake provider executable");
        let digest = sha256_hex_for_test(&executable);
        let config = VergerailConfig {
            protocol_schema: PROVIDER_PROTOCOL_SCHEMA.to_owned(),
            executable,
            executable_sha256: digest,
            runtime_package: directory.path().join("runtime-package"),
            managed_home: directory.path().join("managed-home"),
            model: "fake-model".to_owned(),
            workspace: directory.path().join("workspace"),
        };
        (directory, config)
    }

    #[cfg(unix)]
    fn sha256_hex_for_test(path: &std::path::Path) -> String {
        format!(
            "{:x}",
            sha2::Sha256::digest(fs::read(path).expect("read fake provider"))
        )
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn fake_process_contract_supports_model_and_image_operations() {
        let (_directory, config) = fake_process_config();
        let provider = VergerailProvider::connect(config)
            .await
            .expect("connect fake provider");
        let model = provider.complete(&turn()).await.expect("model response");
        assert_eq!(model.text, "fake model");
        assert_eq!(model.usage.map(|usage| usage.total_tokens), Some(Some(5)));

        let image = provider
            .generate_image(
                "a bounded square",
                &ImageGenerationRequirements::default(),
                ReasoningEffort::XHigh,
                10_000,
                MAXIMUM_IMAGE_BYTES,
            )
            .await
            .expect("image response");
        assert_eq!(image.image.media_type, "image/png");
        assert_eq!((image.image.width, image.image.height), (1, 1));
        assert_eq!(image.image.bytes.len(), 68);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn fake_process_contract_accepts_resolved_multimodal_observation_under_v2() {
        let (_directory, mut config) = fake_process_config();
        config.protocol_schema = PROVIDER_PROTOCOL_SCHEMA.to_owned();
        let provider = VergerailProvider::connect(config)
            .await
            .expect("connect fake provider");
        let bytes = BASE64_STANDARD
            .decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII=")
            .expect("png");
        let digest = format!("{:x}", sha2::Sha256::digest(&bytes));
        let artifact = ArtifactRef {
            id: ArtifactId::parse(format!("art-{digest}-{}", "b".repeat(64))).expect("artifact id"),
            sha256: digest.clone(),
            media_type: "image/png".to_owned(),
            byte_length: bytes.len() as u64,
            relative_path: format!("{}/{}-{}.png", &digest[..2], digest, "b".repeat(64)),
        };
        let mut turn = turn();
        turn.messages.push(
            ModelMessage::tool_result(
                ToolCallId::parse("call-image-observe").expect("call id"),
                "image_generate",
                "{\"ok\":true}",
                false,
            )
            .with_content_parts(vec![ModelContentPart::ImageObservation {
                image: ImageObservation {
                    artifact,
                    role: ImageObservationRole::Full,
                    detail: ImageObservationDetail::Auto,
                    caption: Some("generated image".to_owned()),
                },
            }]),
        );
        let observations = vec![ResolvedImageObservation {
            message_index: 1,
            part_index: 0,
            media_type: "image/png".to_owned(),
            sha256: digest,
            width: 1,
            height: 1,
            role: ImageObservationRole::Full,
            bytes,
        }];
        let result = provider
            .complete_with_observations(&turn, &observations, &CancellationToken::new())
            .await
            .expect("multimodal model response");
        assert_eq!(result.text, "fake model");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn cancellation_reaps_provider_before_removing_exact_observation_staging_root() {
        let (_directory, mut config) = fake_process_config();
        config.model = "fake-staging-cancel".to_owned();
        let started = config.managed_home.join("staging-started");
        let root_path_file = config.managed_home.join("staging-path");
        let provider = VergerailProvider::connect(config)
            .await
            .expect("connect fake provider");
        let cancellation = CancellationToken::new();
        let task = {
            let cancellation = cancellation.clone();
            let (turn, observations) = tiny_observation_turn();
            tokio::spawn(async move {
                provider
                    .complete_with_observations(&turn, &observations, &cancellation)
                    .await
            })
        };
        // Allow the fixture to start under full-workspace process-test load.
        // The cancellation/reap deadline below remains unchanged.
        for _ in 0..500 {
            if started.is_file() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        if !started.is_file() {
            cancellation.cancel();
            let terminal = tokio::time::timeout(Duration::from_secs(2), task).await;
            panic!("provider must create the staging marker; cleanup={terminal:?}");
        }
        let root = PathBuf::from(fs::read_to_string(&root_path_file).expect("staging root path"));
        assert!(
            root.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with(STAGING_ROOT_PREFIX)),
            "provider must receive the exact request root"
        );
        assert!(root.join("provider-created").is_file());
        cancellation.cancel();
        let result = tokio::time::timeout(Duration::from_secs(2), task)
            .await
            .expect("cancellation must reap the provider")
            .expect("provider task must join");
        assert!(result.is_err(), "cancelled provider must not succeed");
        assert!(!root.exists(), "staging root must be absent after reaping");
        tokio::time::sleep(Duration::from_millis(500)).await;
        assert!(
            !root.exists(),
            "staging root must remain absent after delayed descendant cleanup"
        );
        assert!(
            !root.join("provider-survived").exists(),
            "nested provider descendant must not survive cancellation"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn shared_provider_clone_serializes_managed_home_processes() {
        let (_directory, config) = fake_process_config();
        let provider = VergerailProvider::connect(config)
            .await
            .expect("connect fake provider");
        let left = provider.clone();
        let right = provider.clone();
        let first_turn = turn();
        let second_turn = turn();
        let (first, second) =
            tokio::join!(left.complete(&first_turn), right.complete(&second_turn));
        assert!(first.is_ok(), "first process should complete: {first:?}");
        assert!(second.is_ok(), "second process should complete: {second:?}");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn separate_provider_instances_serialize_managed_home_processes() {
        let (_directory, mut config) = fake_process_config();
        config.model = "fake-serialization-barrier".to_owned();
        let started = config.managed_home.join("serialization-started");
        let overlap = config.managed_home.join("serialization-overlap");
        let release = config.managed_home.join("serialization-release");
        let left = VergerailProvider::connect(config.clone())
            .await
            .expect("connect first provider");
        let right = VergerailProvider::connect(config)
            .await
            .expect("connect second provider");
        let first_turn = turn();
        let second_turn = turn();
        let first_task = tokio::spawn(async move { left.complete(&first_turn).await });
        // Full-workspace verification runs several real process-boundary tests
        // concurrently.  This is test coordination, not the provider's public
        // operation timeout, so allow the fake process to reach its barrier
        // under host load without weakening any production deadline.
        for _ in 0..500 {
            if started.is_file() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(started.is_file(), "first provider must reach the barrier");
        let second_task = tokio::spawn(async move { right.complete(&second_turn).await });
        for _ in 0..500 {
            if overlap.is_file() || second_task.is_finished() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(
            !overlap.exists() && !second_task.is_finished(),
            "a second provider instance must wait on the shared home lock"
        );
        fs::write(&release, b"release").expect("release first provider");
        let first = tokio::time::timeout(Duration::from_secs(2), first_task)
            .await
            .expect("first provider must finish after release")
            .expect("first provider task must join");
        let second = tokio::time::timeout(Duration::from_secs(2), second_task)
            .await
            .expect("second provider must finish after first release")
            .expect("second provider task must join");
        assert!(first.is_ok(), "first process should complete: {first:?}");
        assert!(second.is_ok(), "second process should complete: {second:?}");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn leader_exit_first_descendant_is_cleaned_after_bounded_drain() {
        let (_directory, mut config) = fake_process_config();
        config.model = "fake-leader-exits-first".to_owned();
        let started = config.managed_home.join("leader-exits-first-started");
        let survived = config.managed_home.join("leader-exits-first-survived");
        let provider = VergerailProvider::connect(config)
            .await
            .expect("connect leader-exit-first provider");
        let result = provider.complete(&turn()).await;
        assert!(matches!(
            result,
            Err(ProviderError::Failure(failure))
                if failure.code == FailureCode::ProviderFailed
        ));
        assert!(started.is_file(), "leader fixture must reach dispatch");
        tokio::time::sleep(Duration::from_millis(600)).await;
        assert!(
            !survived.exists(),
            "a descendant retaining a pipe must be killed after leader exit"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn cancellation_kills_descendants_and_bounds_pipe_drain() {
        let (_directory, mut config) = fake_process_config();
        config.model = "fake-descendant".to_owned();
        let started = config.managed_home.join("descendant-started");
        let survived = config.managed_home.join("descendant-survived");
        let provider = VergerailProvider::connect(config)
            .await
            .expect("connect fake descendant provider");
        let cancellation = CancellationToken::new();
        let task = tokio::spawn({
            let cancellation = cancellation.clone();
            async move {
                provider
                    .complete_with_cancellation(&turn(), &cancellation)
                    .await
            }
        });
        // This fixture crosses a real shell/process-group boundary. Allow the
        // child to reach its marker under a loaded verification host without
        // changing the provider's 60-second operation deadline.
        for _ in 0..500 {
            if started.is_file() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(started.is_file(), "descendant fixture must reach dispatch");
        cancellation.cancel();
        let result = tokio::time::timeout(Duration::from_secs(2), task)
            .await
            .expect("cancellation must not wait for descendant pipes")
            .expect("provider task must join");
        assert!(matches!(
            result,
            Err(ProviderError::Failure(failure))
                if failure.code == FailureCode::Cancelled
        ));
        tokio::time::sleep(Duration::from_millis(600)).await;
        assert!(
            !survived.exists(),
            "process-group cleanup must kill the descendant before it writes"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn fake_process_round_trips_a_typed_tool_call() {
        let (_directory, config) = fake_process_config();
        let provider = VergerailProvider::connect(config)
            .await
            .expect("connect fake provider");
        let mut request = turn();
        request.tools.push(ToolDefinition {
            name: "fake_tool".to_owned(),
            description: "a typed fake tool".to_owned(),
            input_schema: serde_json::json!({"type":"object"}),
            strict: true,
        });
        let result = provider.complete(&request).await.expect("tool response");
        assert_eq!(result.text, "");
        assert_eq!(result.tool_calls.len(), 1);
        assert_eq!(result.tool_calls[0].name, "fake_tool");
        assert_eq!(
            result.tool_calls[0].arguments,
            serde_json::json!({"value": 1})
        );
    }

    #[test]
    fn assistant_tool_call_turns_allow_empty_text_but_other_empty_messages_fail() {
        let mut with_tool = turn();
        with_tool.messages[0] = ModelMessage::assistant_with_tools(
            "",
            vec![ToolCall {
                id: ToolCallId::parse("call-1").expect("call id"),
                name: "fake_tool".to_owned(),
                arguments: serde_json::json!({"value": 1}),
            }],
        );
        assert!(validate_turn(&with_tool).is_ok());
        let mut empty_user = turn();
        empty_user.messages[0].content.clear();
        assert!(validate_turn(&empty_user).is_err());
        let mut misplaced_ids = turn();
        misplaced_ids.messages[0].tool_call_id = Some(ToolCallId::parse("call-1").expect("id"));
        assert!(validate_turn(&misplaced_ids).is_err());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn cancellation_before_process_dispatch_is_terminal_cancelled() {
        let (_directory, config) = fake_process_config();
        let provider = VergerailProvider::connect(config)
            .await
            .expect("connect fake provider");
        let cancellation = CancellationToken::new();
        cancellation.cancel();
        let error = provider
            .generate_image_with_cancellation(
                "a bounded square",
                &ImageGenerationRequirements::default(),
                ReasoningEffort::Max,
                10_000,
                1024,
                &cancellation,
            )
            .await
            .expect_err("cancelled call");
        assert!(matches!(
            error,
            ProviderError::Failure(failure) if failure.code == FailureCode::Cancelled
        ));
    }
}
