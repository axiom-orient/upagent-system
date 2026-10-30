#![forbid(unsafe_code)]

//! Provider-neutral model and image ports used by `UpAgent`.
//!
//! This crate is the complete replaceable provider boundary. It contains no
//! executable discovery, credential/home semantics, wire protocol, or concrete
//! provider implementation. Adapters such as the stock Vergerail transport
//! implement these traits in separate crates.

use std::fmt;
use std::sync::Arc;

use agent_core::{
    AgentFailure, FailureCode, ImageObservationRole, ModelTurn, ModelTurnResult, ProviderBinding,
    ReasoningEffort,
};
use agent_plugin_kernel::{PluginId, PluginManifest};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;
use tokio_util::sync::CancellationToken;

pub use agent_contract::limits::{
    MAXIMUM_IMAGE_BYTES, MAXIMUM_MODEL_MESSAGE_BYTES, MAXIMUM_MODEL_MESSAGE_SCALARS,
    MAXIMUM_MODEL_MESSAGES_PER_TURN, MAXIMUM_MODEL_RESPONSE_BYTES, MAXIMUM_MODEL_TOOLS_PER_TURN,
    MAXIMUM_TOOL_ARGUMENT_BYTES, MAXIMUM_TOOL_ARGUMENTS_BYTES, MAXIMUM_TOOL_CALL_ID_BYTES,
    MAXIMUM_TOOL_CALLS_PER_TURN, MAXIMUM_TOOL_NAME_BYTES,
};
pub const MAXIMUM_PROVIDER_CONFIGURATION_BYTES: usize = 256 * 1024;

/// Host-selected linked provider plugin and its opaque, plugin-owned
/// configuration.  The host verifies the plugin manifest digest before it
/// gives `configuration` to `resolve_binding` or `connect`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ProviderPluginEnvelope {
    pub plugin_id: String,
    pub manifest_sha256: String,
    pub configuration: Value,
}

impl ProviderPluginEnvelope {
    pub fn validate(&self) -> Result<(), String> {
        PluginId::parse(self.plugin_id.clone()).map_err(|error| error.to_string())?;
        if !is_sha256(&self.manifest_sha256) {
            return Err(
                "provider manifestSha256 must be a 64-character hexadecimal digest".to_owned(),
            );
        }
        let encoded = serde_json::to_vec(&self.configuration)
            .map_err(|error| format!("provider configuration could not be encoded: {error}"))?;
        if encoded.is_empty() || encoded.len() > MAXIMUM_PROVIDER_CONFIGURATION_BYTES {
            return Err(format!(
                "provider configuration must be 1..={MAXIMUM_PROVIDER_CONFIGURATION_BYTES} bytes"
            ));
        }
        Ok(())
    }
}

pub use agent_contract::provider::{
    ImageBackgroundRequirement, ImageGenerationRequirements, ImageQualityRequirement,
    ImageSizeRequirement,
};

/// A mismatch is recoverable image evidence, not a provider transport error.
/// The bytes remain valid and are handed to the deterministic `PerfectPixel`
/// repair path. Structural failures never use this type.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ImageGenerationMismatch {
    Size {
        requested: ImageSizeRequirement,
        observed_width: u32,
        observed_height: u32,
    },
    Background {
        requested: ImageBackgroundRequirement,
        observed_transparent: Option<bool>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ProviderImageStatus {
    #[default]
    Generated,
    GeneratedNeedsRepair,
}

/// Raw image bytes plus provider-observed generation state. A generated image
/// can be valid while still needing deterministic post-processing, so this is
/// deliberately not represented as `Result::Err`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ProviderImageResult {
    pub image: ProviderImage,
    pub status: ProviderImageStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_transparent_background: Option<bool>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mismatches: Vec<ImageGenerationMismatch>,
}

