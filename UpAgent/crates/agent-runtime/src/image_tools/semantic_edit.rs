//! Optional external editing mechanism. Admission/provenance/revision authority
//! remains in `SessionMachine`. No result can promote itself to an image candidate.
use super::{
    AgentFailure, ArtifactDependencyV2, ArtifactRef, BTreeSet, CapabilityId, Deserialize,
    DeterminismClassV2, Duration, EffectReceiptRef, FailureCode, IMAGE_EDIT_SEMANTIC_TOOL,
    IMAGE_TOOL_SCHEMA, ImageToolModule, MAX_IMAGE_BYTES, ModuleArtifact, ModuleExecution,
    ModuleTool, OperationReceiptV2, OperationStatusV2, Path, ProfileId, ReceiptId, TempDir,
    ToolCall, ToolContext, ToolDefinition, ToolModuleError, ToolResult, ToolSideEffect,
    UPAGENT_IMAGE_RECEIPT_SCHEMA, Value, artifact_dependency_closure_sha256, artifact_ref_schema,
    fs, json, read_image_artifact, sha256_hex, strict_arguments, validate_png,
};
use crate::promptgen::read_regular_bounded;
use agent_contract::compilation::{CompilationOutcome, PromptKind};

const MAX_SEMANTIC_RECEIPT_BYTES: usize = 256 * 1024;
const SEMANTIC_BACKEND: &str = "codex-imagegen";
const SEMANTIC_BACKEND_PROFILE: &str = "codex-subscription";
const MAX_SEMANTIC_RASTER_EDGE: u64 = 8192;
const MAX_SEMANTIC_ERROR_CHARS: usize = 2048;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct SemanticEditArguments {
    artifact: ArtifactRef,
    candidate_revision: u32,
    input_json: String,
}

#[cfg(test)]
use agent_contract::receipt::FidelityCheck as SemanticFidelityCheck;
use agent_contract::receipt::SemanticEditReceipt;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SemanticEditResult {
    compilation: CompilationOutcome,
    execution: SemanticEditReceipt,
}

pub(super) fn module_tool() -> Result<ModuleTool, String> {
    Ok(ModuleTool {
        definition: ToolDefinition {
            name: IMAGE_EDIT_SEMANTIC_TOOL.to_owned(),
            description: "Edit a provenance-authorized PNG through the pinned promptGen OpenAI adapter. Requires current candidateRevision and explicit external-effect approval. Unknown post-dispatch results require manual resolution; never automatically retry.".to_owned(),
            input_schema: json!({"type":"object","additionalProperties":false,"required":["artifact","candidateRevision","inputJson"],"properties":{
                "artifact":artifact_ref_schema(),
                "candidateRevision":{"type":"integer","minimum":0,"maximum":u32::MAX},
                "inputJson":{"type":"string","minLength":1,"maxLength":agent_core::MAX_COMPILE_INPUT_BYTES}
            }}),
            strict: true,
        },
        side_effect: ToolSideEffect::External,
        allowed_profiles: BTreeSet::from([ProfileId::creator()]),
        required_capabilities: ["artifact/read", "artifact/write", "model/image"].into_iter().map(|name| CapabilityId::parse(name).map_err(|error| error.to_string())).collect::<Result<BTreeSet<_>, _>>()?,
        activation_skills: BTreeSet::new(),
    })
}

