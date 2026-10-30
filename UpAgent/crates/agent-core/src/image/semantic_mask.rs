use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{ArtifactRef, error::AgentFailure};

use super::validate_image_artifact_ref;

pub const SEMANTIC_MASK_CONTRACT_SCHEMA: &str = "upagent.semantic-mask/1";

/// Identity carried by every semantic-mask effect and result.  The adapter is
/// an effect; it cannot mutate image state directly.  A result is accepted
/// only when every identity field still names the pending operation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct SemanticMaskOperation {
    pub source_artifact: ArtifactRef,
    pub source_revision: u64,
    pub helper_identity: String,
    pub request_revision: u64,
    pub tool_call_id: crate::ToolCallId,
}

impl SemanticMaskOperation {
    pub fn validate(&self) -> Result<(), String> {
        validate_image_artifact_ref(&self.source_artifact)?;
        if self.source_artifact.media_type != "image/png"
            || self.source_artifact.sha256.len() != 64
            || !self
                .source_artifact
                .sha256
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit())
            || self.source_revision == 0
            || self.helper_identity.is_empty()
            || self.helper_identity.len() > 256
            || self
                .helper_identity
                .bytes()
                .any(|byte| byte.is_ascii_control())
            || self.request_revision == 0
        {
            return Err("semantic mask operation identity is invalid".to_owned());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SemanticMaskResultStatus {
    Succeeded,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct SemanticMaskResult {
    pub schema: String,
    pub operation: SemanticMaskOperation,
    pub status: SemanticMaskResultStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mask_artifact: Option<ArtifactRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure: Option<AgentFailure>,
}

impl SemanticMaskResult {
    pub fn validate(&self) -> Result<(), String> {
        if self.schema != SEMANTIC_MASK_CONTRACT_SCHEMA {
            return Err("semantic mask result schema is unsupported".to_owned());
        }
        self.operation.validate()?;
        match self.status {
            SemanticMaskResultStatus::Succeeded => {
                let mask = self.mask_artifact.as_ref().ok_or_else(|| {
                    "successful semantic mask result has no mask artifact".to_owned()
                })?;
                validate_image_artifact_ref(mask).map_err(|error| {
                    format!("semantic mask artifact metadata is invalid: {error}")
                })?;
                if self.failure.is_some() {
                    return Err("successful semantic mask result cannot contain failure".to_owned());
                }
            }
            SemanticMaskResultStatus::Failed | SemanticMaskResultStatus::Cancelled => {
                if self.mask_artifact.is_some() {
                    return Err(
                        "failed or cancelled semantic mask result cannot publish a mask".to_owned(),
                    );
                }
                if self.failure.is_none() {
                    return Err(
                        "failed or cancelled semantic mask result must preserve a failure cause"
                            .to_owned(),
                    );
                }
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SemanticMaskApply {
    Accepted,
    Stale,
    Duplicate,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, Default)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct SemanticMaskState {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending: Option<SemanticMaskOperation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed: Option<SemanticMaskResult>,
}

impl SemanticMaskState {
    pub fn begin(&mut self, operation: SemanticMaskOperation) -> Result<(), String> {
        operation.validate()?;
        if self.pending.is_some() {
            return Err("a semantic mask operation is already pending".to_owned());
        }
        self.pending = Some(operation);
        self.completed = None;
        Ok(())
    }

    pub fn apply_result(
        &mut self,
        result: SemanticMaskResult,
    ) -> Result<SemanticMaskApply, String> {
        result.validate()?;
        if self
            .completed
            .as_ref()
            .is_some_and(|completed| completed.operation == result.operation)
        {
            return Ok(SemanticMaskApply::Duplicate);
        }
        if self.pending.as_ref() != Some(&result.operation) {
            return Ok(SemanticMaskApply::Stale);
        }
        self.pending = None;
        self.completed = Some(result);
        Ok(SemanticMaskApply::Accepted)
    }
}
