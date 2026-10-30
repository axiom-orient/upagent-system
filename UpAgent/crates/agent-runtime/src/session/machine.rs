use std::collections::{BTreeMap, BTreeSet};

use agent_core::{
    AgentFailure, AgentResult, AgentStatus, ArtifactEvidence, ArtifactId, ArtifactRef, ArtifactUse,
    EffectReceiptRef, FailureCode, IMAGE_VERIFICATION_SPEC_SCHEMA, ImageCompareArtifactKind,
    ImageCompareAssertion, ImageCompareAssertionResult, ImageDirectorState, ImageRepairObservation,
    ImageRepairRequirement, ImageVerificationSpec, KernelEvent, SemanticMaskApply,
    SemanticMaskResult, SemanticMaskResultStatus, TaskBudget, TaskEnvelope, TaskState, ToolCall,
    ToolCallId, ToolResult, event_name, transition,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::journal::{JournalError, SessionJournal};

// The task budget permits 256 tool calls and an extension may return 16
// artifacts per call. Leave room for model turns, checkpoints, and terminal
// artifacts without rejecting a valid bounded task before its result commits.
const MAX_LEDGER_ENTRIES: usize = 8_192;
const MAX_USES_PER_ARTIFACT: usize = 256;

/// The single durable owner of artifact state for one session.  The result
/// list and review evidence are projections of this map; callers must not
/// maintain a second artifact collection.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(super) struct ArtifactLedger {
    entries: BTreeMap<ArtifactId, ArtifactLedgerEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct ArtifactLedgerEntry {
    artifact: ArtifactRef,
    uses: Vec<ArtifactUse>,
}

type TypedArtifact = (ArtifactRef, Option<ToolCallId>, Option<u32>);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct PendingImageComparison {
    candidate_revision: u32,
    after_artifact: ArtifactRef,
    after_kind: ImageCompareArtifactKind,
    verification_spec_digest: String,
}

/// An in-flight external call identity, not a second image state owner.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct PendingSemanticEdit {
    contract_digest: String,
    candidate_revision: u32,
    arguments_sha256: String,
}

impl ArtifactLedger {
    fn register(&mut self, artifact: ArtifactRef, use_record: ArtifactUse) -> Result<bool, String> {
        use_record.validate()?;
        if artifact.id.as_str().is_empty()
            || artifact.sha256.len() != 64
            || artifact.byte_length == 0
            || artifact.relative_path.is_empty()
        {
            return Err("artifact ledger received an invalid artifact reference".to_owned());
        }
        let artifact_id = artifact.id.clone();
        let entry = if let Some(entry) = self.entries.get_mut(&artifact_id) {
            if entry.artifact != artifact {
                return Err("one artifact id was bound to different artifact metadata".to_owned());
            }
            entry
        } else {
            if self.entries.len() >= MAX_LEDGER_ENTRIES {
                return Err("artifact ledger exceeded its entry bound".to_owned());
            }
            self.entries.insert(
                artifact_id.clone(),
                ArtifactLedgerEntry {
                    artifact,
                    uses: Vec::new(),
                },
            );
            self.entries
                .get_mut(&artifact_id)
                .ok_or_else(|| "artifact ledger insertion disappeared".to_owned())?
        };
        if entry.uses.iter().any(|existing| existing == &use_record) {
            return Ok(false);
        }
        if entry.uses.len() >= MAX_USES_PER_ARTIFACT {
            return Err("artifact ledger exceeded the uses bound".to_owned());
        }
        entry.uses.push(use_record);
        entry.uses.sort_by(|left, right| {
            left.sequence
                .cmp(&right.sequence)
                .then(left.event.cmp(&right.event))
        });
        Ok(true)
    }

    fn artifacts(&self) -> Vec<ArtifactRef> {
        self.entries
            .values()
            .map(|entry| entry.artifact.clone())
            .collect()
    }

    fn evidence(&self) -> Vec<ArtifactEvidence> {
        self.entries
            .values()
            .map(|entry| ArtifactEvidence {
                artifact: entry.artifact.clone(),
                uses: entry.uses.clone(),
            })
            .collect()
    }
}

fn typed_event_artifacts(
    event: &KernelEvent,
    payload: &Value,
) -> Result<Vec<TypedArtifact>, String> {
    let mut output = Vec::new();
    let add = |output: &mut Vec<TypedArtifact>,
               payload: &Value,
               key: &str,
               call_id: Option<ToolCallId>,
               call_index: Option<u32>| {
        if let Some(value) = payload.get(key)
            && !value.is_null()
        {
            let reference: ArtifactRef = serde_json::from_value(value.clone())
                .map_err(|error| format!("{key} is not an artifact reference: {error}"))?;
            output.push((reference, call_id, call_index));
        }
        Ok::<(), String>(())
    };
    let add_required = |output: &mut Vec<TypedArtifact>,
                        payload: &Value,
                        key: &str,
                        call_id: Option<ToolCallId>,
                        call_index: Option<u32>| {
        if payload.get(key).is_none_or(Value::is_null) {
            return Err(format!("{key} is required for this artifact event"));
        }
        add(output, payload, key, call_id, call_index)
    };
    match event {
        KernelEvent::ModelRequested | KernelEvent::ModelCompleted => {
            let model_call = payload
                .get("modelCall")
                .and_then(Value::as_u64)
                .ok_or_else(|| "model artifact event is missing modelCall".to_owned())?;
            let model_call = u32::try_from(model_call)
                .map_err(|_| "model artifact event modelCall exceeds u32".to_owned())?;
            if model_call == 0 {
                return Err("model artifact event modelCall must be positive".to_owned());
            }
            let key = if matches!(event, KernelEvent::ModelRequested) {
                "requestArtifact"
            } else {
                "responseArtifact"
            };
            add_required(&mut output, payload, key, None, Some(model_call))?;
        }
        KernelEvent::Suspended | KernelEvent::Resumed => {
            add_required(&mut output, payload, "checkpoint", None, None)?;
        }
        KernelEvent::ToolCompleted => {
            let call_id = payload
                .get("callId")
                .and_then(Value::as_str)
                .ok_or_else(|| "tool completion event is missing callId".to_owned())
                .and_then(|value| {
                    ToolCallId::parse(value.to_owned())
                        .map_err(|error| format!("tool completion callId is invalid: {error}"))
                })?;
            let values = payload
                .get("artifacts")
                .ok_or_else(|| "tool completion event is missing artifacts".to_owned())?;
            let references: Vec<ArtifactRef> = serde_json::from_value(values.clone())
                .map_err(|error| format!("tool completion artifacts are invalid: {error}"))?;
            for reference in references {
                output.push((reference, Some(call_id.clone()), None));
            }
        }
        KernelEvent::ResolutionRequired(_) => {
            if let Some(values) = payload.get("artifacts") {
                let call_id = payload
                    .get("callId")
                    .and_then(Value::as_str)
                    .ok_or("resolution evidence is missing callId")?;
                let call_id =
                    ToolCallId::parse(call_id.to_owned()).map_err(|error| error.to_string())?;
                let references: Vec<ArtifactRef> = serde_json::from_value(values.clone())
                    .map_err(|error| format!("resolution artifacts are invalid: {error}"))?;
                if references.len() > 16 {
                    return Err("resolution artifact count exceeds the module bound".to_owned());
                }
                for reference in references {
                    output.push((reference, Some(call_id.clone()), None));
                }
            }
        }
        KernelEvent::Completed => {
            add_required(&mut output, payload, "outputArtifact", None, None)?;
            add(&mut output, payload, "contractArtifact", None, None)?;
            add(&mut output, payload, "reviewSubjectBundle", None, None)?;
        }
        KernelEvent::Validated
        | KernelEvent::Started
        | KernelEvent::InputRequired
        | KernelEvent::ApprovalRequired
        | KernelEvent::ToolRequested
        | KernelEvent::Failed(_)
        | KernelEvent::Cancelled(_) => {}
    }
    Ok(output)
}