impl ImageToolModule {
    pub(super) async fn semantic_edit(
        &self,
        context: ToolContext,
        call: &ToolCall,
    ) -> Result<ModuleExecution, ToolModuleError> {
        let adapter = self.promptgen.as_ref().ok_or_else(|| {
            ToolModuleError::Contract("semantic editing was not configured".to_owned())
        })?;
        let codex_sha256 = adapter.codex_sha256().ok_or_else(|| {
            ToolModuleError::Contract(
                "semantic editing requires a pinned PromptGen Codex runtime".to_owned(),
            )
        })?;
        let args: SemanticEditArguments =
            strict_arguments(call).map_err(|error| preflight_error(&error.to_string()))?;
        let source = read_image_artifact(&context, &args.artifact)
            .map_err(|error| preflight_error(&error.to_string()))?;
        let (source_width, source_height) =
            validate_png(&source).map_err(|error| preflight_error(&error.to_string()))?;
        self.validate_generated_image(
            &source,
            "image/png",
            (source_width, source_height),
            Duration::from_millis(context.timeout_ms),
            &context.cancellation,
        )
        .await
        .map_err(|error| preflight_error(&error.to_string()))?;
        let (width, height, detail) =
            validate_edit_input(&args.input_json).map_err(|error| preflight_error(&error))?;
        // Check both local mechanisms before entering the remote effect boundary.
        let work = TempDir::new().map_err(|error| ToolModuleError::Contract(error.to_string()))?;
        let work_path = work
            .path()
            .canonicalize()
            .map_err(|error| preflight_error(&error.to_string()))?;
        let input_path = work_path.join("request.json");
        let source_path = work_path.join("source.png");
        let output_path = work_path.join("edited.png");
        let receipt_path = work_path.join("receipt.json");
        fs::write(&source_path, &source)
            .map_err(|error| ToolModuleError::Contract(error.to_string()))?;
        let prompt = adapter
            .preflight_edit(
                &args.input_json,
                &input_path,
                context.timeout_ms,
                &context.cancellation,
            )
            .await
            .map_err(|error| preflight_error(&error))?;
        let invocation = adapter
            .dispatch_edit(
                crate::promptgen::EditPaths {
                    input: &input_path,
                    source: &source_path,
                    output: &output_path,
                    receipt: &receipt_path,
                },
                context.timeout_ms,
                &context.cancellation,
            )
            .await;
        // A spawn/preflight failure is clean. After spawn, even a child error can
        // mean the provider completed the paid effect; preserve both files.
        if let Err(error) = &invocation
            && !error.dispatched
        {
            return Err(preflight_error(&error.message));
        }
        let output = read_regular_bounded(&output_path, MAX_IMAGE_BYTES);
        let receipt_bytes = read_regular_bounded(&receipt_path, MAX_SEMANTIC_RECEIPT_BYTES);
        let complete = async {
            invocation.map_err(|error| error.message)?;
            let bytes = output.as_ref().map_err(Clone::clone)?;
            let receipt_bytes = receipt_bytes.as_ref().map_err(Clone::clone)?;
            let receipt = parse_semantic_result(receipt_bytes, &prompt)?;
            validate_receipt(
                &receipt,
                &SemanticEditReceiptBinding {
                    source_ref: &args.artifact,
                    source: &source,
                    output: bytes,
                    output_path: &output_path,
                    prompt: &prompt,
                    detail: &detail,
                    codex_sha256,
                    size: (width, height),
                },
            )?;
            if context.cancellation.is_cancelled() {
                return Err("semantic edit cancelled after dispatch".to_owned());
            }
            let observed = self
                .validate_generated_image(
                    bytes,
                    "image/png",
                    (width, height),
                    Duration::from_millis(context.timeout_ms),
                    &context.cancellation,
                )
                .await
                .map_err(|error| error.to_string())?;
            if (observed.width, observed.height) != (width, height) {
                return Err("semantic edit raster dimensions disagree with the request".to_owned());
            }
            Ok::<SemanticEditReceipt, String>(receipt)
        }
        .await;
        match complete {
            Ok(receipt) => {
                if context.cancellation.is_cancelled() {
                    return uncertain_execution(
                        call,
                        "semantic edit cancelled before candidate publication",
                        output,
                        receipt_bytes,
                    );
                }
                let bytes = output.map_err(|message| post_dispatch_error(&message))?;
                let evidence = receipt_bytes.map_err(|message| post_dispatch_error(&message))?;
                let dependencies = vec![ArtifactDependencyV2::from_ref(&args.artifact)];
                let operation_receipt = OperationReceiptV2 {
                    schema: UPAGENT_IMAGE_RECEIPT_SCHEMA.to_owned(),
                    request_id: format!("semantic-{}", call.id),
                    operation: "semantic_edit".to_owned(),
                    status: OperationStatusV2::Committed,
                    behavior_version: self.behavior_version.clone(),
                    implementation_version: env!("CARGO_PKG_VERSION").to_owned(),
                    request_sha256: sha256_hex(
                        &serde_json::to_vec(&call.arguments)
                            .map_err(|error| post_dispatch_error(&error.to_string()))?,
                    ),
                    dependency_closure_sha256: artifact_dependency_closure_sha256(&dependencies)
                        .map_err(|error| post_dispatch_error(&error))?,
                    dependencies,
                    output_sha256: Some(sha256_hex(&bytes)),
                    determinism: DeterminismClassV2::InferenceObserved,
                };
                let effect_receipt = semantic_effect_receipt(call, &evidence, true)?;
                Ok(ModuleExecution {
                    result: ToolResult {
                        call_id: call.id.clone(),
                        name: call.name.clone(),
                        value: json!({
                        "schema":IMAGE_TOOL_SCHEMA,"operation":"semantic_edit","mediaType":"image/png","source":"pinned_promptgen","sourceArtifact":args.artifact,
                        "admittedCandidateRevision":args.candidate_revision,"width":width,"height":height,"contentSha256":receipt.sha256,
                        "backend":receipt.backend_profile,"codexVersion":receipt.codex_version,
                        "perfectpixelVerified":true,"visualQualified":false,"artifactKind":"working_raster","artifactRoles":["render","receipt"],
                            "pixelSpec":{"format":"rgba8","colorSpace":"srgb","alphaMode":"straight"},"receipt":operation_receipt,
                            "providerReceiptSha256":sha256_hex(&evidence),"compilerSha256":adapter.sha256()
                        }),
                        is_error: false,
                        failure: None,
                    },
                    receipts: vec![effect_receipt],
                    artifacts: vec![
                        ModuleArtifact {
                            bytes,
                            media_type: "image/png".to_owned(),
                            extension: "png".to_owned(),
                        },
                        ModuleArtifact {
                            bytes: evidence,
                            media_type: "application/vnd.upagent.semantic-edit-receipt+json"
                                .to_owned(),
                            extension: "json".to_owned(),
                        },
                    ],
                })
            }
            Err(message) => uncertain_execution(call, &message, output, receipt_bytes),
        }
    }
}