impl ProviderImageResult {
    pub fn validate(&self) -> Result<(), ProviderError> {
        if self.image.media_type != "image/png"
            || self.image.bytes.is_empty()
            || self.image.bytes.len() > MAXIMUM_IMAGE_BYTES
            || self.image.width == 0
            || self.image.height == 0
            || self.image.bytes.len() < 33
            || !self.image.bytes.starts_with(b"\x89PNG\r\n\x1a\n")
            || self.image.bytes.get(12..16) != Some(b"IHDR")
        {
            return Err(ProviderError::Contract(
                "provider image result contains an invalid bounded PNG".to_owned(),
            ));
        }
        let width =
            u32::from_be_bytes(self.image.bytes[16..20].try_into().map_err(|_| {
                ProviderError::Contract("provider PNG width is invalid".to_owned())
            })?);
        let height =
            u32::from_be_bytes(self.image.bytes[20..24].try_into().map_err(|_| {
                ProviderError::Contract("provider PNG height is invalid".to_owned())
            })?);
        if (width, height) != (self.image.width, self.image.height)
            || width > 8_192
            || height > 8_192
            || u64::from(width).saturating_mul(u64::from(height)) > 8_192 * 8_192
        {
            return Err(ProviderError::Contract(
                "provider image dimensions do not match its PNG header".to_owned(),
            ));
        }
        // `status`, `mismatches`, and the provider's alpha observation are
        // advisory metadata.  The host re-inspects the bytes through
        // PerfectPixel and computes the canonical status from that result.
        // Keeping these fields even when they disagree preserves evidence of
        // a faulty or stale provider without discarding a valid PNG.
        Ok(())
    }
}

/// Provider planes opened by a linked provider plugin. The model and image
/// planes are kept separate so a host can use a provider with independent
/// implementations while still checking their immutable bindings.
#[derive(Clone)]
pub struct ProviderPlanes {
    pub model: Arc<dyn ModelPort>,
    pub image: Arc<dyn ImagePort>,
}

impl fmt::Debug for ProviderPlanes {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProviderPlanes")
            .field("modelBinding", &self.model.binding())
            .field("imageBinding", &self.image.binding())
            .finish()
    }
}

/// Provider plugin boundary. Configuration is intentionally opaque JSON here;
/// each linked provider owns its strict parser and binding derivation.
#[async_trait]
pub trait ProviderPlugin: Send + Sync + fmt::Debug {
    fn manifest(&self) -> &PluginManifest;

    fn resolve_binding(&self, configuration: &Value) -> Result<ProviderBinding, ProviderError>;

    async fn connect(&self, configuration: &Value) -> Result<ProviderPlanes, ProviderError>;
}