pub(crate) fn event_artifact_uses(
    sequence: u64,
    event: &KernelEvent,
    payload: &Value,
) -> Result<Vec<(ArtifactRef, ArtifactUse)>, String> {
    typed_event_artifacts(event, payload).map(|references| {
        references
            .into_iter()
            .map(|(artifact, call_id, call_index)| {
                (
                    artifact,
                    ArtifactUse {
                        sequence,
                        event: event_name(event).to_owned(),
                        call_index,
                        call_id,
                    },
                )
            })
            .collect()
    })
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(super) struct SessionMachine {
    pub(super) state: TaskState,
    pub(super) budget: TaskBudget,
    pub(super) transitions: u32,
    pub(super) model_calls: u32,
    pub(super) tool_calls: u32,
    pub(super) input_tokens: u64,
    pub(super) output_tokens: u64,
    pub(super) receipts: Vec<EffectReceiptRef>,
    pub(super) artifact_ledger: ArtifactLedger,
    pub(super) tool_call_ids: BTreeSet<ToolCallId>,
    #[serde(default)]
    pending_image_comparisons: BTreeMap<ToolCallId, PendingImageComparison>,
    #[serde(default)]
    pending_semantic_edits: BTreeMap<ToolCallId, PendingSemanticEdit>,
    pub(super) successful_tools: BTreeSet<String>,
    pub(super) successful_checks: BTreeSet<String>,
    pub(super) successful_tools_after_mutation: BTreeSet<String>,
    pub(super) successful_checks_after_mutation: BTreeSet<String>,
    pub(super) successful_source_mutation: bool,
    pub(super) irreversible_effect_observed: bool,
    #[serde(default)]
    pub(super) image_director: ImageDirectorState,
}

impl SessionMachine {
    pub(super) fn new(budget: TaskBudget) -> Self {
        Self {
            state: TaskState::Created,
            budget,
            transitions: 0,
            model_calls: 0,
            tool_calls: 0,
            input_tokens: 0,
            output_tokens: 0,
            receipts: Vec::new(),
            artifact_ledger: ArtifactLedger::default(),
            tool_call_ids: BTreeSet::new(),
            pending_image_comparisons: BTreeMap::new(),
            pending_semantic_edits: BTreeMap::new(),
            successful_tools: BTreeSet::new(),
            successful_checks: BTreeSet::new(),
            successful_tools_after_mutation: BTreeSet::new(),
            successful_checks_after_mutation: BTreeSet::new(),
            successful_source_mutation: false,
            irreversible_effect_observed: false,
            image_director: ImageDirectorState::default(),
        }
    }

    pub(super) fn admit_image_tool(&mut self, call: &ToolCall) -> Result<(), AgentFailure> {
        let name = call.name.as_str();
        if !agent_core::is_image_tool_name(name) {
            return Ok(());
        }
        if agent_core::is_candidate_image_tool_name(name) {
            self.image_director
                .admit_candidate(name == agent_core::IMAGE_GENERATE_TOOL)
                .map_err(|message| {
                    let code = if message.contains("budget") {
                        FailureCode::BudgetExceeded
                    } else {
                        FailureCode::VerificationFailed
                    };
                    AgentFailure::new(code, message)
                })?;
            if name != agent_core::IMAGE_GENERATE_TOOL {
                let sources = producer_pixel_sources(call).map_err(|message| {
                    AgentFailure::new(FailureCode::VerificationFailed, message)
                })?;
                self.image_director
                    .admit_pixel_sources(&sources)
                    .map_err(|message| {
                        AgentFailure::new(FailureCode::VerificationFailed, message)
                    })?;
            }
        }
        if name == agent_core::IMAGE_EDIT_SEMANTIC_TOOL {
            if self.state.is_terminal() || !self.pending_semantic_edits.is_empty() {
                return Err(AgentFailure::new(
                    FailureCode::VerificationFailed,
                    "semantic edit requires a live task with no in-flight semantic edit",
                ));
            }
            if call
                .arguments
                .get("candidateRevision")
                .and_then(Value::as_u64)
                != Some(u64::from(self.image_director.candidate_revision))
            {
                return Err(AgentFailure::new(
                    FailureCode::VerificationFailed,
                    "semantic edit candidate revision is stale or missing",
                ));
            }
            let contract_digest = self.image_director.contract_digest.clone().ok_or_else(|| {
                AgentFailure::new(
                    FailureCode::VerificationFailed,
                    "semantic edit has no image contract",
                )
            })?;
            let encoded = serde_json::to_vec(&call.arguments).map_err(|error| {
                AgentFailure::new(FailureCode::InternalInvariant, error.to_string())
            })?;
            self.pending_semantic_edits.insert(
                call.id.clone(),
                PendingSemanticEdit {
                    contract_digest,
                    candidate_revision: self.image_director.candidate_revision,
                    arguments_sha256: format!("{:x}", Sha256::digest(&encoded)),
                },
            );
        }
        if name == agent_core::IMAGE_EXTRACT_TOOL
            && call
                .arguments
                .get("selector")
                .and_then(|selector| selector.get("type"))
                .and_then(Value::as_str)
                == Some("semantic")
        {
            let source_artifact: ArtifactRef = call
                .arguments
                .get("artifact")
                .cloned()
                .ok_or_else(|| {
                    AgentFailure::new(
                        FailureCode::VerificationFailed,
                        "semantic extraction requires an image artifact",
                    )
                })
                .and_then(|value| {
                    serde_json::from_value(value).map_err(|error| {
                        AgentFailure::new(
                            FailureCode::VerificationFailed,
                            format!("semantic extraction artifact is invalid: {error}"),
                        )
                    })
                })?;
            let helper_identity = call
                .arguments
                .get("selector")
                .and_then(|selector| selector.get("helperIdentity"))
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    AgentFailure::new(
                        FailureCode::VerificationFailed,
                        "semantic extraction requires helperIdentity",
                    )
                })?
                .to_owned();
            self.image_director
                .begin_semantic_mask(&source_artifact, helper_identity, call.id.clone())
                .map_err(|message| AgentFailure::new(FailureCode::VerificationFailed, message))?;
        }
        if name == agent_core::IMAGE_COMPARE_TOOL
            && self
                .image_director
                .contract
                .as_ref()
                .is_some_and(agent_core::ImageTaskContract::requires_deterministic_compare)
        {
            let after_artifact: ArtifactRef = call
                .arguments
                .get("afterArtifact")
                .cloned()
                .ok_or_else(|| {
                    AgentFailure::new(
                        FailureCode::VerificationFailed,
                        "task-bound image comparison requires afterArtifact",
                    )
                })
                .and_then(|value| {
                    serde_json::from_value(value).map_err(|error| {
                        AgentFailure::new(
                            FailureCode::VerificationFailed,
                            format!(
                                "task-bound image comparison has invalid afterArtifact: {error}"
                            ),
                        )
                    })
                })?;
            let specification = verification_spec_from_call(call)
                .map_err(|message| AgentFailure::new(FailureCode::VerificationFailed, message))?;
            if !self
                .image_director
                .current_candidate_matches(&after_artifact, specification.after_kind)
            {
                return Err(AgentFailure::new(
                    FailureCode::VerificationFailed,
                    "task-bound image comparison artifact and kind do not match the current candidate",
                ));
            }
            let digest = specification
                .digest()
                .map_err(|message| AgentFailure::new(FailureCode::VerificationFailed, message))?;
            if self.image_director.required_verification_digest.as_deref() != Some(digest.as_str())
            {
                return Err(AgentFailure::new(
                    FailureCode::VerificationFailed,
                    "image comparison specification does not match the task contract",
                ));
            }
            if self.pending_image_comparisons.contains_key(&call.id) {
                return Err(AgentFailure::new(
                    FailureCode::ProviderFailed,
                    "provider reused a pending image comparison call identifier",
                ));
            }
            self.pending_image_comparisons.insert(
                call.id.clone(),
                PendingImageComparison {
                    candidate_revision: self.image_director.candidate_revision,
                    after_artifact,
                    after_kind: specification.after_kind,
                    verification_spec_digest: digest,
                },
            );
        }
        Ok(())
    }

    pub(super) fn semantic_mask_operation_for(
        &self,
        call: &ToolCall,
    ) -> Option<agent_core::SemanticMaskOperation> {
        (call.name == agent_core::IMAGE_EXTRACT_TOOL)
            .then(|| self.image_director.semantic_mask.pending.clone())
            .flatten()
            .filter(|operation| operation.tool_call_id == call.id)
    }

    pub(super) fn semantic_mask_event_value(
        &self,
        call: &ToolCall,
        result: &ToolResult,
    ) -> Result<Option<Value>, AgentFailure> {
        if call.name != agent_core::IMAGE_EXTRACT_TOOL {
            return Ok(None);
        }
        if let Some(value) = result
            .value
            .get("semanticMask")
            .filter(|value| !value.is_null())
        {
            return Ok(Some(value.clone()));
        }
        let Some(operation) = self.semantic_mask_operation_for(call) else {
            return Ok(None);
        };
        let Some(failure) = result.failure.clone() else {
            return Ok(None);
        };
        let status = if matches!(
            failure.code,
            FailureCode::Cancelled | FailureCode::BudgetExceeded
        ) {
            SemanticMaskResultStatus::Cancelled
        } else {
            SemanticMaskResultStatus::Failed
        };
        serde_json::to_value(SemanticMaskResult {
            schema: agent_core::SEMANTIC_MASK_CONTRACT_SCHEMA.to_owned(),
            operation,
            status,
            mask_artifact: None,
            failure: Some(failure),
        })
        .map(Some)
        .map_err(|error| {
            AgentFailure::new(
                FailureCode::InternalInvariant,
                "semantic mask event could not be encoded",
            )
            .with_detail("cause", error.to_string())
        })
    }

    pub(super) fn cancel_semantic_mask(
        &mut self,
        call: &ToolCall,
        failure: AgentFailure,
    ) -> Result<bool, AgentFailure> {
        let Some(operation) = self.semantic_mask_operation_for(call) else {
            return Ok(false);
        };
        let result = SemanticMaskResult {
            schema: agent_core::SEMANTIC_MASK_CONTRACT_SCHEMA.to_owned(),
            operation,
            status: SemanticMaskResultStatus::Cancelled,
            mask_artifact: None,
            failure: Some(failure),
        };
        self.image_director
            .apply_semantic_mask_result(result)
            .map(|applied| applied == agent_core::SemanticMaskApply::Accepted)
            .map_err(|message| AgentFailure::new(FailureCode::InternalInvariant, message))
    }

    pub(super) fn cancel_unstarted_image_effect(&mut self, call: &ToolCall) {
        if call.name == agent_core::IMAGE_EDIT_SEMANTIC_TOOL {
            self.pending_semantic_edits.remove(&call.id);
        }
    }

    pub(super) fn record_image_tool_result(
        &mut self,
        call: &ToolCall,
        result: &ToolResult,
        artifacts: &[ArtifactRef],
    ) -> Result<(), AgentFailure> {
        if call.name == agent_core::IMAGE_EDIT_SEMANTIC_TOOL {
            let Some(pending) = self.pending_semantic_edits.remove(&call.id) else {
                // Unknown or duplicate delivery cannot create a candidate.
                return Ok(());
            };
            let encoded = serde_json::to_vec(&call.arguments).map_err(|error| {
                AgentFailure::new(FailureCode::InternalInvariant, error.to_string())
            })?;
            if self.state.is_terminal()
                || self.image_director.contract_digest.as_ref() != Some(&pending.contract_digest)
                || self.image_director.candidate_revision != pending.candidate_revision
                || pending.arguments_sha256 != format!("{:x}", Sha256::digest(&encoded))
                || result.call_id != call.id
                || result.name != call.name
            {
                return Ok(());
            }
        }
        if call.name == agent_core::IMAGE_EXTRACT_TOOL {
            let semantic_result = result
                .value
                .get("semanticMask")
                .filter(|value| !value.is_null())
                .map(|value| {
                    serde_json::from_value::<SemanticMaskResult>(value.clone()).map_err(|error| {
                        AgentFailure::new(
                            FailureCode::InternalInvariant,
                            format!("semantic mask result event is invalid: {error}"),
                        )
                    })
                })
                .transpose()?;
            let semantic_result = match (semantic_result, self.semantic_mask_operation_for(call)) {
                (Some(value), _) => Some(value),
                (None, Some(operation)) if result.is_error => {
                    let failure = result.failure.clone().ok_or_else(|| {
                        AgentFailure::new(
                            FailureCode::InternalInvariant,
                            "semantic mask failure event omitted its cause",
                        )
                    })?;
                    let status = if matches!(
                        failure.code,
                        FailureCode::Cancelled | FailureCode::BudgetExceeded
                    ) {
                        SemanticMaskResultStatus::Cancelled
                    } else {
                        SemanticMaskResultStatus::Failed
                    };
                    Some(SemanticMaskResult {
                        schema: agent_core::SEMANTIC_MASK_CONTRACT_SCHEMA.to_owned(),
                        operation,
                        status,
                        mask_artifact: None,
                        failure: Some(failure),
                    })
                }
                (None, Some(_)) => {
                    return Err(AgentFailure::new(
                        FailureCode::InternalInvariant,
                        "semantic mask success event omitted semanticMask result",
                    ));
                }
                (None, None) => None,
            };
            if let Some(semantic_result) = semantic_result {
                let applied = self
                    .image_director
                    .apply_semantic_mask_result(semantic_result)
                    .map_err(|message| {
                        AgentFailure::new(FailureCode::InternalInvariant, message)
                    })?;
                if applied != SemanticMaskApply::Accepted {
                    // A result for an older operation, or a duplicate delivery
                    // of an already committed result, must not re-enter the
                    // image candidate projection.
                    return Ok(());
                }
            }
        }
        if result.is_error {
            if call.name == agent_core::IMAGE_COMPARE_TOOL {
                self.pending_image_comparisons.remove(&call.id);
            }
            return Ok(());
        }
        let generated_repair_requirement =
            if call.name == agent_core::IMAGE_GENERATE_TOOL
                && result
                    .value
                    .get("repairRequired")
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
            {
                let requirement = result.value.get("repairRequirement").cloned().ok_or_else(
                    || {
                        AgentFailure::new(
                            FailureCode::InternalInvariant,
                            "repair-required image generation omitted its exact repair requirement",
                        )
                    },
                )?;
                let requirement = serde_json::from_value::<ImageRepairRequirement>(requirement)
                    .map_err(|error| {
                        AgentFailure::new(
                            FailureCode::InternalInvariant,
                            format!("image repair requirement is invalid: {error}"),
                        )
                    })?;
                requirement.validate().map_err(|message| {
                    AgentFailure::new(FailureCode::InternalInvariant, message)
                })?;
                Some(requirement)
            } else {
                None
            };
        let repair_observation = if matches!(
            call.name.as_str(),
            agent_core::IMAGE_EXTRACT_TOOL | agent_core::IMAGE_RENDER_TOOL
        ) && result
            .value
            .get("perfectpixelVerified")
            .and_then(Value::as_bool)
            .unwrap_or(false)
            && self.image_director.pending_repair.is_some()
        {
            let observation = result
                .value
                .get("repairObservation")
                .cloned()
                .ok_or_else(|| {
                    AgentFailure::new(
                        FailureCode::InternalInvariant,
                        "PerfectPixel repair candidate omitted its pixel observation",
                    )
                })?;
            let observation = serde_json::from_value::<ImageRepairObservation>(observation)
                .map_err(|error| {
                    AgentFailure::new(
                        FailureCode::InternalInvariant,
                        format!("PerfectPixel repair observation is invalid: {error}"),
                    )
                })?;
            observation
                .validate()
                .map_err(|message| AgentFailure::new(FailureCode::InternalInvariant, message))?;
            Some(observation)
        } else {
            None
        };
        match call.name.as_str() {
            agent_core::IMAGE_GENERATE_TOOL => {
                self.image_director
                    .record_candidate_with_kinds(
                        artifacts
                            .iter()
                            .cloned()
                            .map(|artifact| (artifact, ImageCompareArtifactKind::Raster)),
                        false,
                    )
                    .map_err(|message| {
                        AgentFailure::new(FailureCode::InternalInvariant, message)
                    })?;
            }
            agent_core::IMAGE_EXTRACT_TOOL => {
                let roles = result.value.get("artifactRoles").and_then(Value::as_array);
                let primary = artifacts
                    .iter()
                    .enumerate()
                    .filter(|(index, _)| {
                        roles
                            .and_then(|values| values.get(*index))
                            .and_then(Value::as_str)
                            == Some("object")
                    })
                    .map(|(_, artifact)| (artifact.clone(), ImageCompareArtifactKind::Object))
                    .collect::<Vec<_>>();
                if primary.is_empty() {
                    return Err(AgentFailure::new(
                        FailureCode::InternalInvariant,
                        "successful image_extract omitted its object candidate artifact",
                    ));
                }
                let additional_pixel_sources = artifacts
                    .iter()
                    .enumerate()
                    .filter(|(index, _)| {
                        roles
                            .and_then(|values| values.get(*index))
                            .and_then(Value::as_str)
                            == Some("remainder")
                    })
                    .map(|(_, artifact)| artifact.clone())
                    .collect::<Vec<_>>();
                self.image_director
                    .authorize_image_artifacts(additional_pixel_sources)
                    .map_err(|message| {
                        AgentFailure::new(FailureCode::InternalInvariant, message)
                    })?;
                self.image_director
                    .record_candidate_with_kinds(primary, true)
                    .map_err(|message| {
                        AgentFailure::new(FailureCode::InternalInvariant, message)
                    })?;
            }
            agent_core::IMAGE_RENDER_TOOL | agent_core::IMAGE_EDIT_SEMANTIC_TOOL => {
                self.image_director
                    .record_candidate_with_kinds(
                        artifacts
                            .iter()
                            .cloned()
                            .map(|artifact| (artifact, ImageCompareArtifactKind::Raster)),
                        true,
                    )
                    .map_err(|message| {
                        AgentFailure::new(FailureCode::InternalInvariant, message)
                    })?;
            }
            agent_core::IMAGE_COMPARE_TOOL => {
                if !self.image_director.is_active() {
                    return Ok(());
                }
                if !self
                    .image_director
                    .contract
                    .as_ref()
                    .is_some_and(agent_core::ImageTaskContract::requires_deterministic_compare)
                {
                    return Ok(());
                }
                let after_artifact = call
                    .arguments
                    .get("afterArtifact")
                    .cloned()
                    .and_then(|value| serde_json::from_value::<ArtifactRef>(value).ok());
                let specification = verification_spec_from_call(call).map_err(|message| {
                    AgentFailure::new(FailureCode::InternalInvariant, message)
                })?;
                let bound_to_current = after_artifact.as_ref().is_some_and(|artifact| {
                    self.image_director
                        .current_candidate_matches(artifact, specification.after_kind)
                });
                if !bound_to_current && self.image_director.candidate_revision > 0 {
                    self.pending_image_comparisons.remove(&call.id);
                    return Ok(());
                }
                let Some(after_artifact) = after_artifact else {
                    self.pending_image_comparisons.remove(&call.id);
                    return Ok(());
                };
                let specification_digest = specification.digest().map_err(|message| {
                    AgentFailure::new(FailureCode::InternalInvariant, message)
                })?;
                let Some(pending) = self.pending_image_comparisons.remove(&call.id) else {
                    return Ok(());
                };
                if pending.candidate_revision != self.image_director.candidate_revision
                    || pending.after_artifact != after_artifact
                    || pending.after_kind != specification.after_kind
                    || pending.verification_spec_digest != specification_digest
                {
                    return Ok(());
                }
                if result
                    .value
                    .get("verificationSpecDigest")
                    .and_then(Value::as_str)
                    != Some(specification_digest.as_str())
                {
                    return Err(AgentFailure::new(
                        FailureCode::InternalInvariant,
                        "image compare result verification digest is inconsistent",
                    ));
                }
                let (passed, failed_assertions) =
                    exact_compare_result(&specification, &result.value).ok_or_else(|| {
                        AgentFailure::new(
                            FailureCode::InternalInvariant,
                            "image compare result does not match the exact task specification",
                        )
                    })?;
                self.image_director
                    .record_compare(Some(specification_digest), passed, failed_assertions)
                    .map_err(|message| {
                        AgentFailure::new(FailureCode::InternalInvariant, message)
                    })?;
            }
            _ => {}
        }
        if call.name == agent_core::IMAGE_GENERATE_TOOL {
            if let Some(requirement) = generated_repair_requirement {
                self.image_director
                    .mark_repair_required(requirement)
                    .map_err(|message| {
                        AgentFailure::new(FailureCode::InternalInvariant, message)
                    })?;
            } else {
                self.image_director.clear_repair_requirement();
            }
        } else if let Some(observation) = repair_observation {
            let _ = self
                .image_director
                .verify_repair_observation(observation)
                .map_err(|message| AgentFailure::new(FailureCode::InternalInvariant, message))?;
        }
        Ok(())
    }

    pub(super) fn suspended_snapshot(&self) -> Result<Self, AgentFailure> {
        if !self.pending_semantic_edits.is_empty() {
            return Err(AgentFailure::new(
                FailureCode::ResolutionRequired,
                "an in-flight external image edit cannot be checkpointed for automatic re-dispatch",
            ));
        }
        if !self.pending_image_comparisons.is_empty() {
            return Err(AgentFailure::new(
                FailureCode::InternalInvariant,
                "cannot suspend while image comparisons are pending",
            ));
        }
        if self.transitions.saturating_add(3) > self.budget.maximum_transitions {
            return Err(AgentFailure::new(
                FailureCode::BudgetExceeded,
                "insufficient transition budget to suspend and resume safely",
            ));
        }
        let mut snapshot = self.clone();
        if let Some(operation) = snapshot.image_director.semantic_mask.pending.clone() {
            let cancellation = SemanticMaskResult {
                schema: agent_core::SEMANTIC_MASK_CONTRACT_SCHEMA.to_owned(),
                operation,
                status: SemanticMaskResultStatus::Cancelled,
                mask_artifact: None,
                failure: Some(
                    AgentFailure::new(
                        FailureCode::Cancelled,
                        "semantic mask operation was cancelled before checkpoint suspension",
                    )
                    .with_detail("operation", "semantic_mask")
                    .with_detail("checkpoint", true),
                ),
            };
            snapshot
                .image_director
                .apply_semantic_mask_result(cancellation)
                .map_err(|message| AgentFailure::new(FailureCode::InternalInvariant, message))?;
        }
        snapshot.state = transition(snapshot.state, &KernelEvent::Suspended).map_err(|error| {
            AgentFailure::new(FailureCode::InvalidTransition, error.to_string())
        })?;
        snapshot.transitions = snapshot.transitions.saturating_add(1);
        Ok(snapshot)
    }

    pub(super) fn register_tool_call(&mut self, id: &ToolCallId) -> Result<(), AgentFailure> {
        if !self.tool_call_ids.insert(id.clone()) {
            return Err(AgentFailure::new(
                FailureCode::ProviderFailed,
                "provider reused a tool call identifier within one task",
            )
            .with_detail("toolCallId", id.as_str()));
        }
        Ok(())
    }

    pub(super) fn constrain_budget(&mut self, budget: &TaskBudget) -> Result<(), AgentFailure> {
        budget
            .validate()
            .map_err(|message| AgentFailure::new(FailureCode::InvalidRequest, message))?;
        if self.transitions > budget.maximum_transitions
            || self.model_calls > budget.maximum_model_calls
            || self.tool_calls > budget.maximum_tool_calls
        {
            return Err(AgentFailure::new(
                FailureCode::BudgetExceeded,
                "execution policy budget is already below consumed work",
            ));
        }
        self.budget = budget.clone();
        Ok(())
    }

    #[allow(clippy::needless_pass_by_value)]
    pub(super) fn record(
        &mut self,
        journal: &mut SessionJournal,
        event: KernelEvent,
        payload: Value,
    ) -> Result<(), AgentFailure> {
        let terminal = matches!(
            event,
            KernelEvent::Completed
                | KernelEvent::Failed(_)
                | KernelEvent::Cancelled(_)
                | KernelEvent::ResolutionRequired(_)
        );
        let required = if terminal { 1 } else { 2 };
        if self.transitions.saturating_add(required) > self.budget.maximum_transitions {
            return Err(AgentFailure::new(
                FailureCode::BudgetExceeded,
                "task transition budget was exhausted",
            ));
        }
        let sequence = journal.next_sequence().map_err(journal_failure)?;
        let next = transition(self.state, &event).map_err(|error| {
            AgentFailure::new(FailureCode::InvalidTransition, error.to_string())
        })?;
        self.register_payload_artifacts(sequence, &event, &payload)?;
        if let Err(error) = journal.append(self.state, next, &event, payload.clone()) {
            // A serialization/contract/sequence failure happened before the
            // write began and can be rolled back.  Once the writer entered
            // write_all, the file may contain a complete or partial record;
            // keep the ledger and force recovery instead of fabricating a
            // terminal projection.
            if !error.durability_unknown() {
                self.unregister_payload_artifacts(sequence, &event, &payload);
            }
            return Err(journal_failure(error));
        }
        self.state = next;
        self.transitions = self.transitions.saturating_add(1);
        Ok(())
    }

    #[allow(clippy::needless_pass_by_value)]
    pub(super) fn record_control(
        &mut self,
        journal: &mut SessionJournal,
        event: &KernelEvent,
        payload: Value,
    ) -> Result<(), AgentFailure> {
        let required = match event {
            KernelEvent::Suspended => 3,
            KernelEvent::Resumed => 2,
            _ => {
                return Err(AgentFailure::new(
                    FailureCode::InternalInvariant,
                    "non-control event used the control transition path",
                ));
            }
        };
        if self.transitions.saturating_add(required) > self.budget.maximum_transitions {
            return Err(AgentFailure::new(
                FailureCode::BudgetExceeded,
                "control transition budget was exhausted",
            ));
        }
        let sequence = journal.next_sequence().map_err(journal_failure)?;
        let next = transition(self.state, event).map_err(|error| {
            AgentFailure::new(FailureCode::InvalidTransition, error.to_string())
        })?;
        self.register_payload_artifacts(sequence, event, &payload)?;
        if let Err(error) = journal.append(self.state, next, event, payload.clone()) {
            if !error.durability_unknown() {
                self.unregister_payload_artifacts(sequence, event, &payload);
            }
            return Err(journal_failure(error));
        }
        self.state = next;
        self.transitions = self.transitions.saturating_add(1);
        Ok(())
    }

    pub(super) fn register_event_payload(
        &mut self,
        sequence: u64,
        event: &KernelEvent,
        payload: &Value,
    ) -> Result<(), AgentFailure> {
        self.register_payload_artifacts(sequence, event, payload)
    }

    fn register_payload_artifacts(
        &mut self,
        sequence: u64,
        event: &KernelEvent,
        payload: &Value,
    ) -> Result<(), AgentFailure> {
        let references = typed_event_artifacts(event, payload)
            .map_err(|message| AgentFailure::new(FailureCode::InternalInvariant, message))?;
        let mut seen = BTreeSet::new();
        let mut inserted = Vec::new();
        for (reference, call_id, call_index) in references {
            let use_record = ArtifactUse {
                sequence,
                event: event_name(event).to_owned(),
                call_index,
                call_id,
            };
            let duplicate = (
                reference.id.clone(),
                use_record.sequence,
                use_record.event.clone(),
                use_record.call_index,
                use_record.call_id.clone(),
            );
            if !seen.insert(duplicate) {
                for (artifact_id, use_record) in inserted {
                    self.remove_artifact_use(&artifact_id, &use_record);
                }
                return Err(AgentFailure::new(
                    FailureCode::InternalInvariant,
                    "one event contains duplicate artifact provenance use",
                ));
            }
            match self
                .artifact_ledger
                .register(reference.clone(), use_record.clone())
            {
                Ok(true) => inserted.push((reference.id, use_record)),
                Ok(false) => {}
                Err(message) => {
                    for (artifact_id, use_record) in inserted {
                        self.remove_artifact_use(&artifact_id, &use_record);
                    }
                    return Err(AgentFailure::new(FailureCode::InternalInvariant, message));
                }
            }
        }
        Ok(())
    }

    fn remove_artifact_use(&mut self, artifact_id: &ArtifactId, use_record: &ArtifactUse) {
        if let Some(entry) = self.artifact_ledger.entries.get_mut(artifact_id) {
            entry.uses.retain(|existing| existing != use_record);
            if entry.uses.is_empty() {
                self.artifact_ledger.entries.remove(artifact_id);
            }
        }
    }

    fn unregister_payload_artifacts(
        &mut self,
        sequence: u64,
        event: &KernelEvent,
        payload: &Value,
    ) {
        let Ok(references) = typed_event_artifacts(event, payload) else {
            return;
        };
        for (reference, call_id, call_index) in references {
            let use_record = ArtifactUse {
                sequence,
                event: event_name(event).to_owned(),
                call_index,
                call_id,
            };
            self.remove_artifact_use(&reference.id, &use_record);
        }
    }

    pub(super) fn artifacts(&self) -> Vec<ArtifactRef> {
        self.artifact_ledger.artifacts()
    }

    pub(super) fn evidence(&self) -> Vec<ArtifactEvidence> {
        self.artifact_ledger.evidence()
    }

    pub(super) fn ensure_model_budget(
        &self,
        deadline: tokio::time::Instant,
    ) -> Result<(), AgentFailure> {
        if self.model_calls >= self.budget.maximum_model_calls {
            return Err(AgentFailure::new(
                FailureCode::BudgetExceeded,
                "model call budget was exhausted",
            ));
        }
        if self.transitions.saturating_add(3) > self.budget.maximum_transitions {
            return Err(AgentFailure::new(
                FailureCode::BudgetExceeded,
                "insufficient transition budget for another model turn",
            ));
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(AgentFailure::new(
                FailureCode::BudgetExceeded,
                "task deadline expired",
            ));
        }
        Ok(())
    }

    pub(super) fn reserve_tools(
        &self,
        count: usize,
        deadline: tokio::time::Instant,
    ) -> Result<(), AgentFailure> {
        let count = u32::try_from(count).map_err(|_| {
            AgentFailure::new(FailureCode::BudgetExceeded, "tool count exceeds u32")
        })?;
        if self.tool_calls.saturating_add(count) > self.budget.maximum_tool_calls {
            return Err(AgentFailure::new(
                FailureCode::BudgetExceeded,
                "tool call budget was exhausted",
            ));
        }
        if self
            .transitions
            .saturating_add(count.saturating_mul(2))
            .saturating_add(1)
            > self.budget.maximum_transitions
        {
            return Err(AgentFailure::new(
                FailureCode::BudgetExceeded,
                "insufficient transition budget for tool calls",
            ));
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(AgentFailure::new(
                FailureCode::BudgetExceeded,
                "task deadline expired",
            ));
        }
        Ok(())
    }

    pub(super) fn add_usage(&mut self, usage: Option<&agent_core::ModelUsage>) {
        if let Some(usage) = usage {
            self.input_tokens = self
                .input_tokens
                .saturating_add(usage.input_tokens.unwrap_or(0));
            self.output_tokens = self
                .output_tokens
                .saturating_add(usage.output_tokens.unwrap_or(0));
        }
    }

    pub(super) fn mark_tool_success(
        &mut self,
        name: &str,
        source_mutation: bool,
        requirement_id: Option<&str>,
    ) {
        self.successful_tools.insert(name.to_owned());
        if name == "run_check"
            && let Some(requirement_id) = requirement_id
            && !requirement_id.trim().is_empty()
        {
            self.successful_checks.insert(requirement_id.to_owned());
        }
        if source_mutation {
            self.successful_source_mutation = true;
            self.successful_tools_after_mutation.clear();
            self.successful_checks_after_mutation.clear();
        } else {
            self.successful_tools_after_mutation.insert(name.to_owned());
            if name == "run_check"
                && let Some(requirement_id) = requirement_id
                && !requirement_id.trim().is_empty()
            {
                self.successful_checks_after_mutation
                    .insert(requirement_id.to_owned());
            }
        }
    }

    pub(super) fn metrics(&self) -> BTreeMap<String, Value> {
        BTreeMap::from([
            ("transitions".to_owned(), json!(self.transitions)),
            ("modelCalls".to_owned(), json!(self.model_calls)),
            ("toolCalls".to_owned(), json!(self.tool_calls)),
            ("inputTokens".to_owned(), json!(self.input_tokens)),
            ("outputTokens".to_owned(), json!(self.output_tokens)),
            ("successfulTools".to_owned(), json!(self.successful_tools)),
            ("successfulChecks".to_owned(), json!(self.successful_checks)),
            (
                "successfulSourceMutation".to_owned(),
                json!(self.successful_source_mutation),
            ),
            ("imageDirector".to_owned(), json!(&self.image_director)),
        ])
    }
}