fn parse_semantic_result(bytes: &[u8], prompt: &str) -> Result<SemanticEditReceipt, String> {
    let result: SemanticEditResult = serde_json::from_slice(bytes)
        .map_err(|error| format!("semantic edit result is invalid: {error}"))?;
    result
        .compilation
        .validate()
        .map_err(|error| format!("semantic edit compilation is invalid: {error}"))?;
    if result.compilation.kind != PromptKind::Image
        || result.compilation.status.successful().is_none()
        || result.compilation.prompt.as_deref() != Some(prompt)
    {
        return Err(
            "semantic edit compilation does not bind the valid preflight image prompt".to_owned(),
        );
    }
    Ok(result.execution)
}

fn validate_edit_input(input: &str) -> Result<(u32, u32, String), String> {
    if input.trim().is_empty()
        || input.len() > agent_core::MAX_COMPILE_INPUT_BYTES
        || input.contains('\0')
    {
        return Err("semantic edit input must be non-empty and bounded".to_owned());
    }
    let request: Value = serde_json::from_str(input).map_err(|error| error.to_string())?;
    let references = request
        .get("references")
        .and_then(Value::as_array)
        .ok_or("semantic edit requires one base reference")?;
    if request.get("task_mode").and_then(Value::as_str) != Some("edit")
        || references.len() != 1
        || references[0].get("index").and_then(Value::as_u64) != Some(1)
        || references[0].get("role").and_then(Value::as_str) != Some("base")
    {
        return Err(
            "semantic edit requires task_mode=edit and exactly one index=1, role=base reference"
                .to_owned(),
        );
    }
    let output = request
        .get("output")
        .ok_or("semantic edit requires an output contract")?;
    if output.get("backend").and_then(Value::as_str) != Some(SEMANTIC_BACKEND_PROFILE)
        || output.get("format").and_then(Value::as_str) != Some("png")
    {
        return Err(
            "semantic edit supports the pinned Codex subscription PNG contract only".to_owned(),
        );
    }
    let detail = output
        .get("detail")
        .and_then(Value::as_str)
        .filter(|value| matches!(*value, "auto" | "low" | "medium" | "high"))
        .ok_or("semantic edit detail is missing or unsupported")?
        .to_owned();
    let width = output
        .get("width")
        .and_then(Value::as_u64)
        .ok_or("semantic edit width is missing")?;
    let height = output
        .get("height")
        .and_then(Value::as_u64)
        .ok_or("semantic edit height is missing")?;
    if !(1..=MAX_SEMANTIC_RASTER_EDGE).contains(&width)
        || !(1..=MAX_SEMANTIC_RASTER_EDGE).contains(&height)
    {
        return Err("semantic edit raster dimensions exceed the canonical bound".to_owned());
    }
    Ok((
        u32::try_from(width).map_err(|error| error.to_string())?,
        u32::try_from(height).map_err(|error| error.to_string())?,
        detail,
    ))
}

