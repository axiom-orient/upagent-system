//! Creator-facing discovery, proposal validation and local prompt compilation.
//! Explicit AgentSpec.tools selection is the only activation owner. A prepared
//! contract is a proposal for a caller, never a mutation of the running task.
use crate::{
    ModuleArtifact, ModuleExecution, ModuleTool, PromptGenAdapter, ToolContext, ToolModule,
    ToolModuleError, ToolSideEffect,
};
use agent_core::{
    CapabilityId, IMAGE_CAPABILITIES_TOOL, IMAGE_MAKER_TOOL_NAMES, IMAGE_TOOL_NAMES,
    IMAGEMAKER_PREPARE_TOOL, ImagePromptArtifact, ImageTaskContract, MAX_COMPILE_INPUT_BYTES,
    PROMPT_MEDIA_TYPE, PROMPTGEN_COMPILE_TOOL, ProfileId, PromptCompileArguments, ToolCall,
    ToolDefinition, ToolResult,
};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};
use std::{collections::BTreeSet, sync::Arc};

#[derive(Debug, Clone)]
pub struct ImageMakerToolModule {
    tools: Vec<ModuleTool>,
    selected_image_tools: Vec<String>,
    promptgen: Option<Arc<PromptGenAdapter>>,
}

impl ImageMakerToolModule {
    pub fn new(
        selected: &BTreeSet<String>,
        promptgen: Option<Arc<PromptGenAdapter>>,
    ) -> Result<Self, String> {
        if selected.contains(PROMPTGEN_COMPILE_TOOL) && promptgen.is_none() {
            return Err("promptgen_compile requires a pinned promptGen adapter".to_owned());
        }
        let mut tools = Vec::new();
        for name in IMAGE_MAKER_TOOL_NAMES
            .iter()
            .copied()
            .filter(|name| selected.contains(*name))
        {
            tools.push(ModuleTool {
                definition: definition(name)?,
                side_effect: ToolSideEffect::ReadOnly,
                allowed_profiles: BTreeSet::from([ProfileId::creator()]),
                required_capabilities: if name == PROMPTGEN_COMPILE_TOOL {
                    BTreeSet::from([
                        CapabilityId::parse("artifact/read").map_err(|error| error.to_string())?,
                        CapabilityId::parse("artifact/write").map_err(|error| error.to_string())?,
                    ])
                } else {
                    BTreeSet::new()
                },
                activation_skills: BTreeSet::new(),
            });
        }
        if tools.is_empty() {
            return Err("no ImageMaker tool was explicitly selected".to_owned());
        }
        Ok(Self {
            tools,
            selected_image_tools: selected
                .iter()
                .filter(|name| IMAGE_TOOL_NAMES.contains(&name.as_str()))
                .cloned()
                .collect(),
            promptgen,
        })
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EmptyArguments {}
#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct PrepareArguments {
    contract_json: String,
}

#[async_trait]
impl ToolModule for ImageMakerToolModule {
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
                "ImageMaker tool was not explicitly selected".to_owned(),
            ));
        }
        let mut artifacts = Vec::new();
        let value: Value = match call.name.as_str() {
            IMAGE_CAPABILITIES_TOOL => {
                let _: EmptyArguments = serde_json::from_value(call.arguments.clone())
                    .map_err(|error| ToolModuleError::Contract(error.to_string()))?;
                json!({"selectedImageTools":self.selected_image_tools,"selectedImageMakerTools":self.tools.iter().map(|tool| &tool.definition.name).collect::<Vec<_>>(),"promptGenAdapterActive":self.promptgen.is_some(),"liveAvailability":"not_probed","deterministicEditSurface":"image_render.operations"})
            }
            IMAGEMAKER_PREPARE_TOOL => {
                let args: PrepareArguments = serde_json::from_value(call.arguments.clone())
                    .map_err(|error| ToolModuleError::Contract(error.to_string()))?;
                if args.contract_json.len() > MAX_COMPILE_INPUT_BYTES {
                    return Err(ToolModuleError::Contract(
                        "contract proposal exceeds its byte bound".to_owned(),
                    ));
                }
                let contract: ImageTaskContract = serde_json::from_str(&args.contract_json)
                    .map_err(|error| ToolModuleError::Contract(error.to_string()))?;
                contract.validate().map_err(ToolModuleError::Contract)?;
                let digest = contract.digest().map_err(ToolModuleError::Contract)?;
                json!({"status":"proposal","activeTaskChanged":false,"contract":contract,"contractDigest":digest})
            }
            PROMPTGEN_COMPILE_TOOL => {
                let adapter = self.promptgen.as_ref().ok_or_else(|| {
                    ToolModuleError::Contract("promptGen adapter is not configured".to_owned())
                })?;
                let args: PromptCompileArguments =
                    serde_json::from_value(call.arguments.clone())
                        .map_err(|error| ToolModuleError::Contract(error.to_string()))?;
                let input = args
                    .resolve(|reference| {
                        context
                            .artifacts
                            .read(reference)
                            .map_err(|error| error.to_string())
                    })
                    .map_err(ToolModuleError::Contract)?;
                let compiled = adapter
                    .compile_image(&input, context.timeout_ms, &context.cancellation)
                    .await
                    .map_err(ToolModuleError::Contract)?;
                let artifact = ImagePromptArtifact::new(
                    context.task_id,
                    adapter.sha256().to_owned(),
                    compiled,
                )
                .map_err(ToolModuleError::Contract)?;
                let value = json!({
                    "schema": agent_core::PROMPT_ARTIFACT_SCHEMA,
                    "status": "compiled",
                    "promptSha256": artifact.compiled.prompt_sha256,
                    "inputSha256": artifact.compiled.input_sha256,
                    "normalizedInputSha256": artifact.compiled.normalized_input_sha256,
                    "compilerOutcomeSha256": artifact.compiled.compiler_outcome_sha256,
                    "output": artifact.compiled.output,
                    "imageStateChanged": false
                });
                let bytes = serde_json::to_vec(&artifact)
                    .map_err(|error| ToolModuleError::Contract(error.to_string()))?;
                if bytes.len() > agent_core::MAX_PROMPT_ARTIFACT_BYTES {
                    return Err(ToolModuleError::Contract(
                        "compiled prompt artifact exceeds its byte bound".to_owned(),
                    ));
                }
                artifacts.push(ModuleArtifact {
                    bytes,
                    media_type: PROMPT_MEDIA_TYPE.to_owned(),
                    extension: "json".to_owned(),
                });
                value
            }
            _ => {
                return Err(ToolModuleError::Contract(
                    "unknown ImageMaker tool".to_owned(),
                ));
            }
        };
        Ok(ModuleExecution {
            result: ToolResult {
                call_id: call.id,
                name: call.name,
                value,
                is_error: false,
                failure: None,
            },
            receipts: Vec::new(),
            artifacts,
        })
    }
}