fn producer_pixel_sources(call: &ToolCall) -> Result<Vec<ArtifactRef>, String> {
    match call.name.as_str() {
        agent_core::IMAGE_EXTRACT_TOOL | agent_core::IMAGE_EDIT_SEMANTIC_TOOL => call
            .arguments
            .get("artifact")
            .cloned()
            .ok_or_else(|| "image_extract requires artifact provenance".to_owned())
            .and_then(|value| {
                serde_json::from_value::<ArtifactRef>(value)
                    .map(|artifact| vec![artifact])
                    .map_err(|error| {
                        format!("image_extract artifact provenance is invalid: {error}")
                    })
            }),
        agent_core::IMAGE_RENDER_TOOL => {
            let nodes = call
                .arguments
                .get("nodes")
                .and_then(Value::as_array)
                .ok_or_else(|| "image_render requires a bounded nodes array".to_owned())?;
            let mut sources = Vec::new();
            for node in nodes {
                if node.get("kind").and_then(Value::as_str) == Some("text") {
                    continue;
                }
                let artifact = node
                    .get("artifact")
                    .cloned()
                    .ok_or_else(|| {
                        "image_render raster/object nodes require artifact provenance".to_owned()
                    })
                    .and_then(|value| {
                        serde_json::from_value::<ArtifactRef>(value).map_err(|error| {
                            format!("image_render node artifact provenance is invalid: {error}")
                        })
                    })?;
                sources.push(artifact);
            }
            Ok(sources)
        }
        _ => Ok(Vec::new()),
    }
}

