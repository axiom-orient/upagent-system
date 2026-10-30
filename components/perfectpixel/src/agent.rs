use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::{sha256_hex, PpError, PpResult};

pub const AGENT_PROTOCOL_SCHEMA: &str = "perfectpixel.agent-image/2";
pub const AGENT_PROTOCOL_VERSION: &str = "2.0.0";
pub const AGENT_BEHAVIOR_VERSION: &str = "2.0.0";
pub const AGENT_INSPECT_REQUEST_SCHEMA: &str = "perfectpixel.agent-image/inspect/2";
pub const AGENT_INSPECT_RESULT_SCHEMA: &str = "perfectpixel.agent-image/inspect-result/2";
pub const AGENT_EXTRACT_REQUEST_SCHEMA: &str = "perfectpixel.agent-image/extract/2";
pub const AGENT_EXTRACT_RESULT_SCHEMA: &str = "perfectpixel.agent-image/extract-result/2";
pub const AGENT_RENDER_REQUEST_SCHEMA: &str = "perfectpixel.agent-image/render/2";
pub const AGENT_RENDER_RESULT_SCHEMA: &str = "perfectpixel.agent-image/render-result/2";
pub const AGENT_COMPARE_REQUEST_SCHEMA: &str = "perfectpixel.agent-image/compare/2";
pub const AGENT_COMPARE_RESULT_SCHEMA: &str = "perfectpixel.agent-image/compare-result/2";
pub const AGENT_RECEIPT_SCHEMA: &str = "perfectpixel.agent-image/receipt/2";
pub const AGENT_PIN_SET_SCHEMA: &str = "perfectpixel.agent-image/pin-set/2";
/// Canonical grammar shared with UpAgent's typed image acceptance contract.
pub const IMAGE_ASSERTION_ID_MAX_LENGTH: usize = 64;
pub const IMAGE_ASSERTION_ID_PATTERN: &str = "^[A-Za-z][A-Za-z0-9_-]{0,63}$";
/// Object centroid coordinates are alpha-weighted pixel indices from the
/// top-left origin across the agent image protocol.
pub const IMAGE_OBJECT_CENTROID_COORDINATE_SYSTEM: &str = "pixel-index-top-left";

pub fn validate_canonical_sha256(value: &str) -> PpResult<()> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
    {
        return Err(PpError::InvalidRequest(
            "sha256 must be exactly 64 lowercase hexadecimal characters".to_owned(),
        ));
    }
    Ok(())
}