#[derive(Debug, Error)]
pub enum ProviderError {
    #[error("provider contract failed: {0}")]
    Contract(String),
    #[error("provider execution failed: {0}")]
    Failure(AgentFailure),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ProviderImage {
    pub bytes: Vec<u8>,
    pub media_type: String,
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedImageObservation {
    pub message_index: usize,
    pub part_index: usize,
    pub media_type: String,
    pub sha256: String,
    pub width: u32,
    pub height: u32,
    pub role: ImageObservationRole,
    pub bytes: Vec<u8>,
}

#[async_trait]
pub trait ModelPort: Send + Sync + fmt::Debug {
    /// Immutable adapter-neutral identity used by composition and runtime
    /// admission. Implementations must return the same binding for their
    /// lifetime.
    fn binding(&self) -> ProviderBinding;

    async fn complete(&self, turn: &ModelTurn) -> Result<ModelTurnResult, ProviderError>;

    async fn complete_with_cancellation(
        &self,
        turn: &ModelTurn,
        cancellation: &CancellationToken,
    ) -> Result<ModelTurnResult, ProviderError> {
        tokio::select! {
            biased;
            () = cancellation.cancelled() => Err(cancelled_provider_execution()),
            result = self.complete(turn) => result,
        }
    }

    async fn complete_with_observations(
        &self,
        turn: &ModelTurn,
        observations: &[ResolvedImageObservation],
        cancellation: &CancellationToken,
    ) -> Result<ModelTurnResult, ProviderError> {
        if observations.is_empty() {
            return self.complete_with_cancellation(turn, cancellation).await;
        }
        Err(ProviderError::Failure(AgentFailure::new(
            FailureCode::CapabilityDenied,
            "configured model provider does not support image observations",
        )))
    }
}

#[async_trait]
pub trait ImagePort: Send + Sync + fmt::Debug {
    /// Immutable identity of the provider performing generation. The runtime
    /// binds this to the same `ProviderBinding` as the active `ModelPort`.
    fn binding(&self) -> ProviderBinding;

    async fn generate_image(
        &self,
        prompt: &str,
        requirements: &ImageGenerationRequirements,
        reasoning: ReasoningEffort,
        timeout_ms: u64,
        maximum_bytes: usize,
    ) -> Result<ProviderImageResult, ProviderError>;

    async fn generate_image_with_cancellation(
        &self,
        prompt: &str,
        requirements: &ImageGenerationRequirements,
        reasoning: ReasoningEffort,
        timeout_ms: u64,
        maximum_bytes: usize,
        cancellation: &CancellationToken,
    ) -> Result<ProviderImageResult, ProviderError>;
}

fn is_sha256(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn cancelled_provider_execution() -> ProviderError {
    ProviderError::Failure(AgentFailure::new(
        FailureCode::Cancelled,
        "provider execution cancelled",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png(width: u32, height: u32) -> Vec<u8> {
        let mut bytes = vec![0_u8; 33];
        bytes[..8].copy_from_slice(b"\x89PNG\r\n\x1a\n");
        bytes[12..16].copy_from_slice(b"IHDR");
        bytes[16..20].copy_from_slice(&width.to_be_bytes());
        bytes[20..24].copy_from_slice(&height.to_be_bytes());
        bytes
    }

    #[test]
    fn valid_generated_png_can_require_repair_without_becoming_an_error() {
        let result = ProviderImageResult {
            image: ProviderImage {
                bytes: png(1, 1),
                media_type: "image/png".to_owned(),
                width: 1,
                height: 1,
            },
            status: ProviderImageStatus::GeneratedNeedsRepair,
            observed_transparent_background: Some(false),
            mismatches: vec![ImageGenerationMismatch::Background {
                requested: ImageBackgroundRequirement::Transparent,
                observed_transparent: Some(false),
            }],
        };
        assert!(result.validate().is_ok());
    }

    #[test]
    fn provider_status_and_mismatch_metadata_are_not_host_authority() {
        let result = ProviderImageResult {
            image: ProviderImage {
                bytes: png(1, 1),
                media_type: "image/png".to_owned(),
                width: 1,
                height: 1,
            },
            status: ProviderImageStatus::Generated,
            observed_transparent_background: None,
            mismatches: vec![ImageGenerationMismatch::Size {
                requested: ImageSizeRequirement::Square,
                observed_width: 1,
                observed_height: 1,
            }],
        };
        assert!(result.validate().is_ok());
    }

    #[test]
    fn provider_envelope_requires_plugin_identity_and_digest() {
        let envelope = ProviderPluginEnvelope {
            plugin_id: "upagent.provider.fixture".to_owned(),
            manifest_sha256: "a".repeat(64),
            configuration: serde_json::json!({"route": "fixture"}),
        };
        assert!(envelope.validate().is_ok());
        let mut invalid = envelope.clone();
        invalid.plugin_id = "not valid".to_owned();
        assert!(invalid.validate().is_err());
        invalid = envelope.clone();
        invalid.manifest_sha256 = "not-a-digest".to_owned();
        assert!(invalid.validate().is_err());
        invalid = envelope;
        invalid.configuration = Value::Null;
        assert!(invalid.validate().is_ok());
    }
}