fn verification_spec_from_call(call: &ToolCall) -> Result<ImageVerificationSpec, String> {
    let before_artifact = call
        .arguments
        .get("beforeArtifact")
        .cloned()
        .ok_or_else(|| "image comparison requires beforeArtifact".to_owned())
        .and_then(|value| {
            serde_json::from_value(value)
                .map_err(|error| format!("image comparison beforeArtifact is invalid: {error}"))
        })?;
    let after_kind = match call.arguments.get("afterKind").and_then(Value::as_str) {
        None | Some("raster") => ImageCompareArtifactKind::Raster,
        Some("object") => ImageCompareArtifactKind::Object,
        Some("export") => ImageCompareArtifactKind::Export,
        Some(_) => return Err("image comparison afterKind is invalid".to_owned()),
    };
    let assertions: Vec<ImageCompareAssertion> = call
        .arguments
        .get("assertions")
        .cloned()
        .ok_or_else(|| "image comparison requires assertions".to_owned())
        .and_then(|value| {
            serde_json::from_value(value)
                .map_err(|error| format!("image comparison assertions are invalid: {error}"))
        })?;
    let specification = ImageVerificationSpec {
        schema: IMAGE_VERIFICATION_SPEC_SCHEMA.to_owned(),
        before_artifact,
        after_kind,
        assertions,
    };
    specification.validate()?;
    Ok(specification)
}

