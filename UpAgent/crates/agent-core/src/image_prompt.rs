//! Immutable prompt compiler output contracts. These values never own task,
//! provider, image-candidate, approval, or persistence authority. Digests prove
//! content/compiler binding only; they do not prove model quality.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{ArtifactRef, TaskId};

pub const PROMPT_ARTIFACT_SCHEMA: &str = "upagent.image-prompt-artifact/3";
pub const PROMPT_MEDIA_TYPE: &str = "application/vnd.upagent.image-prompt+json";
pub const PROMPT_INPUT_MEDIA_TYPE: &str = "application/json";
pub const MAX_COMPILE_INPUT_BYTES: usize = 128 * 1024;
pub const MAX_COMPILED_PROMPT_BYTES: usize = 128 * 1024;
pub const MAX_PROMPT_ARTIFACT_BYTES: usize = 1024 * 1024;

pub use agent_contract::compilation::{
    BackgroundMode as CompiledImageBackground, ImageDetail as CompiledImageDetail,
    SuccessfulCompilationStatus as CompiledPromptStatus,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct CompiledImageOutput {
    pub width: u32,
    pub height: u32,
    pub background: CompiledImageBackground,
    /// Prompt intent only; it is not a provider-native quality control.
    pub detail: CompiledImageDetail,
}

impl CompiledImageOutput {
    pub fn validate(&self) -> Result<(), String> {
        self.provider_size()?;
        Ok(())
    }

    /// Canonical `UpAgent` image-provider size. promptGen itself can describe a
    /// wider set of dimensions, but a prompt artifact must be executable by
    /// the configured `image_generate` contract rather than becoming an
    /// unusable artifact.
    pub fn provider_size(&self) -> Result<&'static str, String> {
        match (self.width, self.height) {
            (1024, 1024) => Ok("1024x1024"),
            (1536, 1024) => Ok("1536x1024"),
            (1024, 1536) => Ok("1024x1536"),
            _ => Err(
                "compiled image dimensions are unsupported by the canonical image generation contract"
                    .to_owned(),
            ),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct CompiledImagePrompt {
    pub status: CompiledPromptStatus,
    /// SHA-256 of the exact input JSON bytes supplied by the caller.
    pub input_sha256: String,
    /// SHA-256 of promptGen's normalized typed `metadata.request` JSON value.
    pub normalized_input_sha256: String,
    /// SHA-256 of the complete promptGen compilation result bytes.
    pub compiler_outcome_sha256: String,
    pub prompt: String,
    pub prompt_sha256: String,
    pub output: CompiledImageOutput,
}

impl CompiledImagePrompt {
    pub fn validate(&self) -> Result<(), String> {
        if !valid_prompt_sha256(&self.input_sha256)
            || !valid_prompt_sha256(&self.normalized_input_sha256)
            || !valid_prompt_sha256(&self.compiler_outcome_sha256)
            || !valid_prompt_sha256(&self.prompt_sha256)
        {
            return Err("compiled prompt contains an invalid content digest".to_owned());
        }
        if self.prompt.trim().is_empty()
            || self.prompt.len() > MAX_COMPILED_PROMPT_BYTES
            || self.prompt.contains('\0')
            || self.prompt_sha256 != prompt_content_sha256(self.prompt.as_bytes())
        {
            return Err(
                "compiled prompt content is empty, unbounded, or digest-mismatched".to_owned(),
            );
        }
        self.output.validate()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ImagePromptArtifact {
    pub schema: String,
    pub task_id: TaskId,
    pub compiler_sha256: String,
    pub compiled: CompiledImagePrompt,
}

impl ImagePromptArtifact {
    pub fn new(
        task_id: TaskId,
        compiler_sha256: String,
        compiled: CompiledImagePrompt,
    ) -> Result<Self, String> {
        compiled.validate()?;
        if !valid_prompt_sha256(&compiler_sha256) {
            return Err("invalid prompt compiler identity digest".to_owned());
        }
        Ok(Self {
            schema: PROMPT_ARTIFACT_SCHEMA.to_owned(),
            task_id,
            compiler_sha256,
            compiled,
        })
    }

    pub fn resolve(
        bytes: &[u8],
        task_id: &TaskId,
        expected_compiler_sha256: &str,
        expected_prompt_sha256: &str,
    ) -> Result<Self, String> {
        if bytes.is_empty() || bytes.len() > MAX_PROMPT_ARTIFACT_BYTES {
            return Err("compiled prompt artifact exceeds its byte bound".to_owned());
        }
        let value: Self = serde_json::from_slice(bytes).map_err(|error| error.to_string())?;
        value.compiled.validate()?;
        if value.schema != PROMPT_ARTIFACT_SCHEMA
            || &value.task_id != task_id
            || !valid_prompt_sha256(expected_compiler_sha256)
            || value.compiler_sha256 != expected_compiler_sha256
            || value.compiled.prompt_sha256 != expected_prompt_sha256
        {
            return Err(
                "compiled prompt artifact schema/task/compiler/content binding mismatch".to_owned(),
            );
        }
        Ok(value)
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct PromptCompileArguments {
    #[serde(default)]
    pub input_json: Option<String>,
    #[serde(default)]
    pub input_artifact: Option<ArtifactRef>,
}

impl PromptCompileArguments {
    pub fn resolve(
        self,
        read: impl FnOnce(&ArtifactRef) -> Result<Vec<u8>, String>,
    ) -> Result<String, String> {
        let input = match (self.input_json, self.input_artifact) {
            (Some(input), None) => input,
            (None, Some(reference)) => {
                if reference.media_type != PROMPT_INPUT_MEDIA_TYPE
                    || reference.byte_length == 0
                    || reference.byte_length > MAX_COMPILE_INPUT_BYTES as u64
                {
                    return Err("invalid compiler input artifact type or byte bound".to_owned());
                }
                String::from_utf8(read(&reference)?).map_err(|error| error.to_string())?
            }
            _ => return Err("exactly one of inputJson or inputArtifact is required".to_owned()),
        };
        if input.trim().is_empty() || input.len() > MAX_COMPILE_INPUT_BYTES || input.contains('\0')
        {
            return Err("compiler input must be non-empty and bounded".to_owned());
        }
        Ok(input)
    }
}

#[must_use]
pub fn prompt_content_sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn valid_prompt_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn compiled() -> CompiledImagePrompt {
        let prompt = "Draw the supplied image";
        CompiledImagePrompt {
            status: CompiledPromptStatus::Valid,
            input_sha256: "a".repeat(64),
            normalized_input_sha256: "b".repeat(64),
            compiler_outcome_sha256: "c".repeat(64),
            prompt: prompt.to_owned(),
            prompt_sha256: prompt_content_sha256(prompt.as_bytes()),
            output: CompiledImageOutput {
                width: 1536,
                height: 1024,
                background: CompiledImageBackground::Opaque,
                detail: CompiledImageDetail::High,
            },
        }
    }

    #[test]
    fn compilation_checks_content_task_and_provider_compatibility_without_material_state() {
        let task = TaskId::parse("prompt-task").expect("task");
        let value =
            ImagePromptArtifact::new(task.clone(), "d".repeat(64), compiled()).expect("artifact");
        let bytes = serde_json::to_vec(&value).expect("json");
        assert!(
            ImagePromptArtifact::resolve(
                &bytes,
                &task,
                &value.compiler_sha256,
                &value.compiled.prompt_sha256
            )
            .is_ok()
        );
        assert!(
            ImagePromptArtifact::resolve(&bytes, &task, &value.compiler_sha256, &"e".repeat(64))
                .is_err()
        );
        assert!(
            ImagePromptArtifact::resolve(
                &bytes,
                &TaskId::parse("other-task").expect("task"),
                &value.compiler_sha256,
                &value.compiled.prompt_sha256,
            )
            .is_err()
        );
        assert!(
            ImagePromptArtifact::resolve(
                &bytes,
                &task,
                &"e".repeat(64),
                &value.compiled.prompt_sha256,
            )
            .is_err()
        );
        let mut tampered = value;
        tampered.compiled.prompt.push_str(" altered");
        assert!(tampered.compiled.validate().is_err());

        let mut unsupported = compiled();
        unsupported.output.width = 2048;
        assert!(unsupported.validate().is_err());
    }

    #[test]
    fn input_xor_and_bounds_are_checked_before_io() {
        let neither = PromptCompileArguments {
            input_json: None,
            input_artifact: None,
        };
        assert!(neither.resolve(|_| unreachable!("must not read")).is_err());
        let reference = ArtifactRef {
            id: crate::ArtifactId::parse("art-input").expect("id"),
            sha256: "a".repeat(64),
            media_type: "application/json".to_owned(),
            byte_length: 1,
            relative_path: "input.json".to_owned(),
        };
        let both = PromptCompileArguments {
            input_json: Some("{}".to_owned()),
            input_artifact: Some(reference),
        };
        assert!(both.resolve(|_| unreachable!("must not read")).is_err());
        let input = PromptCompileArguments {
            input_json: Some(" ".to_owned()),
            input_artifact: None,
        };
        assert!(input.resolve(|_| unreachable!("must not read")).is_err());
    }
}