pub fn validate_image_assertion_id(value: &str) -> PpResult<()> {
    let mut bytes = value.bytes();
    if !bytes.next().is_some_and(|byte| byte.is_ascii_alphabetic())
        || !bytes.all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
        || value.len() > IMAGE_ASSERTION_ID_MAX_LENGTH
    {
        return Err(PpError::InvalidRequest(format!(
            "compare assertion id must match {IMAGE_ASSERTION_ID_PATTERN}"
        )));
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeterminismClass {
    BitExact,
    BackendDeterministic,
    InferenceObserved,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Capability {
    pub name: String,
    pub version: String,
    pub determinism: DeterminismClass,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CapabilityManifest {
    pub schema: String,
    pub protocol_version: String,
    pub behavior_version: String,
    pub implementation_version: String,
    pub capabilities: Vec<Capability>,
}

#[must_use]
pub fn agent_capability_manifest(implementation_version: &str) -> CapabilityManifest {
    CapabilityManifest {
        schema: AGENT_PROTOCOL_SCHEMA.to_owned(),
        protocol_version: AGENT_PROTOCOL_VERSION.to_owned(),
        behavior_version: AGENT_BEHAVIOR_VERSION.to_owned(),
        implementation_version: implementation_version.to_owned(),
        capabilities: vec![
            Capability {
                name: "inspect.basic".to_owned(),
                version: "2.0.0".to_owned(),
                determinism: DeterminismClass::BitExact,
            },
            Capability {
                name: "artifact.content_addressed".to_owned(),
                version: "2.0.0".to_owned(),
                determinism: DeterminismClass::BitExact,
            },
            Capability {
                name: "artifact.dependency_closure".to_owned(),
                version: "2.0.0".to_owned(),
                determinism: DeterminismClass::BitExact,
            },
            Capability {
                name: "artifact.pin_set".to_owned(),
                version: "2.0.0".to_owned(),
                determinism: DeterminismClass::BitExact,
            },
            Capability {
                name: "extract.alpha".to_owned(),
                version: "1.0.0".to_owned(),
                determinism: DeterminismClass::BitExact,
            },
            Capability {
                name: "extract.chroma_key".to_owned(),
                version: "1.0.0".to_owned(),
                determinism: DeterminismClass::BitExact,
            },
            Capability {
                name: "extract.color_range".to_owned(),
                version: "1.0.0".to_owned(),
                determinism: DeterminismClass::BitExact,
            },
            Capability {
                name: "extract.alpha_component".to_owned(),
                version: "1.0.0".to_owned(),
                determinism: DeterminismClass::BitExact,
            },
            Capability {
                name: "extract.provided_mask".to_owned(),
                version: "1.0.0".to_owned(),
                determinism: DeterminismClass::BitExact,
            },
            Capability {
                name: "extract.matte_refine".to_owned(),
                version: "1.0.0".to_owned(),
                determinism: DeterminismClass::BitExact,
            },
            Capability {
                name: "render.composition_dag".to_owned(),
                version: "1.0.0".to_owned(),
                determinism: DeterminismClass::BackendDeterministic,
            },
            Capability {
                name: "render.affine_3x3".to_owned(),
                version: "1.0.0".to_owned(),
                determinism: DeterminismClass::BackendDeterministic,
            },
            Capability {
                name: "render.source_over".to_owned(),
                version: "1.0.0".to_owned(),
                determinism: DeterminismClass::BitExact,
            },
            Capability {
                name: "render.text_node".to_owned(),
                version: "1.0.0".to_owned(),
                determinism: DeterminismClass::BitExact,
            },
            Capability {
                name: "compare.basic".to_owned(),
                version: "1.0.0".to_owned(),
                determinism: DeterminismClass::BitExact,
            },
            Capability {
                name: "compare.exact_spec".to_owned(),
                version: "1.0.0".to_owned(),
                determinism: DeterminismClass::BitExact,
            },
            Capability {
                name: "compare.regions".to_owned(),
                version: "1.0.0".to_owned(),
                determinism: DeterminismClass::BitExact,
            },
            Capability {
                name: "compare.masks".to_owned(),
                version: "1.0.0".to_owned(),
                determinism: DeterminismClass::BitExact,
            },
            Capability {
                name: "compare.geometry".to_owned(),
                version: "1.0.0".to_owned(),
                determinism: DeterminismClass::BitExact,
            },
            Capability {
                name: "compare.preview".to_owned(),
                version: "1.0.0".to_owned(),
                determinism: DeterminismClass::BitExact,
            },
        ],
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PixelFormat {
    Rgba8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ColorSpace {
    Srgb,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AlphaMode {
    Straight,
    Premultiplied,
    Opaque,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactKind {
    WorkingRaster,
    ExportRaster,
    Mask,
    Object,
    Json,
    Receipt,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactRetention {
    TaskScoped,
    Cacheable,
    Pinned,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, PartialOrd, Ord)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ArtifactDependency {
    pub sha256: String,
    pub media_type: String,
    pub byte_length: u64,
}

impl ArtifactDependency {
    pub fn validate(&self) -> PpResult<()> {
        validate_canonical_sha256(&self.sha256)?;
        if self.media_type.trim().is_empty() || self.media_type.len() > 256 || self.byte_length == 0
        {
            return Err(PpError::InvalidRequest(
                "artifact dependency metadata is invalid".to_owned(),
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InputArtifact {
    pub path: String,
    pub expected_sha256: String,
    pub media_type: String,
    pub byte_length: u64,
    pub kind: ArtifactKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pixel_spec: Option<PixelSpec>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ArtifactDescriptor {
    pub sha256: String,
    pub media_type: String,
    pub byte_length: u64,
    pub kind: ArtifactKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pixel_spec: Option<PixelSpec>,
    pub retention: ArtifactRetention,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dependencies: Vec<ArtifactDependency>,
    pub dependency_closure_sha256: String,
}

impl ArtifactDescriptor {
    pub fn new(
        sha256: String,
        media_type: String,
        byte_length: u64,
        kind: ArtifactKind,
        pixel_spec: Option<PixelSpec>,
        retention: ArtifactRetention,
        mut dependencies: Vec<ArtifactDependency>,
    ) -> PpResult<Self> {
        validate_canonical_sha256(&sha256)?;
        for dependency in &dependencies {
            dependency.validate()?;
        }
        dependencies.sort();
        dependencies.dedup();
        let dependency_closure_sha256 = dependency_closure_sha256(&dependencies)?;
        Ok(Self {
            sha256,
            media_type,
            byte_length,
            kind,
            pixel_spec,
            retention,
            dependencies,
            dependency_closure_sha256,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationStatus {
    Committed,
    Rejected,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OperationReceipt {
    pub schema: String,
    pub request_id: String,
    pub operation: String,
    pub status: OperationStatus,
    pub behavior_version: String,
    pub implementation_version: String,
    pub request_sha256: String,
    pub dependency_closure_sha256: String,
    pub dependencies: Vec<ArtifactDependency>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_sha256: Option<String>,
    pub determinism: DeterminismClass,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ArtifactPinSet {
    pub schema: String,
    pub pins: Vec<String>,
}

impl ArtifactPinSet {
    pub fn validate(&self) -> PpResult<()> {
        if self.schema != AGENT_PIN_SET_SCHEMA || self.pins.len() > 4096 {
            return Err(PpError::InvalidRequest(
                "artifact pin set contract is invalid".to_owned(),
            ));
        }
        let mut seen = BTreeSet::new();
        for digest in &self.pins {
            validate_canonical_sha256(digest)?;
            if !seen.insert(digest) {
                return Err(PpError::InvalidRequest(
                    "artifact pin set contains an invalid or duplicate digest".to_owned(),
                ));
            }
        }
        Ok(())
    }

    #[must_use]
    pub fn contains(&self, digest: &str) -> bool {
        self.pins.iter().any(|pin| pin == digest)
    }
}

pub fn dependency_closure_sha256(dependencies: &[ArtifactDependency]) -> PpResult<String> {
    let mut normalized = dependencies.to_vec();
    for dependency in &normalized {
        dependency.validate()?;
    }
    normalized.sort();
    normalized.dedup();
    let bytes = serde_json::to_vec(&normalized)
        .map_err(|error| PpError::InvalidRequest(error.to_string()))?;
    Ok(sha256_hex(&bytes))
}