fn exact_compare_result(
    specification: &ImageVerificationSpec,
    value: &Value,
) -> Option<(bool, Vec<String>)> {
    let all_required_passed = value.get("allRequiredPassed")?.as_bool()?;
    let results: Vec<ImageCompareAssertionResult> =
        serde_json::from_value(value.get("assertions")?.clone()).ok()?;
    let failed = specification
        .validate_result(all_required_passed, &results)
        .ok()?;
    Some((all_required_passed, failed))
}

pub(super) fn finish_failure(
    task: &TaskEnvelope,
    machine: &mut SessionMachine,
    journal: &mut SessionJournal,
    mut failure: AgentFailure,
) -> AgentResult {
    if journal.is_poisoned() {
        return undurable_durability_unknown(task, machine, &failure);
    }
    let event = match failure.code {
        FailureCode::Cancelled => KernelEvent::Cancelled(failure.clone()),
        FailureCode::ResolutionRequired => KernelEvent::ResolutionRequired(failure.clone()),
        _ => KernelEvent::Failed(failure.clone()),
    };
    let status = match failure.code {
        FailureCode::Cancelled => AgentStatus::Cancelled,
        FailureCode::ResolutionRequired => AgentStatus::ResolutionRequired,
        _ => AgentStatus::Failed,
    };
    let target_state = match status {
        AgentStatus::Cancelled => TaskState::Cancelled,
        AgentStatus::ResolutionRequired => TaskState::ResolutionRequired,
        _ => TaskState::Failed,
    };
    if let Err(record_failure) = machine.record(journal, event, Value::Null) {
        if is_durability_unknown(&record_failure) || journal.is_poisoned() {
            return undurable_durability_unknown(task, machine, &record_failure);
        }
        if machine.irreversible_effect_observed {
            return undurable_resolution(task, machine, machine.receipts.clone(), record_failure);
        }
        failure = failure
            .with_detail("terminalEventRecordFailure", record_failure.message)
            .with_detail(
                "terminalEventRecordFailureCode",
                record_failure.code.as_str(),
            );
    }
    let mut result = terminal_result(
        task,
        machine,
        status,
        machine.receipts.clone(),
        Some(failure),
    );
    if !result.state.is_terminal() {
        result.state = target_state;
    }
    finish_result(journal, result, machine.irreversible_effect_observed)
}

#[cfg(test)]
mod evidence_tests {
    use super::SessionMachine;
    use agent_core::{
        AgentFailure, ArtifactId, ArtifactRef, FailureCode, IMAGE_TASK_CONTRACT_SCHEMA,
        IMAGE_VERIFICATION_SPEC_SCHEMA, ImageCompareArtifactKind, ImageCompareAssertion,
        ImageCompareSeverity, ImageSource, ImageTaskContract, ImageVerificationContract,
        ImageVerificationSpec, SemanticMaskOperation, SemanticMaskResult, SemanticMaskResultStatus,
        TaskBudget, TaskState, ToolCall, ToolCallId, ToolResult,
    };
    use serde_json::json;

    fn image_artifact(seed: char) -> ArtifactRef {
        let digest = seed.to_string().repeat(64);
        ArtifactRef {
            id: ArtifactId::parse(format!("art-{digest}")).expect("artifact id"),
            sha256: digest.clone(),
            media_type: "image/png".to_owned(),
            byte_length: 1,
            relative_path: format!("{}/{}.png", &digest[..2], digest),
        }
    }

    fn render_call(id: &str, artifact: &ArtifactRef) -> ToolCall {
        ToolCall {
            id: ToolCallId::parse(id).expect("call id"),
            name: "image_render".to_owned(),
            arguments: json!({
                "canvas":{"width":1,"height":1,"background":[0,0,0,0]},
                "nodes":[{
                    "id":"source",
                    "kind":"raster",
                    "artifact":artifact,
                    "transform":{"matrix":[1.0,0.0,0.0,0.0,1.0,0.0,0.0,1.0]},
                    "opacity":255
                }]
            }),
        }
    }

    fn image_contract(compare_required: bool) -> ImageTaskContract {
        ImageTaskContract {
            schema: IMAGE_TASK_CONTRACT_SCHEMA.to_owned(),
            goal: "edit".to_owned(),
            source: ImageSource::Provided {
                artifact: image_artifact('0'),
            },
            maximum_attempts: 16,
            preserve: Vec::new(),
            visual_criteria: vec!["review".to_owned()],
            verification: ImageVerificationContract {
                visual_review: true,
                deterministic: compare_required.then(|| ImageVerificationSpec {
                    schema: IMAGE_VERIFICATION_SPEC_SCHEMA.to_owned(),
                    before_artifact: image_artifact('0'),
                    after_kind: ImageCompareArtifactKind::Raster,
                    assertions: vec![ImageCompareAssertion::ExactEqual {
                        id: "preserve".to_owned(),
                        severity: ImageCompareSeverity::Required,
                    }],
                }),
            },
        }
    }

    fn compare_call(
        id: &str,
        specification: &ImageVerificationSpec,
        after_artifact: &ArtifactRef,
    ) -> ToolCall {
        ToolCall {
            id: ToolCallId::parse(id).expect("call id"),
            name: "image_compare".to_owned(),
            arguments: json!({
                "beforeArtifact": specification.before_artifact,
                "afterArtifact": after_artifact,
                "afterKind": specification.after_kind,
                "assertions": specification.assertions,
                "preview": {"difference":false,"maskOverlay":false,"maximumEdge":256}
            }),
        }
    }

    fn compare_result(
        call: &ToolCall,
        specification: &ImageVerificationSpec,
        passed: bool,
    ) -> ToolResult {
        let assertions = specification
            .assertions
            .iter()
            .map(|assertion| {
                json!({
                    "id":assertion.id(),
                    "assertionType":assertion.assertion_type(),
                    "severity":assertion.severity(),
                    "passed":passed,
                    "observed":passed,
                    "message":"test deterministic evaluation"
                })
            })
            .collect::<Vec<_>>();
        ToolResult {
            call_id: call.id.clone(),
            name: call.name.clone(),
            value: json!({
                "allRequiredPassed":passed,
                "verificationSpecDigest":specification.digest().expect("specification digest"),
                "assertions":assertions,
            }),
            is_error: false,
            failure: None,
        }
    }

    fn semantic_call(
        id: &str,
        source: &ArtifactRef,
        source_revision: u64,
        request_revision: u64,
    ) -> ToolCall {
        ToolCall {
            id: ToolCallId::parse(id).expect("call id"),
            name: "image_extract".to_owned(),
            arguments: json!({
                "artifact": source,
                "selector": {
                    "type": "semantic",
                    "sourceRevision": source_revision,
                    "requestRevision": request_revision,
                    "helperIdentity": "fixture.semantic/1",
                    "target": {
                        "type": "point",
                        "xMillionths": 500_000,
                        "yMillionths": 500_000
                    }
                },
                "includeRemainder": false
            }),
        }
    }

    fn semantic_result(
        operation: SemanticMaskOperation,
        status: SemanticMaskResultStatus,
    ) -> SemanticMaskResult {
        SemanticMaskResult {
            schema: agent_core::SEMANTIC_MASK_CONTRACT_SCHEMA.to_owned(),
            operation,
            status,
            mask_artifact: (status == SemanticMaskResultStatus::Succeeded)
                .then(|| image_artifact('d')),
            failure: (status != SemanticMaskResultStatus::Succeeded).then(|| {
                AgentFailure::new(
                    if status == SemanticMaskResultStatus::Cancelled {
                        FailureCode::Cancelled
                    } else {
                        FailureCode::ProviderFailed
                    },
                    "semantic fixture result",
                )
            }),
        }
    }

    #[test]
    fn semantic_result_identity_is_durable_and_duplicate_or_stale_delivery_is_ignored() {
        let source = image_artifact('c');
        let mut contract = image_contract(false);
        contract.source = ImageSource::Provided {
            artifact: source.clone(),
        };
        let mut machine = SessionMachine::new(TaskBudget::default());
        machine
            .image_director
            .bind_contract(Some(&contract))
            .expect("contract");

        // Caller revision numbers are not the owner: admission binds source
        // and request identity from ImageDirectorState.
        let first_call = semantic_call("semantic-machine-1", &source, 999, 999);
        machine
            .admit_image_tool(&first_call)
            .expect("semantic admission");
        let first_operation = machine
            .image_director
            .semantic_mask
            .pending
            .clone()
            .expect("durable pending operation");
        assert_eq!(first_operation.source_revision, 1);
        assert_eq!(first_operation.request_revision, 1);

        let first_result =
            semantic_result(first_operation.clone(), SemanticMaskResultStatus::Succeeded);
        let mut success = ToolResult {
            call_id: first_call.id.clone(),
            name: first_call.name.clone(),
            value: json!({
                "semanticMask": first_result,
                "artifactRoles": ["object", "mask"]
            }),
            is_error: false,
            failure: None,
        };
        machine
            .record_image_tool_result(
                &first_call,
                &success,
                &[image_artifact('b'), image_artifact('d')],
            )
            .expect("semantic success");
        let committed = machine.image_director.clone();
        assert!(machine.image_director.semantic_mask.pending.is_none());
        assert_eq!(machine.image_director.semantic_request_revision, 1);
        assert!(
            machine
                .image_director
                .is_image_artifact_authorized(&image_artifact('b'))
        );
        assert!(
            !machine
                .image_director
                .is_image_artifact_authorized(&image_artifact('d'))
        );

        // The same result arrives twice: it must not create another candidate.
        machine
            .record_image_tool_result(
                &first_call,
                &success,
                &[image_artifact('b'), image_artifact('d')],
            )
            .expect("duplicate semantic result");
        assert_eq!(machine.image_director, committed);

        let current_source = image_artifact('b');
        let second_call = semantic_call("semantic-machine-2", &current_source, 1, 1);
        machine
            .admit_image_tool(&second_call)
            .expect("semantic retry admission");
        let second_operation = machine
            .image_director
            .semantic_mask
            .pending
            .clone()
            .expect("retry operation");
        assert_eq!(second_operation.source_revision, 2);
        assert_eq!(second_operation.request_revision, 2);

        // A late result from the previous operation is stale and cannot
        // replace the current pending state or candidate projection.
        machine
            .record_image_tool_result(
                &first_call,
                &success,
                &[image_artifact('b'), image_artifact('d')],
            )
            .expect("stale semantic result");
        assert_eq!(
            machine
                .image_director
                .semantic_mask
                .pending
                .as_ref()
                .map(|operation| operation.request_revision),
            Some(2)
        );
        success.value = json!({
            "semanticMask": semantic_result(
                second_operation.clone(),
                SemanticMaskResultStatus::Cancelled
            )
        });
        success.is_error = true;
        success.failure = Some(AgentFailure::new(
            FailureCode::Cancelled,
            "semantic retry cancelled",
        ));
        machine
            .record_image_tool_result(&second_call, &success, &[])
            .expect("semantic cancellation");
        assert!(machine.image_director.semantic_mask.pending.is_none());
        assert_eq!(
            machine
                .image_director
                .semantic_mask
                .completed
                .as_ref()
                .map(|result| result.status),
            Some(SemanticMaskResultStatus::Cancelled)
        );
    }