struct SemanticEditReceiptBinding<'a> {
    source_ref: &'a ArtifactRef,
    source: &'a [u8],
    output: &'a [u8],
    output_path: &'a Path,
    prompt: &'a str,
    detail: &'a str,
    codex_sha256: &'a str,
    size: (u32, u32),
}

fn validate_receipt(
    receipt: &SemanticEditReceipt,
    binding: &SemanticEditReceiptBinding<'_>,
) -> Result<(), String> {
    if receipt.backend != SEMANTIC_BACKEND
        || receipt.authentication != "chatgpt-subscription"
        || receipt.task_mode != "edit"
        || receipt.backend_profile != SEMANTIC_BACKEND_PROFILE
        || receipt.requested_detail != binding.detail
        || receipt.codex_binary_sha256.as_deref() != Some(binding.codex_sha256)
        || receipt.observed_image_model.is_some()
        || receipt.process_exit_code != 0
    {
        return Err("semantic receipt backend/authentication/task/detail mismatch".to_owned());
    }
    if receipt.compiled_prompt_sha256 != sha256_hex(binding.prompt.as_bytes())
        || receipt.compiled_prompt_chars != binding.prompt.chars().count() as u64
        || !valid_sha256(&receipt.executed_prompt_sha256)
        || receipt.executed_prompt_chars == 0
    {
        return Err("semantic receipt does not bind the preflight prompt".to_owned());
    }
    if receipt.reference_sha256.as_deref() != Some(binding.source_ref.sha256.as_str())
        || receipt.reference_sha256.as_deref() != Some(sha256_hex(binding.source).as_str())
        || receipt.reference_bytes != Some(binding.source.len() as u64)
        || receipt.reference_bytes != Some(binding.source_ref.byte_length)
    {
        return Err("semantic receipt source role/content binding mismatch".to_owned());
    }
    if receipt.sha256 != sha256_hex(binding.output)
        || receipt.output_artifact != binding.output_path
        || (receipt.width, receipt.height) != binding.size
        || !valid_sha256(&receipt.sha256)
        || receipt.fidelity_checks.is_empty()
        || receipt
            .fidelity_checks
            .last()
            .is_none_or(|check| !check.pass || check.candidate_sha256 != receipt.sha256)
        || receipt.source_width == 0
        || receipt.source_height == 0
    {
        return Err("semantic receipt output content/path/raster binding mismatch".to_owned());
    }
    if receipt.started_unix_ms == 0
        || receipt.finished_unix_ms < receipt.started_unix_ms
        || receipt.codex_version.is_empty()
        || receipt.event_contract.is_empty()
        || receipt.thread_id.is_empty()
        || receipt.image_call_id.is_empty()
        || receipt.agent_model.is_empty()
        || receipt.promptgen_version.is_empty()
        || receipt.fidelity_checks.iter().any(|check| {
            check.summary.is_empty()
                || check.violations.len() > 64
                || check.repair_instruction.len() > 16 * 1024
                || check.subject_counts.len() > 64
                || check.text_checks.len() > 64
                || check.text_separator_checks.len() > 64
        })
    {
        return Err("semantic receipt time ordering is invalid".to_owned());
    }
    Ok(())
}

