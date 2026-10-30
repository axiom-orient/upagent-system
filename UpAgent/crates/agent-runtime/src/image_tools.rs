//! Creator-scoped image generation, inspection, and deterministic editing.
//!
//! Image bytes cross the runtime boundary only as content-addressed artifacts.
//! `PerfectPixel` is an explicit executable identity, not an ambient command;
//! every invocation rechecks its executable digest and published protocol
//! capability identity.

use std::{
    collections::BTreeSet,
    fs,
    path::{Component, Path, PathBuf},
    sync::Arc,
    time::Duration,
};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

use agent_core::{
    AGENT_IMAGE_BEHAVIOR_VERSION, AGENT_IMAGE_CAPABILITY_MANIFEST_DIGEST_DOMAIN,
    AGENT_IMAGE_PROTOCOL_SCHEMA, AGENT_IMAGE_PROTOCOL_VERSION, AgentFailure, ArtifactDependencyV2,
    ArtifactKindV2, ArtifactRef, ArtifactRetention, CapabilityId, DeterminismClassV2,
    EffectReceiptRef, FailureCode, IMAGE_VERIFICATION_SPEC_SCHEMA, ImageCompareArtifactKind,
    ImageCompareAssertion, ImageCompareAssertionResult, ImageCompareRect, ImageCompareSeverity,
    ImageRepairObservation, ImageRepairRequirement, ImageVerificationSpec, OperationReceiptV2,
    OperationStatusV2, PixelSpec, ProfileId, ProviderBinding, ReceiptId, SemanticMaskResultStatus,
    ToolCall, ToolDefinition, ToolResult, artifact_dependency_closure_sha256,
};
use agent_provider_api::{
    ImageBackgroundRequirement, ImageGenerationMismatch, ImageGenerationRequirements, ImagePort,
    ImageQualityRequirement, ImageSizeRequirement, ProviderError, ProviderImage,
    ProviderImageStatus,
};
use async_trait::async_trait;
use serde::{Deserialize, Serialize, de::Deserializer};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;

use crate::semantic_mask::{
    SEMANTIC_MASK_CAPABILITY, SemanticMaskAdapter, SemanticMaskRequest, SemanticMaskTarget,
    UnavailableSemanticMaskAdapter,
};
use crate::{
    ModuleArtifact, ModuleExecution, ModuleTool, ToolContext, ToolModule, ToolModuleError,
    ToolSideEffect,
};

pub const IMAGE_TOOL_SCHEMA: &str = "upagent.image-tools/1";
pub const PERFECTPIXEL_AGENT_PROTOCOL_SCHEMA: &str = AGENT_IMAGE_PROTOCOL_SCHEMA;
pub const PERFECTPIXEL_AGENT_INSPECT_REQUEST_SCHEMA: &str = "perfectpixel.agent-image/inspect/2";
pub const PERFECTPIXEL_AGENT_INSPECT_RESULT_SCHEMA: &str =
    "perfectpixel.agent-image/inspect-result/2";
pub const PERFECTPIXEL_AGENT_EXTRACT_REQUEST_SCHEMA: &str = "perfectpixel.agent-image/extract/2";
pub const PERFECTPIXEL_AGENT_EXTRACT_RESULT_SCHEMA: &str =
    "perfectpixel.agent-image/extract-result/2";
pub const PERFECTPIXEL_AGENT_RENDER_REQUEST_SCHEMA: &str = "perfectpixel.agent-image/render/2";
pub const PERFECTPIXEL_AGENT_RENDER_RESULT_SCHEMA: &str =
    "perfectpixel.agent-image/render-result/2";
pub const PERFECTPIXEL_AGENT_COMPARE_REQUEST_SCHEMA: &str = "perfectpixel.agent-image/compare/2";
pub const PERFECTPIXEL_AGENT_COMPARE_RESULT_SCHEMA: &str =
    "perfectpixel.agent-image/compare-result/2";
pub const PERFECTPIXEL_AGENT_RECEIPT_SCHEMA: &str = "perfectpixel.agent-image/receipt/2";
pub const UPAGENT_IMAGE_RECEIPT_SCHEMA: &str = "upagent.image-operation-receipt/2";
pub use agent_core::{
    IMAGE_COMPARE_TOOL, IMAGE_EDIT_SEMANTIC_TOOL, IMAGE_EXTRACT_TOOL, IMAGE_GENERATE_TOOL,
    IMAGE_INSPECT_TOOL, IMAGE_RENDER_TOOL,
};
mod definitions;
mod perfectpixel_process;
mod semantic_edit;

pub(crate) use definitions::{artifact_ref_schema, prompt_input_artifact_ref_schema};
use definitions::{
    compare_definition, extract_definition, generate_definition, inspect_definition,
    render_definition,
};
use perfectpixel_process::{PerfectPixelOutput, run_perfectpixel};
const MAX_IMAGE_BYTES: usize = 8 * 1024 * 1024;
const MAX_FONT_BYTES: usize = 32 * 1024 * 1024;
const MAX_CLI_OUTPUT_BYTES: usize = 2 * 1024 * 1024;
const MAX_TOOL_PROMPT_BYTES: usize = 128 * 1024;
const INSPECTION_METRIC_TOLERANCE: f64 = 1e-6;
const BASE_AGENT_CAPABILITIES: [(&str, &str, &str); 4] = [
    ("inspect.basic", "2.0.0", "bit_exact"),
    ("artifact.content_addressed", "2.0.0", "bit_exact"),
    ("artifact.dependency_closure", "2.0.0", "bit_exact"),
    ("artifact.pin_set", "2.0.0", "bit_exact"),
];

struct ResolvedPerfectPixelIdentity {
    executable: PathBuf,
    executable_sha256: String,
    protocol_schema: String,
    protocol_version: String,
    behavior_version: String,
    capability_manifest_sha256: String,
    implementation_version: String,
}

impl ResolvedPerfectPixelIdentity {
    fn resolve(
        executable: impl Into<PathBuf>,
        executable_sha256: impl Into<String>,
        protocol_schema: impl Into<String>,
        protocol_version: impl Into<String>,
        behavior_version: impl Into<String>,
        capability_manifest_sha256: impl Into<String>,
        implementation_version: impl Into<String>,
    ) -> Result<Self, String> {
        let executable = executable.into();
        validate_executable_path(&executable)?;
        let executable = executable
            .canonicalize()
            .map_err(|error| format!("cannot canonicalize PerfectPixel executable: {error}"))?;
        let observed = digest_file(&executable)?;
        let expected = executable_sha256.into();
        if !is_sha256(&expected) || !observed.eq_ignore_ascii_case(&expected) {
            return Err(format!(
                "PerfectPixel executable digest mismatch: expected {expected}, observed {observed}"
            ));
        }
        let protocol_schema =
            bounded_string(protocol_schema.into(), "PerfectPixel protocol schema", 128)?;
        let protocol_version =
            bounded_string(protocol_version.into(), "PerfectPixel protocol version", 64)?;
        let behavior_version =
            bounded_string(behavior_version.into(), "PerfectPixel behavior version", 64)?;
        let implementation_version = bounded_string(
            implementation_version.into(),
            "PerfectPixel implementation version",
            64,
        )?;
        let manifest_sha256 = capability_manifest_sha256.into();
        if protocol_schema != AGENT_IMAGE_PROTOCOL_SCHEMA
            || protocol_version != AGENT_IMAGE_PROTOCOL_VERSION
            || behavior_version != AGENT_IMAGE_BEHAVIOR_VERSION
            || !is_sha256(&manifest_sha256)
        {
            return Err("unsupported PerfectPixel agent protocol identity".to_owned());
        }
        Ok(Self {
            executable,
            executable_sha256: expected.to_ascii_lowercase(),
            protocol_schema,
            protocol_version,
            behavior_version,
            capability_manifest_sha256: manifest_sha256.to_ascii_lowercase(),
            implementation_version,
        })
    }
}

fn validate_provider_binding(
    provider: &dyn ImagePort,
    expected_provider: &ProviderBinding,
) -> Result<ProviderBinding, String> {
    expected_provider
        .validate()
        .map_err(|error| format!("expected image provider binding is invalid: {error}"))?;
    let provider_binding = provider.binding();
    provider_binding
        .validate()
        .map_err(|error| format!("image provider binding is invalid: {error}"))?;
    if &provider_binding != expected_provider {
        return Err(
            "image provider binding does not match the provider sealed by composition".to_owned(),
        );
    }
    Ok(provider_binding)
}

#[derive(Clone)]
pub struct ImageToolModule {
    provider: Arc<dyn ImagePort>,
    provider_binding: ProviderBinding,
    perfectpixel_executable: PathBuf,
    perfectpixel_sha256: String,
    protocol_schema: String,
    protocol_version: String,
    behavior_version: String,
    capability_manifest_sha256: String,
    implementation_version: String,
    semantic_mask_adapter: Arc<dyn SemanticMaskAdapter>,
    promptgen: Option<Arc<crate::PromptGenAdapter>>,
    tools: Vec<ModuleTool>,
}

impl std::fmt::Debug for ImageToolModule {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ImageToolModule")
            .field("perfectpixel_executable", &self.perfectpixel_executable)
            .field("perfectpixel_sha256", &self.perfectpixel_sha256)
            .field("protocol_schema", &self.protocol_schema)
            .field("protocol_version", &self.protocol_version)
            .field("behavior_version", &self.behavior_version)
            .field(
                "capability_manifest_sha256",
                &self.capability_manifest_sha256,
            )
            .field("implementation_version", &self.implementation_version)
            .finish_non_exhaustive()
    }
}