    #[test]
    fn semantic_pending_checkpoint_is_resumed_as_explicit_cancellation() {
        let source = image_artifact('c');
        let mut contract = image_contract(false);
        contract.source = ImageSource::Provided {
            artifact: source.clone(),
        };
        let mut machine = SessionMachine::new(TaskBudget::default());
        machine
            .image_director
            .bind_contract(Some(&contract))
            .expect("contract");
        machine.state = TaskState::Running;
        let call = semantic_call("semantic-checkpoint", &source, 1, 1);
        machine.admit_image_tool(&call).expect("semantic admission");
        let snapshot = machine.suspended_snapshot().expect("checkpoint snapshot");
        assert!(snapshot.image_director.semantic_mask.pending.is_none());
        assert_eq!(
            snapshot
                .image_director
                .semantic_mask
                .completed
                .as_ref()
                .map(|result| result.status),
            Some(SemanticMaskResultStatus::Cancelled)
        );
        let encoded = serde_json::to_vec(&snapshot).expect("machine checkpoint encode");
        let resumed: SessionMachine =
            serde_json::from_slice(&encoded).expect("machine checkpoint decode");
        assert_eq!(
            serde_json::to_value(&resumed).expect("resumed machine JSON"),
            serde_json::to_value(&snapshot).expect("snapshot machine JSON")
        );
    }

    #[test]
    fn each_source_mutation_resets_current_evidence_but_not_metrics() {
        let mut machine = SessionMachine::new(TaskBudget::default());
        machine.mark_tool_success("inspect_diff", false, None);
        machine.mark_tool_success("run_check", false, Some("build"));
        assert!(
            machine
                .successful_tools_after_mutation
                .contains("inspect_diff")
        );
        assert!(machine.successful_checks_after_mutation.contains("build"));

        machine.mark_tool_success("write_file", true, None);
        assert!(machine.successful_tools.contains("inspect_diff"));
        assert!(machine.successful_checks.contains("build"));
        assert!(machine.successful_tools_after_mutation.is_empty());
        assert!(machine.successful_checks_after_mutation.is_empty());

        machine.mark_tool_success("inspect_diff", false, None);
        machine.mark_tool_success("run_check", false, Some("build"));
        machine.mark_tool_success("apply_patch", true, None);
        assert!(machine.successful_tools.contains("inspect_diff"));
        assert!(machine.successful_tools.contains("apply_patch"));
        assert!(machine.successful_tools_after_mutation.is_empty());
        assert!(machine.successful_checks_after_mutation.is_empty());
    }

    #[test]
    fn failed_compare_is_task_state_not_tool_error() {
        let mut machine = SessionMachine::new(TaskBudget::default());
        let contract = image_contract(true);
        let specification = contract
            .verification
            .deterministic
            .clone()
            .expect("verification specification");
        let specification_digest = specification.digest().expect("specification digest");
        machine
            .image_director
            .bind_contract(Some(&contract))
            .expect("contract");
        let candidate = image_artifact('a');
        machine
            .image_director
            .record_candidate([candidate.clone()], true)
            .expect("candidate");
        machine.image_director.mark_observed();
        let call = ToolCall {
            id: ToolCallId::parse("compare-1").expect("call id"),
            name: "image_compare".to_owned(),
            arguments: json!({
                "beforeArtifact": specification.before_artifact,
                "afterArtifact":candidate,
                "afterKind":"raster",
                "assertions": specification.assertions,
            }),
        };
        let result = ToolResult {
            call_id: call.id.clone(),
            name: call.name.clone(),
            value: json!({
                "allRequiredPassed":false,
                "verificationSpecDigest": specification_digest,
                "assertions":[{
                    "id":"preserve",
                    "assertionType":"exact_equal",
                    "severity":"required",
                    "passed":false,
                    "observed":false,
                    "message":"required exact equality failed"
                }]
            }),
            is_error: false,
            failure: None,
        };
        machine.admit_image_tool(&call).expect("admitted compare");
        machine
            .record_image_tool_result(&call, &result, &[])
            .expect("record compare");
        assert!(machine.image_director.completion_blocker().is_some());
        assert_eq!(machine.image_director.failed_assertions, vec!["preserve"]);
    }

    #[test]
    fn unrelated_compare_cannot_approve_or_reject_current_candidate() {
        let mut machine = SessionMachine::new(TaskBudget::default());
        let contract = image_contract(true);
        let specification = contract
            .verification
            .deterministic
            .clone()
            .expect("verification specification");
        let specification_digest = specification.digest().expect("specification digest");
        machine
            .image_director
            .bind_contract(Some(&contract))
            .expect("contract");
        let candidate = image_artifact('a');
        let unrelated = image_artifact('b');
        machine
            .image_director
            .record_candidate([candidate], true)
            .expect("candidate");
        machine.image_director.mark_observed();
        let call = ToolCall {
            id: ToolCallId::parse("compare-2").expect("call id"),
            name: "image_compare".to_owned(),
            arguments: json!({
                "beforeArtifact": specification.before_artifact,
                "afterArtifact":unrelated,
                "afterKind":"raster",
                "assertions": specification.assertions,
            }),
        };
        let result = ToolResult {
            call_id: call.id.clone(),
            name: call.name.clone(),
            value: json!({
                "allRequiredPassed":true,
                "verificationSpecDigest":specification_digest,
                "assertions":[{
                    "id":"preserve",
                    "assertionType":"exact_equal",
                    "severity":"required",
                    "passed":true,
                    "observed":true,
                    "message":"required exact equality passed"
                }]
            }),
            is_error: false,
            failure: None,
        };
        assert!(machine.admit_image_tool(&call).is_err());
        machine
            .record_image_tool_result(&call, &result, &[])
            .expect("unrelated result is ignored");
        assert_eq!(machine.image_director.last_compare_passed, None);
        assert!(machine.image_director.completion_blocker().is_some());
    }

    #[test]
    fn provided_source_is_the_only_initial_pixel_authority() {
        let contract = image_contract(false);
        let source = contract
            .source
            .provided_artifact()
            .expect("provided source")
            .clone();
        let mut machine = SessionMachine::new(TaskBudget::default());
        machine
            .image_director
            .bind_contract(Some(&contract))
            .expect("contract");

        assert!(
            machine
                .admit_image_tool(&render_call("render-source", &source))
                .is_ok()
        );
        let foreign = image_artifact('b');
        let error = machine
            .admit_image_tool(&render_call("render-foreign", &foreign))
            .expect_err("foreign pixels must be rejected");
        assert_eq!(error.code, FailureCode::VerificationFailed);
        assert!(error.message.contains("provenance"));
    }

    #[test]
    fn successful_producer_extends_pixel_provenance_for_the_next_step() {
        let contract = image_contract(false);
        let source = contract
            .source
            .provided_artifact()
            .expect("provided source")
            .clone();
        let candidate = image_artifact('a');
        let call = render_call("render-first", &source);
        let mut machine = SessionMachine::new(TaskBudget::default());
        machine
            .image_director
            .bind_contract(Some(&contract))
            .expect("contract");
        machine.admit_image_tool(&call).expect("admit first render");
        let result = ToolResult {
            call_id: call.id.clone(),
            name: call.name.clone(),
            value: json!({}),
            is_error: false,
            failure: None,
        };
        machine
            .record_image_tool_result(&call, &result, std::slice::from_ref(&candidate))
            .expect("record producer result");
        assert!(
            machine
                .image_director
                .is_image_artifact_authorized(&candidate)
        );
        assert!(
            machine
                .admit_image_tool(&render_call("render-next", &candidate))
                .is_ok()
        );
    }

    #[test]
    fn generate_new_must_establish_a_source_before_deterministic_editing() {
        let mut contract = image_contract(false);
        contract.source = ImageSource::GenerateNew;
        let mut machine = SessionMachine::new(TaskBudget::default());
        machine
            .image_director
            .bind_contract(Some(&contract))
            .expect("contract");
        let unbound = image_artifact('a');
        assert!(
            machine
                .admit_image_tool(&render_call("render-before-generate", &unbound))
                .is_err()
        );

        let generate = ToolCall {
            id: ToolCallId::parse("generate-root").expect("call id"),
            name: "image_generate".to_owned(),
            arguments: json!({"prompt":"fixture"}),
        };
        machine
            .admit_image_tool(&generate)
            .expect("admit generation");
        let generated = image_artifact('e');
        let result = ToolResult {
            call_id: generate.id.clone(),
            name: generate.name.clone(),
            value: json!({
                "generationStatus": "generated_needs_repair",
                "repairRequired": true,
                "repairRequirement": {
                    "width": 1024,
                    "height": 1024,
                    "transparentBackground": true
                },
                "agentMessage": "이미지 생성만 성공했습니다. PerfectPixel 수리·검증이 필요합니다."
            }),
            is_error: false,
            failure: None,
        };
        machine
            .record_image_tool_result(&generate, &result, std::slice::from_ref(&generated))
            .expect("record generated source");
        assert!(
            machine
                .image_director
                .completion_blocker()
                .is_some_and(|message| message.contains("PerfectPixel"))
        );
        let wrong_repair = image_artifact('d');
        let wrong_render = render_call("render-wrong-repair", &generated);
        let wrong_render_result = ToolResult {
            call_id: wrong_render.id.clone(),
            name: wrong_render.name.clone(),
            value: json!({
                "perfectpixelVerified": true,
                "repairObservation": {
                    "width": 512,
                    "height": 512,
                    "transparentBackground": true
                }
            }),
            is_error: false,
            failure: None,
        };
        machine
            .record_image_tool_result(
                &wrong_render,
                &wrong_render_result,
                std::slice::from_ref(&wrong_repair),
            )
            .expect("record nonconforming PerfectPixel candidate");
        assert!(machine.image_director.pending_repair.is_some());
        let repaired = image_artifact('f');
        let render = render_call("render-repaired", &generated);
        let render_result = ToolResult {
            call_id: render.id.clone(),
            name: render.name.clone(),
            value: json!({
                "perfectpixelVerified": true,
                "repairObservation": {
                    "width": 1024,
                    "height": 1024,
                    "transparentBackground": true
                }
            }),
            is_error: false,
            failure: None,
        };
        machine
            .record_image_tool_result(&render, &render_result, std::slice::from_ref(&repaired))
            .expect("record PerfectPixel repair");
        assert!(machine.image_director.pending_repair.is_none());
        assert!(
            machine
                .admit_image_tool(&render_call("render-after-generate", &generated))
                .is_ok()
        );
    }