pub(crate) fn definitions() -> Result<Vec<ToolDefinition>, String> {
    IMAGE_MAKER_TOOL_NAMES
        .iter()
        .map(|name| definition(name))
        .collect()
}

fn definition(name: &str) -> Result<ToolDefinition, String> {
    let (description, schema) = match name {
        IMAGE_CAPABILITIES_TOOL => (
            "Inspect image tools explicitly selected by this composition; this does not activate tools or prove live provider availability.",
            json!({"type":"object","additionalProperties":false,"properties":{}}),
        ),
        IMAGEMAKER_PREPARE_TOOL => (
            "Validate an explicit image-task/5 contract and return a proposal with its digest. The active task is never rebound by this tool.",
            json!({"type":"object","additionalProperties":false,"required":["contractJson"],"properties":{"contractJson":{"type":"string","minLength":1,"maxLength":MAX_COMPILE_INPUT_BYTES}}}),
        ),
        PROMPTGEN_COMPILE_TOOL => (
            "Compile a generate-mode promptGen image request locally using the pinned compiler. Returns an immutable task/content-bound prompt artifact with explicit provider controls; no image provider is called. Reference/edit requests are rejected and must use their dedicated effect path.",
            json!({"type":"object","additionalProperties":false,"oneOf":[{"required":["inputJson"],"not":{"required":["inputArtifact"]}},{"required":["inputArtifact"],"not":{"required":["inputJson"]}}],"properties":{"inputJson":{"type":"string","minLength":1,"maxLength":MAX_COMPILE_INPUT_BYTES},"inputArtifact":super::image_tools::prompt_input_artifact_ref_schema()}}),
        ),
        _ => return Err("unrecognized canonical ImageMaker tool".to_owned()),
    };
    Ok(ToolDefinition {
        name: name.to_owned(),
        description: description.to_owned(),
        input_schema: schema,
        strict: true,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discovery_selection_does_not_activate_compile_or_semantic_edit() {
        let selected = BTreeSet::from([IMAGE_CAPABILITIES_TOOL.to_owned()]);
        let module = ImageMakerToolModule::new(&selected, None).expect("discovery tool");
        let tools = module.tools();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].definition.name, IMAGE_CAPABILITIES_TOOL);
        assert!(tools[0].activation_skills.is_empty());
        assert!(
            !tools
                .iter()
                .any(|tool| tool.definition.name == agent_core::IMAGE_EDIT_SEMANTIC_TOOL)
        );
        assert!(ImageMakerToolModule::new(&BTreeSet::new(), None).is_err());
        assert!(
            ImageMakerToolModule::new(&BTreeSet::from([PROMPTGEN_COMPILE_TOOL.to_owned()]), None)
                .is_err()
        );
    }

    #[test]
    fn compile_schema_enforces_exclusive_input_and_has_no_fake_activation_skill() {
        let compile = definition(PROMPTGEN_COMPILE_TOOL).expect("compile definition");
        assert_eq!(
            compile.input_schema["oneOf"].as_array().map(Vec::len),
            Some(2)
        );
        assert_eq!(compile.input_schema["additionalProperties"], false);
        assert!(
            definitions()
                .expect("definitions")
                .iter()
                .all(|definition| !agent_core::is_external_image_tool_name(&definition.name))
        );
    }
}