impl ImageToolModule {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        provider: Arc<dyn ImagePort>,
        expected_provider: &ProviderBinding,
        perfectpixel_executable: impl Into<PathBuf>,
        perfectpixel_sha256: impl Into<String>,
        protocol_schema: impl Into<String>,
        protocol_version: impl Into<String>,
        behavior_version: impl Into<String>,
        capability_manifest_sha256: impl Into<String>,
        implementation_version: impl Into<String>,
    ) -> Result<Self, String> {
        let identity = ResolvedPerfectPixelIdentity::resolve(
            perfectpixel_executable,
            perfectpixel_sha256,
            protocol_schema,
            protocol_version,
            behavior_version,
            capability_manifest_sha256,
            implementation_version,
        )?;
        let provider_binding = validate_provider_binding(provider.as_ref(), expected_provider)?;
        Self::from_parts(
            provider,
            provider_binding,
            identity,
            Arc::new(UnavailableSemanticMaskAdapter),
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn new_with_semantic_mask_adapter(
        provider: Arc<dyn ImagePort>,
        expected_provider: &ProviderBinding,
        perfectpixel_executable: impl Into<PathBuf>,
        perfectpixel_sha256: impl Into<String>,
        protocol_schema: impl Into<String>,
        protocol_version: impl Into<String>,
        behavior_version: impl Into<String>,
        capability_manifest_sha256: impl Into<String>,
        implementation_version: impl Into<String>,
        semantic_mask_adapter: Arc<dyn SemanticMaskAdapter>,
    ) -> Result<Self, String> {
        let provider_binding = validate_provider_binding(provider.as_ref(), expected_provider)?;
        let identity = ResolvedPerfectPixelIdentity::resolve(
            perfectpixel_executable,
            perfectpixel_sha256,
            protocol_schema,
            protocol_version,
            behavior_version,
            capability_manifest_sha256,
            implementation_version,
        )?;
        Self::from_parts(provider, provider_binding, identity, semantic_mask_adapter)
    }

    fn from_parts(
        provider: Arc<dyn ImagePort>,
        provider_binding: ProviderBinding,
        identity: ResolvedPerfectPixelIdentity,
        semantic_mask_adapter: Arc<dyn SemanticMaskAdapter>,
    ) -> Result<Self, String> {
        let tools = canonical_module_tools()?;
        Ok(Self {
            provider,
            provider_binding,
            perfectpixel_executable: identity.executable,
            perfectpixel_sha256: identity.executable_sha256,
            protocol_schema: identity.protocol_schema,
            protocol_version: identity.protocol_version,
            behavior_version: identity.behavior_version,
            capability_manifest_sha256: identity.capability_manifest_sha256,
            implementation_version: identity.implementation_version,
            semantic_mask_adapter,
            promptgen: None,
            tools,
        })
    }

    /// Register semantic editing only when explicitly selected by composition.
    /// Merely supplying a compiler must never widen the executable surface.
    pub fn with_promptgen(
        mut self,
        adapter: Option<Arc<crate::PromptGenAdapter>>,
        selected: &BTreeSet<String>,
    ) -> Result<Self, String> {
        if selected.contains(IMAGE_EDIT_SEMANTIC_TOOL) {
            if adapter.is_none() {
                return Err("image_edit_semantic requires a pinned promptGen adapter".to_owned());
            }
            self.tools.push(semantic_edit::module_tool()?);
        }
        self.tools
            .retain(|tool| selected.contains(&tool.definition.name));
        if self.tools.is_empty() {
            return Err("no image tool was explicitly selected".to_owned());
        }
        self.promptgen = adapter;
        Ok(self)
    }

    #[must_use]
    pub fn provider(&self) -> &Arc<dyn ImagePort> {
        &self.provider
    }

    #[must_use]
    pub fn executable(&self) -> &Path {
        &self.perfectpixel_executable
    }

    #[must_use]
    pub fn identity(&self) -> ImageToolIdentity {
        ImageToolIdentity {
            executable: self.perfectpixel_executable.display().to_string(),
            executable_sha256: self.perfectpixel_sha256.clone(),
            protocol_schema: self.protocol_schema.clone(),
            protocol_version: self.protocol_version.clone(),
            behavior_version: self.behavior_version.clone(),
            capability_manifest_sha256: self.capability_manifest_sha256.clone(),
            implementation_version: self.implementation_version.clone(),
        }
    }

    fn verify_perfectpixel_identity(&self) -> Result<(), ToolModuleError> {
        let observed =
            digest_file(&self.perfectpixel_executable).map_err(ToolModuleError::Contract)?;
        if !observed.eq_ignore_ascii_case(&self.perfectpixel_sha256) {
            return Err(ToolModuleError::Contract(
                "PerfectPixel executable changed after composition".to_owned(),
            ));
        }
        Ok(())
    }

    async fn verify_agent_manifest(
        &self,
        operation_capabilities: &[(&str, &str, &str)],
        timeout: Duration,
        cancellation: &CancellationToken,
    ) -> Result<(), ToolModuleError> {
        let output = self
            .run_perfectpixel(&["agent-schema".to_owned()], timeout, cancellation)
            .await?;
        let manifest: PerfectPixelAgentManifest =
            serde_json::from_slice(&output.stdout).map_err(|error| {
                ToolModuleError::Contract(format!(
                    "PerfectPixel agent manifest was not strict JSON: {error}"
                ))
            })?;
        let observed_manifest_sha256 =
            perfectpixel_manifest_sha256(&manifest).map_err(ToolModuleError::Contract)?;
        if manifest.schema != self.protocol_schema
            || manifest.protocol_version != self.protocol_version
            || manifest.behavior_version != self.behavior_version
            || manifest.implementation_version != self.implementation_version
            || observed_manifest_sha256 != self.capability_manifest_sha256
        {
            return Err(ToolModuleError::Contract(
                "PerfectPixel agent protocol identity changed after composition".to_owned(),
            ));
        }
        for (name, minimum_version, determinism) in BASE_AGENT_CAPABILITIES
            .iter()
            .chain(operation_capabilities.iter())
        {
            let Some(capability) = manifest
                .capabilities
                .iter()
                .find(|capability| capability.name == *name)
            else {
                return Err(ToolModuleError::Contract(format!(
                    "PerfectPixel missing required capability {name}"
                )));
            };
            if !compatible_semver_at_least(&capability.version, minimum_version)
                || capability.determinism != *determinism
            {
                return Err(ToolModuleError::Contract(format!(
                    "PerfectPixel capability {name} is incompatible"
                )));
            }
        }
        Ok(())
    }

    async fn run_perfectpixel(
        &self,
        args: &[String],
        timeout: Duration,
        cancellation: &CancellationToken,
    ) -> Result<PerfectPixelOutput, ToolModuleError> {
        // Schema probing and the requested operation are separate child
        // processes; each spawn therefore gets its own receipt-bound identity
        // check to close the executable TOCTOU window.
        self.verify_perfectpixel_identity()?;
        run_perfectpixel(&self.perfectpixel_executable, args, timeout, cancellation).await
    }

    async fn generate(
        &self,
        context: ToolContext,
        call: &ToolCall,
    ) -> Result<ModuleExecution, ToolModuleError> {
        if self.provider.binding() != self.provider_binding {
            return Err(ToolModuleError::Contract(
                "image provider binding changed after module admission".to_owned(),
            ));
        }
        let args: GenerateArguments = strict_arguments(call)?;
        let (prompt, requirements) = resolve_generation_prompt(
            &context,
            &args,
            self.promptgen.as_ref().map(|adapter| adapter.sha256()),
        )?;
        if prompt.trim().is_empty() || prompt.contains('\0') || prompt.len() > MAX_TOOL_PROMPT_BYTES
        {
            return Err(ToolModuleError::Contract(
                "image generation prompt exceeds the bounded limit".to_owned(),
            ));
        }
        let maximum_bytes = args.maximum_bytes.unwrap_or(MAX_IMAGE_BYTES);
        if maximum_bytes == 0 || maximum_bytes > MAX_IMAGE_BYTES {
            return Err(ToolModuleError::Contract(
                "image maximumBytes is outside the bounded limit".to_owned(),
            ));
        }
        let repair_requirement = exact_repair_requirement(requirements)?;
        let generated = self
            .provider
            .generate_image_with_cancellation(
                &prompt,
                &requirements,
                context.reasoning,
                context.timeout_ms,
                maximum_bytes,
                &context.cancellation,
            )
            .await
            .map_err(provider_module_error)?;
        generated
            .validate()
            .map_err(provider_module_error)
            .map_err(|error| generated_image_uncertainty(&error))?;
        let host_observation = self
            .validate_generated_image(
                &generated.image.bytes,
                &generated.image.media_type,
                (generated.image.width, generated.image.height),
                Duration::from_millis(context.timeout_ms),
                &context.cancellation,
            )
            .await
            .map_err(|error| generated_image_uncertainty(&error))?;
        let host_mismatches = image_generation_mismatches(
            requirements,
            host_observation.width,
            host_observation.height,
            host_observation.transparent_background,
        );
        let host_status = if host_mismatches.is_empty() {
            ProviderImageStatus::Generated
        } else {
            ProviderImageStatus::GeneratedNeedsRepair
        };
        let provider_mismatches = generated.mismatches;
        let provider_status = generated.status;
        let provider_observed_transparent_background = generated.observed_transparent_background;
        let provider_host_disagreement = provider_status != host_status
            || provider_mismatches != host_mismatches
            || provider_observed_transparent_background != host_observation.transparent_background;
        let image = generated.image;
        // Receipt verification proves a known, validated effect, not acceptance.
        // A valid raster that misses requested constraints remains repairable.
        let receipt = generation_receipt(call, &image)?;
        let operation_receipt = OperationReceiptV2 {
            schema: UPAGENT_IMAGE_RECEIPT_SCHEMA.to_owned(),
            request_id: format!("generate-{}", call.id),
            operation: "generate".to_owned(),
            status: OperationStatusV2::Committed,
            behavior_version: self.behavior_version.clone(),
            implementation_version: env!("CARGO_PKG_VERSION").to_owned(),
            request_sha256: sha256_hex(
                &serde_json::to_vec(&call.arguments)
                    .map_err(|error| ToolModuleError::Contract(error.to_string()))?,
            ),
            dependency_closure_sha256: artifact_dependency_closure_sha256(&[])
                .map_err(ToolModuleError::Contract)?,
            dependencies: Vec::new(),
            output_sha256: Some(receipt.digest.clone()),
            determinism: DeterminismClassV2::InferenceObserved,
        };
        let media_type = image.media_type;
        Ok(ModuleExecution {
            result: ToolResult {
                call_id: call.id.clone(),
                name: call.name.clone(),
                value: json!({
                    "schema": IMAGE_TOOL_SCHEMA,
                    "operation": "generate",
                    "mediaType": media_type,
                    "width": host_observation.width,
                    "height": host_observation.height,
                    "source": "provider_plugin",
                    "generationStatus": host_status,
                    "repairRequired": host_status == ProviderImageStatus::GeneratedNeedsRepair,
                    "repairRequirement": if host_status == ProviderImageStatus::GeneratedNeedsRepair { repair_requirement } else { None },
                    "repairStatus": if host_status == ProviderImageStatus::GeneratedNeedsRepair { "required" } else { "not_required" },
                    "agentMessage": if host_status == ProviderImageStatus::GeneratedNeedsRepair {
                        "이미지 생성만 성공했습니다. PerfectPixel 수리·검증이 필요합니다."
                    } else {
                        "이미지 생성 및 PerfectPixel 기본 검증이 완료되었습니다."
                    },
                    "observedTransparentBackground": host_observation.transparent_background,
                    "mismatches": host_mismatches,
                    "hostObservedWidth": host_observation.width,
                    "hostObservedHeight": host_observation.height,
                    "hostObservedTransparentBackground": host_observation.transparent_background,
                    "providerStatus": provider_status,
                    "providerObservedTransparentBackground": provider_observed_transparent_background,
                    "providerMismatches": provider_mismatches,
                    "providerHostDisagreement": provider_host_disagreement,
                    "contentSha256": receipt.digest,
                    "protocolVersion": self.protocol_version,
                    "behaviorVersion": self.behavior_version,
                    "artifactKind": "working_raster",
                    "pixelSpec": {"format":"rgba8","colorSpace":"srgb","alphaMode":"straight"},
                    "receipt": operation_receipt
                }),
                is_error: false,
                failure: None,
            },
            receipts: vec![receipt],
            artifacts: vec![ModuleArtifact {
                bytes: image.bytes,
                media_type,
                extension: "png".to_owned(),
            }],
        })
    }

    async fn validate_generated_image(
        &self,
        bytes: &[u8],
        media_type: &str,
        expected_dimensions: (u32, u32),
        timeout: Duration,
        cancellation: &CancellationToken,
    ) -> Result<GeneratedImageObservation, ToolModuleError> {
        if media_type != "image/png" || bytes.is_empty() || bytes.len() > MAX_IMAGE_BYTES {
            return Err(ToolModuleError::Failure(AgentFailure::new(
                FailureCode::ProviderFailed,
                "image provider returned an invalid bounded image artifact",
            )));
        }
        let (width, height) = validate_png(bytes)?;
        if expected_dimensions != (width, height) {
            return Err(ToolModuleError::Failure(AgentFailure::new(
                FailureCode::ProviderFailed,
                "image provider dimensions do not match its PNG payload",
            )));
        }
        self.verify_agent_manifest(&[], timeout, cancellation)
            .await?;
        let result = self
            .agent_inspect_bytes(
                bytes,
                &format!("generate-{}", sha256_hex(bytes)),
                timeout,
                cancellation,
            )
            .await?;
        let inspection = &result.inspection;
        validate_inspection_v2(inspection)?;
        if inspection.width != width || inspection.height != height {
            return Err(ToolModuleError::Failure(AgentFailure::new(
                FailureCode::ProviderFailed,
                "PerfectPixel did not validate the generated PNG dimensions",
            )));
        }
        Ok(GeneratedImageObservation {
            width: inspection.width,
            height: inspection.height,
            // `hasAlpha` is computed by PerfectPixel from decoded pixels and
            // means that at least one pixel is not fully opaque.  It is the
            // host's canonical transparent/opaque observation; provider
            // metadata is intentionally ignored for this decision.
            transparent_background: Some(inspection.has_alpha),
        })
    }

    async fn inspect(
        &self,
        context: ToolContext,
        call: &ToolCall,
    ) -> Result<ModuleExecution, ToolModuleError> {
        let args: InspectArguments = strict_arguments(call)?;
        let bytes = read_image_artifact(&context, &args.artifact)?;
        self.verify_agent_manifest(
            &[],
            Duration::from_millis(context.timeout_ms),
            &context.cancellation,
        )
        .await?;
        let result = self
            .agent_inspect_bytes(
                &bytes,
                &format!("inspect-{}", call.id),
                Duration::from_millis(context.timeout_ms),
                &context.cancellation,
            )
            .await?;
        let inspection = result.inspection;
        validate_inspection_v2(&inspection)?;
        if inspection.width == 0 || inspection.height == 0 {
            return Err(ToolModuleError::Failure(AgentFailure::new(
                FailureCode::ProviderFailed,
                "PerfectPixel returned an invalid inspection",
            )));
        }
        Ok(ModuleExecution {
            result: ToolResult {
                call_id: call.id.clone(),
                name: call.name.clone(),
                value: json!({
                    "schema": IMAGE_TOOL_SCHEMA,
                    "operation": "inspect",
                    "artifact": args.artifact,
                    "inspectionSchema": PERFECTPIXEL_AGENT_INSPECT_RESULT_SCHEMA,
                    "protocolVersion": result.protocol_version,
                    "behaviorVersion": result.behavior_version,
                    "inputSha256": result.source.sha256,
                    "width": inspection.width,
                    "height": inspection.height,
                    "foregroundPixels": inspection.foreground_pixels,
                    "alphaRatio": inspection.alpha_ratio,
                    "touchesEdge": inspection.touches_edge,
                    "contentBox": inspection.content_box,
                    "center": inspection.center,
                    "groundY": inspection.ground_y,
                    "hasAlpha": inspection.has_alpha,
                    "pixelFormat": inspection.pixel_format,
                    "colorSpace": inspection.color_space,
                    "edgePixelCount": inspection.edge_pixel_count,
                    "edgePalette": inspection.edge_palette,
                    "receipt": result.receipt,
                    "perfectpixel": {
                        "protocolSchema": self.protocol_schema,
                        "protocolVersion": self.protocol_version,
                        "behaviorVersion": self.behavior_version,
                        "implementationVersion": self.implementation_version,
                        "capabilityManifestSha256": self.capability_manifest_sha256,
                        "executableSha256": self.perfectpixel_sha256
                    }
                }),
                is_error: false,
                failure: None,
            },
            receipts: Vec::new(),
            artifacts: Vec::new(),
        })
    }

    async fn agent_inspect_bytes(
        &self,
        bytes: &[u8],
        request_id: &str,
        timeout: Duration,
        cancellation: &CancellationToken,
    ) -> Result<PerfectPixelAgentInspectResult, ToolModuleError> {
        let directory = private_temp_directory("agent-inspect")?;
        let input = directory.path().join("input.png");
        let request_path = directory.path().join("request.json");
        fs::write(&input, bytes).map_err(|error| {
            ToolModuleError::Failure(AgentFailure::new(
                FailureCode::StorageFailed,
                format!("agent inspect input could not be staged: {error}"),
            ))
        })?;
        let request = json!({
            "schema": PERFECTPIXEL_AGENT_INSPECT_REQUEST_SCHEMA,
            "requestId": request_id,
            "source": {
                "path": input.display().to_string(),
                "expectedSha256": sha256_hex(bytes),
                "mediaType": "image/png",
                "byteLength": bytes.len(),
                "kind": "working_raster",
                "pixelSpec": {"format":"rgba8","colorSpace":"srgb","alphaMode":"straight"}
            }
        });
        let encoded = serde_json::to_vec(&request).map_err(|error| {
            ToolModuleError::Contract(format!(
                "agent inspect request could not be encoded: {error}"
            ))
        })?;
        fs::write(&request_path, &encoded).map_err(|error| {
            ToolModuleError::Failure(AgentFailure::new(
                FailureCode::StorageFailed,
                format!("agent inspect request could not be staged: {error}"),
            ))
        })?;
        let output = self
            .run_perfectpixel(
                &[
                    "agent-inspect".to_owned(),
                    "--request".to_owned(),
                    request_path.display().to_string(),
                ],
                timeout,
                cancellation,
            )
            .await?;
        let result: PerfectPixelAgentInspectResult = serde_json::from_slice(&output.stdout)
            .map_err(|error| {
                ToolModuleError::Contract(format!(
                    "PerfectPixel agent inspect result was not strict JSON: {error}"
                ))
            })?;
        let dependency = ArtifactDependencyV2 {
            sha256: sha256_hex(bytes),
            media_type: "image/png".to_owned(),
            byte_length: u64::try_from(bytes.len()).map_err(|_| {
                ToolModuleError::Contract("agent inspect input length overflowed".to_owned())
            })?,
        };
        let expected_closure =
            artifact_dependency_closure_sha256(std::slice::from_ref(&dependency))
                .map_err(ToolModuleError::Contract)?;
        let expected_source_closure =
            artifact_dependency_closure_sha256(&[]).map_err(ToolModuleError::Contract)?;
        if result.schema != PERFECTPIXEL_AGENT_INSPECT_RESULT_SCHEMA
            || result.request_id != request_id
            || result.status != "committed"
            || result.protocol_version != self.protocol_version
            || result.behavior_version != self.behavior_version
            || result.source.sha256 != dependency.sha256
            || result.source.media_type != "image/png"
            || result.source.byte_length != dependency.byte_length
            || result.source.kind != ArtifactKindV2::WorkingRaster
            || result.source.pixel_spec != Some(PixelSpec::rgba8_srgb_straight())
            || result.source.retention != ArtifactRetention::TaskScoped
            || !result.source.dependencies.is_empty()
            || result.source.dependency_closure_sha256 != expected_source_closure
            || result.receipt.schema != PERFECTPIXEL_AGENT_RECEIPT_SCHEMA
            || result.receipt.request_id != request_id
            || result.receipt.operation != "inspect"
            || result.receipt.status != OperationStatusV2::Committed
            || result.receipt.behavior_version != self.behavior_version
            || result.receipt.implementation_version != self.implementation_version
            || result.receipt.request_sha256 != sha256_hex(&encoded)
            || result.receipt.dependencies != vec![dependency]
            || result.receipt.dependency_closure_sha256 != expected_closure
            || result.receipt.output_sha256.is_some()
            || result.receipt.determinism != DeterminismClassV2::BitExact
        {
            return Err(ToolModuleError::Contract(
                "PerfectPixel agent inspect result violated protocol v2".to_owned(),
            ));
        }
        Ok(result)
    }

    async fn extract(
        &self,
        context: ToolContext,
        call: &ToolCall,
    ) -> Result<ModuleExecution, ToolModuleError> {
        let args: ExtractArguments = strict_arguments(call)?;
        let semantic_target = match &args.selector {
            ExtractSelectorArguments::Semantic {
                source_revision,
                request_revision,
                helper_identity,
                target,
            } => {
                if args.mask_artifact.is_some() {
                    return Err(ToolModuleError::Contract(
                        "semantic selector cannot provide maskArtifact; the adapter owns mask production"
                            .to_owned(),
                    ));
                }
                target.validate().map_err(ToolModuleError::Contract)?;
                Some((
                    *source_revision,
                    *request_revision,
                    helper_identity.clone(),
                    target.clone(),
                ))
            }
            _ => None,
        };
        // SemanticMaskApply is deliberately owned by ImageDirector; this
        // module only returns the adapter Result event to that state owner.
        let source_bytes = read_image_artifact(&context, &args.artifact)?;
        let (selector, semantic_result, effective_mask_artifact) = if let Some((
            caller_source_revision,
            caller_request_revision,
            caller_helper_identity,
            target,
        )) = semantic_target
        {
            let operation = context.semantic_mask_operation.clone().ok_or_else(|| {
                ToolModuleError::Contract(
                    "semantic extraction has no authoritative pending operation".to_owned(),
                )
            })?;
            if operation.source_artifact != args.artifact
                || operation.source_revision != caller_source_revision
                || operation.request_revision != caller_request_revision
                || operation.helper_identity != caller_helper_identity
                || operation.tool_call_id != call.id
            {
                return Err(ToolModuleError::Contract(
                    "semantic extraction identity does not match authoritative task state"
                        .to_owned(),
                ));
            }
            let result = self
                .semantic_mask_adapter
                .infer(
                    SemanticMaskRequest {
                        operation: operation.clone(),
                        target,
                    },
                    &context.cancellation,
                )
                .await;
            result.validate().map_err(ToolModuleError::Contract)?;
            if result.operation != operation {
                return Err(ToolModuleError::Failure(
                    AgentFailure::new(
                        FailureCode::InvalidTransition,
                        "semantic mask adapter returned a result for a different operation",
                    )
                    .with_detail("operation", "semantic_mask")
                    .with_detail("toolCallId", call.id.as_str()),
                ));
            }
            match result.status {
                SemanticMaskResultStatus::Succeeded => {
                    let mask = result.mask_artifact.clone().ok_or_else(|| {
                        ToolModuleError::Contract(
                            "semantic adapter success omitted maskArtifact".to_owned(),
                        )
                    })?;
                    (
                        ExtractSelectorArguments::ProvidedMask,
                        Some(result),
                        Some(mask),
                    )
                }
                SemanticMaskResultStatus::Failed | SemanticMaskResultStatus::Cancelled => {
                    let failure = result.failure.clone().ok_or_else(|| {
                        ToolModuleError::Contract(
                            "semantic mask adapter failure omitted its cause".to_owned(),
                        )
                    })?;
                    return Err(ToolModuleError::Failure(failure));
                }
            }
        } else {
            (args.selector.clone(), None, args.mask_artifact.clone())
        };
        self.verify_agent_manifest(
            &[
                ("extract.alpha", "1.0.0", "bit_exact"),
                (selector_capability(&selector), "1.0.0", "bit_exact"),
                ("extract.matte_refine", "1.0.0", "bit_exact"),
            ],
            Duration::from_millis(context.timeout_ms),
            &context.cancellation,
        )
        .await?;
        let mask_bytes = effective_mask_artifact
            .as_ref()
            .map(|artifact| read_image_artifact(&context, artifact))
            .transpose()?;
        if matches!(&selector, ExtractSelectorArguments::ProvidedMask) != mask_bytes.is_some() {
            return Err(ToolModuleError::Contract(
                "provided_mask selector requires exactly one maskArtifact".to_owned(),
            ));
        }
        let directory = private_temp_directory("extract")?;
        let source_path = directory.path().join("source.png");
        let request_path = directory.path().join("request.json");
        let output_dir = directory.path().join("out");
        fs::write(&source_path, &source_bytes)
            .map_err(storage_failure("image extract source could not be staged"))?;
        let mask_input = if let (Some(artifact), Some(bytes)) =
            (effective_mask_artifact.as_ref(), mask_bytes.as_ref())
        {
            let path = directory.path().join("mask.png");
            fs::write(&path, bytes)
                .map_err(storage_failure("image extract mask could not be staged"))?;
            Some(json!({
                "path": path.display().to_string(),
                "expectedSha256": artifact.sha256,
                "mediaType": "image/png",
                "byteLength": artifact.byte_length,
                "kind": "mask",
                "pixelSpec": {"format":"rgba8","colorSpace":"srgb","alphaMode":"straight"}
            }))
        } else {
            None
        };
        let request = json!({
            "schema": PERFECTPIXEL_AGENT_EXTRACT_REQUEST_SCHEMA,
            "requestId": format!("extract-{}", call.id),
            "source": agent_input_json(&args.artifact, &source_path, "working_raster"),
            "selector": selector,
            "mask": mask_input,
            "refinement": args.refinement,
            "includeRemainder": args.include_remainder
        });
        let encoded = serde_json::to_vec(&request).map_err(|error| {
            ToolModuleError::Contract(format!(
                "image extract request could not be encoded: {error}"
            ))
        })?;
        fs::write(&request_path, &encoded)
            .map_err(storage_failure("image extract request could not be staged"))?;
        let output = self
            .run_perfectpixel(
                &[
                    "agent-extract".to_owned(),
                    "--request".to_owned(),
                    request_path.display().to_string(),
                    "--out-dir".to_owned(),
                    output_dir.display().to_string(),
                ],
                Duration::from_millis(context.timeout_ms),
                &context.cancellation,
            )
            .await?;
        let result: PerfectPixelAgentExtractResult = serde_json::from_slice(&output.stdout)
            .map_err(|error| {
                ToolModuleError::Contract(format!(
                    "PerfectPixel extract result was not strict JSON: {error}"
                ))
            })?;
        let mut expected_dependencies = vec![ArtifactDependencyV2::from_ref(&args.artifact)];
        if let Some(mask) = effective_mask_artifact.as_ref() {
            expected_dependencies.push(ArtifactDependencyV2::from_ref(mask));
        }
        expected_dependencies.sort();
        expected_dependencies.dedup();
        let expected_closure = artifact_dependency_closure_sha256(&expected_dependencies)
            .map_err(ToolModuleError::Contract)?;
        if result.schema != PERFECTPIXEL_AGENT_EXTRACT_RESULT_SCHEMA
            || result.request_id != format!("extract-{}", call.id)
            || result.status != "committed"
            || result.protocol_version != self.protocol_version
            || result.behavior_version != self.behavior_version
            || result.receipt.schema != PERFECTPIXEL_AGENT_RECEIPT_SCHEMA
            || result.receipt.request_id != result.request_id
            || result.receipt.operation != "extract"
            || result.receipt.status != OperationStatusV2::Committed
            || result.receipt.behavior_version != self.behavior_version
            || result.receipt.implementation_version != self.implementation_version
            || result.receipt.request_sha256 != sha256_hex(&encoded)
            || result.receipt.dependencies != expected_dependencies
            || result.receipt.dependency_closure_sha256 != expected_closure
            || result.receipt.output_sha256.as_deref()
                != Some(result.object.descriptor.sha256.as_str())
            || result.receipt.determinism != DeterminismClassV2::BitExact
        {
            return Err(ToolModuleError::Contract(
                "PerfectPixel extract result violated protocol v2".to_owned(),
            ));
        }
        let object = read_bounded_file(
            &output_dir.join(&result.object.relative_path),
            MAX_IMAGE_BYTES,
            "extract object",
        )?;
        let mask = read_bounded_file(
            &output_dir.join(&result.mask.relative_path),
            MAX_IMAGE_BYTES,
            "extract mask",
        )?;
        validate_published_descriptor(&result.object.descriptor, &object, ArtifactKindV2::Object)?;
        validate_published_descriptor(&result.mask.descriptor, &mask, ArtifactKindV2::Mask)?;
        if result.object.descriptor.dependencies != expected_dependencies
            || result.mask.descriptor.dependencies != expected_dependencies
        {
            return Err(ToolModuleError::Contract(
                "PerfectPixel extract artifacts were not bound to the expected dependencies"
                    .to_owned(),
            ));
        }
        let object_inspection = self
            .agent_inspect_bytes(
                &object,
                &format!("extract-repair-{}", call.id),
                Duration::from_millis(context.timeout_ms),
                &context.cancellation,
            )
            .await?;
        validate_inspection_v2(&object_inspection.inspection)?;
        let repair_observation = ImageRepairObservation {
            width: object_inspection.inspection.width,
            height: object_inspection.inspection.height,
            transparent_background: object_inspection.inspection.has_alpha,
        };
        repair_observation
            .validate()
            .map_err(ToolModuleError::Contract)?;
        let mut artifacts = vec![
            ModuleArtifact {
                bytes: object,
                media_type: "image/png".to_owned(),
                extension: "png".to_owned(),
            },
            ModuleArtifact {
                bytes: mask,
                media_type: "image/png".to_owned(),
                extension: "png".to_owned(),
            },
        ];
        let remainder_present = if let Some(remainder) = &result.remainder {
            let bytes = read_bounded_file(
                &output_dir.join(&remainder.relative_path),
                MAX_IMAGE_BYTES,
                "extract remainder",
            )?;
            validate_published_descriptor(
                &remainder.descriptor,
                &bytes,
                ArtifactKindV2::WorkingRaster,
            )?;
            if remainder.descriptor.dependencies != expected_dependencies {
                return Err(ToolModuleError::Contract(
                    "PerfectPixel extract remainder was not bound to the expected dependencies"
                        .to_owned(),
                ));
            }
            artifacts.push(ModuleArtifact {
                bytes,
                media_type: "image/png".to_owned(),
                extension: "png".to_owned(),
            });
            true
        } else {
            false
        };
        Ok(ModuleExecution {
            result: ToolResult {
                call_id: call.id.clone(),
                name: call.name.clone(),
                value: json!({
                    "schema": IMAGE_TOOL_SCHEMA,
                    "operation": "extract",
                    "sourceArtifact": args.artifact,
                    "sourceCanvas": result.source_canvas,
                    "sourceBBox": result.source_bbox,
                    "pivot": result.pivot,
                    "completeness": result.completeness,
                    "occludedBackground": result.occluded_background,
                    "softAlphaPixels": result.soft_alpha_pixels,
                    "semanticMask": semantic_result,
                    "perfectpixelVerified": true,
                    "repairObservation": repair_observation,
                    "artifactRoles": if remainder_present { vec!["object", "mask", "remainder"] } else { vec!["object", "mask"] },
                    "receipt": result.receipt,
                    "protocolVersion": self.protocol_version,
                    "behaviorVersion": self.behavior_version
                }),
                is_error: false,
                failure: None,
            },
            receipts: Vec::new(),
            artifacts,
        })
    }

    async fn render(
        &self,
        context: ToolContext,
        call: &ToolCall,
    ) -> Result<ModuleExecution, ToolModuleError> {
        let args: RenderArguments = strict_arguments(call)?;
        if args.nodes.is_empty() || args.nodes.len() > 16 {
            return Err(ToolModuleError::Contract(
                "image render nodes must contain 1..=16 entries".to_owned(),
            ));
        }
        self.verify_agent_manifest(
            &[
                ("render.composition_dag", "1.0.0", "backend_deterministic"),
                ("render.affine_3x3", "1.0.0", "backend_deterministic"),
                ("render.source_over", "1.0.0", "bit_exact"),
                ("render.text_node", "1.0.0", "bit_exact"),
            ],
            Duration::from_millis(context.timeout_ms),
            &context.cancellation,
        )
        .await?;
        let directory = private_temp_directory("render")?;
        let request_path = directory.path().join("request.json");
        let output_dir = directory.path().join("out");
        let mut request_nodes = Vec::with_capacity(args.nodes.len());
        let mut expected_dependencies = Vec::with_capacity(args.nodes.len());
        for (index, node) in args.nodes.iter().enumerate() {
            let source = match node.kind {
                RenderSourceKind::Raster | RenderSourceKind::Object => {
                    if node.text.is_some() {
                        return Err(ToolModuleError::Contract(
                            "raster render nodes cannot contain a text node".to_owned(),
                        ));
                    }
                    let artifact = node.artifact.as_ref().ok_or_else(|| {
                        ToolModuleError::Contract(
                            "raster render nodes require an artifact".to_owned(),
                        )
                    })?;
                    let bytes = read_image_artifact(&context, artifact)?;
                    let path = directory.path().join(format!("node-{index}.png"));
                    fs::write(&path, &bytes)
                        .map_err(storage_failure("image render source could not be staged"))?;
                    expected_dependencies.push(ArtifactDependencyV2::from_ref(artifact));
                    json!({
                        "source": agent_input_json(artifact, &path, if node.kind == RenderSourceKind::Object { "object" } else { "working_raster" }),
                        "kind": if node.kind == RenderSourceKind::Object { "object" } else { "raster" }
                    })
                }
                RenderSourceKind::Text => {
                    if node.artifact.is_some() {
                        return Err(ToolModuleError::Contract(
                            "text render nodes cannot contain an image artifact".to_owned(),
                        ));
                    }
                    let text = node.text.as_ref().ok_or_else(|| {
                        ToolModuleError::Contract(
                            "text render nodes require a typed text node".to_owned(),
                        )
                    })?;
                    if !node.operations.is_empty() {
                        return Err(ToolModuleError::Contract(
                            "text render nodes do not accept raster operations".to_owned(),
                        ));
                    }
                    let font_bytes = read_font_artifact(&context, &text.font_artifact)?;
                    let path = directory.path().join(format!("font-{index}.ttf"));
                    fs::write(&path, &font_bytes)
                        .map_err(storage_failure("text font could not be staged"))?;
                    expected_dependencies.push(ArtifactDependencyV2::from_ref(&text.font_artifact));
                    json!({
                        "kind": "text",
                        "text": text_node_json(text, &path)?
                    })
                }
            };
            let mut request_node = json!({
                "id": node.id,
                "z": node.z,
                "transform": {"matrix": node.transform.matrix},
                "opacity": node.opacity,
                "filter": "nearest",
                "operations": node.operations
            });
            if let Some(object) = source.as_object() {
                for (key, value) in object {
                    request_node[key] = value.clone();
                }
            }
            request_nodes.push(request_node);
        }
        let request = json!({
            "schema": PERFECTPIXEL_AGENT_RENDER_REQUEST_SCHEMA,
            "requestId": format!("render-{}", call.id),
            "canvas": {"width":args.canvas.width,"height":args.canvas.height,"background":args.canvas.background},
            "nodes": request_nodes
        });
        let encoded = serde_json::to_vec(&request).map_err(|error| {
            ToolModuleError::Contract(format!(
                "image render request could not be encoded: {error}"
            ))
        })?;
        fs::write(&request_path, &encoded)
            .map_err(storage_failure("image render request could not be staged"))?;
        let output = self
            .run_perfectpixel(
                &[
                    "agent-render".to_owned(),
                    "--request".to_owned(),
                    request_path.display().to_string(),
                    "--out-dir".to_owned(),
                    output_dir.display().to_string(),
                ],
                Duration::from_millis(context.timeout_ms),
                &context.cancellation,
            )
            .await?;
        let result: PerfectPixelAgentRenderResult = serde_json::from_slice(&output.stdout)
            .map_err(|error| {
                ToolModuleError::Contract(format!(
                    "PerfectPixel render result was not strict JSON: {error}"
                ))
            })?;
        expected_dependencies.sort();
        expected_dependencies.dedup();
        let expected_closure = artifact_dependency_closure_sha256(&expected_dependencies)
            .map_err(ToolModuleError::Contract)?;
        if result.schema != PERFECTPIXEL_AGENT_RENDER_RESULT_SCHEMA
            || result.request_id != format!("render-{}", call.id)
            || result.status != "committed"
            || result.protocol_version != self.protocol_version
            || result.behavior_version != self.behavior_version
            || result.receipt.schema != PERFECTPIXEL_AGENT_RECEIPT_SCHEMA
            || result.receipt.request_id != result.request_id
            || result.receipt.operation != "render"
            || result.receipt.status != OperationStatusV2::Committed
            || result.receipt.behavior_version != self.behavior_version
            || result.receipt.implementation_version != self.implementation_version
            || result.receipt.request_sha256 != sha256_hex(&encoded)
            || result.receipt.dependencies != expected_dependencies
            || result.receipt.dependency_closure_sha256 != expected_closure
            || result.receipt.output_sha256.as_deref()
                != Some(result.output.descriptor.sha256.as_str())
            || result.receipt.determinism != DeterminismClassV2::BackendDeterministic
        {
            return Err(ToolModuleError::Contract(
                "PerfectPixel render result violated protocol v2".to_owned(),
            ));
        }
        validate_text_render_evidence(&args.nodes, &result.text_nodes)?;
        let rendered = read_bounded_file(
            &output_dir.join(&result.output.relative_path),
            MAX_IMAGE_BYTES,
            "render output",
        )?;
        validate_published_descriptor(
            &result.output.descriptor,
            &rendered,
            ArtifactKindV2::WorkingRaster,
        )?;
        if result.output.descriptor.dependencies != expected_dependencies {
            return Err(ToolModuleError::Contract(
                "PerfectPixel render output was not bound to the expected dependencies".to_owned(),
            ));
        }
        let (width, height) = validate_png(&rendered)?;
        let render_inspection = self
            .agent_inspect_bytes(
                &rendered,
                &format!("render-repair-{}", call.id),
                Duration::from_millis(context.timeout_ms),
                &context.cancellation,
            )
            .await?;
        validate_inspection_v2(&render_inspection.inspection)?;
        if (
            render_inspection.inspection.width,
            render_inspection.inspection.height,
        ) != (width, height)
        {
            return Err(ToolModuleError::Contract(
                "PerfectPixel render inspection disagrees with the rendered PNG dimensions"
                    .to_owned(),
            ));
        }
        let repair_observation = ImageRepairObservation {
            width,
            height,
            transparent_background: render_inspection.inspection.has_alpha,
        };
        repair_observation
            .validate()
            .map_err(ToolModuleError::Contract)?;
        Ok(ModuleExecution {
            result: ToolResult {
                call_id: call.id.clone(),
                name: call.name.clone(),
                value: json!({
                    "schema": IMAGE_TOOL_SCHEMA,
                    "operation": "render",
                    "perfectpixelVerified": true,
                    "width": width,
                    "height": height,
                    "repairObservation": repair_observation,
                    "operationSteps": args.nodes.iter()
                        .flat_map(|node| node.operations.iter().map(RenderOperation::evidence_name))
                        .collect::<Vec<_>>(),
                    "textNodes": result.text_nodes,
                    "artifactRoles": ["render"],
                    "receipt": result.receipt,
                    "protocolVersion": self.protocol_version,
                    "behaviorVersion": self.behavior_version
                }),
                is_error: false,
                failure: None,
            },
            receipts: Vec::new(),
            artifacts: vec![ModuleArtifact {
                bytes: rendered,
                media_type: "image/png".to_owned(),
                extension: "png".to_owned(),
            }],
        })
    }

    async fn compare(
        &self,
        context: ToolContext,
        call: &ToolCall,
    ) -> Result<ModuleExecution, ToolModuleError> {
        let args: CompareArguments = strict_arguments(call)?;
        if args.assertions.is_empty() || args.assertions.len() > 256 {
            return Err(ToolModuleError::Contract(
                "image compare assertions must contain 1..=256 entries".to_owned(),
            ));
        }
        let verification_specification = ImageVerificationSpec {
            schema: IMAGE_VERIFICATION_SPEC_SCHEMA.to_owned(),
            before_artifact: args.before_artifact.clone(),
            after_kind: args.after_kind,
            assertions: args.assertions.clone(),
        };
        let verification_spec_digest = verification_specification
            .digest()
            .map_err(ToolModuleError::Contract)?;
        self.verify_agent_manifest(
            &[
                ("compare.basic", "1.0.0", "bit_exact"),
                ("compare.exact_spec", "1.0.0", "bit_exact"),
                ("compare.regions", "1.0.0", "bit_exact"),
                ("compare.masks", "1.0.0", "bit_exact"),
                ("compare.geometry", "1.0.0", "bit_exact"),
                ("compare.preview", "1.0.0", "bit_exact"),
            ],
            Duration::from_millis(context.timeout_ms),
            &context.cancellation,
        )
        .await?;
        let directory = private_temp_directory("compare")?;
        let request_path = directory.path().join("request.json");
        let output_dir = directory.path().join("out");
        let before_bytes = read_image_artifact(&context, &args.before_artifact)?;
        let after_bytes = read_image_artifact(&context, &args.after_artifact)?;
        let before_path = directory.path().join("before.png");
        let after_path = directory.path().join("after.png");
        fs::write(&before_path, &before_bytes).map_err(storage_failure(
            "image compare before artifact could not be staged",
        ))?;
        fs::write(&after_path, &after_bytes).map_err(storage_failure(
            "image compare after artifact could not be staged",
        ))?;

        let mut request_assertions = Vec::with_capacity(args.assertions.len());
        let mut expected_dependencies = vec![
            ArtifactDependencyV2::from_ref(&args.before_artifact),
            ArtifactDependencyV2::from_ref(&args.after_artifact),
        ];
        for (index, assertion) in args.assertions.iter().enumerate() {
            request_assertions.push(compare_assertion_json(
                assertion,
                index,
                directory.path(),
                &context,
                &mut expected_dependencies,
            )?);
        }
        expected_dependencies.sort();
        expected_dependencies.dedup();
        let request_id = format!("compare-{}", call.id);
        let request = json!({
            "schema": PERFECTPIXEL_AGENT_COMPARE_REQUEST_SCHEMA,
            "requestId": request_id,
            "before": agent_input_json(&args.before_artifact, &before_path, "working_raster"),
            "after": agent_input_json(
                &args.after_artifact,
                &after_path,
                match args.after_kind {
                    ImageCompareArtifactKind::Raster => "working_raster",
                    ImageCompareArtifactKind::Object => "object",
                    ImageCompareArtifactKind::Export => "export_raster",
                },
            ),
            "assertions": request_assertions,
            "preview": {
                "difference": args.preview.difference,
                "maskOverlay": args.preview.mask_overlay,
                "maximumEdge": args.preview.maximum_edge,
            }
        });
        let encoded = serde_json::to_vec(&request).map_err(|error| {
            ToolModuleError::Contract(format!(
                "image compare request could not be encoded: {error}"
            ))
        })?;
        fs::write(&request_path, &encoded)
            .map_err(storage_failure("image compare request could not be staged"))?;
        let output = self
            .run_perfectpixel(
                &[
                    "agent-compare".to_owned(),
                    "--request".to_owned(),
                    request_path.display().to_string(),
                    "--out-dir".to_owned(),
                    output_dir.display().to_string(),
                ],
                Duration::from_millis(context.timeout_ms),
                &context.cancellation,
            )
            .await?;
        let result: PerfectPixelAgentCompareResult = serde_json::from_slice(&output.stdout)
            .map_err(|error| {
                ToolModuleError::Contract(format!(
                    "PerfectPixel compare result was not strict JSON: {error}"
                ))
            })?;
        let expected_closure = artifact_dependency_closure_sha256(&expected_dependencies)
            .map_err(ToolModuleError::Contract)?;
        let expected_status = if result.all_required_passed {
            OperationStatusV2::Committed
        } else {
            OperationStatusV2::Rejected
        };
        if result.schema != PERFECTPIXEL_AGENT_COMPARE_RESULT_SCHEMA
            || result.request_id != request_id
            || result.status
                != if result.all_required_passed {
                    "committed"
                } else {
                    "rejected"
                }
            || result.protocol_version != self.protocol_version
            || result.behavior_version != self.behavior_version
            || result.receipt.schema != PERFECTPIXEL_AGENT_RECEIPT_SCHEMA
            || result.receipt.request_id != request_id
            || result.receipt.operation != "compare"
            || result.receipt.status != expected_status
            || result.receipt.behavior_version != self.behavior_version
            || result.receipt.implementation_version != self.implementation_version
            || result.receipt.request_sha256 != sha256_hex(&encoded)
            || result.receipt.dependencies != expected_dependencies
            || result.receipt.dependency_closure_sha256 != expected_closure
            || result.receipt.output_sha256.is_some()
            || result.receipt.determinism != DeterminismClassV2::BitExact
        {
            return Err(ToolModuleError::Contract(
                "PerfectPixel compare result violated protocol v2".to_owned(),
            ));
        }
        verification_specification
            .validate_result(result.all_required_passed, &result.assertions)
            .map_err(ToolModuleError::Contract)?;
        let mut artifacts = Vec::with_capacity(result.previews.len());
        let mut roles = Vec::with_capacity(result.previews.len());
        for preview in &result.previews {
            if !matches!(preview.role.as_str(), "difference" | "mask_overlay") {
                return Err(ToolModuleError::Contract(
                    "PerfectPixel compare returned an unknown preview role".to_owned(),
                ));
            }
            let bytes = read_bounded_file(
                &output_dir.join(&preview.relative_path),
                MAX_IMAGE_BYTES,
                "compare preview",
            )?;
            validate_published_descriptor(
                &preview.descriptor,
                &bytes,
                ArtifactKindV2::WorkingRaster,
            )?;
            if preview.descriptor.dependencies != expected_dependencies {
                return Err(ToolModuleError::Contract(
                    "PerfectPixel compare preview was not bound to the expected dependencies"
                        .to_owned(),
                ));
            }
            roles.push(preview.role.clone());
            artifacts.push(ModuleArtifact {
                bytes,
                media_type: "image/png".to_owned(),
                extension: "png".to_owned(),
            });
        }
        Ok(ModuleExecution {
            result: ToolResult {
                call_id: call.id.clone(),
                name: call.name.clone(),
                value: json!({
                    "schema": IMAGE_TOOL_SCHEMA,
                    "operation": "compare",
                    "allRequiredPassed": result.all_required_passed,
                    "verificationSpecDigest": verification_spec_digest,
                    "status": result.status,
                    "metrics": result.metrics,
                    "assertions": result.assertions,
                    "artifactRoles": roles,
                    "receipt": result.receipt,
                    "protocolVersion": self.protocol_version,
                    "behaviorVersion": self.behavior_version,
                }),
                is_error: false,
                failure: None,
            },
            receipts: Vec::new(),
            artifacts,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ImageToolIdentity {
    pub executable: String,
    pub executable_sha256: String,
    pub protocol_schema: String,
    pub protocol_version: String,
    pub behavior_version: String,
    pub capability_manifest_sha256: String,
    pub implementation_version: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct GeneratedImageObservation {
    width: u32,
    height: u32,
    transparent_background: Option<bool>,
}

#[async_trait]
impl ToolModule for ImageToolModule {
    fn tools(&self) -> Vec<ModuleTool> {
        self.tools.clone()
    }

    async fn execute(
        &self,
        context: ToolContext,
        call: ToolCall,
    ) -> Result<ModuleExecution, ToolModuleError> {
        if !self
            .tools
            .iter()
            .any(|tool| tool.definition.name == call.name)
        {
            return Err(ToolModuleError::Contract(
                "image tool was not registered by composition".to_owned(),
            ));
        }
        match call.name.as_str() {
            IMAGE_EDIT_SEMANTIC_TOOL => self.semantic_edit(context, &call).await,
            IMAGE_GENERATE_TOOL => self.generate(context, &call).await,
            IMAGE_INSPECT_TOOL => self.inspect(context, &call).await,
            IMAGE_EXTRACT_TOOL => self.extract(context, &call).await,
            IMAGE_RENDER_TOOL => self.render(context, &call).await,
            IMAGE_COMPARE_TOOL => self.compare(context, &call).await,
            _ => Err(ToolModuleError::Contract(format!(
                "unknown image tool {}",
                call.name
            ))),
        }
    }
}

#[must_use]
pub fn tool_definitions() -> Vec<ToolDefinition> {
    vec![
        generate_definition(),
        inspect_definition(),
        extract_definition(),
        render_definition(),
        compare_definition(),
    ]
}

pub(crate) fn semantic_tool_definition() -> Result<ToolDefinition, String> {
    semantic_edit::module_tool().map(|tool| tool.definition)
}

fn canonical_module_tools() -> Result<Vec<ModuleTool>, String> {
    let creator = BTreeSet::from([ProfileId::creator()]);
    let artifact_read = CapabilityId::parse("artifact/read")
        .map_err(|error| format!("canonical image capability is invalid: {error}"))?;
    let artifact_write = CapabilityId::parse("artifact/write")
        .map_err(|error| format!("canonical image capability is invalid: {error}"))?;
    let model_image = CapabilityId::parse("model/image")
        .map_err(|error| format!("canonical image capability is invalid: {error}"))?;
    let mut definitions = tool_definitions().into_iter();
    let generate = definitions
        .next()
        .ok_or_else(|| "canonical image tool definitions are incomplete".to_owned())?;
    let inspect = definitions
        .next()
        .ok_or_else(|| "canonical image tool definitions are incomplete".to_owned())?;
    let extract = definitions
        .next()
        .ok_or_else(|| "canonical image tool definitions are incomplete".to_owned())?;
    let render = definitions
        .next()
        .ok_or_else(|| "canonical image tool definitions are incomplete".to_owned())?;
    let compare = definitions
        .next()
        .ok_or_else(|| "canonical image tool definitions are incomplete".to_owned())?;
    if definitions.next().is_some() {
        return Err("canonical image tool definitions contain unexpected entries".to_owned());
    }
    Ok(vec![
        ModuleTool {
            definition: generate,
            side_effect: ToolSideEffect::External,
            allowed_profiles: creator.clone(),
            required_capabilities: BTreeSet::from([model_image]),
            activation_skills: BTreeSet::new(),
        },
        ModuleTool {
            definition: inspect,
            side_effect: ToolSideEffect::ReadOnly,
            allowed_profiles: creator.clone(),
            required_capabilities: BTreeSet::from([artifact_read.clone()]),
            activation_skills: BTreeSet::new(),
        },
        ModuleTool {
            definition: extract,
            side_effect: ToolSideEffect::ReadOnly,
            allowed_profiles: creator.clone(),
            required_capabilities: BTreeSet::from([artifact_read.clone(), artifact_write.clone()]),
            activation_skills: BTreeSet::new(),
        },
        ModuleTool {
            definition: render,
            side_effect: ToolSideEffect::ReadOnly,
            allowed_profiles: creator.clone(),
            required_capabilities: BTreeSet::from([artifact_read.clone(), artifact_write.clone()]),
            activation_skills: BTreeSet::new(),
        },
        ModuleTool {
            definition: compare,
            side_effect: ToolSideEffect::ReadOnly,
            allowed_profiles: creator.clone(),
            required_capabilities: BTreeSet::from([artifact_read.clone(), artifact_write.clone()]),
            activation_skills: BTreeSet::new(),
        },
    ])
}

fn strict_arguments<T: for<'de> Deserialize<'de>>(call: &ToolCall) -> Result<T, ToolModuleError> {
    serde_json::from_value(call.arguments.clone()).map_err(|error| {
        ToolModuleError::Contract(format!("{} arguments are invalid: {error}", call.name))
    })
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct GenerateArguments {
    #[serde(default)]
    prompt: Option<String>,
    #[serde(default)]
    prompt_artifact: Option<ArtifactRef>,
    #[serde(default)]
    prompt_sha256: Option<String>,
    #[serde(default)]
    maximum_bytes: Option<usize>,
    #[serde(default)]
    background: Option<ImageBackgroundRequirement>,
    #[serde(default)]
    size: Option<ImageSizeRequirement>,
    #[serde(default)]
    quality: Option<ImageQualityRequirement>,
}

fn resolve_generation_prompt(
    context: &ToolContext,
    args: &GenerateArguments,
    expected_compiler_sha256: Option<&str>,
) -> Result<(String, ImageGenerationRequirements), ToolModuleError> {
    match (&args.prompt, &args.prompt_artifact, &args.prompt_sha256) {
        (Some(prompt), None, None) => Ok((
            prompt.clone(),
            ImageGenerationRequirements {
                background: args.background,
                size: args.size,
                quality: args.quality,
            },
        )),
        (None, Some(reference), Some(expected)) => {
            if reference.media_type != agent_core::PROMPT_MEDIA_TYPE
                || reference.byte_length == 0
                || reference.byte_length > agent_core::MAX_PROMPT_ARTIFACT_BYTES as u64
            {
                return Err(ToolModuleError::Contract(
                    "invalid compiled prompt artifact type or byte bound".to_owned(),
                ));
            }
            let bytes = context
                .artifacts
                .read(reference)
                .map_err(|error| ToolModuleError::Contract(error.to_string()))?;
            let expected_compiler_sha256 = expected_compiler_sha256.ok_or_else(|| {
                ToolModuleError::Contract(
                    "compiled prompt artifacts require a pinned promptGen compiler identity"
                        .to_owned(),
                )
            })?;
            let artifact = agent_core::ImagePromptArtifact::resolve(
                &bytes,
                &context.task_id,
                expected_compiler_sha256,
                expected,
            )
            .map_err(ToolModuleError::Contract)?;
            let compiled_size: ImageSizeRequirement = serde_json::from_value(Value::String(
                artifact
                    .compiled
                    .output
                    .provider_size()
                    .map_err(ToolModuleError::Contract)?
                    .to_owned(),
            ))
            .map_err(|error| ToolModuleError::Contract(error.to_string()))?;
            let compiled_background = match artifact.compiled.output.background {
                agent_core::CompiledImageBackground::Auto => ImageBackgroundRequirement::Auto,
                agent_core::CompiledImageBackground::Opaque => ImageBackgroundRequirement::Opaque,
            };
            if args.size.is_some_and(|size| size != compiled_size)
                || args
                    .background
                    .is_some_and(|background| background != compiled_background)
            {
                return Err(ToolModuleError::Contract(
                    "generation size or background conflicts with the compiled prompt artifact"
                        .to_owned(),
                ));
            }
            Ok((
                artifact.compiled.prompt,
                ImageGenerationRequirements {
                    background: Some(compiled_background),
                    size: Some(compiled_size),
                    quality: args.quality,
                },
            ))
        }
        _ => Err(ToolModuleError::Contract(
            "use prompt XOR (promptArtifact AND promptSha256)".to_owned(),
        )),
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct InspectArguments {
    artifact: ArtifactRef,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct ExtractArguments {
    artifact: ArtifactRef,
    selector: ExtractSelectorArguments,
    #[serde(default)]
    mask_artifact: Option<ArtifactRef>,
    #[serde(default)]
    refinement: ExtractRefinementArguments,
    #[serde(default = "default_true_bool")]
    include_remainder: bool,
}

const fn default_true_bool() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
enum ExtractSelectorArguments {
    ExistingAlpha {
        #[serde(default = "default_minimum_alpha")]
        minimum_alpha: u8,
    },
    ChromaKey {
        keys: Vec<[u8; 3]>,
        tolerance: u8,
    },
    ColorRange {
        minimum: [u8; 3],
        maximum: [u8; 3],
    },
    AlphaComponent {
        point: ExtractNormalizedPoint,
        #[serde(default = "default_minimum_alpha")]
        minimum_alpha: u8,
    },
    ProvidedMask,
    Semantic {
        source_revision: u64,
        request_revision: u64,
        helper_identity: String,
        target: SemanticMaskTarget,
    },
}

const fn default_minimum_alpha() -> u8 {
    1
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct ExtractNormalizedPoint {
    x_millionths: u32,
    y_millionths: u32,
}

#[derive(Debug, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct ExtractRefinementArguments {
    #[serde(default)]
    feather_radius: u8,
    #[serde(default)]
    threshold: Option<u8>,
    #[serde(default)]
    require_soft_alpha: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct RenderArguments {
    canvas: RenderCanvasArguments,
    nodes: Vec<RenderNodeArguments>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct RenderCanvasArguments {
    width: u32,
    height: u32,
    #[serde(default)]
    background: [u8; 4],
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct RenderNodeArguments {
    id: String,
    #[serde(default)]
    z: i32,
    #[serde(default)]
    artifact: Option<ArtifactRef>,
    #[serde(default)]
    kind: RenderSourceKind,
    #[serde(default)]
    text: Option<TextNodeArguments>,
    transform: RenderTransformArguments,
    #[serde(default = "default_opacity")]
    opacity: u8,
    /// Deterministic source preprocessing. This is the canonical replacement
    /// for the removed compatibility surface.
    #[serde(default)]
    operations: Vec<RenderOperation>,
}

const fn default_opacity() -> u8 {
    255
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
enum RenderSourceKind {
    #[default]
    Raster,
    Object,
    Text,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct TextNodeArguments {
    content: String,
    direction: TextDirectionArgument,
    language: String,
    box_width: u32,
    box_height: u32,
    alignment: TextAlignmentArgument,
    line_break: TextLineBreakArgument,
    font_size: f32,
    color: [u8; 4],
    pixel_spec: TextPixelSpecArguments,
    font_artifact: ArtifactRef,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum TextDirectionArgument {
    Ltr,
    Rtl,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum TextAlignmentArgument {
    Start,
    Center,
    End,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum TextLineBreakArgument {
    NoWrap,
    WordWrap,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct TextPixelSpecArguments {
    format: String,
    color_space: String,
    alpha_mode: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct RenderTransformArguments {
    matrix: [f64; 9],
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct CompareArguments {
    before_artifact: ArtifactRef,
    after_artifact: ArtifactRef,
    #[serde(default)]
    after_kind: ImageCompareArtifactKind,
    assertions: Vec<ImageCompareAssertion>,
    preview: ComparePreviewArguments,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct ComparePreviewArguments {
    difference: bool,
    #[serde(default)]
    mask_overlay: bool,
    maximum_edge: u32,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(
    tag = "op",
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
enum RenderOperation {
    Crop {
        x: u32,
        y: u32,
        width: u32,
        height: u32,
    },
    Rotate {
        #[serde(rename = "quarterTurns")]
        quarter_turns: u8,
    },
    Flip {
        axis: RenderOperationFlipAxis,
    },
    Resize {
        width: u32,
        height: u32,
        filter: RenderOperationFilter,
    },
    RemoveBackground {
        keys: Vec<[u8; 3]>,
        tolerance: u8,
        feather: u8,
    },
    RemoveBackgroundAuto {
        max_keys: u8,
        min_edge_coverage_basis_points: u16,
        tolerance: u8,
        feather: u8,
    },
}

impl RenderOperation {
    fn evidence_name(&self) -> &'static str {
        match self {
            Self::Crop { .. } => "crop",
            Self::Rotate { .. } => "rotate",
            Self::Flip { .. } => "flip",
            Self::Resize { .. } => "resize",
            Self::RemoveBackground { .. } => "remove_background",
            Self::RemoveBackgroundAuto { .. } => "remove_background_auto",
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum RenderOperationFlipAxis {
    Horizontal,
    Vertical,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum RenderOperationFilter {
    Nearest,
    Lanczos3,
}

#[derive(Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct InspectionRect {
    x: u32,
    y: u32,
    w: u32,
    h: u32,
}

#[derive(Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct InspectionPoint {
    x: u32,
    y: u32,
}

fn deserialize_nullable<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer)
}

#[derive(Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct EdgePaletteEntry {
    rgb: [u8; 3],
    count: u64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct PerfectPixelAgentManifest {
    schema: String,
    protocol_version: String,
    behavior_version: String,
    implementation_version: String,
    capabilities: Vec<PerfectPixelAgentCapability>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct PerfectPixelAgentCapability {
    name: String,
    version: String,
    determinism: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct PerfectPixelAgentArtifactDescriptor {
    sha256: String,
    media_type: String,
    byte_length: u64,
    kind: ArtifactKindV2,
    pixel_spec: Option<PixelSpec>,
    retention: ArtifactRetention,
    #[serde(default)]
    dependencies: Vec<ArtifactDependencyV2>,
    dependency_closure_sha256: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct PerfectPixelPublishedArtifact {
    relative_path: String,
    descriptor: PerfectPixelAgentArtifactDescriptor,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct PerfectPixelAgentExtractResult {
    schema: String,
    request_id: String,
    status: String,
    protocol_version: String,
    behavior_version: String,
    source_canvas: Value,
    source_bbox: Value,
    pivot: [f64; 2],
    completeness: String,
    occluded_background: String,
    soft_alpha_pixels: u64,
    object: PerfectPixelPublishedArtifact,
    mask: PerfectPixelPublishedArtifact,
    #[serde(default)]
    remainder: Option<PerfectPixelPublishedArtifact>,
    receipt: OperationReceiptV2,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct PerfectPixelAgentRenderResult {
    schema: String,
    request_id: String,
    status: String,
    protocol_version: String,
    behavior_version: String,
    output: PerfectPixelPublishedArtifact,
    #[serde(default)]
    text_nodes: Vec<PerfectPixelTextNodeEvidence>,
    receipt: OperationReceiptV2,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct PerfectPixelTextNodeEvidence {
    node_id: String,
    layout: PerfectPixelTextLayoutSnapshot,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct PerfectPixelTextLayoutSnapshot {
    schema: String,
    font_sha256: String,
    lines: Vec<String>,
    line_snapshots: Vec<PerfectPixelTextLineSnapshot>,
    glyphs: Vec<PerfectPixelTextGlyphSnapshot>,
    glyph_count: u32,
    width: u32,
    height: u32,
    raster_sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct PerfectPixelTextLineSnapshot {
    text: String,
    glyph_start: u32,
    glyph_count: u32,
    advance_units: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct PerfectPixelTextGlyphSnapshot {
    glyph_id: u32,
    cluster: u32,
    x_advance: i32,
    y_advance: i32,
    x_offset: i32,
    y_offset: i32,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct PerfectPixelAgentCompareResult {
    schema: String,
    request_id: String,
    status: String,
    protocol_version: String,
    behavior_version: String,
    all_required_passed: bool,
    metrics: PerfectPixelCompareMetrics,
    assertions: Vec<ImageCompareAssertionResult>,
    previews: Vec<PerfectPixelComparePreview>,
    receipt: OperationReceiptV2,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct PerfectPixelCompareMetrics {
    changed_pixel_ratio: f64,
    mean_absolute_error: f64,
    maximum_channel_error: u8,
    alpha_iou: f64,
    after_foreground_pixels: u64,
    #[serde(default)]
    after_bounds: Option<ImageCompareRect>,
    #[serde(default)]
    after_centroid: Option<[f64; 2]>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct PerfectPixelComparePreview {
    role: String,
    relative_path: String,
    descriptor: PerfectPixelAgentArtifactDescriptor,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct PerfectPixelAgentInspectResult {
    schema: String,
    request_id: String,
    status: String,
    protocol_version: String,
    behavior_version: String,
    source: PerfectPixelAgentArtifactDescriptor,
    inspection: PerfectPixelInspectionV2,
    receipt: OperationReceiptV2,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct PerfectPixelInspectionV2 {
    width: u32,
    height: u32,
    foreground_pixels: usize,
    alpha_ratio: f64,
    #[serde(deserialize_with = "deserialize_nullable")]
    content_box: Option<InspectionRect>,
    #[serde(deserialize_with = "deserialize_nullable")]
    center: Option<InspectionPoint>,
    ground_y: u32,
    touches_edge: bool,
    has_alpha: bool,
    pixel_format: String,
    color_space: String,
    edge_pixel_count: u64,
    edge_palette: Vec<EdgePaletteEntry>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CanonicalPerfectPixelManifest<'a> {
    schema: &'a str,
    protocol_version: &'a str,
    behavior_version: &'a str,
    implementation_version: &'a str,
    capabilities: Vec<CanonicalPerfectPixelCapability<'a>>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CanonicalPerfectPixelCapability<'a> {
    name: &'a str,
    version: &'a str,
    determinism: &'a str,
}

fn perfectpixel_manifest_sha256(manifest: &PerfectPixelAgentManifest) -> Result<String, String> {
    if manifest.schema != AGENT_IMAGE_PROTOCOL_SCHEMA
        || manifest.protocol_version != AGENT_IMAGE_PROTOCOL_VERSION
        || manifest.behavior_version != AGENT_IMAGE_BEHAVIOR_VERSION
        || manifest.implementation_version.trim().is_empty()
        || manifest.implementation_version.len() > 64
        || manifest.capabilities.is_empty()
        || manifest.capabilities.len() > 256
    {
        return Err("PerfectPixel capability manifest identity is invalid".to_owned());
    }
    let mut names = BTreeSet::new();
    for capability in &manifest.capabilities {
        if capability.name.is_empty()
            || capability.name.len() > 128
            || !capability.name.bytes().all(|byte| {
                byte.is_ascii_lowercase()
                    || byte.is_ascii_digit()
                    || matches!(byte, b'.' | b'_' | b'-')
            })
            || capability.version.is_empty()
            || capability.version.len() > 64
            || !names.insert(capability.name.as_str())
        {
            return Err(
                "PerfectPixel capability manifest contains an invalid capability".to_owned(),
            );
        }
    }
    let mut capabilities = manifest.capabilities.iter().collect::<Vec<_>>();
    capabilities.sort_by(|left, right| {
        (
            left.name.as_str(),
            left.version.as_str(),
            left.determinism.as_str(),
        )
            .cmp(&(
                right.name.as_str(),
                right.version.as_str(),
                right.determinism.as_str(),
            ))
    });
    let canonical = CanonicalPerfectPixelManifest {
        schema: &manifest.schema,
        protocol_version: &manifest.protocol_version,
        behavior_version: &manifest.behavior_version,
        implementation_version: &manifest.implementation_version,
        capabilities: capabilities
            .into_iter()
            .map(|capability| CanonicalPerfectPixelCapability {
                name: &capability.name,
                version: &capability.version,
                determinism: &capability.determinism,
            })
            .collect(),
    };
    let encoded = serde_json::to_vec(&canonical)
        .map_err(|error| format!("PerfectPixel capability manifest encoding failed: {error}"))?;
    let mut input =
        Vec::with_capacity(AGENT_IMAGE_CAPABILITY_MANIFEST_DIGEST_DOMAIN.len() + encoded.len() + 1);
    input.extend_from_slice(AGENT_IMAGE_CAPABILITY_MANIFEST_DIGEST_DOMAIN.as_bytes());
    input.push(0);
    input.extend_from_slice(&encoded);
    Ok(sha256_hex(&input))
}

fn compatible_semver_at_least(observed: &str, minimum: &str) -> bool {
    fn parse(value: &str) -> Option<[u64; 3]> {
        let core = value.split('+').next()?;
        if core.contains('-') {
            return None;
        }
        let mut parts = core.split('.');
        let parsed = [
            parts.next()?.parse().ok()?,
            parts.next()?.parse().ok()?,
            parts.next()?.parse().ok()?,
        ];
        if parts.next().is_some() {
            return None;
        }
        Some(parsed)
    }
    matches!((parse(observed), parse(minimum)), (Some(observed), Some(minimum)) if observed[0] == minimum[0] && observed >= minimum)
}

fn selector_capability(selector: &ExtractSelectorArguments) -> &'static str {
    match selector {
        ExtractSelectorArguments::ExistingAlpha { .. } => "extract.alpha",
        ExtractSelectorArguments::ChromaKey { .. } => "extract.chroma_key",
        ExtractSelectorArguments::ColorRange { .. } => "extract.color_range",
        ExtractSelectorArguments::AlphaComponent { .. } => "extract.alpha_component",
        ExtractSelectorArguments::ProvidedMask => "extract.provided_mask",
        ExtractSelectorArguments::Semantic { .. } => SEMANTIC_MASK_CAPABILITY,
    }
}

fn agent_input_json(reference: &ArtifactRef, path: &Path, kind: &str) -> Value {
    json!({
        "path": path.display().to_string(),
        "expectedSha256": reference.sha256,
        "mediaType": "image/png",
        "byteLength": reference.byte_length,
        "kind": kind,
        "pixelSpec": {"format":"rgba8","colorSpace":"srgb","alphaMode":"straight"}
    })
}

fn text_node_json(text: &TextNodeArguments, font_path: &Path) -> Result<Value, ToolModuleError> {
    if text.pixel_spec.format != "rgba8"
        || text.pixel_spec.color_space != "srgb"
        || text.pixel_spec.alpha_mode != "straight"
    {
        return Err(ToolModuleError::Contract(
            "text pixelSpec must be rgba8/srgb/straight".to_owned(),
        ));
    }
    let extension = Path::new(&text.font_artifact.relative_path)
        .extension()
        .and_then(std::ffi::OsStr::to_str)
        .unwrap_or_default()
        .to_ascii_lowercase();
    if !matches!(
        text.font_artifact.media_type.as_str(),
        "font/ttf" | "font/otf"
    ) || !matches!(extension.as_str(), "ttf" | "otf")
    {
        return Err(ToolModuleError::Contract(
            "text fontArtifact must be a content-addressed font/ttf or font/otf artifact"
                .to_owned(),
        ));
    }
    Ok(json!({
        "content": text.content,
        "direction": match text.direction {
            TextDirectionArgument::Ltr => "ltr",
            TextDirectionArgument::Rtl => "rtl",
        },
        "language": text.language,
        "boxWidth": text.box_width,
        "boxHeight": text.box_height,
        "alignment": match text.alignment {
            TextAlignmentArgument::Start => "start",
            TextAlignmentArgument::Center => "center",
            TextAlignmentArgument::End => "end",
        },
        "lineBreak": match text.line_break {
            TextLineBreakArgument::NoWrap => "no_wrap",
            TextLineBreakArgument::WordWrap => "word_wrap",
        },
        "fontSize": text.font_size,
        "color": text.color,
        "pixelSpec": {
            "format": text.pixel_spec.format,
            "colorSpace": text.pixel_spec.color_space,
            "alphaMode": text.pixel_spec.alpha_mode
        },
        "font": {
            "artifact": text.font_artifact,
            "path": font_path.display().to_string()
        }
    }))
}

fn compare_assertion_json(
    assertion: &ImageCompareAssertion,
    index: usize,
    directory: &Path,
    context: &ToolContext,
    dependencies: &mut Vec<ArtifactDependencyV2>,
) -> Result<Value, ToolModuleError> {
    let severity = |value: ImageCompareSeverity| match value {
        ImageCompareSeverity::Required => "required",
        ImageCompareSeverity::Advisory => "advisory",
    };
    let rect = |value: ImageCompareRect| json!({"x":value.x,"y":value.y,"width":value.width,"height":value.height});
    let stage_mask = |reference: &ArtifactRef,
                      suffix: &str,
                      dependencies: &mut Vec<ArtifactDependencyV2>|
     -> Result<Value, ToolModuleError> {
        let bytes = read_image_artifact(context, reference)?;
        let path = directory.join(format!("compare-mask-{index}-{suffix}.png"));
        fs::write(&path, &bytes)
            .map_err(storage_failure("image compare mask could not be staged"))?;
        dependencies.push(ArtifactDependencyV2::from_ref(reference));
        Ok(agent_input_json(reference, &path, "mask"))
    };
    Ok(match assertion {
        ImageCompareAssertion::ExactEqual {
            id,
            severity: level,
        } => json!({
            "type":"exact_equal","id":id,"severity":severity(*level)
        }),
        ImageCompareAssertion::ChangedRatio {
            id,
            severity: level,
            minimum,
            maximum,
        } => {
            let mut request = json!({
                "type":"changed_ratio","id":id,"severity":severity(*level)
            });
            if let Some(value) = minimum {
                request["minimum"] = json!(value);
            }
            if let Some(value) = maximum {
                request["maximum"] = json!(value);
            }
            request
        }
        ImageCompareAssertion::OutsideMaskChangedRatio {
            id,
            severity: level,
            maximum,
            mask_artifact,
        } => json!({
            "type":"outside_mask_changed_ratio","id":id,"severity":severity(*level),"maximum":maximum,
            "mask":stage_mask(mask_artifact,"outside",dependencies)?
        }),
        ImageCompareAssertion::InsideMaskChangedRatio {
            id,
            severity: level,
            minimum,
            maximum,
            mask_artifact,
        } => {
            let mut request = json!({
                "type":"inside_mask_changed_ratio","id":id,"severity":severity(*level),"minimum":minimum,
                "mask":stage_mask(mask_artifact,"inside",dependencies)?
            });
            if let Some(value) = maximum {
                request["maximum"] = json!(value);
            }
            request
        }
        ImageCompareAssertion::UnchangedRegionExact {
            id,
            severity: level,
            region,
        } => json!({
            "type":"unchanged_region_exact","id":id,"severity":severity(*level),"region":rect(*region)
        }),
        ImageCompareAssertion::AlphaIou {
            id,
            severity: level,
            minimum,
            mask_artifact,
        } => json!({
            "type":"alpha_iou","id":id,"severity":severity(*level),"minimum":minimum,
            "mask":stage_mask(mask_artifact,"alpha-iou",dependencies)?
        }),
        ImageCompareAssertion::MaskLeakageRatio {
            id,
            severity: level,
            maximum,
            mask_artifact,
        } => json!({
            "type":"mask_leakage_ratio","id":id,"severity":severity(*level),"maximum":maximum,
            "mask":stage_mask(mask_artifact,"leakage",dependencies)?
        }),
        ImageCompareAssertion::ObjectBounds {
            id,
            severity: level,
            expected,
            tolerance,
        } => json!({
            "type":"object_bounds","id":id,"severity":severity(*level),"expected":rect(*expected),"tolerance":tolerance
        }),
        ImageCompareAssertion::ObjectCentroid {
            id,
            severity: level,
            expected,
            tolerance,
        } => json!({
            "type":"object_centroid","id":id,"severity":severity(*level),"expected":expected,"tolerance":tolerance
        }),
        ImageCompareAssertion::ObjectArea {
            id,
            severity: level,
            expected,
            tolerance,
        } => json!({
            "type":"object_area","id":id,"severity":severity(*level),"expected":expected,"tolerance":tolerance
        }),
        ImageCompareAssertion::MaximumChannelError {
            id,
            severity: level,
            maximum,
        } => json!({
            "type":"maximum_channel_error","id":id,"severity":severity(*level),"maximum":maximum
        }),
        ImageCompareAssertion::MeanAbsoluteError {
            id,
            severity: level,
            maximum,
        } => json!({
            "type":"mean_absolute_error","id":id,"severity":severity(*level),"maximum":maximum
        }),
    })
}

fn storage_failure(message: &'static str) -> impl Fn(std::io::Error) -> ToolModuleError {
    move |error| {
        ToolModuleError::Failure(AgentFailure::new(
            FailureCode::StorageFailed,
            format!("{message}: {error}"),
        ))
    }
}

fn read_bounded_file(path: &Path, maximum: usize, label: &str) -> Result<Vec<u8>, ToolModuleError> {
    let metadata = fs::symlink_metadata(path).map_err(|error| {
        ToolModuleError::Failure(AgentFailure::new(
            FailureCode::StorageFailed,
            format!("{label} could not be inspected: {error}"),
        ))
    })?;
    if metadata.file_type().is_symlink()
        || !metadata.is_file()
        || metadata.len() == 0
        || metadata.len() > maximum as u64
    {
        return Err(ToolModuleError::Contract(format!(
            "{label} is not a bounded regular file"
        )));
    }
    fs::read(path).map_err(|error| {
        ToolModuleError::Failure(AgentFailure::new(
            FailureCode::StorageFailed,
            format!("{label} could not be read: {error}"),
        ))
    })
}

fn validate_published_descriptor(
    descriptor: &PerfectPixelAgentArtifactDescriptor,
    bytes: &[u8],
    expected_kind: ArtifactKindV2,
) -> Result<(), ToolModuleError> {
    let closure = artifact_dependency_closure_sha256(&descriptor.dependencies)
        .map_err(ToolModuleError::Contract)?;
    if descriptor.sha256 != sha256_hex(bytes)
        || descriptor.media_type != "image/png"
        || descriptor.byte_length != bytes.len() as u64
        || descriptor.kind != expected_kind
        || descriptor.pixel_spec != Some(PixelSpec::rgba8_srgb_straight())
        || descriptor.retention != ArtifactRetention::TaskScoped
        || descriptor.dependency_closure_sha256 != closure
    {
        return Err(ToolModuleError::Contract(
            "PerfectPixel published artifact descriptor is inconsistent".to_owned(),
        ));
    }
    Ok(())
}

fn validate_text_render_evidence(
    nodes: &[RenderNodeArguments],
    evidence: &[PerfectPixelTextNodeEvidence],
) -> Result<(), ToolModuleError> {
    let expected = nodes
        .iter()
        .filter(|node| node.kind == RenderSourceKind::Text)
        .map(|node| {
            let text = node.text.as_ref().ok_or_else(|| {
                ToolModuleError::Contract(
                    "text render node omitted its typed text payload".to_owned(),
                )
            })?;
            Ok::<_, ToolModuleError>((
                node.id.as_str(),
                text.font_artifact.sha256.as_str(),
                text.box_width,
                text.box_height,
            ))
        })
        .collect::<Result<Vec<_>, _>>()?;
    if evidence.len() != expected.len() {
        return Err(ToolModuleError::Contract(
            "PerfectPixel text evidence count does not match text nodes".to_owned(),
        ));
    }
    let mut seen = BTreeSet::new();
    for entry in evidence {
        if !seen.insert(entry.node_id.as_str()) {
            return Err(ToolModuleError::Contract(
                "PerfectPixel text evidence contains duplicate node ids".to_owned(),
            ));
        }
        let Some((_, font_sha256, width, height)) = expected
            .iter()
            .find(|(node_id, _, _, _)| *node_id == entry.node_id.as_str())
        else {
            return Err(ToolModuleError::Contract(
                "PerfectPixel text evidence names an unknown node".to_owned(),
            ));
        };
        let layout = &entry.layout;
        if layout.schema != "perfectpixel.text-node/1"
            || !layout.font_sha256.eq_ignore_ascii_case(font_sha256)
            || layout.width != *width
            || layout.height != *height
            || layout.glyph_count != u32::try_from(layout.glyphs.len()).unwrap_or(u32::MAX)
            || layout.line_snapshots.len() != layout.lines.len()
            || !is_sha256(&layout.raster_sha256)
        {
            return Err(ToolModuleError::Contract(
                "PerfectPixel text layout evidence is inconsistent".to_owned(),
            ));
        }
        for line in &layout.line_snapshots {
            let end = line
                .glyph_start
                .checked_add(line.glyph_count)
                .ok_or_else(|| {
                    ToolModuleError::Contract(
                        "PerfectPixel text line glyph range overflowed".to_owned(),
                    )
                })?;
            if end > layout.glyph_count {
                return Err(ToolModuleError::Contract(
                    "PerfectPixel text line glyph range exceeds glyph evidence".to_owned(),
                ));
            }
        }
    }
    Ok(())
}

fn read_image_artifact(
    context: &ToolContext,
    artifact: &ArtifactRef,
) -> Result<Vec<u8>, ToolModuleError> {
    if artifact.media_type != "image/png"
        || artifact.byte_length == 0
        || artifact.byte_length > MAX_IMAGE_BYTES as u64
    {
        return Err(ToolModuleError::Contract(
            "image tool requires a bounded image/png artifact".to_owned(),
        ));
    }
    context.artifacts.read(artifact).map_err(|error| {
        ToolModuleError::Failure(AgentFailure::new(
            FailureCode::StorageFailed,
            format!("image artifact could not be verified: {error}"),
        ))
    })
}

fn read_font_artifact(
    context: &ToolContext,
    artifact: &ArtifactRef,
) -> Result<Vec<u8>, ToolModuleError> {
    if !matches!(artifact.media_type.as_str(), "font/ttf" | "font/otf")
        || artifact.byte_length == 0
        || artifact.byte_length > MAX_FONT_BYTES as u64
    {
        return Err(ToolModuleError::Contract(
            "text render requires a bounded font/ttf or font/otf artifact".to_owned(),
        ));
    }
    let extension = Path::new(&artifact.relative_path)
        .extension()
        .and_then(std::ffi::OsStr::to_str)
        .unwrap_or_default()
        .to_ascii_lowercase();
    if !matches!(extension.as_str(), "ttf" | "otf") {
        return Err(ToolModuleError::Contract(
            "text font artifact extension must match its font format".to_owned(),
        ));
    }
    context.artifacts.read(artifact).map_err(|error| {
        ToolModuleError::Failure(AgentFailure::new(
            FailureCode::StorageFailed,
            format!("text font artifact could not be verified: {error}"),
        ))
    })
}

fn validate_png(bytes: &[u8]) -> Result<(u32, u32), ToolModuleError> {
    if bytes.len() < 33 || !bytes.starts_with(b"\x89PNG\r\n\x1a\n") || &bytes[12..16] != b"IHDR" {
        return Err(ToolModuleError::Contract(
            "image artifact is not a PNG raster".to_owned(),
        ));
    }
    let width_bytes: [u8; 4] = bytes[16..20]
        .try_into()
        .map_err(|_| ToolModuleError::Contract("PNG width header is invalid".to_owned()))?;
    let height_bytes: [u8; 4] = bytes[20..24]
        .try_into()
        .map_err(|_| ToolModuleError::Contract("PNG height header is invalid".to_owned()))?;
    let width = u32::from_be_bytes(width_bytes);
    let height = u32::from_be_bytes(height_bytes);
    if width == 0 || height == 0 || width > 8192 || height > 8192 {
        return Err(ToolModuleError::Contract(
            "PNG dimensions exceed the bounded raster limit".to_owned(),
        ));
    }
    Ok((width, height))
}

fn exact_repair_requirement(
    requirements: ImageGenerationRequirements,
) -> Result<Option<ImageRepairRequirement>, ToolModuleError> {
    let (width, height) = requirements
        .size
        .and_then(ImageSizeRequirement::dimensions)
        .map_or((None, None), |(width, height)| (Some(width), Some(height)));
    let transparent_background = match requirements.background {
        Some(ImageBackgroundRequirement::Transparent) => Some(true),
        Some(ImageBackgroundRequirement::Opaque) => Some(false),
        Some(ImageBackgroundRequirement::Auto) | None => None,
    };
    let requirement = ImageRepairRequirement {
        width,
        height,
        transparent_background,
    };
    if requirement.width.is_none() && requirement.transparent_background.is_none() {
        return Ok(None);
    }
    requirement.validate().map_err(ToolModuleError::Contract)?;
    Ok(Some(requirement))
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

fn generation_receipt(
    call: &ToolCall,
    image: &ProviderImage,
) -> Result<EffectReceiptRef, ToolModuleError> {
    let digest = sha256_hex(&image.bytes);
    let id = ReceiptId::parse(format!("receipt-{digest}"))
        .map_err(|error| ToolModuleError::Contract(error.to_string()))?;
    Ok(EffectReceiptRef {
        id,
        digest,
        operation: format!("{IMAGE_GENERATE_TOOL}:{}", call.id),
        verified: true,
    })
}

#[cfg(test)]
#[test]
fn exact_repair_requirement_preserves_all_explicit_verifiable_constraints() {
    let exact = exact_repair_requirement(ImageGenerationRequirements {
        background: Some(ImageBackgroundRequirement::Transparent),
        size: Some(ImageSizeRequirement::Square),
        quality: None,
    })
    .expect("exact repair requirement")
    .expect("requirement");
    assert_eq!(
        exact,
        ImageRepairRequirement {
            width: Some(1024),
            height: Some(1024),
            transparent_background: Some(true),
        }
    );

    let unconstrained = exact_repair_requirement(ImageGenerationRequirements {
        background: Some(ImageBackgroundRequirement::Auto),
        size: Some(ImageSizeRequirement::Auto),
        quality: None,
    })
    .expect("auto repair requirement");
    assert_eq!(unconstrained, None);
}

fn provider_module_error(error: ProviderError) -> ToolModuleError {
    match error {
        ProviderError::Contract(message) => ToolModuleError::Contract(message),
        ProviderError::Failure(failure) if failure.code == FailureCode::Cancelled => {
            ToolModuleError::Failure(failure)
        }
        ProviderError::Failure(failure) if failure.code == FailureCode::ResolutionRequired => {
            ToolModuleError::Cancelled(failure)
        }
        ProviderError::Failure(failure) => ToolModuleError::Failure(failure),
    }
}

fn generated_image_uncertainty(error: &ToolModuleError) -> ToolModuleError {
    ToolModuleError::Cancelled(
        AgentFailure::new(
            FailureCode::ResolutionRequired,
            "generated image validation failed after external provider dispatch",
        )
        .with_detail("cause", error.to_string()),
    )
}

fn private_temp_directory(label: &str) -> Result<TempDir, ToolModuleError> {
    tempfile::Builder::new()
        .prefix(&format!("upagent-image-{label}-"))
        .tempdir()
        .map_err(|error| {
            ToolModuleError::Failure(AgentFailure::new(
                FailureCode::StorageFailed,
                format!("image tool temporary directory could not be created: {error}"),
            ))
        })
}

fn validate_executable_path(path: &Path) -> Result<(), String> {
    if !path.is_absolute()
        || path.as_os_str().is_empty()
        || path.to_string_lossy().contains('\0')
        || path
            .components()
            .any(|component| component == Component::ParentDir)
    {
        return Err(
            "PerfectPixel executable must be an absolute path without traversal".to_owned(),
        );
    }
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| format!("cannot inspect PerfectPixel executable: {error}"))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err("PerfectPixel executable must be a regular file".to_owned());
    }
    #[cfg(unix)]
    if metadata.permissions().mode() & 0o111 == 0 {
        return Err("PerfectPixel executable is not executable".to_owned());
    }
    Ok(())
}

fn digest_file(path: &Path) -> Result<String, String> {
    let bytes =
        fs::read(path).map_err(|error| format!("cannot read executable identity: {error}"))?;
    if bytes.is_empty() || bytes.len() > 128 * 1024 * 1024 {
        return Err("PerfectPixel executable size is outside the identity bound".to_owned());
    }
    Ok(sha256_hex(&bytes))
}

fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

#[allow(clippy::cast_precision_loss)]
fn validate_inspection_v2(inspection: &PerfectPixelInspectionV2) -> Result<(), ToolModuleError> {
    if inspection.pixel_format != "rgba8"
        || inspection.color_space != "srgb"
        || inspection.width == 0
        || inspection.height == 0
        || inspection.width > 8192
        || inspection.height > 8192
        || u64::from(inspection.width) * u64::from(inspection.height) > 8192 * 8192
        || inspection.edge_palette.len() > 16
    {
        return Err(ToolModuleError::Contract(
            "PerfectPixel v2 inspection is invalid".to_owned(),
        ));
    }
    let pixel_count = u64::from(inspection.width) * u64::from(inspection.height);
    let foreground_pixels = u64::try_from(inspection.foreground_pixels)
        .map_err(|_| ToolModuleError::Contract("foreground pixel count overflowed".to_owned()))?;
    if foreground_pixels > pixel_count {
        return Err(ToolModuleError::Contract(
            "foreground pixel count exceeds raster".to_owned(),
        ));
    }
    let expected_alpha_ratio = foreground_pixels as f64 / pixel_count as f64;
    if !inspection.alpha_ratio.is_finite()
        || !(0.0..=1.0).contains(&inspection.alpha_ratio)
        || (inspection.alpha_ratio - expected_alpha_ratio).abs() > INSPECTION_METRIC_TOLERANCE
    {
        return Err(ToolModuleError::Contract(
            "alpha ratio is inconsistent with foreground coverage".to_owned(),
        ));
    }
    // `foregroundPixels` is thresholded, but `hasAlpha` is true for any pixel
    // below full opacity. A fully foreground-covered raster may still contain
    // partial alpha, so validate only the implication that no alpha requires
    // every pixel to be foreground.
    if !inspection.has_alpha && foreground_pixels != pixel_count {
        return Err(ToolModuleError::Contract(
            "opaque inspection has incomplete foreground coverage".to_owned(),
        ));
    }
    match (
        foreground_pixels == 0,
        &inspection.content_box,
        &inspection.center,
    ) {
        (true, None, None) if inspection.ground_y == 0 && !inspection.touches_edge => {}
        (false, Some(content_box), Some(center)) => {
            if content_box.w == 0
                || content_box.h == 0
                || content_box.x >= inspection.width
                || content_box.y >= inspection.height
                || content_box
                    .x
                    .checked_add(content_box.w)
                    .is_none_or(|right| right > inspection.width)
                || content_box
                    .y
                    .checked_add(content_box.h)
                    .is_none_or(|bottom| bottom > inspection.height)
                || u64::from(content_box.w) * u64::from(content_box.h) < foreground_pixels
            {
                return Err(ToolModuleError::Contract(
                    "contentBox is outside the raster or foreground bounds".to_owned(),
                ));
            }
            let expected_center = InspectionPoint {
                x: content_box.x + content_box.w / 2,
                y: content_box.y + content_box.h / 2,
            };
            let expected_ground_y = content_box.y + content_box.h;
            let expected_touches_edge = content_box.x == 0
                || content_box.y == 0
                || content_box.x + content_box.w == inspection.width
                || content_box.y + content_box.h == inspection.height;
            if center != &expected_center
                || inspection.ground_y != expected_ground_y
                || inspection.touches_edge != expected_touches_edge
            {
                return Err(ToolModuleError::Contract(
                    "inspection geometry is internally inconsistent".to_owned(),
                ));
            }
        }
        _ => {
            return Err(ToolModuleError::Contract(
                "empty foreground geometry is inconsistent".to_owned(),
            ));
        }
    }
    let expected_edge_pixel_count =
        unique_perimeter_pixel_count(inspection.width, inspection.height);
    if inspection.edge_pixel_count != expected_edge_pixel_count {
        return Err(ToolModuleError::Contract(
            "edgePixelCount is not the unique raster perimeter".to_owned(),
        ));
    }
    let mut previous: Option<(u64, [u8; 3])> = None;
    let mut colors = BTreeSet::new();
    let mut reported = 0_u64;
    for entry in &inspection.edge_palette {
        if entry.count == 0 || !colors.insert(entry.rgb) {
            return Err(ToolModuleError::Contract(
                "edge palette contains invalid entries".to_owned(),
            ));
        }
        if let Some((count, rgb)) = previous
            && (entry.count > count || (entry.count == count && entry.rgb < rgb))
        {
            return Err(ToolModuleError::Contract(
                "edge palette ordering is invalid".to_owned(),
            ));
        }
        previous = Some((entry.count, entry.rgb));
        reported = reported
            .checked_add(entry.count)
            .ok_or_else(|| ToolModuleError::Contract("edge palette count overflowed".to_owned()))?;
    }
    if reported > inspection.edge_pixel_count
        || (inspection.edge_palette.len() < 16 && reported != inspection.edge_pixel_count)
    {
        return Err(ToolModuleError::Contract(
            "edge palette counts violate the bounded top-color contract".to_owned(),
        ));
    }
    Ok(())
}

fn unique_perimeter_pixel_count(width: u32, height: u32) -> u64 {
    match (width, height) {
        (1, 1) => 1,
        (1, height) => u64::from(height),
        (width, 1) => u64::from(width),
        (width, height) => 2 * u64::from(width) + 2 * u64::from(height) - 4,
    }
}

fn is_sha256(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn bounded_string(value: String, label: &str, maximum: usize) -> Result<String, String> {
    if value.trim().is_empty() || value.len() > maximum || value.contains('\0') {
        return Err(format!("{label} must be bounded and non-empty"));
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ArtifactStore;
    use agent_core::{ReasoningEffort, SelectedToolSideEffect, TaskId, ToolCallId};
    use agent_provider_api::ProviderImageResult;
    use std::fs;
    use tempfile::TempDir;

    const CANONICAL_PERFECTPIXEL_IMPLEMENTATION_VERSION: &str = "0.3.1";
    const CANONICAL_PERFECTPIXEL_CAPABILITY_MANIFEST_SHA256: &str =
        "c2d7ba67a85e4b7b2d29d61fd51f03134545f44a4eba9cf65f06b2afb298a0a8";

    // Test doubles use the current canonical identity. Tests that exercise
    // identity rejection alter the digest or provider binding explicitly.
    fn canonical_perfectpixel_manifest() -> PerfectPixelAgentManifest {
        let capabilities = [
            ("artifact.content_addressed", "2.0.0", "bit_exact"),
            ("artifact.dependency_closure", "2.0.0", "bit_exact"),
            ("artifact.pin_set", "2.0.0", "bit_exact"),
            ("compare.basic", "1.0.0", "bit_exact"),
            ("compare.exact_spec", "1.0.0", "bit_exact"),
            ("compare.geometry", "1.0.0", "bit_exact"),
            ("compare.masks", "1.0.0", "bit_exact"),
            ("compare.preview", "1.0.0", "bit_exact"),
            ("compare.regions", "1.0.0", "bit_exact"),
            ("extract.alpha", "1.0.0", "bit_exact"),
            ("extract.alpha_component", "1.0.0", "bit_exact"),
            ("extract.chroma_key", "1.0.0", "bit_exact"),
            ("extract.color_range", "1.0.0", "bit_exact"),
            ("extract.matte_refine", "1.0.0", "bit_exact"),
            ("extract.provided_mask", "1.0.0", "bit_exact"),
            ("inspect.basic", "2.0.0", "bit_exact"),
            ("render.affine_3x3", "1.0.0", "backend_deterministic"),
            ("render.composition_dag", "1.0.0", "backend_deterministic"),
            ("render.source_over", "1.0.0", "bit_exact"),
            ("render.text_node", "1.0.0", "bit_exact"),
        ]
        .into_iter()
        .map(|(name, version, determinism)| PerfectPixelAgentCapability {
            name: name.to_owned(),
            version: version.to_owned(),
            determinism: determinism.to_owned(),
        })
        .collect();
        PerfectPixelAgentManifest {
            schema: AGENT_IMAGE_PROTOCOL_SCHEMA.to_owned(),
            protocol_version: AGENT_IMAGE_PROTOCOL_VERSION.to_owned(),
            behavior_version: AGENT_IMAGE_BEHAVIOR_VERSION.to_owned(),
            implementation_version: CANONICAL_PERFECTPIXEL_IMPLEMENTATION_VERSION.to_owned(),
            capabilities,
        }
    }

    #[derive(Debug, Clone)]
    struct FakeImageProvider {
        image: ProviderImage,
        observed_reasoning: Option<Arc<std::sync::Mutex<Option<ReasoningEffort>>>>,
    }

    fn fake_provider_binding() -> ProviderBinding {
        ProviderBinding {
            id: "fixture-provider".to_owned(),
            model: "fixture-image-model".to_owned(),
            route_sha256: "a".repeat(64),
            capabilities: BTreeSet::from([
                agent_core::PROVIDER_CAP_TEXT.to_owned(),
                agent_core::PROVIDER_CAP_IMAGE_GENERATION.to_owned(),
            ]),
            maximum_output_bytes: 8 * 1024 * 1024,
        }
    }

    #[async_trait]
    impl ImagePort for FakeImageProvider {
        fn binding(&self) -> ProviderBinding {
            fake_provider_binding()
        }

        async fn generate_image(
            &self,
            _prompt: &str,
            _requirements: &ImageGenerationRequirements,
            reasoning: ReasoningEffort,
            _timeout_ms: u64,
            _maximum_bytes: usize,
        ) -> Result<ProviderImageResult, ProviderError> {
            if let Some(observed) = &self.observed_reasoning {
                *observed.lock().expect("reasoning observation") = Some(reasoning);
            }
            Ok(ProviderImageResult {
                image: self.image.clone(),
                status: ProviderImageStatus::Generated,
                observed_transparent_background: Some(false),
                mismatches: Vec::new(),
            })
        }

        async fn generate_image_with_cancellation(
            &self,
            _prompt: &str,
            _requirements: &ImageGenerationRequirements,
            reasoning: ReasoningEffort,
            _timeout_ms: u64,
            _maximum_bytes: usize,
            cancellation: &CancellationToken,
        ) -> Result<ProviderImageResult, ProviderError> {
            if cancellation.is_cancelled() {
                return Err(ProviderError::Failure(AgentFailure::new(
                    FailureCode::Cancelled,
                    "fake provider cancelled",
                )));
            }
            if let Some(observed) = &self.observed_reasoning {
                *observed.lock().expect("reasoning observation") = Some(reasoning);
            }
            Ok(ProviderImageResult {
                image: self.image.clone(),
                status: ProviderImageStatus::Generated,
                observed_transparent_background: Some(false),
                mismatches: Vec::new(),
            })
        }
    }

    #[derive(Debug, Clone)]
    struct DriftImageProvider {
        image: ProviderImage,
    }

    #[async_trait]
    impl ImagePort for DriftImageProvider {
        fn binding(&self) -> ProviderBinding {
            fake_provider_binding()
        }

        async fn generate_image(
            &self,
            _prompt: &str,
            _requirements: &ImageGenerationRequirements,
            _reasoning: ReasoningEffort,
            _timeout_ms: u64,
            _maximum_bytes: usize,
        ) -> Result<ProviderImageResult, ProviderError> {
            Ok(self.result())
        }

        async fn generate_image_with_cancellation(
            &self,
            _prompt: &str,
            _requirements: &ImageGenerationRequirements,
            _reasoning: ReasoningEffort,
            _timeout_ms: u64,
            _maximum_bytes: usize,
            cancellation: &CancellationToken,
        ) -> Result<ProviderImageResult, ProviderError> {
            if cancellation.is_cancelled() {
                return Err(ProviderError::Failure(AgentFailure::new(
                    FailureCode::Cancelled,
                    "drift provider cancelled",
                )));
            }
            Ok(self.result())
        }
    }

    impl DriftImageProvider {
        fn result(&self) -> ProviderImageResult {
            ProviderImageResult {
                image: self.image.clone(),
                status: ProviderImageStatus::GeneratedNeedsRepair,
                observed_transparent_background: Some(false),
                mismatches: vec![agent_provider_api::ImageGenerationMismatch::Size {
                    requested: agent_provider_api::ImageSizeRequirement::Square,
                    observed_width: self.image.width,
                    observed_height: self.image.height,
                }],
            }
        }
    }

    #[derive(Debug, Clone)]
    struct LyingImageProvider {
        image: ProviderImage,
    }

    #[async_trait]
    impl ImagePort for LyingImageProvider {
        fn binding(&self) -> ProviderBinding {
            fake_provider_binding()
        }

        async fn generate_image(
            &self,
            _prompt: &str,
            _requirements: &ImageGenerationRequirements,
            _reasoning: ReasoningEffort,
            _timeout_ms: u64,
            _maximum_bytes: usize,
        ) -> Result<ProviderImageResult, ProviderError> {
            Ok(self.result())
        }

        async fn generate_image_with_cancellation(
            &self,
            _prompt: &str,
            _requirements: &ImageGenerationRequirements,
            _reasoning: ReasoningEffort,
            _timeout_ms: u64,
            _maximum_bytes: usize,
            cancellation: &CancellationToken,
        ) -> Result<ProviderImageResult, ProviderError> {
            if cancellation.is_cancelled() {
                return Err(ProviderError::Failure(AgentFailure::new(
                    FailureCode::Cancelled,
                    "lying provider cancelled",
                )));
            }
            Ok(self.result())
        }
    }

    impl LyingImageProvider {
        fn result(&self) -> ProviderImageResult {
            ProviderImageResult {
                image: self.image.clone(),
                // Deliberately claim success and transparent output while the
                // host's PerfectPixel observation reports an opaque pixel.
                status: ProviderImageStatus::Generated,
                observed_transparent_background: Some(true),
                mismatches: Vec::new(),
            }
        }
    }

    #[derive(Debug)]
    struct SuccessfulSemanticMaskAdapter {
        mask: ArtifactRef,
    }

    #[async_trait]
    impl SemanticMaskAdapter for SuccessfulSemanticMaskAdapter {
        async fn infer(
            &self,
            request: SemanticMaskRequest,
            _cancellation: &CancellationToken,
        ) -> agent_core::SemanticMaskResult {
            agent_core::SemanticMaskResult {
                schema: agent_core::SEMANTIC_MASK_CONTRACT_SCHEMA.to_owned(),
                operation: request.operation,
                status: SemanticMaskResultStatus::Succeeded,
                mask_artifact: Some(self.mask.clone()),
                failure: None,
            }
        }
    }

    fn minimal_png(width: u32, height: u32) -> Vec<u8> {
        let mut bytes = b"\x89PNG\r\n\x1a\n".to_vec();
        bytes.extend_from_slice(&13u32.to_be_bytes());
        bytes.extend_from_slice(b"IHDR");
        bytes.extend_from_slice(&width.to_be_bytes());
        bytes.extend_from_slice(&height.to_be_bytes());
        bytes.extend_from_slice(&[8, 6, 0, 0, 0]);
        bytes.extend_from_slice(&0u32.to_be_bytes());
        bytes
    }

    fn valid_png() -> Vec<u8> {
        vec![
            0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48,
            0x44, 0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x04, 0x00, 0x00,
            0x00, 0xb5, 0x1c, 0x0c, 0x02, 0x00, 0x00, 0x00, 0x0b, 0x49, 0x44, 0x41, 0x54, 0x78,
            0xda, 0x63, 0x64, 0xf8, 0x0f, 0x00, 0x01, 0x05, 0x01, 0x01, 0x27, 0x18, 0xe3, 0x66,
            0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
        ]
    }

    fn fake_executable(directory: &TempDir) -> (PathBuf, String, String) {
        let path = directory.path().join("perfectpixel");
        let script = r#"#!/bin/sh
case "$1" in
agent-schema)
  printf '%s\n' '{"schema":"perfectpixel.agent-image/2","protocolVersion":"2.0.0","behaviorVersion":"2.0.0","implementationVersion":"0.3.1","capabilities":[{"name":"inspect.basic","version":"2.0.0","determinism":"bit_exact"},{"name":"artifact.content_addressed","version":"2.0.0","determinism":"bit_exact"},{"name":"artifact.dependency_closure","version":"2.0.0","determinism":"bit_exact"},{"name":"artifact.pin_set","version":"2.0.0","determinism":"bit_exact"},{"name":"extract.alpha","version":"1.0.0","determinism":"bit_exact"},{"name":"extract.chroma_key","version":"1.0.0","determinism":"bit_exact"},{"name":"extract.color_range","version":"1.0.0","determinism":"bit_exact"},{"name":"extract.alpha_component","version":"1.0.0","determinism":"bit_exact"},{"name":"extract.provided_mask","version":"1.0.0","determinism":"bit_exact"},{"name":"extract.matte_refine","version":"1.0.0","determinism":"bit_exact"},{"name":"render.composition_dag","version":"1.0.0","determinism":"backend_deterministic"},{"name":"render.affine_3x3","version":"1.0.0","determinism":"backend_deterministic"},{"name":"render.source_over","version":"1.0.0","determinism":"bit_exact"},{"name":"render.text_node","version":"1.0.0","determinism":"bit_exact"},{"name":"compare.basic","version":"1.0.0","determinism":"bit_exact"},{"name":"compare.exact_spec","version":"1.0.0","determinism":"bit_exact"},{"name":"compare.regions","version":"1.0.0","determinism":"bit_exact"},{"name":"compare.masks","version":"1.0.0","determinism":"bit_exact"},{"name":"compare.geometry","version":"1.0.0","determinism":"bit_exact"},{"name":"compare.preview","version":"1.0.0","determinism":"bit_exact"}]}'
  ;;
agent-inspect)
  request_path="$3"
  directory="$(dirname "$request_path")"
  input="$directory/input.png"
  if [ "$(wc -c < "$input")" -lt 60 ]; then exit 3; fi
  digest="$(shasum -a 256 "$input" | awk '{print $1}')"
  size="$(wc -c < "$input" | tr -d ' ')"
  request_id="$(sed -n 's/.*"requestId":"\([^"]*\)".*/\1/p' "$request_path")"
  request_digest="$(shasum -a 256 "$request_path" | awk '{print $1}')"
  source_closure="4f53cda18c2baa0c0354bb5f9a3ecbe5ed12ab4d8e11ba873c2f11161202b945"
  dependency_json="[{\"sha256\":\"$digest\",\"mediaType\":\"image/png\",\"byteLength\":$size}]"
  dependency_closure="$(printf '%s' "$dependency_json" | shasum -a 256 | awk '{print $1}')"
  printf '%s\n' "{\"schema\":\"perfectpixel.agent-image/inspect-result/2\",\"requestId\":\"$request_id\",\"status\":\"committed\",\"protocolVersion\":\"2.0.0\",\"behaviorVersion\":\"2.0.0\",\"source\":{\"sha256\":\"$digest\",\"mediaType\":\"image/png\",\"byteLength\":$size,\"kind\":\"working_raster\",\"pixelSpec\":{\"format\":\"rgba8\",\"colorSpace\":\"srgb\",\"alphaMode\":\"straight\"},\"retention\":\"task_scoped\",\"dependencies\":[],\"dependencyClosureSha256\":\"$source_closure\"},\"inspection\":{\"width\":1,\"height\":1,\"foregroundPixels\":1,\"alphaRatio\":1.0,\"contentBox\":{\"x\":0,\"y\":0,\"w\":1,\"h\":1},\"center\":{\"x\":0,\"y\":0},\"groundY\":1,\"touchesEdge\":true,\"hasAlpha\":false,\"pixelFormat\":\"rgba8\",\"colorSpace\":\"srgb\",\"edgePixelCount\":1,\"edgePalette\":[{\"rgb\":[0,0,0],\"count\":1}]},\"receipt\":{\"schema\":\"perfectpixel.agent-image/receipt/2\",\"requestId\":\"$request_id\",\"operation\":\"inspect\",\"status\":\"committed\",\"behaviorVersion\":\"2.0.0\",\"implementationVersion\":\"0.3.1\",\"requestSha256\":\"$request_digest\",\"dependencyClosureSha256\":\"$dependency_closure\",\"dependencies\":$dependency_json,\"determinism\":\"bit_exact\"}}"
  ;;
agent-extract|agent-render|agent-compare)
  python3 - "$1" "$3" "$5" <<'PY'
import hashlib
import json
import os
import sys

operation, request_path, output_dir = sys.argv[1:]
request_bytes = open(request_path, "rb").read()
request = json.loads(request_bytes)

def dependency(value):
    return {
        "sha256": value["expectedSha256"],
        "mediaType": value["mediaType"],
        "byteLength": value["byteLength"],
    }

def normalized(values):
    unique = {
        (value["sha256"], value["mediaType"], value["byteLength"]): value
        for value in values
    }
    return [unique[key] for key in sorted(unique)]

def closure(values):
    encoded = json.dumps(values, separators=(",", ":")).encode()
    return hashlib.sha256(encoded).hexdigest()

def descriptor(payload, kind, dependencies):
    return {
        "sha256": hashlib.sha256(payload).hexdigest(),
        "mediaType": "image/png",
        "byteLength": len(payload),
        "kind": kind,
        "pixelSpec": {"format": "rgba8", "colorSpace": "srgb", "alphaMode": "straight"},
        "retention": "task_scoped",
        "dependencies": dependencies,
        "dependencyClosureSha256": closure(dependencies),
    }

def receipt(name, status, dependencies, determinism, output_sha=None):
    value = {
        "schema": "perfectpixel.agent-image/receipt/2",
        "requestId": request["requestId"],
        "operation": name,
        "status": status,
        "behaviorVersion": "2.0.0",
        "implementationVersion": "0.3.1",
        "requestSha256": hashlib.sha256(request_bytes).hexdigest(),
        "dependencyClosureSha256": closure(dependencies),
        "dependencies": dependencies,
        "determinism": determinism,
    }
    if output_sha is not None:
        value["outputSha256"] = output_sha
    return value

if operation == "agent-extract":
    source = request["source"]
    dependencies = normalized([dependency(source)])
    payload = open(source["path"], "rb").read()
    os.makedirs(output_dir)
    for name in ("object.png", "mask.png"):
        open(os.path.join(output_dir, name), "wb").write(payload)
    object_descriptor = descriptor(payload, "object", dependencies)
    mask_descriptor = descriptor(payload, "mask", dependencies)
    response = {
        "schema": "perfectpixel.agent-image/extract-result/2",
        "requestId": request["requestId"],
        "status": "committed",
        "protocolVersion": "2.0.0",
        "behaviorVersion": "2.0.0",
        "sourceCanvas": {"width": 1, "height": 1},
        "sourceBbox": {"x": 0, "y": 0, "width": 1, "height": 1},
        "pivot": [0.5, 0.5],
        "completeness": "complete",
        "occludedBackground": "unknown",
        "softAlphaPixels": 0,
        "object": {"relativePath": "object.png", "descriptor": object_descriptor},
        "mask": {"relativePath": "mask.png", "descriptor": mask_descriptor},
        "receipt": receipt("extract", "committed", dependencies, "bit_exact", object_descriptor["sha256"]),
    }
elif operation == "agent-render":
    sources = [node["source"] for node in request["nodes"] if "source" in node]
    text_nodes = [node for node in request["nodes"] if node.get("kind") == "text"]
    dependencies = [dependency(source) for source in sources]
    dependencies.extend({
        "sha256": node["text"]["font"]["artifact"]["sha256"],
        "mediaType": node["text"]["font"]["artifact"]["mediaType"],
        "byteLength": node["text"]["font"]["artifact"]["byteLength"],
    } for node in text_nodes)
    dependencies = normalized(dependencies)
    if sources:
        payload = open(sources[0]["path"], "rb").read()
    else:
        payload = bytes.fromhex("89504e470d0a1a0a0000000d4948445200000001000000010804000000b51c0c020000000049454e44ae426082")
    os.makedirs(output_dir)
    open(os.path.join(output_dir, "render.png"), "wb").write(payload)
    output_descriptor = descriptor(payload, "working_raster", dependencies)
    response = {
        "schema": "perfectpixel.agent-image/render-result/2",
        "requestId": request["requestId"],
        "status": "committed",
        "protocolVersion": "2.0.0",
        "behaviorVersion": "2.0.0",
        "output": {"relativePath": "render.png", "descriptor": output_descriptor},
        "textNodes": [{
            "nodeId": node["id"],
            "layout": {
                "schema": "perfectpixel.text-node/1",
                "fontSha256": node["text"]["font"]["artifact"]["sha256"],
                "lines": [node["text"]["content"]],
                "lineSnapshots": [{"text": node["text"]["content"], "glyphStart": 0, "glyphCount": 0, "advanceUnits": 0}],
                "glyphs": [],
                "glyphCount": 0,
                "width": node["text"]["boxWidth"],
                "height": node["text"]["boxHeight"],
                "rasterSha256": hashlib.sha256(payload).hexdigest(),
            },
        } for node in text_nodes],
        "receipt": receipt("render", "committed", dependencies, "backend_deterministic", output_descriptor["sha256"]),
    }
else:
    inputs = [request["before"], request["after"]]
    for assertion in request["assertions"]:
        if "mask" in assertion:
            inputs.append(assertion["mask"])
    dependencies = normalized([dependency(value) for value in inputs])
    assertions = [{
        "id": assertion["id"],
        "assertionType": assertion["type"],
        "severity": assertion["severity"],
        "passed": True,
        "observed": True,
        "message": "fake deterministic evaluation",
    } for assertion in request["assertions"]]
    response = {
        "schema": "perfectpixel.agent-image/compare-result/2",
        "requestId": request["requestId"],
        "status": "committed",
        "protocolVersion": "2.0.0",
        "behaviorVersion": "2.0.0",
        "allRequiredPassed": True,
        "metrics": {
            "changedPixelRatio": 0.0,
            "meanAbsoluteError": 0.0,
            "maximumChannelError": 0,
            "alphaIou": 1.0,
            "afterForegroundPixels": 1,
            "afterBounds": {"x": 0, "y": 0, "width": 1, "height": 1},
            "afterCentroid": [0.0, 0.0],
        },
        "assertions": assertions,
        "previews": [],
        "receipt": receipt("compare", "committed", dependencies, "bit_exact"),
    }

print(json.dumps(response, separators=(",", ":")))
PY
  ;;
*)
  exit 2
  ;;
esac
"#;
        fs::write(&path, script).expect("fake PerfectPixel");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut permissions = fs::metadata(&path).expect("fake metadata").permissions();
            permissions.set_mode(0o700);
            fs::set_permissions(&path, permissions).expect("fake executable permissions");
        }
        let digest = sha256_hex(&fs::read(&path).expect("fake executable bytes"));
        let manifest_digest = observed_manifest_digest(&path);
        assert_eq!(
            manifest_digest, CANONICAL_PERFECTPIXEL_CAPABILITY_MANIFEST_SHA256,
            "the fake must publish the current canonical PerfectPixel manifest"
        );
        (path, digest, manifest_digest)
    }

    fn observed_manifest_digest(path: &Path) -> String {
        let output = std::process::Command::new(path)
            .arg("agent-schema")
            .output()
            .expect("fake agent manifest");
        assert!(output.status.success());
        let manifest: PerfectPixelAgentManifest =
            serde_json::from_slice(&output.stdout).expect("strict fake agent manifest");
        perfectpixel_manifest_sha256(&manifest).expect("fake agent manifest digest")
    }

    fn external_perfectpixel_font() -> Vec<u8> {
        let font = PathBuf::from(
            std::env::var_os("PERFECTPIXEL_FONT")
                .expect("PERFECTPIXEL_FONT must point to a caller-supplied valid TTF"),
        );
        fs::read(font).expect("caller-supplied PerfectPixel font")
    }

    fn test_image_module(
        provider: Arc<dyn ImagePort>,
        executable: impl Into<PathBuf>,
        executable_sha256: impl Into<String>,
        manifest_sha256: impl Into<String>,
    ) -> ImageToolModule {
        test_image_module_with_version(
            provider,
            executable,
            executable_sha256,
            manifest_sha256,
            CANONICAL_PERFECTPIXEL_IMPLEMENTATION_VERSION,
        )
    }

    fn test_image_module_with_version(
        provider: Arc<dyn ImagePort>,
        executable: impl Into<PathBuf>,
        executable_sha256: impl Into<String>,
        manifest_sha256: impl Into<String>,
        implementation_version: &str,
    ) -> ImageToolModule {
        let provider_binding = provider.binding();
        ImageToolModule::new(
            provider,
            &provider_binding,
            executable,
            executable_sha256,
            AGENT_IMAGE_PROTOCOL_SCHEMA,
            AGENT_IMAGE_PROTOCOL_VERSION,
            AGENT_IMAGE_BEHAVIOR_VERSION,
            manifest_sha256,
            implementation_version,
        )
        .expect("image module")
    }

    fn context(store: &ArtifactStore, cancellation: CancellationToken) -> ToolContext {
        ToolContext {
            task_id: TaskId::parse("task-image").expect("task id"),
            profile: ProfileId::creator(),
            track: None,
            reasoning: ReasoningEffort::XHigh,
            timeout_ms: 10_000,
            cancellation,
            artifacts: store.reader(),
            semantic_mask_operation: None,
        }
    }

    fn generate_call() -> ToolCall {
        ToolCall {
            id: ToolCallId::parse("call-image").expect("call id"),
            name: IMAGE_GENERATE_TOOL.to_owned(),
            arguments: json!({"prompt": "a bounded test image"}),
        }
    }

    #[test]
    fn compiled_prompt_artifact_is_bound_to_the_admitted_compiler_identity() {
        let artifact_directory = TempDir::new().expect("artifact directory");
        let store = ArtifactStore::open(artifact_directory.path()).expect("artifact store");
        let task = TaskId::parse("task-image").expect("task id");
        let compiler_sha256 = "d".repeat(64);
        let prompt = "draw a bounded test image".to_owned();
        let prompt_sha256 = agent_core::prompt_content_sha256(prompt.as_bytes());
        let compiled = agent_core::CompiledImagePrompt {
            status: agent_core::CompiledPromptStatus::Valid,
            input_sha256: "a".repeat(64),
            normalized_input_sha256: "b".repeat(64),
            compiler_outcome_sha256: "c".repeat(64),
            prompt: prompt.clone(),
            prompt_sha256: prompt_sha256.clone(),
            output: agent_core::CompiledImageOutput {
                width: 1024,
                height: 1024,
                background: agent_core::CompiledImageBackground::Opaque,
                detail: agent_core::CompiledImageDetail::High,
            },
        };
        let artifact =
            agent_core::ImagePromptArtifact::new(task.clone(), compiler_sha256.clone(), compiled)
                .expect("prompt artifact");
        let bytes = serde_json::to_vec(&artifact).expect("prompt artifact json");
        let reference = store
            .put(&bytes, agent_core::PROMPT_MEDIA_TYPE, "json")
            .expect("store prompt artifact");
        let context = context(&store, CancellationToken::new());
        let args = GenerateArguments {
            prompt: None,
            prompt_artifact: Some(reference),
            prompt_sha256: Some(prompt_sha256),
            maximum_bytes: None,
            background: None,
            size: None,
            quality: None,
        };

        let (_, requirements) = resolve_generation_prompt(&context, &args, Some(&compiler_sha256))
            .expect("artifact request");
        assert_eq!(requirements.size, Some(ImageSizeRequirement::Square));
        assert_eq!(
            requirements.background,
            Some(ImageBackgroundRequirement::Opaque)
        );
        assert_eq!(requirements.quality, None);
        assert!(resolve_generation_prompt(&context, &args, Some(&"e".repeat(64))).is_err());
        assert!(resolve_generation_prompt(&context, &args, None).is_err());
    }

    #[test]
    fn render_operations_are_canonical_and_strict() {
        let definition = render_definition();
        let operations =
            &definition.input_schema["properties"]["nodes"]["items"]["properties"]["operations"];
        assert_eq!(operations["maxItems"], 64);
        assert_eq!(
            operations["items"]["oneOf"][3]["properties"]["filter"]["enum"][1],
            "lanczos3"
        );
        let valid = json!({
            "op": "remove_background_auto",
            "maxKeys": 1,
            "minEdgeCoverageBasisPoints": 9000,
            "tolerance": 8,
            "feather": 4
        });
        assert!(serde_json::from_value::<RenderOperation>(valid.clone()).is_ok());
        let mut missing = valid.clone();
        missing
            .as_object_mut()
            .expect("operation")
            .remove("feather");
        assert!(serde_json::from_value::<RenderOperation>(missing).is_err());
        let mut unknown = valid;
        unknown["unexpected"] = json!(true);
        assert!(serde_json::from_value::<RenderOperation>(unknown).is_err());
    }

    #[test]
    fn compare_tool_schema_matches_exact_verification_preconditions() {
        let definition = compare_definition();
        let assertions = &definition.input_schema["properties"]["assertions"];
        assert_eq!(
            assertions["contains"]["properties"]["severity"]["const"],
            "required"
        );
        assert_eq!(assertions["minContains"], 1);
        let changed_ratio = &assertions["items"]["oneOf"][1];
        assert_eq!(
            changed_ratio["properties"]["type"]["const"],
            "changed_ratio"
        );
        assert_eq!(changed_ratio["anyOf"][0]["required"][0], "minimum");
        assert_eq!(changed_ratio["anyOf"][1]["required"][0], "maximum");
    }

    #[test]
    fn capability_manifest_digest_and_version_compatibility_are_strict() {
        let manifest = canonical_perfectpixel_manifest();
        assert_eq!(
            perfectpixel_manifest_sha256(&manifest).expect("manifest digest"),
            CANONICAL_PERFECTPIXEL_CAPABILITY_MANIFEST_SHA256
        );
        let mut duplicate = manifest.clone();
        duplicate
            .capabilities
            .push(manifest.capabilities[0].clone());
        assert!(perfectpixel_manifest_sha256(&duplicate).is_err());
        assert!(compatible_semver_at_least("2.1.0", "2.0.0"));
        assert!(!compatible_semver_at_least("3.0.0", "2.0.0"));
    }

    #[test]
    fn module_tools_match_canonical_receipt_bindings() {
        let module_tools = canonical_module_tools().expect("canonical module tools");
        let names = vec![
            IMAGE_GENERATE_TOOL.to_owned(),
            IMAGE_INSPECT_TOOL.to_owned(),
            IMAGE_EXTRACT_TOOL.to_owned(),
            IMAGE_RENDER_TOOL.to_owned(),
            IMAGE_COMPARE_TOOL.to_owned(),
        ];
        let bindings = crate::canonical_tool_bindings(&names).expect("canonical bindings");
        assert_eq!(module_tools.len(), bindings.len());
        for module_tool in &module_tools {
            let binding = bindings
                .iter()
                .find(|binding| binding.name == module_tool.definition.name)
                .expect("binding for module tool");
            assert_eq!(binding.definition, module_tool.definition);
            assert_eq!(
                binding.side_effect,
                match module_tool.side_effect {
                    ToolSideEffect::ReadOnly => SelectedToolSideEffect::ReadOnly,
                    ToolSideEffect::External => SelectedToolSideEffect::External,
                }
            );
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn fake_generate_returns_one_receipt_and_one_bounded_artifact() {
        let executable_directory = TempDir::new().expect("executable directory");
        let (executable, digest, manifest_digest) = fake_executable(&executable_directory);
        let bytes = valid_png();
        let observed_reasoning = Arc::new(std::sync::Mutex::new(None));
        let provider = Arc::new(FakeImageProvider {
            image: ProviderImage {
                bytes: bytes.clone(),
                media_type: "image/png".to_owned(),
                width: 1,
                height: 1,
            },
            observed_reasoning: Some(observed_reasoning.clone()),
        });
        let module = test_image_module(provider, executable, digest, manifest_digest);
        let artifact_directory = TempDir::new().expect("artifact directory");
        let store = ArtifactStore::open(artifact_directory.path()).expect("artifact store");
        let execution = module
            .execute(context(&store, CancellationToken::new()), generate_call())
            .await
            .expect("image generation");
        assert_eq!(execution.artifacts.len(), 1);
        assert_eq!(execution.receipts.len(), 1);
        assert_eq!(execution.artifacts[0].bytes, bytes);
        let published_sha256 = sha256_hex(&execution.artifacts[0].bytes);
        assert_eq!(execution.receipts[0].digest, published_sha256);
        assert_eq!(
            execution.receipts[0].id.as_str(),
            format!("receipt-{published_sha256}")
        );
        assert_eq!(execution.result.value["contentSha256"], published_sha256);
        assert_eq!(
            execution.result.value["receipt"]["outputSha256"],
            published_sha256
        );
        assert_eq!(
            *observed_reasoning.lock().expect("reasoning observation"),
            Some(ReasoningEffort::XHigh)
        );
        assert!(execution.result.value.get("artifact").is_none());
        let reference = store
            .put(
                &execution.artifacts[0].bytes,
                &execution.artifacts[0].media_type,
                &execution.artifacts[0].extension,
            )
            .expect("durable artifact");
        assert_eq!(store.read(&reference).expect("read artifact"), bytes);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn generated_drift_preserves_success_artifact_and_marks_repair() {
        let executable_directory = TempDir::new().expect("executable directory");
        let (executable, digest, manifest_digest) = fake_executable(&executable_directory);
        let bytes = valid_png();
        let module = test_image_module(
            Arc::new(DriftImageProvider {
                image: ProviderImage {
                    bytes: bytes.clone(),
                    media_type: "image/png".to_owned(),
                    width: 1,
                    height: 1,
                },
            }),
            executable,
            digest,
            manifest_digest,
        );
        let artifact_directory = TempDir::new().expect("artifact directory");
        let store = ArtifactStore::open(artifact_directory.path()).expect("artifact store");
        let mut call = generate_call();
        call.arguments = json!({
            "prompt": "a bounded test image",
            "size": "1024x1024"
        });
        let execution = module
            .execute(context(&store, CancellationToken::new()), call)
            .await
            .expect("valid drift is still a successful generation");
        assert!(!execution.result.is_error);
        assert_eq!(
            execution.result.value["generationStatus"],
            json!("generated_needs_repair")
        );
        assert_eq!(execution.result.value["repairRequired"], json!(true));
        assert!(
            execution.result.value["agentMessage"]
                .as_str()
                .is_some_and(|message| message.contains("PerfectPixel"))
        );
        assert_eq!(execution.artifacts[0].bytes, bytes);
        assert!(
            execution.receipts[0].verified,
            "known validated effect is separate from repair/acceptance"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn host_perfectpixel_observation_overrides_lying_provider_metadata() {
        let executable_directory = TempDir::new().expect("executable directory");
        let (executable, digest, manifest_digest) = fake_executable(&executable_directory);
        let bytes = valid_png();
        let module = test_image_module(
            Arc::new(LyingImageProvider {
                image: ProviderImage {
                    bytes: bytes.clone(),
                    media_type: "image/png".to_owned(),
                    width: 1,
                    height: 1,
                },
            }),
            executable,
            digest,
            manifest_digest,
        );
        let artifact_directory = TempDir::new().expect("artifact directory");
        let store = ArtifactStore::open(artifact_directory.path()).expect("artifact store");
        let mut call = generate_call();
        call.arguments = json!({
            "prompt": "a bounded test image",
            "background": "transparent",
            "size": "1024x1024"
        });
        let execution = module
            .execute(context(&store, CancellationToken::new()), call)
            .await
            .expect("valid provider bytes remain a successful generation");
        assert!(!execution.result.is_error);
        assert_eq!(
            execution.result.value["generationStatus"],
            json!("generated_needs_repair")
        );
        assert_eq!(execution.result.value["repairRequired"], json!(true));
        assert_eq!(execution.result.value["providerStatus"], json!("generated"));
        assert_eq!(execution.result.value["providerMismatches"], json!([]));
        assert_eq!(
            execution.result.value["hostObservedTransparentBackground"],
            json!(false)
        );
        assert_eq!(
            execution.result.value["providerHostDisagreement"],
            json!(true)
        );
        assert!(
            execution.result.value["mismatches"]
                .as_array()
                .is_some_and(|mismatches| {
                    mismatches
                        .iter()
                        .any(|mismatch| mismatch["kind"] == json!("background"))
                })
        );
        assert_eq!(execution.artifacts[0].bytes, bytes);
        assert!(
            execution.receipts[0].verified,
            "known validated effect is separate from repair/acceptance"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn image_constructors_share_identity_tools_and_unavailable_default_adapter() {
        let directory = TempDir::new().expect("executable directory");
        let (executable, digest, manifest_digest) = fake_executable(&directory);
        let provider: Arc<dyn ImagePort> = Arc::new(FakeImageProvider {
            image: ProviderImage {
                bytes: valid_png(),
                media_type: "image/png".to_owned(),
                width: 1,
                height: 1,
            },
            observed_reasoning: None,
        });
        let default = test_image_module(
            provider.clone(),
            &executable,
            digest.to_ascii_uppercase(),
            manifest_digest.to_ascii_uppercase(),
        );
        let explicit = ImageToolModule::new_with_semantic_mask_adapter(
            provider,
            &fake_provider_binding(),
            &executable,
            &digest,
            AGENT_IMAGE_PROTOCOL_SCHEMA,
            AGENT_IMAGE_PROTOCOL_VERSION,
            AGENT_IMAGE_BEHAVIOR_VERSION,
            &manifest_digest,
            CANONICAL_PERFECTPIXEL_IMPLEMENTATION_VERSION,
            Arc::new(UnavailableSemanticMaskAdapter),
        )
        .expect("explicit unavailable adapter");
        assert_eq!(default.identity(), explicit.identity());
        assert_eq!(default.identity().executable_sha256, digest);
        assert_eq!(
            default.identity().capability_manifest_sha256,
            manifest_digest
        );
        let store = ArtifactStore::open(directory.path().join("artifacts")).expect("store");
        let source = store.put(&valid_png(), "image/png", "png").expect("source");
        let selected =
            BTreeSet::from([IMAGE_INSPECT_TOOL.to_owned(), IMAGE_EXTRACT_TOOL.to_owned()]);
        for module in [default, explicit] {
            let result = module
                .semantic_mask_adapter
                .infer(
                    SemanticMaskRequest {
                        operation: agent_core::SemanticMaskOperation {
                            source_artifact: source.clone(),
                            source_revision: 1,
                            helper_identity: "semantic.fixture/1".to_owned(),
                            request_revision: 1,
                            tool_call_id: ToolCallId::parse("constructor-mask").expect("call id"),
                        },
                        target: SemanticMaskTarget::Point {
                            x_millionths: 500_000,
                            y_millionths: 500_000,
                        },
                    },
                    &CancellationToken::new(),
                )
                .await;
            assert_eq!(result.status, SemanticMaskResultStatus::Failed);
            assert_eq!(
                result.failure.as_ref().map(|failure| failure.code),
                Some(FailureCode::CapabilityDenied)
            );
            assert!(result.mask_artifact.is_none());
            let module = module
                .with_promptgen(None, &selected)
                .expect("selected tools");
            assert_eq!(
                module
                    .tools()
                    .iter()
                    .map(|tool| tool.definition.name.as_str())
                    .collect::<Vec<_>>(),
                [IMAGE_INSPECT_TOOL, IMAGE_EXTRACT_TOOL]
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn image_constructors_preserve_distinct_provider_and_perfectpixel_error_priority() {
        let provider: Arc<dyn ImagePort> = Arc::new(FakeImageProvider {
            image: ProviderImage {
                bytes: valid_png(),
                media_type: "image/png".to_owned(),
                width: 1,
                height: 1,
            },
            observed_reasoning: None,
        });
        let mut wrong_binding = fake_provider_binding();
        wrong_binding.route_sha256 = "b".repeat(64);
        let default_error = ImageToolModule::new(
            provider.clone(),
            &wrong_binding,
            "relative-executable",
            "not-a-digest",
            "unsupported-protocol",
            "1.0.0",
            "1.0.0",
            "not-a-manifest-digest",
            "fixture",
        )
        .expect_err("default constructor validates PerfectPixel first");
        assert_eq!(
            default_error,
            "PerfectPixel executable must be an absolute path without traversal"
        );
        let explicit_error = ImageToolModule::new_with_semantic_mask_adapter(
            provider,
            &wrong_binding,
            "relative-executable",
            "not-a-digest",
            "unsupported-protocol",
            "1.0.0",
            "1.0.0",
            "not-a-manifest-digest",
            "fixture",
            Arc::new(UnavailableSemanticMaskAdapter),
        )
        .expect_err("explicit constructor validates provider first");
        assert_eq!(
            explicit_error,
            "image provider binding does not match the provider sealed by composition"
        );
    }

    #[cfg(unix)]
    #[test]
    #[cfg_attr(
        target_os = "macos",
        ignore = "requires a filesystem accepting non-UTF-8 names; current macOS volume rejects them"
    )]
    fn image_constructors_preserve_non_utf8_executable_paths()
    -> Result<(), Box<dyn std::error::Error>> {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;

        let directory = TempDir::new()?;
        let (executable, digest, manifest_digest) = fake_executable(&directory);
        let non_utf8 = directory
            .path()
            .join(OsString::from_vec(b"perfectpixel-\xff".to_vec()));
        fs::rename(&executable, &non_utf8)?;
        let provider: Arc<dyn ImagePort> = Arc::new(FakeImageProvider {
            image: ProviderImage {
                bytes: valid_png(),
                media_type: "image/png".to_owned(),
                width: 1,
                height: 1,
            },
            observed_reasoning: None,
        });
        let default = test_image_module(provider.clone(), &non_utf8, &digest, &manifest_digest);
        let explicit = ImageToolModule::new_with_semantic_mask_adapter(
            provider,
            &fake_provider_binding(),
            &non_utf8,
            &digest,
            AGENT_IMAGE_PROTOCOL_SCHEMA,
            AGENT_IMAGE_PROTOCOL_VERSION,
            AGENT_IMAGE_BEHAVIOR_VERSION,
            &manifest_digest,
            CANONICAL_PERFECTPIXEL_IMPLEMENTATION_VERSION,
            Arc::new(UnavailableSemanticMaskAdapter),
        )?;
        for module in [default, explicit] {
            assert_eq!(module.executable(), non_utf8.canonicalize()?);
            assert!(module.executable().to_str().is_none());
        }
        Ok(())
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn both_image_constructors_recheck_binary_drift_before_spawning() {
        for explicit in [false, true] {
            let directory = TempDir::new().expect("executable directory");
            let (executable, digest, manifest_digest) = fake_executable(&directory);
            let provider: Arc<dyn ImagePort> = Arc::new(FakeImageProvider {
                image: ProviderImage {
                    bytes: valid_png(),
                    media_type: "image/png".to_owned(),
                    width: 1,
                    height: 1,
                },
                observed_reasoning: None,
            });
            let module = if explicit {
                ImageToolModule::new_with_semantic_mask_adapter(
                    provider,
                    &fake_provider_binding(),
                    &executable,
                    digest,
                    AGENT_IMAGE_PROTOCOL_SCHEMA,
                    AGENT_IMAGE_PROTOCOL_VERSION,
                    AGENT_IMAGE_BEHAVIOR_VERSION,
                    manifest_digest,
                    CANONICAL_PERFECTPIXEL_IMPLEMENTATION_VERSION,
                    Arc::new(UnavailableSemanticMaskAdapter),
                )
                .expect("explicit module")
            } else {
                test_image_module(provider, &executable, digest, manifest_digest)
            };
            let marker = directory.path().join("drifted-perfectpixel-spawned");
            let marker = marker.to_str().expect("fixture marker path");
            fs::write(
                &executable,
                format!("#!/bin/sh\n: > '{}'\n", marker.replace('\'', "'\\''")),
            )
            .expect("replace pinned executable");
            let store = ArtifactStore::open(directory.path().join("artifacts")).expect("store");
            let source = store.put(&valid_png(), "image/png", "png").expect("source");
            let error = module
                .execute(
                    context(&store, CancellationToken::new()),
                    ToolCall {
                        id: ToolCallId::parse("binary-drift").expect("call id"),
                        name: IMAGE_INSPECT_TOOL.to_owned(),
                        arguments: json!({"artifact":source}),
                    },
                )
                .await
                .expect_err("binary drift must fail before the image operation");
            assert!(
                matches!(error, ToolModuleError::Contract(message) if message == "PerfectPixel executable changed after composition")
            );
            assert!(
                !Path::new(marker).exists(),
                "drifted executable must not spawn"
            );
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn manifest_digest_drift_fails_before_image_operation() {
        let executable_directory = TempDir::new().expect("executable directory");
        let (executable, digest, _) = fake_executable(&executable_directory);
        let module = ImageToolModule::new(
            Arc::new(FakeImageProvider {
                image: ProviderImage {
                    bytes: valid_png(),
                    media_type: "image/png".to_owned(),
                    width: 1,
                    height: 1,
                },
                observed_reasoning: None,
            }),
            &fake_provider_binding(),
            executable,
            digest,
            AGENT_IMAGE_PROTOCOL_SCHEMA,
            AGENT_IMAGE_PROTOCOL_VERSION,
            AGENT_IMAGE_BEHAVIOR_VERSION,
            "f".repeat(64),
            CANONICAL_PERFECTPIXEL_IMPLEMENTATION_VERSION,
        )
        .expect("module construction only measures executable identity");
        let artifact_directory = TempDir::new().expect("artifact directory");
        let store = ArtifactStore::open(artifact_directory.path()).expect("artifact store");
        let source_ref = store
            .put(&valid_png(), "image/png", "png")
            .expect("source artifact");
        let error = module
            .execute(
                context(&store, CancellationToken::new()),
                ToolCall {
                    id: ToolCallId::parse("call-manifest-drift").expect("call id"),
                    name: IMAGE_INSPECT_TOOL.to_owned(),
                    arguments: json!({"artifact":source_ref}),
                },
            )
            .await
            .expect_err("manifest drift must fail");
        assert!(
            matches!(error, ToolModuleError::Contract(message) if message.contains("protocol identity changed"))
        );
    }

    #[cfg(unix)]
    #[test]
    fn image_module_rejects_provider_not_sealed_by_composition() {
        let executable_directory = TempDir::new().expect("executable directory");
        let (executable, digest, manifest_digest) = fake_executable(&executable_directory);
        let provider: Arc<dyn ImagePort> = Arc::new(FakeImageProvider {
            image: ProviderImage {
                bytes: valid_png(),
                media_type: "image/png".to_owned(),
                width: 1,
                height: 1,
            },
            observed_reasoning: None,
        });
        let mut wrong_binding = fake_provider_binding();
        wrong_binding.route_sha256 = "b".repeat(64);
        let error = ImageToolModule::new(
            provider,
            &wrong_binding,
            executable,
            digest,
            AGENT_IMAGE_PROTOCOL_SCHEMA,
            AGENT_IMAGE_PROTOCOL_VERSION,
            AGENT_IMAGE_BEHAVIOR_VERSION,
            manifest_digest,
            CANONICAL_PERFECTPIXEL_IMPLEMENTATION_VERSION,
        )
        .expect_err("composition/provider drift must fail at image module admission");
        assert!(error.contains("does not match the provider sealed by composition"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn borrowed_image_validation_preserves_media_bounds_and_dimension_errors() {
        let executable_directory = TempDir::new().expect("executable directory");
        let (executable, digest, manifest_digest) = fake_executable(&executable_directory);
        let bytes = valid_png();
        let module = test_image_module(
            Arc::new(FakeImageProvider {
                image: ProviderImage {
                    bytes: Vec::new(),
                    media_type: "image/png".to_owned(),
                    width: 1,
                    height: 1,
                },
                observed_reasoning: None,
            }),
            executable,
            digest,
            manifest_digest,
        );
        let oversized = vec![0; MAX_IMAGE_BYTES + 1];
        let cancellation = CancellationToken::new();
        for (input, media_type, dimensions, expected) in [
            (
                bytes.as_slice(),
                "image/jpeg",
                (2, 1),
                "image provider returned an invalid bounded image artifact",
            ),
            (
                &[][..],
                "image/png",
                (1, 1),
                "image provider returned an invalid bounded image artifact",
            ),
            (
                oversized.as_slice(),
                "image/png",
                (1, 1),
                "image provider returned an invalid bounded image artifact",
            ),
            (
                bytes.as_slice(),
                "image/png",
                (2, 1),
                "image provider dimensions do not match its PNG payload",
            ),
        ] {
            let error = module
                .validate_generated_image(
                    input,
                    media_type,
                    dimensions,
                    Duration::from_secs(1),
                    &cancellation,
                )
                .await
                .expect_err("invalid image validation inputs");
            assert!(matches!(
                error,
                ToolModuleError::Failure(failure)
                    if failure.code == FailureCode::ProviderFailed && failure.message == expected
            ));
        }
        let error = module
            .validate_generated_image(
                b"not a PNG",
                "image/png",
                (1, 1),
                Duration::from_secs(1),
                &cancellation,
            )
            .await
            .expect_err("invalid PNG header");
        assert!(matches!(
            error,
            ToolModuleError::Contract(message) if message == "image artifact is not a PNG raster"
        ));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn fake_generate_rejects_invalid_media_and_dimensions() {
        let executable_directory = TempDir::new().expect("executable directory");
        let (executable, digest, manifest_digest) = fake_executable(&executable_directory);
        let artifact_directory = TempDir::new().expect("artifact directory");
        let store = ArtifactStore::open(artifact_directory.path()).expect("artifact store");

        let invalid_media = test_image_module(
            Arc::new(FakeImageProvider {
                image: ProviderImage {
                    bytes: minimal_png(1, 1),
                    media_type: "image/jpeg".to_owned(),
                    width: 1,
                    height: 1,
                },
                observed_reasoning: None,
            }),
            &executable,
            &digest,
            &manifest_digest,
        );
        assert!(matches!(
            invalid_media
                .execute(context(&store, CancellationToken::new()), generate_call())
                .await,
            Err(ToolModuleError::Cancelled(failure))
                if failure.code == FailureCode::ResolutionRequired
        ));

        let invalid_dimensions = test_image_module(
            Arc::new(FakeImageProvider {
                image: ProviderImage {
                    bytes: minimal_png(1, 1),
                    media_type: "image/png".to_owned(),
                    width: 2,
                    height: 1,
                },
                observed_reasoning: None,
            }),
            executable,
            digest,
            manifest_digest,
        );
        assert!(matches!(
            invalid_dimensions
                .execute(context(&store, CancellationToken::new()), generate_call())
                .await,
            Err(ToolModuleError::Cancelled(failure))
                if failure.code == FailureCode::ResolutionRequired
        ));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn fake_generate_rejects_truncated_png_as_resolution_required() {
        let executable_directory = TempDir::new().expect("executable directory");
        let (executable, digest, manifest_digest) = fake_executable(&executable_directory);
        let provider = Arc::new(FakeImageProvider {
            image: ProviderImage {
                bytes: minimal_png(1, 1),
                media_type: "image/png".to_owned(),
                width: 1,
                height: 1,
            },
            observed_reasoning: None,
        });
        let module = test_image_module(provider, executable, digest, manifest_digest);
        let artifact_directory = TempDir::new().expect("artifact directory");
        let store = ArtifactStore::open(artifact_directory.path()).expect("artifact store");
        let error = module
            .execute(context(&store, CancellationToken::new()), generate_call())
            .await
            .expect_err("truncated PNG must not become durable");
        assert!(matches!(
            error,
            ToolModuleError::Cancelled(failure)
                if failure.code == FailureCode::ResolutionRequired
        ));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn fake_perfectpixel_inspect_round_trip_artifacts() {
        let executable_directory = TempDir::new().expect("executable directory");
        let (executable, digest, manifest_digest) = fake_executable(&executable_directory);
        let module = test_image_module(
            Arc::new(FakeImageProvider {
                image: ProviderImage {
                    bytes: valid_png(),
                    media_type: "image/png".to_owned(),
                    width: 1,
                    height: 1,
                },
                observed_reasoning: None,
            }),
            executable,
            digest,
            manifest_digest,
        );
        let artifact_directory = TempDir::new().expect("artifact directory");
        let store = ArtifactStore::open(artifact_directory.path()).expect("artifact store");
        let source = valid_png();
        let source_ref = store
            .put(&source, "image/png", "png")
            .expect("source artifact");
        let inspection = module
            .execute(
                context(&store, CancellationToken::new()),
                ToolCall {
                    id: ToolCallId::parse("call-inspect").expect("call id"),
                    name: IMAGE_INSPECT_TOOL.to_owned(),
                    arguments: json!({"artifact": source_ref}),
                },
            )
            .await
            .expect("inspection");
        assert_eq!(inspection.result.value["operation"], "inspect");
        assert_eq!(inspection.result.value["width"], 1);
        assert!(inspection.artifacts.is_empty());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn semantic_extract_reenters_the_canonical_provided_mask_path() {
        let executable_directory = TempDir::new().expect("executable directory");
        let (executable, digest, manifest_digest) = fake_executable(&executable_directory);
        let artifact_directory = TempDir::new().expect("artifact directory");
        let store = ArtifactStore::open(artifact_directory.path()).expect("artifact store");
        let source = valid_png();
        let source_ref = store
            .put(&source, "image/png", "png")
            .expect("source artifact");
        let provider: Arc<dyn ImagePort> = Arc::new(FakeImageProvider {
            image: ProviderImage {
                bytes: source.clone(),
                media_type: "image/png".to_owned(),
                width: 1,
                height: 1,
            },
            observed_reasoning: None,
        });
        let provider_binding = provider.binding();
        let module = ImageToolModule::new_with_semantic_mask_adapter(
            provider,
            &provider_binding,
            executable,
            digest,
            AGENT_IMAGE_PROTOCOL_SCHEMA,
            AGENT_IMAGE_PROTOCOL_VERSION,
            AGENT_IMAGE_BEHAVIOR_VERSION,
            manifest_digest,
            CANONICAL_PERFECTPIXEL_IMPLEMENTATION_VERSION,
            Arc::new(SuccessfulSemanticMaskAdapter {
                mask: source_ref.clone(),
            }),
        )
        .expect("image module");
        let mut tool_context = context(&store, CancellationToken::new());
        tool_context.semantic_mask_operation = Some(agent_core::SemanticMaskOperation {
            source_artifact: source_ref.clone(),
            source_revision: 1,
            helper_identity: "semantic.fixture/1".to_owned(),
            request_revision: 1,
            tool_call_id: ToolCallId::parse("call-semantic-success").expect("call id"),
        });
        let execution = module
            .execute(
                tool_context,
                ToolCall {
                    id: ToolCallId::parse("call-semantic-success").expect("call id"),
                    name: IMAGE_EXTRACT_TOOL.to_owned(),
                    arguments: json!({
                        "artifact": source_ref,
                        "selector": {
                            "type": "semantic",
                            "sourceRevision": 1,
                            "requestRevision": 1,
                            "helperIdentity": "semantic.fixture/1",
                            "target": {"type":"point","xMillionths":500_000,"yMillionths":500_000}
                        },
                        "includeRemainder": false
                    }),
                },
            )
            .await
            .expect("semantic extraction");
        assert_eq!(
            execution.result.value["semanticMask"]["status"],
            "succeeded"
        );
        assert_eq!(execution.artifacts.len(), 2);
    }

    #[cfg(unix)]
    async fn reject_mismatched_receipt_request_id(tool: &str) {
        let executable_directory = TempDir::new().expect("executable directory");
        let (executable, _, manifest_digest) = fake_executable(&executable_directory);
        let script = fs::read_to_string(&executable).expect("fake executable script");
        let receipt_id = "\"requestId\": request[\"requestId\"],";
        assert!(script.contains(receipt_id));
        let script = script.replacen(receipt_id, "\"requestId\": \"wrong-request\",", 1);
        fs::write(&executable, &script).expect("mismatched receipt fixture");
        let digest = sha256_hex(script.as_bytes());
        let module = test_image_module(
            Arc::new(FakeImageProvider {
                image: ProviderImage {
                    bytes: valid_png(),
                    media_type: "image/png".to_owned(),
                    width: 1,
                    height: 1,
                },
                observed_reasoning: None,
            }),
            executable,
            digest,
            manifest_digest,
        );
        let artifact_directory = TempDir::new().expect("artifact directory");
        let store = ArtifactStore::open(artifact_directory.path()).expect("artifact store");
        let source_ref = store
            .put(&valid_png(), "image/png", "png")
            .expect("source artifact");
        let arguments = if tool == IMAGE_EXTRACT_TOOL {
            json!({
                "artifact": source_ref,
                "selector": {"type":"existing_alpha","minimumAlpha":1},
                "includeRemainder": false
            })
        } else {
            json!({
                "canvas": {"width":1,"height":1,"background":[0,0,0,0]},
                "nodes": [{
                    "id":"source", "kind":"raster", "artifact":source_ref,
                    "transform":{"matrix":[1.0,0.0,0.0,0.0,1.0,0.0,0.0,0.0,1.0]},
                    "opacity":255
                }]
            })
        };
        let error = module
            .execute(
                context(&store, CancellationToken::new()),
                ToolCall {
                    id: ToolCallId::parse("call-receipt-request-id").expect("call id"),
                    name: tool.to_owned(),
                    arguments,
                },
            )
            .await
            .expect_err("mismatched receipt request ID must be rejected");
        let operation = if tool == IMAGE_EXTRACT_TOOL {
            "extract"
        } else {
            "render"
        };
        assert!(matches!(error, ToolModuleError::Contract(message)
            if message == format!("PerfectPixel {operation} result violated protocol v2")));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn extract_rejects_mismatched_receipt_request_id() {
        reject_mismatched_receipt_request_id(IMAGE_EXTRACT_TOOL).await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn render_rejects_mismatched_receipt_request_id() {
        reject_mismatched_receipt_request_id(IMAGE_RENDER_TOOL).await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn fake_perfectpixel_extract_render_compare_shapes_strict_v2_contracts() {
        let executable_directory = TempDir::new().expect("executable directory");
        let (executable, digest, manifest_digest) = fake_executable(&executable_directory);
        let module = test_image_module(
            Arc::new(FakeImageProvider {
                image: ProviderImage {
                    bytes: valid_png(),
                    media_type: "image/png".to_owned(),
                    width: 1,
                    height: 1,
                },
                observed_reasoning: None,
            }),
            executable,
            digest,
            manifest_digest,
        );
        let artifact_directory = TempDir::new().expect("artifact directory");
        let store = ArtifactStore::open(artifact_directory.path()).expect("artifact store");
        let source = valid_png();
        let source_ref = store
            .put(&source, "image/png", "png")
            .expect("source artifact");

        let extracted = module
            .execute(
                context(&store, CancellationToken::new()),
                ToolCall {
                    id: ToolCallId::parse("call-extract-v2").expect("call id"),
                    name: IMAGE_EXTRACT_TOOL.to_owned(),
                    arguments: json!({
                        "artifact":source_ref,
                        "selector":{"type":"existing_alpha","minimumAlpha":1},
                        "refinement":{"featherRadius":0,"requireSoftAlpha":false},
                        "includeRemainder":false
                    }),
                },
            )
            .await
            .expect("extract");
        assert_eq!(extracted.result.value["operation"], "extract");
        assert_eq!(extracted.artifacts.len(), 2);
        let object_ref = store
            .put(
                &extracted.artifacts[0].bytes,
                &extracted.artifacts[0].media_type,
                &extracted.artifacts[0].extension,
            )
            .expect("object artifact");
        let font_ref = store
            .put(b"fake-font-fixture", "font/ttf", "ttf")
            .expect("portable fixture font");

        let rendered = module
            .execute(
                context(&store, CancellationToken::new()),
                ToolCall {
                    id: ToolCallId::parse("call-render-v2").expect("call id"),
                    name: IMAGE_RENDER_TOOL.to_owned(),
                    arguments: json!({
                        "canvas":{"width":1,"height":1,"background":[0,0,0,0]},
                        "nodes":[{
                            "id":"object",
                            "artifact":object_ref,
                            "kind":"object",
                            "transform":{"matrix":[1.0,0.0,0.0,0.0,1.0,0.0,0.0,0.0,1.0]},
                            "opacity":255
                        },{
                            "id":"text",
                            "kind":"text",
                            "transform":{"matrix":[1.0,0.0,0.0,0.0,1.0,0.0,0.0,0.0,1.0]},
                            "opacity":255,
                            "text":{
                                "content":"Hello",
                                "direction":"ltr",
                                "language":"en",
                                "boxWidth":192,
                                "boxHeight":48,
                                "alignment":"start",
                                "lineBreak":"no_wrap",
                                "fontSize":24.0,
                                "color":[255,255,255,255],
                                "pixelSpec":{"format":"rgba8","colorSpace":"srgb","alphaMode":"straight"},
                                "fontArtifact":font_ref
                            }
                        }]
                    }),
                },
            )
            .await
            .expect("render");
        assert_eq!(rendered.result.value["operation"], "render");
        assert_eq!(
            rendered.result.value["textNodes"].as_array().map(Vec::len),
            Some(1)
        );
        assert_eq!(rendered.result.value["textNodes"][0]["nodeId"], "text");
        assert_eq!(rendered.artifacts.len(), 1);
        let rendered_ref = store
            .put(
                &rendered.artifacts[0].bytes,
                &rendered.artifacts[0].media_type,
                &rendered.artifacts[0].extension,
            )
            .expect("rendered artifact");

        let compared = module
            .execute(
                context(&store, CancellationToken::new()),
                ToolCall {
                    id: ToolCallId::parse("call-compare-v2").expect("call id"),
                    name: IMAGE_COMPARE_TOOL.to_owned(),
                    arguments: json!({
                        "beforeArtifact":source_ref,
                        "afterArtifact":rendered_ref,
                        "afterKind":"raster",
                        "assertions":[{
                            "type":"exact_equal",
                            "id":"preserve",
                            "severity":"required"
                        }],
                        "preview":{"difference":false,"maskOverlay":false,"maximumEdge":256}
                    }),
                },
            )
            .await
            .expect("compare");
        assert_eq!(compared.result.value["operation"], "compare");
        assert_eq!(compared.result.value["allRequiredPassed"], true);
        assert_eq!(
            compared.result.value["verificationSpecDigest"]
                .as_str()
                .map(str::len),
            Some(64)
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    #[ignore = "requires PERFECTPIXEL_BIN pointing to a canonical build"]
    async fn real_perfectpixel_round_trips_all_exact_compare_assertions() {
        let executable =
            PathBuf::from(std::env::var("PERFECTPIXEL_BIN").expect("PERFECTPIXEL_BIN must be set"))
                .canonicalize()
                .expect("canonical PerfectPixel binary");
        let digest = sha256_hex(&fs::read(&executable).expect("PerfectPixel binary bytes"));
        let manifest_digest = observed_manifest_digest(&executable);
        let module = test_image_module_with_version(
            Arc::new(FakeImageProvider {
                image: ProviderImage {
                    bytes: valid_png(),
                    media_type: "image/png".to_owned(),
                    width: 1,
                    height: 1,
                },
                observed_reasoning: None,
            }),
            executable,
            digest,
            manifest_digest,
            CANONICAL_PERFECTPIXEL_IMPLEMENTATION_VERSION,
        );
        let artifact_directory = TempDir::new().expect("artifact directory");
        let store = ArtifactStore::open(artifact_directory.path()).expect("artifact store");
        let source = store
            .put(&valid_png(), "image/png", "png")
            .expect("source artifact");
        let rect = ImageCompareRect {
            x: 0,
            y: 0,
            width: 1,
            height: 1,
        };
        let assertions = vec![
            ImageCompareAssertion::ExactEqual {
                id: "exact".to_owned(),
                severity: ImageCompareSeverity::Required,
            },
            ImageCompareAssertion::ChangedRatio {
                id: "changed".to_owned(),
                severity: ImageCompareSeverity::Required,
                minimum: None,
                maximum: Some(0.0),
            },
            ImageCompareAssertion::OutsideMaskChangedRatio {
                id: "outside".to_owned(),
                severity: ImageCompareSeverity::Required,
                maximum: 0.0,
                mask_artifact: source.clone(),
            },
            ImageCompareAssertion::InsideMaskChangedRatio {
                id: "inside".to_owned(),
                severity: ImageCompareSeverity::Required,
                minimum: 0.0,
                maximum: Some(0.0),
                mask_artifact: source.clone(),
            },
            ImageCompareAssertion::UnchangedRegionExact {
                id: "region".to_owned(),
                severity: ImageCompareSeverity::Required,
                region: rect,
            },
            ImageCompareAssertion::AlphaIou {
                id: "alpha".to_owned(),
                severity: ImageCompareSeverity::Required,
                minimum: 1.0,
                mask_artifact: source.clone(),
            },
            ImageCompareAssertion::MaskLeakageRatio {
                id: "leakage".to_owned(),
                severity: ImageCompareSeverity::Required,
                maximum: 0.0,
                mask_artifact: source.clone(),
            },
            ImageCompareAssertion::ObjectBounds {
                id: "bounds".to_owned(),
                severity: ImageCompareSeverity::Required,
                expected: rect,
                tolerance: 0,
            },
            ImageCompareAssertion::ObjectCentroid {
                id: "centroid".to_owned(),
                severity: ImageCompareSeverity::Required,
                expected: [0.0, 0.0],
                tolerance: 0.0,
            },
            ImageCompareAssertion::ObjectArea {
                id: "area".to_owned(),
                severity: ImageCompareSeverity::Required,
                expected: 1,
                tolerance: 0,
            },
            ImageCompareAssertion::MaximumChannelError {
                id: "channel".to_owned(),
                severity: ImageCompareSeverity::Required,
                maximum: 0,
            },
            ImageCompareAssertion::MeanAbsoluteError {
                id: "mean".to_owned(),
                severity: ImageCompareSeverity::Required,
                maximum: 0.0,
            },
        ];
        let compared = module
            .execute(
                context(&store, CancellationToken::new()),
                ToolCall {
                    id: ToolCallId::parse("call-real-all-assertions").expect("call id"),
                    name: IMAGE_COMPARE_TOOL.to_owned(),
                    arguments: json!({
                        "beforeArtifact":source,
                        "afterArtifact":source,
                        "afterKind":"raster",
                        "assertions":assertions,
                        "preview":{"difference":false,"maskOverlay":false,"maximumEdge":256}
                    }),
                },
            )
            .await
            .expect("real compare round trip");
        assert_eq!(compared.result.value["allRequiredPassed"], true);
        assert_eq!(
            compared.result.value["assertions"].as_array().map(Vec::len),
            Some(12)
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    #[ignore = "requires canonical PERFECTPIXEL_BIN and local PERFECTPIXEL_IMAGE PNG for post-effect validation"]
    async fn real_perfectpixel_validates_local_generated_image() {
        let executable =
            PathBuf::from(std::env::var("PERFECTPIXEL_BIN").expect("PerfectPixel binary"))
                .canonicalize()
                .expect("canonical PerfectPixel binary");
        let digest = sha256_hex(&fs::read(&executable).expect("binary bytes"));
        let manifest_digest = observed_manifest_digest(&executable);
        let module = test_image_module_with_version(
            Arc::new(FakeImageProvider {
                image: ProviderImage {
                    bytes: valid_png(),
                    media_type: "image/png".to_owned(),
                    width: 1,
                    height: 1,
                },
                observed_reasoning: None,
            }),
            executable,
            digest,
            manifest_digest,
            CANONICAL_PERFECTPIXEL_IMPLEMENTATION_VERSION,
        );
        let bytes = fs::read(std::env::var("PERFECTPIXEL_IMAGE").expect("local PNG path"))
            .expect("local PNG bytes");
        let (width, height) = validate_png(&bytes).expect("bounded local PNG");
        let inspection = module
            .agent_inspect_bytes(
                &bytes,
                "local-posteffect-reproduction",
                Duration::from_secs(30),
                &CancellationToken::new(),
            )
            .await
            .expect("real local PNG inspection");
        validate_inspection_v2(&inspection.inspection)
            .expect("real PerfectPixel inspection metric contract");
        module
            .validate_generated_image(
                &bytes,
                "image/png",
                (width, height),
                Duration::from_secs(30),
                &CancellationToken::new(),
            )
            .await
            .expect("real PerfectPixel post-effect validation of a local PNG");
    }

    #[tokio::test]
    #[ignore = "requires a canonical PERFECTPIXEL_BIN and caller-supplied PERFECTPIXEL_FONT TTF"]
    async fn real_perfectpixel_text_render_round_trips_font_artifact_and_layout_evidence() {
        let executable =
            PathBuf::from(std::env::var("PERFECTPIXEL_BIN").expect("PERFECTPIXEL_BIN must be set"))
                .canonicalize()
                .expect("canonical PerfectPixel binary");
        let executable_bytes = fs::read(&executable).expect("PerfectPixel binary bytes");
        let digest = sha256_hex(&executable_bytes);
        let manifest_digest = observed_manifest_digest(&executable);
        let module = test_image_module_with_version(
            Arc::new(FakeImageProvider {
                image: ProviderImage {
                    bytes: valid_png(),
                    media_type: "image/png".to_owned(),
                    width: 1,
                    height: 1,
                },
                observed_reasoning: None,
            }),
            executable,
            digest,
            manifest_digest,
            CANONICAL_PERFECTPIXEL_IMPLEMENTATION_VERSION,
        );
        let artifact_directory = TempDir::new().expect("artifact directory");
        let store = ArtifactStore::open(artifact_directory.path()).expect("artifact store");
        let font_ref = store
            .put(external_perfectpixel_font().as_slice(), "font/ttf", "ttf")
            .expect("portable fixture font");

        let rendered = module
            .execute(
                context(&store, CancellationToken::new()),
                ToolCall {
                    id: ToolCallId::parse("call-real-text-render").expect("call id"),
                    name: IMAGE_RENDER_TOOL.to_owned(),
                    arguments: json!({
                        "canvas":{"width":192,"height":48,"background":[0,0,0,0]},
                        "nodes":[{
                            "id":"text",
                            "kind":"text",
                            "transform":{"matrix":[1.0,0.0,0.0,0.0,1.0,0.0,0.0,0.0,1.0]},
                            "opacity":255,
                            "text":{
                                "content":"Hello",
                                "direction":"ltr",
                                "language":"en",
                                "boxWidth":192,
                                "boxHeight":48,
                                "alignment":"start",
                                "lineBreak":"no_wrap",
                                "fontSize":24.0,
                                "color":[255,255,255,255],
                                "pixelSpec":{"format":"rgba8","colorSpace":"srgb","alphaMode":"straight"},
                                "fontArtifact":font_ref.clone()
                            }
                        }]
                    }),
                },
            )
            .await
            .expect("real text render");
        assert_eq!(rendered.artifacts.len(), 1);
        assert_eq!(rendered.result.value["operation"], "render");
        assert_eq!(
            rendered.result.value["textNodes"].as_array().map(Vec::len),
            Some(1)
        );
        assert_eq!(rendered.result.value["textNodes"][0]["nodeId"], "text");
        assert!(
            rendered.result.value["textNodes"][0]["layout"]["rasterSha256"]
                .as_str()
                .is_some_and(|value| value.len() == 64)
        );
        let dependencies = rendered.result.value["receipt"]["dependencies"]
            .as_array()
            .expect("receipt dependencies");
        assert!(dependencies.iter().any(|dependency| {
            dependency["sha256"] == font_ref.sha256
                && dependency["mediaType"] == "font/ttf"
                && dependency["byteLength"] == font_ref.byte_length
        }));
    }

    fn valid_inspection(width: u32, height: u32) -> PerfectPixelInspectionV2 {
        let edge_pixel_count = unique_perimeter_pixel_count(width, height);
        PerfectPixelInspectionV2 {
            width,
            height,
            foreground_pixels: usize::try_from(u64::from(width) * u64::from(height))
                .unwrap_or(usize::MAX),
            alpha_ratio: 1.0,
            content_box: Some(InspectionRect {
                x: 0,
                y: 0,
                w: width,
                h: height,
            }),
            center: Some(InspectionPoint {
                x: width / 2,
                y: height / 2,
            }),
            ground_y: height,
            touches_edge: true,
            has_alpha: false,
            pixel_format: "rgba8".to_owned(),
            color_space: "srgb".to_owned(),
            edge_pixel_count,
            edge_palette: vec![EdgePaletteEntry {
                rgb: [0, 0, 0],
                count: edge_pixel_count,
            }],
        }
    }

    fn empty_inspection(width: u32, height: u32) -> PerfectPixelInspectionV2 {
        let mut inspection = valid_inspection(width, height);
        inspection.foreground_pixels = 0;
        inspection.alpha_ratio = 0.0;
        inspection.content_box = None;
        inspection.center = None;
        inspection.ground_y = 0;
        inspection.touches_edge = false;
        inspection.has_alpha = true;
        inspection
    }

    fn assert_invalid_inspection(inspection: &PerfectPixelInspectionV2) {
        assert!(validate_inspection_v2(inspection).is_err());
    }

    #[test]
    fn perfectpixel_inspection_geometry_is_typed_and_strict() {
        let valid = valid_inspection(4, 3);
        assert!(validate_inspection_v2(&valid).is_ok());

        let mut arbitrary_content_box = serde_json::to_value(&valid).expect("inspection JSON");
        arbitrary_content_box["contentBox"]["unexpected"] = json!(true);
        assert!(serde_json::from_value::<PerfectPixelInspectionV2>(arbitrary_content_box).is_err());

        let mut missing_center = serde_json::to_value(&valid).expect("inspection JSON");
        missing_center
            .as_object_mut()
            .expect("inspection object")
            .remove("center");
        assert!(serde_json::from_value::<PerfectPixelInspectionV2>(missing_center).is_err());

        let mut invalid = valid;
        invalid.content_box.as_mut().expect("content box").w = 5;
        assert_invalid_inspection(&invalid);

        let mut invalid = valid_inspection(4, 3);
        invalid.center.as_mut().expect("center").x = 0;
        assert_invalid_inspection(&invalid);

        let mut invalid = valid_inspection(4, 3);
        invalid.ground_y = 2;
        assert_invalid_inspection(&invalid);

        let mut invalid = valid_inspection(4, 3);
        invalid.touches_edge = false;
        assert_invalid_inspection(&invalid);

        let mut invalid = valid_inspection(4, 3);
        invalid.alpha_ratio = 0.5;
        assert_invalid_inspection(&invalid);

        let mut invalid = valid_inspection(4, 3);
        invalid.alpha_ratio = f64::NAN;
        assert_invalid_inspection(&invalid);

        let mut partially_transparent_but_foreground = valid_inspection(4, 3);
        partially_transparent_but_foreground.has_alpha = true;
        assert!(validate_inspection_v2(&partially_transparent_but_foreground).is_ok());

        let mut invalid_opaque_coverage = valid_inspection(4, 3);
        invalid_opaque_coverage.foreground_pixels = 11;
        invalid_opaque_coverage.alpha_ratio = 11.0 / 12.0;
        assert_invalid_inspection(&invalid_opaque_coverage);

        let mut invalid = valid_inspection(4, 3);
        invalid.foreground_pixels = 13;
        assert_invalid_inspection(&invalid);

        let mut invalid = valid_inspection(4, 3);
        invalid.edge_pixel_count -= 1;
        assert_invalid_inspection(&invalid);

        let mut invalid = valid_inspection(4, 3);
        invalid.edge_palette[0].count -= 1;
        assert_invalid_inspection(&invalid);
    }

    #[test]
    fn perfectpixel_empty_foreground_and_one_dimensional_edges_are_exact() {
        let empty = empty_inspection(4, 3);
        assert!(validate_inspection_v2(&empty).is_ok());

        let mut invalid_empty = empty_inspection(4, 3);
        invalid_empty.content_box = Some(InspectionRect {
            x: 0,
            y: 0,
            w: 1,
            h: 1,
        });
        assert_invalid_inspection(&invalid_empty);

        for (width, height, expected_edge_pixels) in [(1, 1, 1), (1, 4, 4), (4, 1, 4)] {
            let inspection = valid_inspection(width, height);
            assert_eq!(inspection.edge_pixel_count, expected_edge_pixels);
            assert!(validate_inspection_v2(&inspection).is_ok());
        }
    }

    #[test]
    fn perfectpixel_edge_palette_requires_ordered_unique_counts_and_exact_total() {
        let mut valid = valid_inspection(2, 2);
        valid.edge_palette = vec![
            EdgePaletteEntry {
                rgb: [0, 0, 0],
                count: 2,
            },
            EdgePaletteEntry {
                rgb: [255, 255, 255],
                count: 2,
            },
        ];
        assert!(validate_inspection_v2(&valid).is_ok());

        let mut invalid = valid_inspection(2, 2);
        invalid.edge_palette = vec![
            EdgePaletteEntry {
                rgb: [255, 255, 255],
                count: 2,
            },
            EdgePaletteEntry {
                rgb: [0, 0, 0],
                count: 2,
            },
        ];
        assert_invalid_inspection(&invalid);

        let mut invalid = valid_inspection(2, 2);
        invalid.edge_palette.push(EdgePaletteEntry {
            rgb: [0, 0, 0],
            count: 1,
        });
        assert_invalid_inspection(&invalid);
    }

    #[test]
    fn perfectpixel_top_sixteen_palette_allows_omitted_colors_but_not_invalid_totals() {
        let mut inspection = valid_inspection(10, 10);
        inspection.edge_palette = (0..16)
            .map(|color| EdgePaletteEntry {
                rgb: [color, 0, 0],
                count: 2,
            })
            .collect();
        assert!(validate_inspection_v2(&inspection).is_ok());
        inspection.edge_palette.pop();
        assert!(validate_inspection_v2(&inspection).is_err());
        inspection.edge_palette = (0..16)
            .map(|color| EdgePaletteEntry {
                rgb: [color, 0, 0],
                count: 3,
            })
            .collect();
        assert!(validate_inspection_v2(&inspection).is_err());
    }

    #[test]
    fn provider_cancellation_and_resolution_keep_distinct_runtime_states() {
        let cancelled = provider_module_error(ProviderError::Failure(AgentFailure::new(
            FailureCode::Cancelled,
            "cancelled before dispatch",
        )));
        assert!(
            matches!(cancelled, ToolModuleError::Failure(failure) if failure.code == FailureCode::Cancelled)
        );
        let uncertain = provider_module_error(ProviderError::Failure(AgentFailure::new(
            FailureCode::ResolutionRequired,
            "outcome unknown",
        )));
        assert!(
            matches!(uncertain, ToolModuleError::Cancelled(failure) if failure.code == FailureCode::ResolutionRequired)
        );
    }
}