    #[test]
    fn task_bound_compare_admission_rejects_exact_spec_drift() {
        let mut contract = image_contract(true);
        contract
            .verification
            .deterministic
            .as_mut()
            .expect("specification")
            .assertions
            .push(ImageCompareAssertion::MaximumChannelError {
                id: "channel_error".to_owned(),
                severity: ImageCompareSeverity::Required,
                maximum: 0,
            });
        let specification = contract
            .verification
            .deterministic
            .clone()
            .expect("specification");
        let candidate = image_artifact('a');
        let mut base = SessionMachine::new(TaskBudget::default());
        base.image_director
            .bind_contract(Some(&contract))
            .expect("contract");
        base.image_director
            .record_candidate([candidate.clone()], true)
            .expect("candidate");

        let matching = compare_call("compare-matching", &specification, &candidate);
        let mut matching_machine = base.clone();
        assert!(matching_machine.admit_image_tool(&matching).is_ok());

        let mut wrong_before = compare_call("compare-before", &specification, &candidate);
        wrong_before.arguments["beforeArtifact"] = json!(image_artifact('b'));
        assert!(base.clone().admit_image_tool(&wrong_before).is_err());

        let mut wrong_kind = compare_call("compare-kind", &specification, &candidate);
        wrong_kind.arguments["afterKind"] = json!("object");
        assert!(base.clone().admit_image_tool(&wrong_kind).is_err());

        let mut omitted = compare_call("compare-omitted", &specification, &candidate);
        omitted.arguments["assertions"]
            .as_array_mut()
            .expect("assertions")
            .pop();
        assert!(base.clone().admit_image_tool(&omitted).is_err());

        let mut reordered = compare_call("compare-reordered", &specification, &candidate);
        reordered.arguments["assertions"]
            .as_array_mut()
            .expect("assertions")
            .reverse();
        assert!(base.clone().admit_image_tool(&reordered).is_err());

        let mut weakened = contract;
        weakened
            .verification
            .deterministic
            .as_mut()
            .expect("specification")
            .assertions[0] = ImageCompareAssertion::ChangedRatio {
            id: "preserve".to_owned(),
            severity: ImageCompareSeverity::Required,
            minimum: Some(0.1),
            maximum: Some(0.2),
        };
        let exact = weakened
            .verification
            .deterministic
            .clone()
            .expect("specification");
        let mut threshold_machine = SessionMachine::new(TaskBudget::default());
        threshold_machine
            .image_director
            .bind_contract(Some(&weakened))
            .expect("contract");
        threshold_machine
            .image_director
            .record_candidate([candidate.clone()], true)
            .expect("candidate");
        let mut threshold_call = compare_call("compare-threshold", &exact, &candidate);
        threshold_call.arguments["assertions"][0]["maximum"] = json!(0.3);
        assert!(threshold_machine.admit_image_tool(&threshold_call).is_err());
    }

    #[test]
    fn task_bound_compare_rejects_raster_candidate_for_object_contract() {
        let mut contract = image_contract(true);
        contract
            .verification
            .deterministic
            .as_mut()
            .expect("specification")
            .after_kind = ImageCompareArtifactKind::Object;
        let specification = contract
            .verification
            .deterministic
            .clone()
            .expect("specification");
        let candidate = image_artifact('a');
        let mut machine = SessionMachine::new(TaskBudget::default());
        machine
            .image_director
            .bind_contract(Some(&contract))
            .expect("contract");
        let render_call = ToolCall {
            id: ToolCallId::parse("render-object-kind").expect("call id"),
            name: "image_render".to_owned(),
            arguments: json!({}),
        };
        let render_result = ToolResult {
            call_id: render_call.id.clone(),
            name: render_call.name.clone(),
            value: json!({"artifactRoles":["render"]}),
            is_error: false,
            failure: None,
        };
        machine
            .record_image_tool_result(
                &render_call,
                &render_result,
                std::slice::from_ref(&candidate),
            )
            .expect("record render candidate");

        let call = compare_call("compare-object-kind", &specification, &candidate);
        let error = machine
            .admit_image_tool(&call)
            .expect_err("raster candidate must not satisfy object contract");
        assert_eq!(error.code, FailureCode::VerificationFailed);
        assert!(error.message.contains("artifact and kind"));
    }

    #[test]
    fn stale_same_hash_and_duplicate_compare_results_do_not_mutate_current_revision() {
        let contract = image_contract(true);
        let specification = contract
            .verification
            .deterministic
            .clone()
            .expect("specification");
        let candidate = image_artifact('a');
        let mut machine = SessionMachine::new(TaskBudget::default());
        machine
            .image_director
            .bind_contract(Some(&contract))
            .expect("contract");
        machine
            .image_director
            .record_candidate([candidate.clone()], true)
            .expect("candidate");
        let stale_call = compare_call("compare-stale", &specification, &candidate);
        let stale_result = compare_result(&stale_call, &specification, true);
        machine
            .admit_image_tool(&stale_call)
            .expect("admitted stale compare");
        machine
            .image_director
            .record_candidate([candidate.clone()], true)
            .expect("candidate");
        machine
            .record_image_tool_result(&stale_call, &stale_result, &[])
            .expect("stale result ignored");
        assert_eq!(machine.image_director.candidate_revision, 2);
        assert_eq!(machine.image_director.last_compare_passed, None);

        let current_call = compare_call("compare-current", &specification, &candidate);
        let current_result = compare_result(&current_call, &specification, true);
        machine
            .admit_image_tool(&current_call)
            .expect("admitted current compare");
        machine
            .record_image_tool_result(&current_call, &current_result, &[])
            .expect("current result");
        let accepted_state = machine.image_director.clone();
        machine
            .record_image_tool_result(&current_call, &current_result, &[])
            .expect("duplicate result ignored");
        assert_eq!(machine.image_director, accepted_state);
    }

    #[test]
    fn pending_compare_prevents_checkpoint_suspension() {
        let contract = image_contract(true);
        let specification = contract
            .verification
            .deterministic
            .clone()
            .expect("specification");
        let candidate = image_artifact('a');
        let mut machine = SessionMachine::new(TaskBudget::default());
        machine
            .image_director
            .bind_contract(Some(&contract))
            .expect("contract");
        machine
            .image_director
            .record_candidate([candidate.clone()], true)
            .expect("candidate");
        let call = compare_call("compare-pending", &specification, &candidate);
        machine
            .admit_image_tool(&call)
            .expect("admitted pending compare");
        let error = machine
            .suspended_snapshot()
            .expect_err("pending compare must not enter a checkpoint");
        assert_eq!(error.code, FailureCode::InternalInvariant);
        assert_eq!(
            error.message,
            "cannot suspend while image comparisons are pending"
        );
    }

    #[test]
    fn malformed_or_failed_compare_never_records_pass() {
        let contract = image_contract(true);
        let specification = contract
            .verification
            .deterministic
            .clone()
            .expect("specification");
        let candidate = image_artifact('a');
        let mut machine = SessionMachine::new(TaskBudget::default());
        machine
            .image_director
            .bind_contract(Some(&contract))
            .expect("contract");
        machine
            .image_director
            .record_candidate([candidate.clone()], true)
            .expect("candidate");

        let malformed_call = compare_call("compare-malformed", &specification, &candidate);
        let mut malformed_result = compare_result(&malformed_call, &specification, true);
        malformed_result.value["assertions"][0]["assertionType"] = json!("changed_ratio");
        machine
            .admit_image_tool(&malformed_call)
            .expect("admitted malformed compare");
        let error = machine
            .record_image_tool_result(&malformed_call, &malformed_result, &[])
            .expect_err("malformed result must fail");
        assert_eq!(error.code, FailureCode::InternalInvariant);
        assert_eq!(machine.image_director.last_compare_passed, None);

        let wrong_digest_call = compare_call("compare-wrong-digest", &specification, &candidate);
        let mut wrong_digest_result = compare_result(&wrong_digest_call, &specification, true);
        wrong_digest_result.value["verificationSpecDigest"] = json!("f".repeat(64));
        machine
            .admit_image_tool(&wrong_digest_call)
            .expect("admitted wrong-digest compare");
        let error = machine
            .record_image_tool_result(&wrong_digest_call, &wrong_digest_result, &[])
            .expect_err("wrong result digest must fail");
        assert_eq!(error.code, FailureCode::InternalInvariant);
        assert_eq!(machine.image_director.last_compare_passed, None);

        let failed_call = compare_call("compare-error", &specification, &candidate);
        machine
            .admit_image_tool(&failed_call)
            .expect("admitted failed compare");
        let mut failed_result = compare_result(&failed_call, &specification, true);
        failed_result.is_error = true;
        machine
            .record_image_tool_result(&failed_call, &failed_result, &[])
            .expect("tool failure cleanup");
        let passing_result = compare_result(&failed_call, &specification, true);
        machine
            .record_image_tool_result(&failed_call, &passing_result, &[])
            .expect("late result ignored");
        assert_eq!(machine.image_director.last_compare_passed, None);
    }

    #[test]
    fn visual_only_task_compare_is_diagnostic_and_does_not_change_qualification() {
        let contract = image_contract(false);
        let specification = image_contract(true)
            .verification
            .deterministic
            .expect("specification");
        let candidate = image_artifact('a');
        let mut machine = SessionMachine::new(TaskBudget::default());
        machine
            .image_director
            .bind_contract(Some(&contract))
            .expect("contract");
        machine
            .image_director
            .record_candidate([candidate.clone()], true)
            .expect("candidate");
        let call = compare_call("compare-diagnostic", &specification, &candidate);
        let result = compare_result(&call, &specification, false);
        machine
            .record_image_tool_result(&call, &result, &[])
            .expect("diagnostic result");
        assert_eq!(machine.image_director.last_compare_passed, None);
        assert_eq!(
            machine.image_director.failed_assertions,
            [] as [std::string::String; 0]
        );
    }
}

pub(super) fn terminal_result(
    task: &TaskEnvelope,
    machine: &SessionMachine,
    status: AgentStatus,
    receipts: Vec<EffectReceiptRef>,
    failure: Option<AgentFailure>,
) -> AgentResult {
    AgentResult {
        schema_version: agent_core::PROTOCOL_VERSION,
        task_id: task.task_id.clone(),
        session_id: task.session_id.clone(),
        profile: task.profile.clone(),
        track: task.track.clone(),
        state: machine.state,
        status,
        output: None,
        artifacts: machine.artifacts(),
        receipts,
        composition_digest: task
            .context
            .get("_upagent_composition_digest")
            .and_then(Value::as_str)
            .map(str::to_owned),
        review_subject_bundle: None,
        approval: None,
        failure,
        metrics: machine.metrics(),
    }
}

pub(super) fn finish_result(
    journal: &mut SessionJournal,
    mut result: AgentResult,
    irreversible_effect_observed: bool,
) -> AgentResult {
    if journal.is_poisoned() {
        return undurable_result_durability_unknown(result, &JournalError::Poisoned);
    }
    if let Err(error) = journal.finish(&result) {
        if error.durability_unknown() {
            return undurable_result_durability_unknown(result, &error);
        }
        let (state, status, failure) = terminal_record_failure(
            result.status,
            irreversible_effect_observed,
            error.to_string(),
        );
        result.state = state;
        result.status = status;
        result.failure = Some(failure);
    }
    result
}