fn valid_sha256(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn semantic_effect_receipt(
    call: &ToolCall,
    bytes: &[u8],
    verified: bool,
) -> Result<EffectReceiptRef, ToolModuleError> {
    let digest = sha256_hex(bytes);
    let id = ReceiptId::parse(format!("receipt-{digest}"))
        .map_err(|error| post_dispatch_error(&error.to_string()))?;
    Ok(EffectReceiptRef {
        id,
        digest,
        operation: format!("{IMAGE_EDIT_SEMANTIC_TOOL}:{}", call.id),
        verified,
    })
}
fn bounded_error(message: &str) -> String {
    if message.chars().count() <= MAX_SEMANTIC_ERROR_CHARS {
        return message.to_owned();
    }
    format!(
        "{} [truncated; fullErrorSha256={}]",
        message
            .chars()
            .take(MAX_SEMANTIC_ERROR_CHARS)
            .collect::<String>(),
        sha256_hex(message.as_bytes())
    )
}
fn preflight_error(message: &str) -> ToolModuleError {
    ToolModuleError::Failure(
        AgentFailure::new(
            FailureCode::InvalidRequest,
            "semantic edit failed before external dispatch",
        )
        .with_detail("cause", bounded_error(message)),
    )
}
fn post_dispatch_error(message: &str) -> ToolModuleError {
    ToolModuleError::Cancelled(
        AgentFailure::new(
            FailureCode::ResolutionRequired,
            "semantic edit outcome is uncertain after external dispatch",
        )
        .with_detail("cause", bounded_error(message)),
    )
}
fn uncertain_execution(
    call: &ToolCall,
    message: &str,
    output: Result<Vec<u8>, String>,
    receipt: Result<Vec<u8>, String>,
) -> Result<ModuleExecution, ToolModuleError> {
    let mut artifacts = Vec::new();
    // Collect independently. A receipt-only or output-only result is still
    // useful evidence, but opaque bytes must never become image candidates.
    let mut collection = Vec::new();
    for (role, evidence) in [("output", output), ("receipt", receipt)] {
        match evidence {
            Ok(bytes) => {
                collection
                    .push(json!({"role":role,"sha256":sha256_hex(&bytes),"bytes":bytes.len()}));
                artifacts.push(ModuleArtifact {
                    bytes,
                    media_type: "application/octet-stream".to_owned(),
                    extension: "bin".to_owned(),
                });
            }
            Err(error) => collection.push(json!({"role":role,"collectionError":error})),
        }
    }
    let value = json!({"schema":IMAGE_TOOL_SCHEMA,"operation":"semantic_edit","status":"resolution_required","automaticRetryAllowed":false,"evidence":collection});
    let bytes =
        serde_json::to_vec(&value).map_err(|error| post_dispatch_error(&error.to_string()))?;
    let effect_receipt = semantic_effect_receipt(call, &bytes, false)?;
    let failure = AgentFailure::new(
        FailureCode::ResolutionRequired,
        "semantic edit needs manual resolution; automatic re-dispatch is forbidden",
    )
    .with_detail("cause", bounded_error(message));
    artifacts.push(ModuleArtifact {
        bytes,
        media_type: "application/vnd.upagent.semantic-edit-evidence+json".to_owned(),
        extension: "json".to_owned(),
    });
    Ok(ModuleExecution {
        result: ToolResult {
            call_id: call.id.clone(),
            name: call.name.clone(),
            value,
            is_error: true,
            failure: Some(failure),
        },
        receipts: vec![effect_receipt],
        artifacts,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn fixture_call() -> ToolCall {
        ToolCall {
            id: agent_core::ToolCallId::parse("semantic-test").expect("test ID"),
            name: IMAGE_EDIT_SEMANTIC_TOOL.to_owned(),
            arguments: json!({}),
        }
    }

    fn fixture_receipt() -> (SemanticEditReceipt, ArtifactRef) {
        let source_sha = sha256_hex(b"source");
        let reference = ArtifactRef {
            id: agent_core::ArtifactId::parse(format!("art-{source_sha}")).expect("artifact ID"),
            sha256: source_sha.clone(),
            media_type: "image/png".to_owned(),
            byte_length: 6,
            relative_path: format!("{}/{}.png", &source_sha[..2], source_sha),
        };
        let output_sha = sha256_hex(b"output");
        let receipt = SemanticEditReceipt {
            backend: SEMANTIC_BACKEND.to_owned(),
            authentication: "chatgpt-subscription".to_owned(),
            promptgen_version: "4.1.0".to_owned(),
            agent_model: "gpt-5.6-luna".to_owned(),
            task_mode: "edit".to_owned(),
            backend_profile: SEMANTIC_BACKEND_PROFILE.to_owned(),
            requested_detail: "high".to_owned(),
            observed_image_model: None,
            reference_sha256: Some(source_sha),
            reference_bytes: Some(6),
            codex_version: "codex 0.150.1".to_owned(),
            codex_binary_sha256: Some("c".repeat(64)),
            compiled_prompt_chars: 2,
            compiled_prompt_sha256: sha256_hex("편집".as_bytes()),
            executed_prompt_chars: 2,
            executed_prompt_sha256: sha256_hex("편집".as_bytes()),
            control_notes: vec!["test evidence".to_owned()],
            event_contract: "codex-events/1".to_owned(),
            dimensions_normalized: false,
            fidelity_checks: vec![SemanticFidelityCheck {
                candidate_sha256: output_sha.clone(),
                pass: true,
                summary: "all checks passed".to_owned(),
                violations: Vec::new(),
                repair_instruction: String::new(),
                subject_counts: Vec::new(),
                text_checks: Vec::new(),
                text_separator_checks: Vec::new(),
            }],
            finished_unix_ms: 101,
            width: 1024,
            height: 1024,
            image_call_id: "image-call".to_owned(),
            output_artifact: PathBuf::from("/private/edited.png"),
            process_exit_code: 0,
            prompt_refinement: serde_json::from_value(json!({"provider":"codex-cli", "model":"gpt-5.6-luna", "review_summary":"approved", "additions":[], "source_prompt_sha256":sha256_hex("편집".as_bytes()), "refined_prompt_sha256":sha256_hex("편집".as_bytes()), "source_prompt_chars":2, "refined_prompt_chars":2, "prompt":"편집"})).expect("refinement evidence"),
            sha256: output_sha,
            source_artifact: PathBuf::from("/private/candidate.png"),
            source_width: 1024,
            source_height: 1024,
            started_unix_ms: 100,
            thread_id: "thread-test".to_owned(),
        };
        (receipt, reference)
    }

    fn fixture_result() -> agent_contract::json::JsonValue {
        use agent_contract::json::JsonValue;

        let compilation = CompilationOutcome::new(
            PromptKind::Image,
            Some("편집".to_owned()),
            Vec::new(),
            JsonValue::object([] as [(&str, JsonValue); 0]),
        );
        // Match the canonical promptGen CLI serializer, including receipt nesting.
        JsonValue::object([
            ("compilation", compilation.to_json()),
            ("execution", fixture_receipt().0.to_json()),
        ])
    }

    #[test]
    fn canonical_cli_result_wraps_compilation_and_execution() {
        let bytes = fixture_result().to_compact_string();
        let receipt = parse_semantic_result(bytes.as_bytes(), "편집")
            .expect("canonical promptGen CLI result");
        assert_eq!(receipt, fixture_receipt().0);
    }

    #[test]
    fn semantic_result_requires_the_strict_canonical_wrapper() {
        let valid: Value =
            serde_json::from_str(&fixture_result().to_compact_string()).expect("canonical JSON");
        let mut invalid = vec![valid["execution"].clone()];
        for field in ["compilation", "execution"] {
            let mut value = valid.clone();
            value.as_object_mut().expect("wrapper object").remove(field);
            invalid.push(value);
        }
        let mut value = valid.clone();
        value["legacy"] = json!(true);
        invalid.push(value);
        let mut value = valid.clone();
        value["compilation"]["legacy"] = json!(true);
        invalid.push(value);
        let mut value = valid;
        value["execution"]["output_bytes"] = json!(6);
        invalid.push(value);
        for value in invalid {
            assert!(
                parse_semantic_result(&serde_json::to_vec(&value).expect("JSON"), "편집").is_err()
            );
        }
    }

    #[test]
    fn semantic_result_checks_compilation_diagnostics_and_preflight_prompt() {
        let valid: Value =
            serde_json::from_str(&fixture_result().to_compact_string()).expect("canonical JSON");
        let verify = |value: &Value| {
            parse_semantic_result(&serde_json::to_vec(value).expect("JSON"), "편집")
        };
        let warning =
            json!({"severity":"warning","code":"test","path":"$","message":"test warning"});
        let error = json!({"severity":"error","code":"test","path":"$","message":"test error"});
        let mut value = valid.clone();
        value["compilation"]["status"] = json!("valid_with_warnings");
        value["compilation"]["diagnostics"] = json!([warning]);
        assert!(verify(&value).is_ok());
        // A successful status cannot hide diagnostic errors or warnings.
        for diagnostic in [warning, error.clone()] {
            let mut value = valid.clone();
            value["compilation"]["diagnostics"] = json!([diagnostic]);
            assert!(verify(&value).is_err());
        }
        let mut value = valid.clone();
        value["compilation"]["status"] = json!("invalid");
        value["compilation"]["prompt"] = Value::Null;
        value["compilation"]["diagnostics"] = json!([error]);
        assert!(verify(&value).is_err());
        for (field, replacement) in [
            ("status", json!("valid_with_warnings")),
            ("prompt", json!("다른 편집")),
            ("prompt", Value::Null),
            ("kind", json!("text")),
            ("metadata", Value::Null),
        ] {
            let mut value = valid.clone();
            value["compilation"][field] = replacement;
            assert!(verify(&value).is_err());
        }
    }

    #[test]
    fn invalid_result_preserves_raw_post_dispatch_evidence() {
        let bytes = fixture_result().to_compact_string().into_bytes();
        let error = parse_semantic_result(&bytes, "다른 편집").expect_err("changed prompt");
        let execution = uncertain_execution(
            &fixture_call(),
            &error,
            Ok(b"output".to_vec()),
            Ok(bytes.clone()),
        )
        .expect("uncertain result");
        assert!(execution.result.is_error);
        assert_eq!(execution.result.value["automaticRetryAllowed"], false);
        assert_eq!(execution.artifacts.len(), 3);
        assert_eq!(execution.artifacts[0].bytes, b"output");
        assert_eq!(execution.artifacts[1].bytes, bytes);
        assert!(
            execution
                .artifacts
                .iter()
                .all(|artifact| artifact.media_type != "image/png")
        );
        assert!(execution.receipts.iter().all(|receipt| !receipt.verified));
    }

    #[test]
    fn receipt_binds_actual_provider_prompt_source_and_output_contract() {
        let (valid, source) = fixture_receipt();
        let pinned_codex_sha256 = "c".repeat(64);
        let verify = |receipt: &SemanticEditReceipt| {
            validate_receipt(
                receipt,
                &SemanticEditReceiptBinding {
                    source_ref: &source,
                    source: b"source",
                    output: b"output",
                    output_path: Path::new("/private/edited.png"),
                    prompt: "편집",
                    detail: "high",
                    codex_sha256: &pinned_codex_sha256,
                    size: (1024, 1024),
                },
            )
        };
        assert!(verify(&valid).is_ok());
        let mut value = valid.clone();
        value.backend_profile = "other".to_owned();
        assert!(verify(&value).is_err());
        let mut value = valid.clone();
        value.requested_detail = "low".to_owned();
        assert!(verify(&value).is_err());
        let mut value = valid.clone();
        value.codex_binary_sha256 = Some("0".repeat(64));
        assert!(verify(&value).is_err());
        let mut value = valid.clone();
        value.reference_bytes = Some(7);
        assert!(verify(&value).is_err());
        let mut value = valid.clone();
        value.compiled_prompt_chars = 6;
        assert!(verify(&value).is_err());
        let mut value = valid.clone();
        value.sha256 = "0".repeat(64);
        assert!(verify(&value).is_err());
        let mut value = valid.clone();
        value.output_artifact = PathBuf::from("/other.png");
        assert!(verify(&value).is_err());
        let mut value = valid.clone();
        value.fidelity_checks[0].pass = false;
        assert!(verify(&value).is_err());
        let mut value = valid.clone();
        value.finished_unix_ms = 99;
        assert!(verify(&value).is_err());
        // No fictitious output_bytes field is accepted as production evidence.
        let mut json = serde_json::to_value(&valid).expect("receipt JSON");
        json["output_bytes"] = json!(6);
        assert!(serde_json::from_value::<SemanticEditReceipt>(json).is_err());
    }

    #[test]
    fn partial_output_or_receipt_stays_opaque_and_unverified() {
        for (output, receipt) in [
            (Ok(b"output".to_vec()), Err("missing receipt".to_owned())),
            (Err("missing output".to_owned()), Ok(b"receipt".to_vec())),
        ] {
            let execution = uncertain_execution(&fixture_call(), "uncertain", output, receipt)
                .expect("evidence result");
            assert!(execution.result.is_error);
            assert_eq!(
                execution
                    .result
                    .failure
                    .as_ref()
                    .map(|failure| failure.code),
                Some(FailureCode::ResolutionRequired)
            );
            assert_eq!(execution.result.value["automaticRetryAllowed"], false);
            assert_eq!(execution.artifacts.len(), 2);
            assert!(
                execution
                    .artifacts
                    .iter()
                    .all(|artifact| artifact.media_type != "image/png")
            );
            assert!(execution.receipts.iter().all(|receipt| !receipt.verified));
        }
    }

    #[test]
    fn semantic_input_rejects_non_edit_foreign_roles_and_missing_raster() {
        let input = json!({"task_mode":"edit","references":[{"index":1,"role":"base"}],"output":{"backend":SEMANTIC_BACKEND_PROFILE,"detail":"high","format":"png","width":1024,"height":1024}});
        assert_eq!(
            validate_edit_input(&input.to_string()).expect("host preflight"),
            (1024, 1024, "high".to_owned())
        );
        let mut value = input.clone();
        value["task_mode"] = json!("generate");
        assert!(validate_edit_input(&value.to_string()).is_err());
        let mut value = input.clone();
        value["references"][0]["role"] = json!("style");
        assert!(validate_edit_input(&value.to_string()).is_err());
        let mut value = input;
        value["output"]["width"] = json!(0);
        assert!(validate_edit_input(&value.to_string()).is_err());
        // Full promptGen schema validation is a separate pinned local preflight.
    }
}
