use std::collections::BTreeSet;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::ArtifactRef;
#[cfg(test)]
use crate::error::AgentFailure;

mod semantic_mask;
mod verification;

pub use semantic_mask::*;
pub use verification::*;

pub const AGENT_IMAGE_PROTOCOL_SCHEMA: &str = "perfectpixel.agent-image/2";
pub const AGENT_IMAGE_PROTOCOL_VERSION: &str = "2.0.0";
pub const AGENT_IMAGE_BEHAVIOR_VERSION: &str = "2.0.0";
pub const AGENT_IMAGE_CAPABILITY_MANIFEST_DIGEST_DOMAIN: &str =
    "perfectpixel.agent-image/capability-manifest/2";
pub const IMAGE_TASK_CONTRACT_SCHEMA: &str = "upagent.image-task/5";
pub const IMAGE_VERIFICATION_SPEC_SCHEMA: &str = "perfectpixel.agent-image/verification-spec/1";
pub const MAX_IMAGE_DIRECTOR_ATTEMPTS: u32 = 16;
pub const MAX_IMAGE_PROVENANCE_ARTIFACTS: usize = 128;
pub const MAX_IMAGE_COMPARE_ASSERTIONS: usize = 256;

pub use agent_contract::provider::{
    ImageObservation, ImageObservationDetail, ImageObservationRole,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PixelFormat {
    Rgba8,
    Rgba16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ColorSpace {
    Srgb,
    DisplayP3,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AlphaMode {
    Straight,
    Premultiplied,
    Opaque,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct PixelSpec {
    pub format: PixelFormat,
    pub color_space: ColorSpace,
    pub alpha_mode: AlphaMode,
}

impl PixelSpec {
    #[must_use]
    pub const fn rgba8_srgb_straight() -> Self {
        Self {
            format: PixelFormat::Rgba8,
            color_space: ColorSpace::Srgb,
            alpha_mode: AlphaMode::Straight,
        }
    }

    #[must_use]
    pub const fn rgba16_srgb_straight() -> Self {
        Self {
            format: PixelFormat::Rgba16,
            color_space: ColorSpace::Srgb,
            alpha_mode: AlphaMode::Straight,
        }
    }

    #[must_use]
    pub const fn rgba8_display_p3_straight() -> Self {
        Self {
            format: PixelFormat::Rgba8,
            color_space: ColorSpace::DisplayP3,
            alpha_mode: AlphaMode::Straight,
        }
    }

    #[must_use]
    pub const fn rgba16_display_p3_straight() -> Self {
        Self {
            format: PixelFormat::Rgba16,
            color_space: ColorSpace::DisplayP3,
            alpha_mode: AlphaMode::Straight,
        }
    }

    /// The default working-raster contract remains compact sRGB RGBA8.
    #[must_use]
    pub const fn default_working() -> Self {
        Self::rgba8_srgb_straight()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactKindV2 {
    WorkingRaster,
    ExportRaster,
    Mask,
    Object,
    Json,
    Receipt,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactRetention {
    #[default]
    TaskScoped,
    Cacheable,
    Pinned,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ArtifactDependencyV2 {
    pub sha256: String,
    pub media_type: String,
    pub byte_length: u64,
}

impl ArtifactDependencyV2 {
    #[must_use]
    pub fn from_ref(reference: &ArtifactRef) -> Self {
        Self {
            sha256: reference.sha256.clone(),
            media_type: reference.media_type.clone(),
            byte_length: reference.byte_length,
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.sha256.len() != 64 || !self.sha256.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err("artifact dependency sha256 is invalid".to_owned());
        }
        if self.media_type.trim().is_empty() || self.media_type.len() > 256 || self.byte_length == 0
        {
            return Err("artifact dependency metadata is invalid".to_owned());
        }
        Ok(())
    }
}

pub fn artifact_dependency_closure_sha256(
    dependencies: &[ArtifactDependencyV2],
) -> Result<String, String> {
    let mut normalized = dependencies.to_vec();
    for dependency in &normalized {
        dependency.validate()?;
    }
    normalized.sort();
    normalized.dedup();
    let encoded = serde_json::to_vec(&normalized)
        .map_err(|error| format!("dependency closure could not be encoded: {error}"))?;
    Ok(format!("{:x}", Sha256::digest(&encoded)))
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ArtifactDescriptorV2 {
    pub protocol_version: String,
    pub artifact: ArtifactRef,
    pub kind: ArtifactKindV2,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pixel_spec: Option<PixelSpec>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dependencies: Vec<ArtifactDependencyV2>,
    pub dependency_closure_sha256: String,
    #[serde(default)]
    pub retention: ArtifactRetention,
}

impl ArtifactDescriptorV2 {
    pub fn new(
        artifact: ArtifactRef,
        kind: ArtifactKindV2,
        pixel_spec: Option<PixelSpec>,
        mut dependencies: Vec<ArtifactDependencyV2>,
        retention: ArtifactRetention,
    ) -> Result<Self, String> {
        for dependency in &dependencies {
            dependency.validate()?;
        }
        dependencies.sort();
        dependencies.dedup();
        let dependency_closure_sha256 = artifact_dependency_closure_sha256(&dependencies)?;
        Ok(Self {
            protocol_version: AGENT_IMAGE_PROTOCOL_VERSION.to_owned(),
            artifact,
            kind,
            pixel_spec,
            dependencies,
            dependency_closure_sha256,
            retention,
        })
    }

    pub fn working_raster(
        artifact: ArtifactRef,
        dependencies: Vec<ArtifactDependencyV2>,
    ) -> Result<Self, String> {
        Self::new(
            artifact,
            ArtifactKindV2::WorkingRaster,
            Some(PixelSpec::rgba8_srgb_straight()),
            dependencies,
            ArtifactRetention::TaskScoped,
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum OperationStatusV2 {
    Committed,
    Rejected,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DeterminismClassV2 {
    BitExact,
    BackendDeterministic,
    InferenceObserved,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct OperationReceiptV2 {
    pub schema: String,
    pub request_id: String,
    pub operation: String,
    pub status: OperationStatusV2,
    pub behavior_version: String,
    pub implementation_version: String,
    pub request_sha256: String,
    pub dependency_closure_sha256: String,
    pub dependencies: Vec<ArtifactDependencyV2>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_sha256: Option<String>,
    pub determinism: DeterminismClassV2,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "mode",
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum ImageSource {
    Provided { artifact: ArtifactRef },
    GenerateNew,
}

impl ImageSource {
    pub fn validate(&self) -> Result<(), String> {
        match self {
            Self::Provided { artifact } => validate_image_artifact_ref(artifact),
            Self::GenerateNew => Ok(()),
        }
    }

    #[must_use]
    pub const fn is_generate_new(&self) -> bool {
        matches!(self, Self::GenerateNew)
    }

    #[must_use]
    pub const fn is_provided(&self) -> bool {
        matches!(self, Self::Provided { .. })
    }

    #[must_use]
    pub const fn provided_artifact(&self) -> Option<&ArtifactRef> {
        match self {
            Self::Provided { artifact } => Some(artifact),
            Self::GenerateNew => None,
        }
    }
}

/// The typed identity of an image candidate currently owned by the Image
/// Director.  The artifact reference is the content-addressed identity; the
/// kind is the producer contract used to bind `ImageVerificationSpec.after_kind`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ImageCandidateArtifact {
    pub artifact: ArtifactRef,
    pub kind: ImageCompareArtifactKind,
}

fn validate_image_artifact_ref(reference: &ArtifactRef) -> Result<(), String> {
    let path = std::path::Path::new(&reference.relative_path);
    if reference.sha256.len() != 64
        || !reference
            .sha256
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
        || reference.media_type != "image/png"
        || reference.byte_length == 0
        || reference.relative_path.is_empty()
        || reference.relative_path.len() > 4_096
        || path.is_absolute()
        || path
            .components()
            .any(|component| component == std::path::Component::ParentDir)
    {
        return Err("image verification artifact reference is invalid".to_owned());
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "snake_case")]
pub enum ImageDirectorStage {
    #[default]
    Idle,
    Observing,
    Editing,
    Verifying,
    Reviewing,
    Revising,
    Accepted,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ImageTaskContract {
    pub schema: String,
    pub goal: String,
    pub source: ImageSource,
    pub maximum_attempts: u32,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub preserve: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub visual_criteria: Vec<String>,
    pub verification: ImageVerificationContract,
}

impl ImageTaskContract {
    pub fn validate(&self) -> Result<(), String> {
        if self.schema != IMAGE_TASK_CONTRACT_SCHEMA {
            return Err("image task contract schema is unsupported".to_owned());
        }
        if self.goal.trim().is_empty() || self.goal.len() > 256 * 1024 {
            return Err("image task goal must be bounded and non-empty".to_owned());
        }
        if self.maximum_attempts == 0 || self.maximum_attempts > MAX_IMAGE_DIRECTOR_ATTEMPTS {
            return Err("image task maximumAttempts is outside the supported bound".to_owned());
        }
        if self.verification.visual_review && self.visual_criteria.is_empty() {
            return Err("visual review requires at least one explicit criterion".to_owned());
        }
        if self.preserve.len() > MAX_IMAGE_CRITERIA
            || self.visual_criteria.len() > MAX_IMAGE_CRITERIA
        {
            return Err("image task criteria exceed the bounded count".to_owned());
        }
        for value in self.preserve.iter().chain(&self.visual_criteria) {
            if value.trim().is_empty()
                || value.len() > MAX_IMAGE_CRITERION_BYTES
                || value.contains('\0')
            {
                return Err("image task criteria contain an invalid value".to_owned());
            }
        }
        self.source.validate()?;
        self.verification.validate()
    }

    pub fn digest(&self) -> Result<String, String> {
        self.validate()?;
        let bytes = serde_json::to_vec(self)
            .map_err(|error| format!("image task contract could not be encoded: {error}"))?;
        let mut hasher = Sha256::new();
        hasher.update(b"upagent-image-task-contract-v5");
        hasher.update([0]);
        hasher.update(bytes);
        Ok(format!("{:x}", hasher.finalize()))
    }

    #[must_use]
    pub const fn requires_deterministic_compare(&self) -> bool {
        self.verification.deterministic.is_some()
    }
}

pub const MAX_IMAGE_CRITERIA: usize = 32;
pub const MAX_IMAGE_CRITERION_BYTES: usize = 2048;
pub const MAX_IMAGE_REVIEW_BYTES: usize = 128 * 1024;
pub const IMAGE_REVIEW_PREFIX: &str = "IMAGE_REVIEW:";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ImageVisualDecision {
    Pass,
    Fail,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ImageVisualCriterionEvidence {
    pub criterion_id: String,
    pub decision: ImageVisualDecision,
    pub evidence: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ImageVisualReview {
    pub contract_digest: String,
    pub candidate_revision: u32,
    pub candidate_set_digest: String,
    pub criteria: Vec<ImageVisualCriterionEvidence>,
}

impl ImageVisualReview {
    /// A control response is strict JSON with a fixed prefix, never user output.
    pub fn from_model_response(text: &str) -> Result<Option<Self>, String> {
        let Some(payload) = text.trim_start().strip_prefix(IMAGE_REVIEW_PREFIX) else {
            return Ok(None);
        };
        if text.len() > MAX_IMAGE_REVIEW_BYTES {
            return Err("image review exceeds its byte bound".to_owned());
        }
        serde_json::from_str(payload)
            .map(Some)
            .map_err(|error| format!("invalid IMAGE_REVIEW evidence: {error}"))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ImageRepairRequirement {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub width: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub height: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transparent_background: Option<bool>,
}

impl ImageRepairRequirement {
    pub fn validate(&self) -> Result<(), String> {
        if self.width.is_some() != self.height.is_some() {
            return Err("image repair size requires both width and height".to_owned());
        }
        if let Some((width, height)) = self.width.zip(self.height)
            && (width == 0 || height == 0 || width > 8_192 || height > 8_192)
        {
            return Err("image repair size is outside the supported raster bound".to_owned());
        }
        if self.width.is_none() && self.transparent_background.is_none() {
            return Err(
                "image repair requirement must contain an exact verifiable constraint".to_owned(),
            );
        }
        Ok(())
    }

    #[must_use]
    pub fn is_satisfied_by(&self, observation: &ImageRepairObservation) -> bool {
        self.validate().is_ok()
            && observation.validate().is_ok()
            && self
                .width
                .zip(self.height)
                .is_none_or(|size| size == (observation.width, observation.height))
            && self
                .transparent_background
                .is_none_or(|expected| expected == observation.transparent_background)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ImageRepairObservation {
    pub width: u32,
    pub height: u32,
    pub transparent_background: bool,
}

impl ImageRepairObservation {
    pub fn validate(&self) -> Result<(), String> {
        if self.width == 0 || self.height == 0 || self.width > 8_192 || self.height > 8_192 {
            return Err(
                "image repair observation is outside the supported raster bound".to_owned(),
            );
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, Default)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ImageDirectorState {
    /// Current-candidate qualification evidence, never a second task authority.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub visual_review: Option<ImageVisualReview>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contract: Option<ImageTaskContract>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contract_digest: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub required_verification_digest: Option<String>,
    #[serde(default)]
    pub stage: ImageDirectorStage,
    #[serde(default)]
    pub attempts: u32,
    #[serde(default)]
    pub candidate_revision: u32,
    #[serde(default)]
    pub observed_revision: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compared_revision: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_compare_passed: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verified_spec_digest: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub failed_assertions: Vec<String>,
    #[serde(default)]
    pub editing_started: bool,
    /// Exact, verifiable generation constraints that a subsequent
    /// PerfectPixel-produced candidate must satisfy before completion.
    /// `None` means no deterministic repair is pending.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_repair: Option<ImageRepairRequirement>,
    #[serde(default)]
    pub revision_requests: u32,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub current_candidate_artifacts: Vec<ImageCandidateArtifact>,
    /// Complete task-local closure of image artifacts that are permitted to
    /// contribute pixel content to a future candidate. The task contract
    /// seeds this set for provided-source tasks; successful producer results
    /// extend it only after their direct pixel inputs were already authorized.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub authorized_image_artifacts: Vec<ArtifactRef>,
    #[serde(default)]
    pub semantic_request_revision: u64,
    #[serde(default)]
    pub semantic_mask: SemanticMaskState,
}

impl ImageDirectorState {
    #[must_use]
    pub fn is_active(&self) -> bool {
        self.contract.is_some()
    }

    fn has_task_state_without_contract(&self) -> bool {
        self.visual_review.is_some()
            || self.contract_digest.is_some()
            || self.required_verification_digest.is_some()
            || self.stage != ImageDirectorStage::Idle
            || self.attempts != 0
            || self.candidate_revision != 0
            || self.observed_revision != 0
            || self.compared_revision.is_some()
            || self.last_compare_passed.is_some()
            || self.verified_spec_digest.is_some()
            || !self.failed_assertions.is_empty()
            || self.editing_started
            || self.pending_repair.is_some()
            || self.revision_requests != 0
            || !self.current_candidate_artifacts.is_empty()
            || !self.authorized_image_artifacts.is_empty()
            || self.semantic_request_revision != 0
            || self.semantic_mask != SemanticMaskState::default()
    }

    pub fn bind_contract(&mut self, contract: Option<&ImageTaskContract>) -> Result<(), String> {
        match (self.contract.as_ref(), contract) {
            (None, None) => {
                if self.has_task_state_without_contract() {
                    return Err("image director checkpoint has orphaned task authority".to_owned());
                }
                Ok(())
            }
            (None, Some(contract)) => {
                if self.has_task_state_without_contract() {
                    return Err(
                        "image director cannot bind a contract onto orphaned task state".to_owned(),
                    );
                }
                contract.validate()?;
                let contract_digest = contract.digest()?;
                let verification_digest = contract
                    .verification
                    .deterministic
                    .as_ref()
                    .map(ImageVerificationSpec::digest)
                    .transpose()?;

                self.contract = Some(contract.clone());
                self.contract_digest = Some(contract_digest);
                self.required_verification_digest = verification_digest;
                if let Some(source) = contract.source.provided_artifact() {
                    self.authorize_image_artifacts([source.clone()])?;
                }
                self.validate_provenance_state()
            }
            (Some(existing), Some(contract)) => {
                let existing_digest = existing.digest()?;
                let requested_digest = contract.digest()?;
                let required_verification_digest = existing
                    .verification
                    .deterministic
                    .as_ref()
                    .map(ImageVerificationSpec::digest)
                    .transpose()?;
                if existing_digest != requested_digest
                    || self.contract_digest.as_deref() != Some(existing_digest.as_str())
                    || self.required_verification_digest != required_verification_digest
                {
                    return Err(
                        "image director contract does not match the requested task".to_owned()
                    );
                }
                self.validate_provenance_state()
            }
            (Some(_), None) => Err(
                "image director checkpoint contains a contract for a task without one".to_owned(),
            ),
        }
    }

    fn validate_provenance_state(&self) -> Result<(), String> {
        if let Some(review) = &self.visual_review {
            self.validate_visual_review(review)?;
        }
        if self.authorized_image_artifacts.len() > MAX_IMAGE_PROVENANCE_ARTIFACTS {
            return Err("image provenance closure exceeds the supported bound".to_owned());
        }
        for (index, artifact) in self.authorized_image_artifacts.iter().enumerate() {
            validate_image_artifact_ref(artifact)?;
            if self.authorized_image_artifacts[..index].contains(artifact) {
                return Err("image provenance closure contains duplicate artifacts".to_owned());
            }
        }
        if let Some(contract) = self.contract.as_ref() {
            if let Some(source) = contract.source.provided_artifact()
                && !self.authorized_image_artifacts.contains(source)
            {
                return Err("provided image source is missing from provenance authority".to_owned());
            }
            if contract.source.is_generate_new()
                && self.candidate_revision > 0
                && self.authorized_image_artifacts.is_empty()
            {
                return Err(
                    "generated image task has candidate state without provenance".to_owned(),
                );
            }
        }
        if self.current_candidate_artifacts.iter().any(|candidate| {
            !self
                .authorized_image_artifacts
                .contains(&candidate.artifact)
        }) {
            return Err(
                "current image candidate is outside the authorized provenance closure".to_owned(),
            );
        }
        Ok(())
    }

    #[must_use]
    pub fn is_image_artifact_authorized(&self, artifact: &ArtifactRef) -> bool {
        self.authorized_image_artifacts.contains(artifact)
    }

    /// Extends the transitive pixel-source closure after a successful producer
    /// result. Callers must have admitted every direct pixel input before the
    /// effect ran; `PerfectPixel` independently validates the producer receipt's
    /// direct dependency closure.
    pub fn authorize_image_artifacts(
        &mut self,
        artifacts: impl IntoIterator<Item = ArtifactRef>,
    ) -> Result<(), String> {
        let mut additions = Vec::new();
        for artifact in artifacts {
            validate_image_artifact_ref(&artifact)?;
            if !self.authorized_image_artifacts.contains(&artifact)
                && !additions.contains(&artifact)
            {
                additions.push(artifact);
            }
        }
        if self
            .authorized_image_artifacts
            .len()
            .saturating_add(additions.len())
            > MAX_IMAGE_PROVENANCE_ARTIFACTS
        {
            return Err("image provenance closure exceeds the supported bound".to_owned());
        }
        if additions.is_empty() {
            return Ok(());
        }
        self.authorized_image_artifacts.extend(additions);
        Ok(())
    }

    pub fn admit_candidate(&self, is_generate: bool) -> Result<(), String> {
        let Some(contract) = &self.contract else {
            return Err(
                "candidate-producing image tools require an explicit image task contract"
                    .to_owned(),
            );
        };
        if self.attempts >= contract.maximum_attempts {
            return Err("image director candidate budget is exhausted".to_owned());
        }
        if is_generate {
            if !contract.source.is_generate_new() {
                return Err(
                    "image_generate is forbidden for a provided-source image task".to_owned(),
                );
            }
            if self.editing_started {
                return Err(
                    "image_generate is forbidden after deterministic editing starts".to_owned(),
                );
            }
        } else if self.authorized_image_artifacts.is_empty() {
            return Err(
                "deterministic image editing requires an authorized source artifact".to_owned(),
            );
        }
        Ok(())
    }

    /// Every pixel-bearing input to a deterministic candidate producer must
    /// already belong to the task's provenance closure. Masks and fonts are
    /// control inputs and are validated by their own tool contracts; they do
    /// not contribute source pixels directly.
    pub fn admit_pixel_sources(&self, artifacts: &[ArtifactRef]) -> Result<(), String> {
        if artifacts.is_empty() {
            return Err(
                "deterministic image editing requires at least one authorized pixel source"
                    .to_owned(),
            );
        }
        for artifact in artifacts {
            validate_image_artifact_ref(artifact)?;
            if !self.is_image_artifact_authorized(artifact) {
                return Err(
                    "image producer input is outside the authorized provenance closure".to_owned(),
                );
            }
        }
        Ok(())
    }

    pub fn record_candidate(
        &mut self,
        artifacts: impl IntoIterator<Item = ArtifactRef>,
        editing: bool,
    ) -> Result<(), String> {
        self.record_candidate_with_kinds(
            artifacts
                .into_iter()
                .map(|artifact| (artifact, ImageCompareArtifactKind::Raster)),
            editing,
        )
    }

    /// Records a producer result with the artifact kind established by that
    /// operation's protocol result. Exact comparison admission uses this typed
    /// state rather than a SHA-only projection.
    pub fn record_candidate_with_kinds(
        &mut self,
        candidates: impl IntoIterator<Item = (ArtifactRef, ImageCompareArtifactKind)>,
        editing: bool,
    ) -> Result<(), String> {
        let mut candidates = candidates
            .into_iter()
            .filter(|(artifact, _)| artifact.media_type == "image/png")
            .collect::<Vec<_>>();
        if candidates.is_empty() {
            return Err("successful image producer returned no PNG candidate artifact".to_owned());
        }
        self.authorize_image_artifacts(candidates.iter().map(|(artifact, _)| artifact.clone()))?;

        candidates.sort_by(|left, right| {
            left.0
                .sha256
                .cmp(&right.0.sha256)
                .then(left.1.cmp(&right.1))
                .then(left.0.id.cmp(&right.0.id))
        });
        candidates.dedup_by(|left, right| left.0 == right.0 && left.1 == right.1);

        self.attempts = self.attempts.saturating_add(1);
        self.candidate_revision = self.candidate_revision.saturating_add(1);
        self.editing_started |= editing;
        self.visual_review = None;
        self.compared_revision = None;
        self.last_compare_passed = None;
        self.verified_spec_digest = None;
        self.failed_assertions.clear();
        self.revision_requests = 0;
        self.current_candidate_artifacts = candidates
            .into_iter()
            .map(|(artifact, kind)| ImageCandidateArtifact { artifact, kind })
            .collect();
        self.stage = ImageDirectorStage::Observing;
        self.validate_provenance_state()
    }

    /// Starts one semantic-mask effect against an already-authorized image.
    /// `source_revision` is derived from the current candidate revision rather
    /// than stored independently, so an older async result cannot supersede a
    /// newer candidate transition. Revision 1 denotes the contract-bound
    /// source before the first candidate is produced.
    pub fn begin_semantic_mask(
        &mut self,
        requested_source: &ArtifactRef,
        helper_identity: String,
        tool_call_id: crate::ToolCallId,
    ) -> Result<SemanticMaskOperation, String> {
        if self.contract.is_none() {
            return Err("semantic mask requires an active image task contract".to_owned());
        }
        validate_image_artifact_ref(requested_source)?;
        if helper_identity.is_empty()
            || helper_identity.len() > 256
            || helper_identity.bytes().any(|byte| byte.is_ascii_control())
        {
            return Err("semantic mask helper identity is invalid".to_owned());
        }
        if self.semantic_mask.pending.is_some() {
            return Err("a semantic mask operation is already pending".to_owned());
        }
        if !self.is_image_artifact_authorized(requested_source) {
            return Err(
                "semantic mask source is outside the authorized provenance closure".to_owned(),
            );
        }
        let source_revision = u64::from(self.candidate_revision)
            .checked_add(1)
            .ok_or_else(|| "semantic mask source revision overflowed".to_owned())?;
        self.semantic_request_revision = self
            .semantic_request_revision
            .checked_add(1)
            .ok_or_else(|| "semantic mask request revision overflowed".to_owned())?;
        let operation = SemanticMaskOperation {
            source_artifact: requested_source.clone(),
            source_revision,
            helper_identity,
            request_revision: self.semantic_request_revision,
            tool_call_id,
        };
        self.semantic_mask.begin(operation.clone())?;
        Ok(operation)
    }

    pub fn apply_semantic_mask_result(
        &mut self,
        result: SemanticMaskResult,
    ) -> Result<SemanticMaskApply, String> {
        result.validate()?;
        if self
            .semantic_mask
            .completed
            .as_ref()
            .is_some_and(|completed| completed.operation == result.operation)
        {
            return Ok(SemanticMaskApply::Duplicate);
        }
        let expected_source_revision = u64::from(self.candidate_revision)
            .checked_add(1)
            .ok_or_else(|| "semantic mask source revision overflowed".to_owned())?;
        if self.semantic_mask.pending.as_ref() == Some(&result.operation)
            && result.operation.source_revision != expected_source_revision
        {
            self.semantic_mask.pending = None;
            return Ok(SemanticMaskApply::Stale);
        }
        self.semantic_mask.apply_result(result)
    }

    #[must_use]
    pub fn current_candidate_matches(
        &self,
        artifact: &ArtifactRef,
        kind: ImageCompareArtifactKind,
    ) -> bool {
        self.current_candidate_artifacts
            .iter()
            .any(|candidate| candidate.artifact == *artifact && candidate.kind == kind)
    }

    pub fn record_compare(
        &mut self,
        verification_spec_digest: Option<String>,
        passed: bool,
        failed_assertions: Vec<String>,
    ) -> Result<(), String> {
        if self.candidate_revision == 0 {
            return Err("image comparison requires a current candidate".to_owned());
        }
        if let Some(required) = self.required_verification_digest.as_deref()
            && verification_spec_digest.as_deref() != Some(required)
        {
            return Err(
                "image comparison specification does not match the task contract".to_owned(),
            );
        }
        self.compared_revision = Some(self.candidate_revision);
        self.last_compare_passed = Some(passed);
        self.verified_spec_digest = verification_spec_digest;
        self.failed_assertions = failed_assertions;
        self.stage = if passed {
            ImageDirectorStage::Reviewing
        } else {
            ImageDirectorStage::Revising
        };
        Ok(())
    }

    pub fn mark_observed(&mut self) {
        if self.is_active() && self.candidate_revision > 0 {
            self.observed_revision = self.candidate_revision;
            if self.last_compare_passed == Some(false)
                && self.compared_revision == Some(self.candidate_revision)
            {
                self.stage = ImageDirectorStage::Revising;
            } else {
                self.stage = ImageDirectorStage::Reviewing;
            }
        }
    }

    pub fn mark_observed_hashes(&mut self, observed_sha256: &[String]) {
        if !self.current_candidate_artifacts.is_empty()
            && self.current_candidate_artifacts.iter().all(|candidate| {
                observed_sha256
                    .iter()
                    .any(|observed| observed == &candidate.artifact.sha256)
            })
        {
            self.mark_observed();
        }
    }

    pub fn candidate_set_digest(&self) -> Result<String, String> {
        if self.current_candidate_artifacts.is_empty() {
            return Err("visual review requires a candidate set".to_owned());
        }
        let mut identities = self
            .current_candidate_artifacts
            .iter()
            .map(serde_json::to_vec)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| error.to_string())?;
        identities.sort();
        identities.dedup();
        let mut hasher = Sha256::new();
        hasher.update(b"upagent-image-candidate-set-v1\0");
        for identity in identities {
            hasher.update((identity.len() as u64).to_be_bytes());
            hasher.update(identity);
        }
        Ok(format!("{:x}", hasher.finalize()))
    }

    fn validate_visual_review(&self, review: &ImageVisualReview) -> Result<(), String> {
        let contract = self
            .contract
            .as_ref()
            .ok_or("visual review requires a task contract")?;
        if self.candidate_revision == 0 || self.observed_revision != self.candidate_revision {
            return Err(
                "visual review requires actual observation of the complete current candidate set"
                    .to_owned(),
            );
        }
        if review.contract_digest != contract.digest()?
            || self.contract_digest.as_deref() != Some(review.contract_digest.as_str())
            || review.candidate_revision != self.candidate_revision
            || review.candidate_set_digest != self.candidate_set_digest()?
        {
            return Err("visual review binding is stale or mismatched".to_owned());
        }
        if review.criteria.len() != contract.visual_criteria.len() || review.criteria.is_empty() {
            return Err(
                "visual review must cover every required criterion exactly once".to_owned(),
            );
        }
        let expected = (1..=contract.visual_criteria.len())
            .map(|index| format!("visual-{index}"))
            .collect::<BTreeSet<_>>();
        let actual = review
            .criteria
            .iter()
            .map(|entry| entry.criterion_id.clone())
            .collect::<BTreeSet<_>>();
        if actual != expected || actual.len() != review.criteria.len() {
            return Err("visual review contains duplicate or unknown criterion IDs".to_owned());
        }
        for entry in &review.criteria {
            if entry.evidence.trim().is_empty()
                || entry.evidence.len() > MAX_IMAGE_CRITERION_BYTES
                || entry.evidence.contains('\0')
            {
                return Err("visual review evidence must be non-empty and bounded".to_owned());
            }
        }
        Ok(())
    }

    pub fn record_visual_review(&mut self, review: ImageVisualReview) -> Result<(), String> {
        self.validate_visual_review(&review)?;
        if self.visual_review.as_ref() == Some(&review) {
            return Err("duplicate visual review evidence".to_owned());
        }
        self.stage = if review
            .criteria
            .iter()
            .all(|entry| entry.decision == ImageVisualDecision::Pass)
        {
            ImageDirectorStage::Reviewing
        } else {
            ImageDirectorStage::Revising
        };
        self.visual_review = Some(review);
        Ok(())
    }

    #[must_use]
    pub fn has_passing_visual_review(&self) -> bool {
        self.visual_review.as_ref().is_some_and(|review| {
            self.validate_visual_review(review).is_ok()
                && review
                    .criteria
                    .iter()
                    .all(|entry| entry.decision == ImageVisualDecision::Pass)
        })
    }

    /// Request data is derived from the sole image state; no hidden latest-review state.
    pub fn visual_review_request(&self) -> Result<Option<serde_json::Value>, String> {
        let Some(contract) = self.contract.as_ref() else {
            return Ok(None);
        };
        if self.candidate_revision == 0 || self.has_passing_visual_review() {
            return Ok(None);
        }
        Ok(Some(serde_json::json!({
            "contractDigest": contract.digest()?,
            "candidateRevision": self.candidate_revision,
            "candidateSetDigest": self.candidate_set_digest()?,
            "criteria": contract.visual_criteria.iter().enumerate().map(|(index, description)| {
                serde_json::json!({"criterionId":format!("visual-{}", index + 1),"description":description})
            }).collect::<Vec<_>>()
        })))
    }

    pub fn mark_repair_required(
        &mut self,
        requirement: ImageRepairRequirement,
    ) -> Result<(), String> {
        if !self.is_active() {
            return Err("image repair requires an active image task contract".to_owned());
        }
        requirement.validate()?;
        self.pending_repair = Some(requirement);
        Ok(())
    }

    pub fn clear_repair_requirement(&mut self) {
        self.pending_repair = None;
    }

    /// A `PerfectPixel` observation may clear a pending repair only when it
    /// satisfies every exact constraint captured from the original generation
    /// request. A verified tool execution alone is insufficient.
    pub fn verify_repair_observation(
        &mut self,
        observation: ImageRepairObservation,
    ) -> Result<bool, String> {
        observation.validate()?;
        let Some(requirement) = self.pending_repair else {
            return Ok(false);
        };
        if !requirement.is_satisfied_by(&observation) {
            return Ok(false);
        }
        self.pending_repair = None;
        Ok(true)
    }

    #[must_use]
    pub fn completion_blocker(&self) -> Option<String> {
        let contract = self.contract.as_ref()?;
        if self.candidate_revision == 0 {
            return Some("image task has no candidate artifact".to_owned());
        }
        if self.current_candidate_artifacts.is_empty() {
            return Some("latest image candidate has no PNG artifact identity".to_owned());
        }
        if self.current_candidate_artifacts.iter().any(|candidate| {
            !self
                .authorized_image_artifacts
                .contains(&candidate.artifact)
        }) {
            return Some(
                "latest image candidate is outside the authorized provenance closure".to_owned(),
            );
        }
        if let Some(requirement) = self.pending_repair {
            let size = requirement
                .width
                .zip(requirement.height)
                .map(|(width, height)| format!("{width}x{height}"));
            let background = requirement.transparent_background.map(|transparent| {
                if transparent { "transparent" } else { "opaque" }
            });
            let detail = match (size, background) {
                (Some(size), Some(background)) => format!("{size}, {background} background"),
                (Some(size), None) => size,
                (None, Some(background)) => format!("{background} background"),
                (None, None) => "invalid repair requirement".to_owned(),
            };
            return Some(format!(
                "latest generated image requires PerfectPixel repair satisfying {detail}"
            ));
        }
        if self.last_compare_passed == Some(false)
            && self.compared_revision == Some(self.candidate_revision)
        {
            return Some(if self.failed_assertions.is_empty() {
                "latest image candidate failed deterministic verification".to_owned()
            } else {
                format!(
                    "latest image candidate failed deterministic verification: {}",
                    self.failed_assertions.join(", ")
                )
            });
        }
        if contract.requires_deterministic_compare()
            && (self.compared_revision != Some(self.candidate_revision)
                || self.verified_spec_digest != self.required_verification_digest)
        {
            return Some(
                "latest image candidate has not satisfied the exact deterministic comparison"
                    .to_owned(),
            );
        }
        if contract.requires_deterministic_compare() && self.last_compare_passed != Some(true) {
            return Some(
                "latest image candidate has not passed required deterministic comparison"
                    .to_owned(),
            );
        }
        if contract.verification.visual_review && self.observed_revision != self.candidate_revision
        {
            return Some(
                "latest complete image candidate set has not been visually observed".to_owned(),
            );
        }
        if contract.verification.visual_review && !self.has_passing_visual_review() {
            return Some(
                "latest image candidate set lacks bound PASS evidence for every visual criterion"
                    .to_owned(),
            );
        }
        None
    }

    pub fn request_revision(&mut self) -> Result<(), String> {
        let Some(contract) = &self.contract else {
            return Err("image director is inactive".to_owned());
        };
        if self.revision_requests >= 2 || self.attempts >= contract.maximum_attempts {
            return Err("image director revision budget is exhausted".to_owned());
        }
        self.revision_requests = self.revision_requests.saturating_add(1);
        self.stage = ImageDirectorStage::Revising;
        Ok(())
    }

    pub fn accept(&mut self) -> Result<(), String> {
        if let Some(blocker) = self.completion_blocker() {
            return Err(blocker);
        }
        if self.is_active() {
            self.stage = ImageDirectorStage::Accepted;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{FailureCode, ToolCallId};

    fn artifact(seed: char) -> ArtifactRef {
        let digest = seed.to_string().repeat(64);
        ArtifactRef {
            id: crate::ArtifactId::parse(format!("art-{digest}")).expect("artifact id"),
            sha256: digest.clone(),
            media_type: "image/png".to_owned(),
            byte_length: 1,
            relative_path: format!("{}/{}.png", &digest[..2], digest),
        }
    }

    fn verification(deterministic: bool) -> ImageVerificationContract {
        ImageVerificationContract {
            visual_review: true,
            deterministic: deterministic.then(|| ImageVerificationSpec {
                schema: IMAGE_VERIFICATION_SPEC_SCHEMA.to_owned(),
                before_artifact: artifact('0'),
                after_kind: ImageCompareArtifactKind::Raster,
                assertions: vec![ImageCompareAssertion::ExactEqual {
                    id: "preserve".to_owned(),
                    severity: ImageCompareSeverity::Required,
                }],
            }),
        }
    }

    fn contract(source: ImageSource, deterministic: bool) -> ImageTaskContract {
        ImageTaskContract {
            schema: IMAGE_TASK_CONTRACT_SCHEMA.to_owned(),
            goal: "edit the image".to_owned(),
            source,
            maximum_attempts: 16,
            preserve: Vec::new(),
            visual_criteria: vec!["visually review the current candidate".to_owned()],
            verification: verification(deterministic),
        }
    }

    #[test]
    fn empty_dependency_closure_is_stable() {
        assert_eq!(
            artifact_dependency_closure_sha256(&[]).ok().as_deref(),
            Some("4f53cda18c2baa0c0354bb5f9a3ecbe5ed12ab4d8e11ba873c2f11161202b945")
        );
    }

    #[test]
    fn dependency_closure_is_order_independent_and_deduplicated() {
        let a = ArtifactDependencyV2 {
            sha256: "a".repeat(64),
            media_type: "image/png".to_owned(),
            byte_length: 10,
        };
        let b = ArtifactDependencyV2 {
            sha256: "b".repeat(64),
            media_type: "image/png".to_owned(),
            byte_length: 20,
        };
        let left = artifact_dependency_closure_sha256(&[a.clone(), b.clone(), a.clone()]);
        let right = artifact_dependency_closure_sha256(&[b, a]);
        assert!(left.is_ok());
        assert_eq!(left, right);
    }

    fn semantic_operation(request_revision: u64, call_id: &str) -> SemanticMaskOperation {
        SemanticMaskOperation {
            source_artifact: artifact('a'),
            source_revision: 1,
            helper_identity: "fixture.semantic/1".to_owned(),
            request_revision,
            tool_call_id: ToolCallId::parse(call_id).expect("tool call id"),
        }
    }

    fn semantic_failure(
        operation: SemanticMaskOperation,
        status: SemanticMaskResultStatus,
    ) -> SemanticMaskResult {
        SemanticMaskResult {
            schema: SEMANTIC_MASK_CONTRACT_SCHEMA.to_owned(),
            operation,
            status,
            mask_artifact: None,
            failure: Some(AgentFailure::new(
                match status {
                    SemanticMaskResultStatus::Cancelled => FailureCode::Cancelled,
                    SemanticMaskResultStatus::Failed => FailureCode::ProviderFailed,
                    SemanticMaskResultStatus::Succeeded => FailureCode::InternalInvariant,
                },
                "fixture semantic result",
            )),
        }
    }

    #[test]
    fn semantic_mask_state_accepts_once_and_rejects_duplicate_or_stale_results() {
        let operation = semantic_operation(1, "semantic-call-1");
        let mask = artifact('b');
        let success = SemanticMaskResult {
            schema: SEMANTIC_MASK_CONTRACT_SCHEMA.to_owned(),
            operation: operation.clone(),
            status: SemanticMaskResultStatus::Succeeded,
            mask_artifact: Some(mask),
            failure: None,
        };
        let mut state = SemanticMaskState::default();
        state
            .begin(operation.clone())
            .expect("begin semantic operation");
        assert_eq!(
            state.apply_result(success.clone()).expect("first result"),
            SemanticMaskApply::Accepted
        );
        assert_eq!(
            state.apply_result(success).expect("duplicate result"),
            SemanticMaskApply::Duplicate
        );

        let newer = semantic_operation(2, "semantic-call-2");
        state.begin(newer).expect("begin newer operation");
        assert_eq!(
            state
                .apply_result(semantic_failure(
                    operation,
                    SemanticMaskResultStatus::Failed
                ))
                .expect("stale result is classified"),
            SemanticMaskApply::Stale
        );
    }

    #[test]
    fn semantic_mask_state_preserves_explicit_failure_and_cancel_events() {
        for status in [
            SemanticMaskResultStatus::Failed,
            SemanticMaskResultStatus::Cancelled,
        ] {
            let operation = semantic_operation(
                if status == SemanticMaskResultStatus::Failed {
                    3
                } else {
                    4
                },
                if status == SemanticMaskResultStatus::Failed {
                    "semantic-failed"
                } else {
                    "semantic-cancelled"
                },
            );
            let mut state = SemanticMaskState::default();
            state.begin(operation.clone()).expect("begin event");
            assert_eq!(
                state
                    .apply_result(semantic_failure(operation, status))
                    .expect("failure event"),
                SemanticMaskApply::Accepted
            );
            assert!(state.pending.is_none());
            assert_eq!(
                state.completed.as_ref().map(|result| result.status),
                Some(status)
            );
        }
    }

    #[test]
    fn semantic_mask_result_rejects_failure_without_a_preserved_cause() {
        let operation = semantic_operation(5, "semantic-missing-cause");
        let result = SemanticMaskResult {
            schema: SEMANTIC_MASK_CONTRACT_SCHEMA.to_owned(),
            operation,
            status: SemanticMaskResultStatus::Failed,
            mask_artifact: None,
            failure: None,
        };
        assert!(result.validate().is_err());
    }

    #[test]
    fn image_director_owns_semantic_identity_across_retry_and_checkpoint() {
        let source = artifact('c');
        let mut state = ImageDirectorState::default();
        let task_contract = contract(
            ImageSource::Provided {
                artifact: source.clone(),
            },
            false,
        );
        state
            .bind_contract(Some(&task_contract))
            .expect("bind provided image task");

        let first = state
            .begin_semantic_mask(
                &source,
                "fixture.semantic/1".to_owned(),
                ToolCallId::parse("semantic-owned-1").expect("call id"),
            )
            .expect("begin first operation");
        assert_eq!(first.source_revision, 1);
        assert_eq!(first.request_revision, 1);
        assert!(state.is_image_artifact_authorized(&source));
        assert!(
            state
                .begin_semantic_mask(
                    &source,
                    "fixture.semantic/1".to_owned(),
                    ToolCallId::parse("semantic-owned-duplicate").expect("call id"),
                )
                .is_err()
        );

        let first_success = SemanticMaskResult {
            schema: SEMANTIC_MASK_CONTRACT_SCHEMA.to_owned(),
            operation: first.clone(),
            status: SemanticMaskResultStatus::Succeeded,
            mask_artifact: Some(artifact('d')),
            failure: None,
        };
        assert_eq!(
            state
                .apply_semantic_mask_result(first_success.clone())
                .expect("accept first result"),
            SemanticMaskApply::Accepted
        );
        assert_eq!(
            state
                .apply_semantic_mask_result(first_success)
                .expect("classify duplicate result"),
            SemanticMaskApply::Duplicate
        );

        let second = state
            .begin_semantic_mask(
                &source,
                "fixture.semantic/1".to_owned(),
                ToolCallId::parse("semantic-owned-2").expect("call id"),
            )
            .expect("begin retry operation");
        assert_eq!(second.source_revision, first.source_revision);
        assert_eq!(second.request_revision, 2);
        assert_eq!(
            state
                .apply_semantic_mask_result(semantic_failure(
                    first,
                    SemanticMaskResultStatus::Failed,
                ))
                .expect("classify stale first result"),
            SemanticMaskApply::Stale
        );
        let cancelled = semantic_failure(second.clone(), SemanticMaskResultStatus::Cancelled);
        assert_eq!(
            state
                .apply_semantic_mask_result(cancelled)
                .expect("accept cancellation"),
            SemanticMaskApply::Accepted
        );
        let encoded = serde_json::to_vec(&state).expect("checkpoint encode");
        let resumed: ImageDirectorState =
            serde_json::from_slice(&encoded).expect("checkpoint decode");
        assert_eq!(resumed, state);
        assert!(resumed.semantic_mask.pending.is_none());
        assert_eq!(
            resumed
                .semantic_mask
                .completed
                .as_ref()
                .map(|result| result.status),
            Some(SemanticMaskResultStatus::Cancelled)
        );
    }

    #[test]
    fn semantic_mask_result_is_stale_after_candidate_changes() {
        let source = artifact('c');
        let mut state = ImageDirectorState::default();
        state
            .bind_contract(Some(&contract(
                ImageSource::Provided {
                    artifact: source.clone(),
                },
                false,
            )))
            .expect("contract");
        let operation = state
            .begin_semantic_mask(
                &source,
                "fixture.semantic/1".to_owned(),
                ToolCallId::parse("semantic-stale-candidate").expect("call id"),
            )
            .expect("semantic operation");
        state
            .record_candidate([artifact('d')], true)
            .expect("newer candidate transition");
        let result = SemanticMaskResult {
            schema: SEMANTIC_MASK_CONTRACT_SCHEMA.to_owned(),
            operation,
            status: SemanticMaskResultStatus::Succeeded,
            mask_artifact: Some(artifact('e')),
            failure: None,
        };
        assert_eq!(
            state
                .apply_semantic_mask_result(result)
                .expect("stale result"),
            SemanticMaskApply::Stale
        );
        assert!(state.semantic_mask.pending.is_none());
    }

    #[test]
    fn image_director_closed_loop_contract_fixtures() {
        let cases = [
            ("observed visual-only", true, false, false, true),
            ("unobserved visual-only", false, false, false, false),
            ("required compare omitted", true, false, true, false),
            ("required compare pass", true, true, true, true),
            ("required compare fail", true, true, true, false),
            (
                "multiple candidates latest observed",
                true,
                false,
                false,
                true,
            ),
            ("mask diagnostic", true, true, true, false),
            ("geometry diagnostic", true, true, true, false),
            ("locality diagnostic", true, true, true, false),
            ("alpha diagnostic", true, true, true, false),
        ];
        for (index, (name, observed, compared, compare_required, expected_complete)) in
            cases.iter().enumerate()
        {
            let mut state = ImageDirectorState::default();
            state
                .bind_contract(Some(&contract(
                    ImageSource::Provided {
                        artifact: artifact('0'),
                    },
                    *compare_required,
                )))
                .expect("contract");
            state
                .record_candidate(
                    [artifact(char::from(
                        b'a' + (u8::try_from(index).unwrap_or(0) % 6),
                    ))],
                    true,
                )
                .expect("candidate");
            if *observed {
                state.mark_observed();
                state
                    .record_visual_review(passing_review(&state))
                    .expect("bound test review");
            }
            if *compared {
                let passed = !matches!(
                    *name,
                    "required compare fail"
                        | "mask diagnostic"
                        | "geometry diagnostic"
                        | "locality diagnostic"
                        | "alpha diagnostic"
                );
                state
                    .record_compare(
                        state.required_verification_digest.clone(),
                        passed,
                        if passed {
                            Vec::new()
                        } else {
                            vec!["required".to_owned()]
                        },
                    )
                    .expect("compare evidence");
            }
            assert_eq!(
                state.completion_blocker().is_none(),
                *expected_complete,
                "{name}"
            );
        }
    }

    #[test]
    fn valid_generated_drift_is_repair_blocked_until_perfectpixel_candidate() {
        let mut state = ImageDirectorState::default();
        state
            .bind_contract(Some(&contract(ImageSource::GenerateNew, false)))
            .expect("contract");
        state
            .record_candidate([artifact('a')], false)
            .expect("generated candidate");
        state.mark_observed();
        state
            .record_visual_review(passing_review(&state))
            .expect("bound review");
        state
            .mark_repair_required(ImageRepairRequirement {
                width: Some(1024),
                height: Some(1024),
                transparent_background: Some(true),
            })
            .expect("repair requirement");
        assert!(
            state
                .completion_blocker()
                .is_some_and(|message| message.contains("1024x1024"))
        );
        assert!(
            !state
                .verify_repair_observation(ImageRepairObservation {
                    width: 512,
                    height: 512,
                    transparent_background: true,
                })
                .expect("wrong-size observation")
        );
        assert!(state.pending_repair.is_some());
        assert!(
            state
                .verify_repair_observation(ImageRepairObservation {
                    width: 1024,
                    height: 1024,
                    transparent_background: true,
                })
                .expect("matching observation")
        );
        assert!(state.pending_repair.is_none());
        assert!(state.completion_blocker().is_none());
    }

    #[test]
    fn image_director_rejects_contract_drift_on_resume() {
        let mut state = ImageDirectorState::default();
        let original = contract(
            ImageSource::Provided {
                artifact: artifact('0'),
            },
            false,
        );
        state.bind_contract(Some(&original)).expect("contract");
        let mut changed = original.clone();
        changed.goal = "different goal".to_owned();
        assert!(state.bind_contract(Some(&changed)).is_err());
        assert_eq!(state.contract.as_ref(), Some(&original));
    }

    #[test]
    fn image_director_rejects_exact_verification_drift_on_resume() {
        let mut state = ImageDirectorState::default();
        let original = contract(
            ImageSource::Provided {
                artifact: artifact('0'),
            },
            true,
        );
        state.bind_contract(Some(&original)).expect("contract");
        let mut changed = original.clone();
        changed
            .verification
            .deterministic
            .as_mut()
            .expect("verification specification")
            .assertions
            .push(ImageCompareAssertion::MaximumChannelError {
                id: "channel_error".to_owned(),
                severity: ImageCompareSeverity::Required,
                maximum: 0,
            });
        assert!(state.bind_contract(Some(&changed)).is_err());
        assert_eq!(state.contract.as_ref(), Some(&original));
        assert_eq!(
            state.required_verification_digest,
            original
                .verification
                .deterministic
                .as_ref()
                .map(ImageVerificationSpec::digest)
                .transpose()
                .expect("verification digest")
        );
    }

    #[test]
    fn image_director_binds_current_candidate_identity_and_kind() {
        let mut state = ImageDirectorState::default();
        state
            .bind_contract(Some(&contract(
                ImageSource::Provided {
                    artifact: artifact('0'),
                },
                true,
            )))
            .expect("contract");
        let candidate = artifact('a');
        state
            .record_candidate_with_kinds(
                [(candidate.clone(), ImageCompareArtifactKind::Raster)],
                true,
            )
            .expect("candidate");

        assert!(state.current_candidate_matches(&candidate, ImageCompareArtifactKind::Raster));
        assert!(!state.current_candidate_matches(&candidate, ImageCompareArtifactKind::Object));
        let mut different_identity = candidate.clone();
        different_identity.relative_path = "different.png".to_owned();
        assert!(
            !state.current_candidate_matches(&different_identity, ImageCompareArtifactKind::Raster)
        );

        let encoded = serde_json::to_string(&state).expect("candidate state JSON");
        let resumed: ImageDirectorState =
            serde_json::from_str(&encoded).expect("candidate state checkpoint JSON");
        assert_eq!(resumed, state);
        assert_eq!(
            resumed.current_candidate_artifacts[0].kind,
            ImageCompareArtifactKind::Raster
        );
    }

    #[test]
    fn advisory_failure_does_not_block_a_passing_required_specification() {
        let mut contract = contract(
            ImageSource::Provided {
                artifact: artifact('0'),
            },
            true,
        );
        let specification = contract
            .verification
            .deterministic
            .as_mut()
            .expect("verification specification");
        specification
            .assertions
            .push(ImageCompareAssertion::MaximumChannelError {
                id: "advisory_channel_error".to_owned(),
                severity: ImageCompareSeverity::Advisory,
                maximum: 0,
            });
        let results = vec![
            ImageCompareAssertionResult {
                id: "preserve".to_owned(),
                assertion_type: "exact_equal".to_owned(),
                severity: ImageCompareSeverity::Required,
                passed: true,
                observed: serde_json::json!(true),
                message: "required assertion passed".to_owned(),
            },
            ImageCompareAssertionResult {
                id: "advisory_channel_error".to_owned(),
                assertion_type: "maximum_channel_error".to_owned(),
                severity: ImageCompareSeverity::Advisory,
                passed: false,
                observed: serde_json::json!(1),
                message: "advisory assertion failed".to_owned(),
            },
        ];
        let specification = contract
            .verification
            .deterministic
            .as_ref()
            .expect("verification specification");
        assert_eq!(
            specification
                .validate_result(true, &results)
                .expect("consistent result"),
            Vec::<String>::new()
        );

        let mut state = ImageDirectorState::default();
        state.bind_contract(Some(&contract)).expect("contract");
        state
            .record_candidate([artifact('a')], true)
            .expect("candidate");
        state.mark_observed();
        state
            .record_compare(
                Some(specification.digest().expect("digest")),
                true,
                Vec::new(),
            )
            .expect("compare evidence");
        state
            .record_visual_review(passing_review(&state))
            .expect("bound review");
        assert!(state.completion_blocker().is_none());
    }

    #[test]
    fn image_director_forbids_generation_after_editing_starts() {
        let mut state = ImageDirectorState::default();
        state
            .bind_contract(Some(&contract(ImageSource::GenerateNew, false)))
            .expect("contract");
        assert!(state.admit_candidate(true).is_ok());
        state
            .record_candidate([artifact('a')], true)
            .expect("candidate");
        state.mark_observed();
        assert!(state.admit_candidate(true).is_err());
        assert!(state.admit_candidate(false).is_ok());
    }

    #[test]
    fn image_director_rejects_generation_for_provided_source() {
        let mut state = ImageDirectorState::default();
        state
            .bind_contract(Some(&contract(
                ImageSource::Provided {
                    artifact: artifact('0'),
                },
                false,
            )))
            .expect("contract");
        assert!(state.admit_candidate(true).is_err());
        assert!(state.admit_candidate(false).is_ok());
    }

    #[test]
    fn image_director_blocks_active_task_without_candidate() {
        let mut state = ImageDirectorState::default();
        state
            .bind_contract(Some(&contract(
                ImageSource::Provided {
                    artifact: artifact('0'),
                },
                false,
            )))
            .expect("contract");
        assert_eq!(
            state.completion_blocker().as_deref(),
            Some("image task has no candidate artifact")
        );
    }

    #[test]
    fn image_director_provenance_closure_is_unique_and_state_owned() {
        let source = artifact('0');
        let mut state = ImageDirectorState::default();
        state
            .bind_contract(Some(&contract(
                ImageSource::Provided {
                    artifact: source.clone(),
                },
                false,
            )))
            .expect("contract");
        assert_eq!(state.authorized_image_artifacts, vec![source.clone()]);

        state
            .authorize_image_artifacts([source.clone(), artifact('a'), artifact('a')])
            .expect("extend provenance");
        assert_eq!(state.authorized_image_artifacts.len(), 2);
        assert!(state.is_image_artifact_authorized(&source));
        assert!(state.is_image_artifact_authorized(&artifact('a')));
    }

    #[test]
    fn exact_verification_digest_changes_with_threshold_and_severity() {
        let mut specification = ImageVerificationSpec {
            schema: IMAGE_VERIFICATION_SPEC_SCHEMA.to_owned(),
            before_artifact: artifact('0'),
            after_kind: ImageCompareArtifactKind::Raster,
            assertions: vec![ImageCompareAssertion::ChangedRatio {
                id: "local_change".to_owned(),
                severity: ImageCompareSeverity::Required,
                minimum: Some(0.1),
                maximum: Some(0.2),
            }],
        };
        let original = specification.digest().expect("original digest");
        if let ImageCompareAssertion::ChangedRatio { maximum, .. } =
            &mut specification.assertions[0]
        {
            *maximum = Some(0.3);
        }
        assert_ne!(original, specification.digest().expect("threshold digest"));
        if let ImageCompareAssertion::ChangedRatio { severity, .. } =
            &mut specification.assertions[0]
        {
            *severity = ImageCompareSeverity::Advisory;
        }
        assert!(specification.validate().is_err());
    }

    #[test]
    fn wrong_verification_spec_cannot_qualify_current_candidate() {
        let mut state = ImageDirectorState::default();
        state
            .bind_contract(Some(&contract(
                ImageSource::Provided {
                    artifact: artifact('0'),
                },
                true,
            )))
            .expect("contract");
        state
            .record_candidate([artifact('a')], true)
            .expect("candidate");
        state.mark_observed();
        assert!(
            state
                .record_compare(Some("f".repeat(64)), true, Vec::new())
                .is_err()
        );
        assert_eq!(state.last_compare_passed, None);
        assert!(state.completion_blocker().is_some());
    }

    fn passing_review(state: &ImageDirectorState) -> ImageVisualReview {
        let contract = state.contract.as_ref().expect("bound test contract");
        ImageVisualReview {
            contract_digest: contract.digest().expect("contract digest"),
            candidate_revision: state.candidate_revision,
            candidate_set_digest: state.candidate_set_digest().expect("candidate set digest"),
            criteria: contract
                .visual_criteria
                .iter()
                .enumerate()
                .map(|(index, _)| ImageVisualCriterionEvidence {
                    criterion_id: format!("visual-{}", index + 1),
                    decision: ImageVisualDecision::Pass,
                    evidence: "Explicit test evidence; not a production visual inspection"
                        .to_owned(),
                })
                .collect(),
        }
    }

    fn visual_state() -> ImageDirectorState {
        let mut state = ImageDirectorState::default();
        state
            .bind_contract(Some(&contract(ImageSource::GenerateNew, false)))
            .expect("contract");
        state
            .record_candidate([artifact('a'), artifact('b')], false)
            .expect("candidates");
        state
    }

    #[test]
    fn visual_observation_alone_is_not_qualification() {
        let mut state = visual_state();
        let evidence = passing_review(&state);
        assert!(state.record_visual_review(evidence.clone()).is_err());
        state.mark_observed_hashes(&[artifact('a').sha256]);
        assert_ne!(state.observed_revision, state.candidate_revision);
        assert!(state.record_visual_review(evidence.clone()).is_err());
        state.mark_observed_hashes(&[artifact('a').sha256, artifact('b').sha256]);
        assert_eq!(state.observed_revision, state.candidate_revision);
        assert!(!state.has_passing_visual_review());
        assert!(state.completion_blocker().is_some());
        state
            .record_visual_review(evidence)
            .expect("complete bound review");
        assert!(state.completion_blocker().is_none());
    }

    #[test]
    fn candidate_set_digest_is_order_independent_and_covers_every_identity() {
        let mut state = visual_state();
        let digest = state.candidate_set_digest().expect("digest");
        state.current_candidate_artifacts.reverse();
        assert_eq!(state.candidate_set_digest().expect("digest"), digest);
        state.current_candidate_artifacts[1].artifact = artifact('c');
        assert_ne!(state.candidate_set_digest().expect("digest"), digest);
    }

    #[test]
    fn stale_or_malformed_review_is_rejected_without_mutation() {
        let mut state = visual_state();
        state.mark_observed();
        let valid = passing_review(&state);
        let mut invalid = Vec::new();
        let mut value = valid.clone();
        value.contract_digest = "0".repeat(64);
        invalid.push(value);
        let mut value = valid.clone();
        value.candidate_revision += 1;
        invalid.push(value);
        let mut value = valid.clone();
        value.candidate_set_digest = "0".repeat(64);
        invalid.push(value);
        let mut value = valid.clone();
        value.criteria[0].criterion_id = "unknown".to_owned();
        invalid.push(value);
        let mut value = valid.clone();
        value.criteria.push(value.criteria[0].clone());
        invalid.push(value);
        let mut value = valid.clone();
        value.criteria[0].evidence.clear();
        invalid.push(value);
        for review in invalid {
            let before = state.clone();
            assert!(state.record_visual_review(review).is_err());
            assert_eq!(state, before);
        }
        state.record_visual_review(valid.clone()).expect("review");
        let before = state.clone();
        assert!(state.record_visual_review(valid).is_err());
        assert_eq!(state, before);
    }

    #[test]
    fn new_candidate_clears_qualification_and_rejects_old_review() {
        let mut state = visual_state();
        state.mark_observed();
        let old = passing_review(&state);
        state.record_visual_review(old.clone()).expect("review");
        state
            .record_candidate([artifact('c')], true)
            .expect("revision");
        assert!(state.visual_review.is_none());
        assert!(!state.has_passing_visual_review());
        state.mark_observed();
        assert!(state.record_visual_review(old).is_err());
    }

    #[test]
    fn visual_review_fail_and_empty_criteria_cannot_complete() {
        let mut task = contract(ImageSource::GenerateNew, false);
        task.visual_criteria.clear();
        assert!(task.validate().is_err());
        let mut state = visual_state();
        state.mark_observed();
        let mut review = passing_review(&state);
        review.criteria[0].decision = ImageVisualDecision::Fail;
        state
            .record_visual_review(review)
            .expect("valid FAIL evidence");
        assert_eq!(state.stage, ImageDirectorStage::Revising);
        assert!(state.completion_blocker().is_some());
    }

    #[test]
    fn visual_review_control_response_is_strict_and_distinct_from_final_text() {
        let state = visual_state();
        let evidence = passing_review(&state);
        let text = format!(
            "{IMAGE_REVIEW_PREFIX}{}",
            serde_json::to_string(&evidence).expect("JSON")
        );
        assert_eq!(
            ImageVisualReview::from_model_response(&text).expect("parse"),
            Some(evidence)
        );
        assert_eq!(
            ImageVisualReview::from_model_response("Finished image").expect("normal"),
            None
        );
        assert!(ImageVisualReview::from_model_response("IMAGE_REVIEW:{\"extra\":true}").is_err());
        assert!(
            ImageVisualReview::from_model_response(&format!("{text}\nFinished image")).is_err()
        );
    }
}