fn terminal_record_failure(
    original_status: AgentStatus,
    irreversible_effect_observed: bool,
    cause: String,
) -> (TaskState, AgentStatus, AgentFailure) {
    let original_status_text = format!("{original_status:?}").to_lowercase();
    if irreversible_effect_observed || original_status == AgentStatus::ResolutionRequired {
        return (
            TaskState::ResolutionRequired,
            AgentStatus::ResolutionRequired,
            AgentFailure::new(
                FailureCode::ResolutionRequired,
                "terminal result could not be durably recorded after an irreversible effect",
            )
            .with_detail("cause", cause)
            .with_detail("originalStatus", original_status_text),
        );
    }
    (
        TaskState::Failed,
        AgentStatus::Failed,
        AgentFailure::new(
            FailureCode::StorageFailed,
            "terminal result could not be durably recorded",
        )
        .with_detail("cause", cause)
        .with_detail("originalStatus", original_status_text),
    )
}

pub(super) fn undurable_failure(task: &TaskEnvelope, failure: AgentFailure) -> AgentResult {
    let (state, status) = match failure.code {
        FailureCode::Cancelled => (TaskState::Cancelled, AgentStatus::Cancelled),
        FailureCode::ResolutionRequired => (
            TaskState::ResolutionRequired,
            AgentStatus::ResolutionRequired,
        ),
        _ => (TaskState::Failed, AgentStatus::Failed),
    };
    AgentResult {
        schema_version: agent_core::PROTOCOL_VERSION,
        task_id: task.task_id.clone(),
        session_id: task.session_id.clone(),
        profile: task.profile.clone(),
        track: task.track.clone(),
        state,
        status,
        output: None,
        artifacts: Vec::new(),
        receipts: Vec::new(),
        composition_digest: task
            .context
            .get("_upagent_composition_digest")
            .and_then(Value::as_str)
            .map(str::to_owned),
        review_subject_bundle: None,
        approval: None,
        failure: Some(failure),
        metrics: BTreeMap::new(),
    }
}

fn undurable_result_durability_unknown(
    mut result: AgentResult,
    error: &JournalError,
) -> AgentResult {
    result.state = TaskState::ResolutionRequired;
    result.status = AgentStatus::ResolutionRequired;
    result.output = None;
    // The machine's in-memory artifact set is not a durable result projection
    // after an unknown journal commit.  Preserve only already-issued RAX
    // receipts, which retain their independent effect-recovery meaning.
    result.artifacts.clear();
    result.review_subject_bundle = None;
    result.failure = Some(durability_unknown_failure(error.to_string()));
    result
}

pub(super) fn undurable_durability_unknown(
    task: &TaskEnvelope,
    machine: &SessionMachine,
    cause: &AgentFailure,
) -> AgentResult {
    AgentResult {
        schema_version: agent_core::PROTOCOL_VERSION,
        task_id: task.task_id.clone(),
        session_id: task.session_id.clone(),
        profile: task.profile.clone(),
        track: task.track.clone(),
        state: TaskState::ResolutionRequired,
        status: AgentStatus::ResolutionRequired,
        output: None,
        artifacts: Vec::new(),
        receipts: machine.receipts.clone(),
        composition_digest: task
            .context
            .get("_upagent_composition_digest")
            .and_then(Value::as_str)
            .map(str::to_owned),
        review_subject_bundle: None,
        approval: None,
        failure: Some(durability_unknown_failure(cause.to_string())),
        metrics: machine.metrics(),
    }
}

fn durability_unknown_failure(cause: String) -> AgentFailure {
    AgentFailure::new(
        FailureCode::ResolutionRequired,
        "journal durability is unknown; recover the task before retrying",
    )
    .with_detail("durability", "unknown")
    .with_detail("recovery", "agent recover task-list")
    .with_detail("cause", cause)
}

fn is_durability_unknown(failure: &AgentFailure) -> bool {
    failure.details.get("durability").and_then(Value::as_str) == Some("unknown")
}

pub(super) fn undurable_suspension(task: &TaskEnvelope, failure: AgentFailure) -> AgentResult {
    AgentResult {
        schema_version: agent_core::PROTOCOL_VERSION,
        task_id: task.task_id.clone(),
        session_id: task.session_id.clone(),
        profile: task.profile.clone(),
        track: task.track.clone(),
        state: TaskState::Suspended,
        status: AgentStatus::Suspended,
        output: None,
        artifacts: Vec::new(),
        receipts: Vec::new(),
        composition_digest: task
            .context
            .get("_upagent_composition_digest")
            .and_then(Value::as_str)
            .map(str::to_owned),
        review_subject_bundle: None,
        approval: None,
        failure: Some(failure.retryable(true)),
        metrics: BTreeMap::new(),
    }
}

#[allow(clippy::needless_pass_by_value)]
pub(super) fn undurable_resolution(
    task: &TaskEnvelope,
    machine: &SessionMachine,
    receipts: Vec<EffectReceiptRef>,
    record_failure: AgentFailure,
) -> AgentResult {
    if is_durability_unknown(&record_failure) {
        return undurable_durability_unknown(task, machine, &record_failure);
    }
    let record_failure_message = record_failure.message.clone();
    let record_failure_code = record_failure.code.as_str();
    let failure = AgentFailure::new(
        FailureCode::ResolutionRequired,
        "an effect completed but its post-effect journal record failed",
    )
    .with_detail("recordFailure", record_failure_message)
    .with_detail("recordFailureCode", record_failure_code);
    let mut result = terminal_result(
        task,
        machine,
        AgentStatus::ResolutionRequired,
        receipts,
        Some(failure),
    );
    result.state = TaskState::ResolutionRequired;
    result
}

#[allow(clippy::needless_pass_by_value)]
pub(super) fn resolution_after_effect_failure(
    task: &TaskEnvelope,
    machine: &mut SessionMachine,
    journal: &mut SessionJournal,
    cause: AgentFailure,
) -> AgentResult {
    let failure = post_effect_failure(&cause);
    if let Err(record_failure) = machine.record(
        journal,
        KernelEvent::ResolutionRequired(failure.clone()),
        Value::Null,
    ) {
        return undurable_resolution(task, machine, machine.receipts.clone(), record_failure);
    }
    let result = terminal_result(
        task,
        machine,
        AgentStatus::ResolutionRequired,
        machine.receipts.clone(),
        Some(failure),
    );
    finish_result(journal, result, true)
}

fn post_effect_failure(cause: &AgentFailure) -> AgentFailure {
    let mut failure = AgentFailure::new(
        FailureCode::ResolutionRequired,
        "a side effect completed but post-effect processing failed",
    )
    .with_detail("cause", cause.message.clone())
    .with_detail("causeCode", cause.code.as_str());
    // Carry the diagnostic cause, not arbitrary tool arguments or payloads.
    if let Some(detail) = cause.details.get("cause").and_then(Value::as_str) {
        failure = failure.with_detail(
            "causeDetails",
            json!({"cause": detail.chars().take(1024).collect::<String>()}),
        );
    }
    failure
}

#[allow(clippy::needless_pass_by_value)]
fn journal_failure(error: JournalError) -> AgentFailure {
    if error.durability_unknown() {
        durability_unknown_failure(error.to_string())
    } else {
        AgentFailure::new(FailureCode::StorageFailed, error.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_core::{EffectApprovalMode, ProfileId, SessionId, TaskId};

    #[test]
    fn post_effect_failure_preserves_only_bounded_diagnostic_cause() {
        let cause = AgentFailure::new(FailureCode::ResolutionRequired, "validation failed")
            .with_detail("cause", "edge palette mismatch")
            .with_detail("toolArguments", json!({"prompt":"private-input"}));
        let failure = post_effect_failure(&cause);
        assert_eq!(
            failure.details["causeDetails"]["cause"],
            "edge palette mismatch"
        );
        assert!(
            !serde_json::to_string(&failure)
                .unwrap()
                .contains("private-input")
        );
        assert!(!failure.retryable);
        let oversized = cause.with_detail("cause", "가".repeat(2048));
        assert_eq!(
            post_effect_failure(&oversized).details["causeDetails"]["cause"]
                .as_str()
                .unwrap()
                .chars()
                .count(),
            1024
        );
    }

    #[test]
    fn terminal_record_failure_only_requires_resolution_after_irreversible_effects() {
        let (state, status, failure) =
            terminal_record_failure(AgentStatus::Succeeded, false, "disk full".to_owned());
        assert_eq!(state, TaskState::Failed);
        assert_eq!(status, AgentStatus::Failed);
        assert_eq!(failure.code, FailureCode::StorageFailed);

        let (state, status, failure) =
            terminal_record_failure(AgentStatus::Succeeded, true, "disk full".to_owned());
        assert_eq!(state, TaskState::ResolutionRequired);
        assert_eq!(status, AgentStatus::ResolutionRequired);
        assert_eq!(failure.code, FailureCode::ResolutionRequired);
    }

    #[test]
    fn parent_sync_unknown_cannot_project_terminal_success()
    -> Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let state = temp.path().join("state");
        std::fs::create_dir_all(state.join("artifacts"))?;
        let task = TaskEnvelope {
            schema_version: agent_core::PROTOCOL_VERSION,
            task_id: TaskId::parse("task-machine-parent-sync")?,
            session_id: SessionId::parse("session-machine-parent-sync")?,
            parent_task_id: None,
            profile: ProfileId::creator(),
            track: None,
            objective: "parent sync projection".to_owned(),
            image_task: None,
            history: Vec::new(),
            workspace: None,
            skills: Vec::new(),
            context: BTreeMap::new(),
            budget: TaskBudget::default(),
            effect_approval: EffectApprovalMode::Deny,
            allow_memory_write: false,
            approvals: Vec::new(),
        };
        let mut journal = SessionJournal::create(&state, &task)?;
        let output_artifact = crate::ArtifactStore::open(state.join("artifacts"))?
            .put_text("terminal output", "text/plain")?;
        journal.append(
            TaskState::Created,
            TaskState::Ready,
            &KernelEvent::Validated,
            Value::Null,
        )?;
        journal.append(
            TaskState::Ready,
            TaskState::Running,
            &KernelEvent::Started,
            Value::Null,
        )?;
        journal.append(
            TaskState::Running,
            TaskState::Succeeded,
            &KernelEvent::Completed,
            json!({"outputArtifact": &output_artifact}),
        )?;
        let result = AgentResult {
            schema_version: agent_core::PROTOCOL_VERSION,
            task_id: task.task_id.clone(),
            session_id: task.session_id.clone(),
            profile: task.profile.clone(),
            track: task.track.clone(),
            state: TaskState::Succeeded,
            status: AgentStatus::Succeeded,
            output: None,
            artifacts: vec![output_artifact],
            receipts: Vec::new(),
            composition_digest: None,
            review_subject_bundle: None,
            approval: None,
            failure: None,
            metrics: BTreeMap::new(),
        };
        SessionJournal::inject_parent_sync_failure_for_test(&state);
        let projected = finish_result(&mut journal, result, false);
        assert_eq!(projected.state, TaskState::ResolutionRequired);
        assert_eq!(projected.status, AgentStatus::ResolutionRequired);
        assert_eq!(
            projected
                .failure
                .as_ref()
                .and_then(|failure| failure.details.get("durability"))
                .and_then(Value::as_str),
            Some("unknown")
        );
        drop(journal);
        assert!(crate::journal::list_incomplete(&state)?.is_empty());
        Ok(())
    }
}
