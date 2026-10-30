use agent_core::{
    AgentFailure, AgentResult, AgentStatus, ApprovalRequest, ArtifactRef, EffectReceiptRef,
    FailureCode, KernelEvent, ModelMessage, TaskEnvelope, ToolCall, ToolResult,
};
use agent_effects::EffectExecution;
use serde_json::{Value, json};

use crate::tool_authority::{is_mutating_tool, is_source_mutating_tool};
use crate::{
    ArtifactStore, ModuleExecution, ToolSideEffect, journal::SessionJournal, memory_tools,
};

use super::machine::{
    SessionMachine, finish_failure, finish_result, resolution_after_effect_failure,
    terminal_result, undurable_resolution,
};
use super::tool_dispatch::ToolExecution;
use super::tool_output::{persist_module_artifacts, prepare_tool_message};

#[allow(
    clippy::large_enum_variant,
    reason = "ToolProgress is an internal control-flow enum and boxing would add churn without changing the protocol"
)]
pub(super) enum ToolProgress {
    Continue(ModelMessage),
    Finished(AgentResult),
}

#[derive(Clone, Copy)]
struct ToolCompletionEffects {
    recovery_required: bool,
    source_mutation: bool,
}

pub(super) fn handle_tool_execution(
    artifacts: &ArtifactStore,
    task: &TaskEnvelope,
    call: &ToolCall,
    execution: ToolExecution,
    machine: &mut SessionMachine,
    journal: &mut SessionJournal,
) -> ToolProgress {
    match execution {
        ToolExecution::InputRequest(input) => input_required(task, input, machine, journal),
        ToolExecution::Effect(EffectExecution::ApprovalRequired(approval))
        | ToolExecution::ExtensionApproval(approval) => {
            machine.cancel_unstarted_image_effect(call);
            approval_required(task, approval, machine, journal)
        }
        ToolExecution::Effect(EffectExecution::ResolutionRequired { result, receipt }) => {
            if let Some(receipt) = receipt {
                machine.receipts.push(receipt);
            }
            let failure = result.failure.unwrap_or_else(|| {
                AgentFailure::new(
                    FailureCode::ResolutionRequired,
                    "effect state requires manual resolution",
                )
            });
            resolution_required(task, call, failure, &[], machine, journal)
        }
        ToolExecution::Effect(EffectExecution::Completed { result, receipt }) => {
            handle_effect_result(artifacts, task, call, &result, receipt, machine, journal)
        }
        ToolExecution::Memory(value) => {
            handle_memory_result(artifacts, task, call, value, machine, journal)
        }
        ToolExecution::Extension(value, side_effect) => {
            handle_extension_result(artifacts, task, call, value, side_effect, machine, journal)
        }
        ToolExecution::ExtensionInterrupted(failure, side_effect) => {
            if call.name == "image_extract"
                && let Err(error) = machine.cancel_semantic_mask(call, failure.clone())
            {
                return ToolProgress::Finished(finish_failure(task, machine, journal, error));
            }
            if side_effect.recovery_required_after_record_failure() {
                machine.irreversible_effect_observed = true;
                ToolProgress::Finished(resolution_after_effect_failure(
                    task, machine, journal, failure,
                ))
            } else {
                ToolProgress::Finished(finish_failure(task, machine, journal, failure))
            }
        }
        ToolExecution::Plain(result) => complete_tool_result(
            artifacts,
            task,
            call,
            &result,
            Vec::new(),
            ToolCompletionEffects {
                recovery_required: false,
                source_mutation: false,
            },
            machine,
            journal,
        ),
    }
}

fn input_required(
    task: &TaskEnvelope,
    input: crate::interaction_tools::InputRequest,
    machine: &mut SessionMachine,
    journal: &mut SessionJournal,
) -> ToolProgress {
    if let Err(error) = machine.record(
        journal,
        KernelEvent::InputRequired,
        json!({"question":input.question.as_str(),"options":&input.options}),
    ) {
        return ToolProgress::Finished(finish_failure(task, machine, journal, error));
    }
    let output = if input.options.is_empty() {
        input.question
    } else {
        let options = input
            .options
            .iter()
            .enumerate()
            .map(|(index, option)| format!("{}. {option}", index + 1))
            .collect::<Vec<_>>()
            .join("\n");
        format!("{}\n\n{options}", input.question)
    };
    let result = AgentResult {
        schema_version: agent_core::PROTOCOL_VERSION,
        task_id: task.task_id.clone(),
        session_id: task.session_id.clone(),
        profile: task.profile.clone(),
        track: task.track.clone(),
        state: machine.state,
        status: AgentStatus::AwaitingInput,
        output: Some(output),
        artifacts: machine.artifacts(),
        receipts: machine.receipts.clone(),
        composition_digest: task
            .context
            .get("_upagent_composition_digest")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned),
        review_subject_bundle: None,
        approval: None,
        failure: None,
        metrics: machine.metrics(),
    };
    ToolProgress::Finished(finish_result(
        journal,
        result,
        machine.irreversible_effect_observed,
    ))
}

fn approval_required(
    task: &TaskEnvelope,
    approval: ApprovalRequest,
    machine: &mut SessionMachine,
    journal: &mut SessionJournal,
) -> ToolProgress {
    if let Err(error) = machine.record(
        journal,
        KernelEvent::ApprovalRequired,
        json!({"planId":approval.plan_id.as_str(),"fingerprint":approval.fingerprint.as_str()}),
    ) {
        return ToolProgress::Finished(finish_failure(task, machine, journal, error));
    }
    let result = AgentResult {
        schema_version: agent_core::PROTOCOL_VERSION,
        task_id: task.task_id.clone(),
        session_id: task.session_id.clone(),
        profile: task.profile.clone(),
        track: task.track.clone(),
        state: machine.state,
        status: AgentStatus::AwaitingApproval,
        output: None,
        artifacts: machine.artifacts(),
        receipts: machine.receipts.clone(),
        composition_digest: task
            .context
            .get("_upagent_composition_digest")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned),
        review_subject_bundle: None,
        approval: Some(approval),
        failure: None,
        metrics: machine.metrics(),
    };
    ToolProgress::Finished(finish_result(
        journal,
        result,
        machine.irreversible_effect_observed,
    ))
}

fn resolution_required(
    task: &TaskEnvelope,
    call: &ToolCall,
    failure: AgentFailure,
    evidence: &[agent_core::ArtifactRef],
    machine: &mut SessionMachine,
    journal: &mut SessionJournal,
) -> ToolProgress {
    if let Err(error) = machine.record(
        journal,
        KernelEvent::ResolutionRequired(failure.clone()),
        json!({"callId":call.id.as_str(),"name":call.name.as_str(),"artifacts":evidence}),
    ) {
        return ToolProgress::Finished(undurable_resolution(
            task,
            machine,
            machine.receipts.clone(),
            error,
        ));
    }
    let result = terminal_result(
        task,
        machine,
        AgentStatus::ResolutionRequired,
        machine.receipts.clone(),
        Some(failure),
    );
    ToolProgress::Finished(finish_result(
        journal,
        result,
        machine.irreversible_effect_observed,
    ))
}

fn handle_effect_result(
    artifacts: &ArtifactStore,
    task: &TaskEnvelope,
    call: &ToolCall,
    result: &ToolResult,
    receipt: Option<EffectReceiptRef>,
    machine: &mut SessionMachine,
    journal: &mut SessionJournal,
) -> ToolProgress {
    if let Some(receipt) = receipt {
        machine.receipts.push(receipt);
    }
    if let Some(failure) = result.failure.as_ref()
        && failure.code == FailureCode::Cancelled
    {
        return ToolProgress::Finished(finish_failure(task, machine, journal, failure.clone()));
    }
    // RAX maps any uncertain or partially applied mutation to
    // ResolutionRequired. A typed Completed error therefore did not prove a
    // durable mutation. Only successful mutating operations establish this
    // recovery obligation.
    let recovery_required = is_mutating_tool(&call.name) && !result.is_error;
    let source_mutation = is_source_mutating_tool(&call.name) && !result.is_error;
    machine.irreversible_effect_observed |= recovery_required;
    complete_tool_result(
        artifacts,
        task,
        call,
        result,
        Vec::new(),
        ToolCompletionEffects {
            recovery_required,
            source_mutation,
        },
        machine,
        journal,
    )
}

fn handle_memory_result(
    artifacts: &ArtifactStore,
    task: &TaskEnvelope,
    call: &ToolCall,
    value: memory_tools::MemoryToolExecution,
    machine: &mut SessionMachine,
    journal: &mut SessionJournal,
) -> ToolProgress {
    let memory_tools::MemoryToolExecution {
        result,
        receipt,
        durable_effect,
        resolution_required: requires_resolution,
    } = value;
    if let Some(receipt) = receipt {
        machine.receipts.push(receipt);
    }
    machine.irreversible_effect_observed |= durable_effect;
    if requires_resolution {
        let cause = result.failure.clone().unwrap_or_else(|| {
            AgentFailure::new(
                FailureCode::MemoryFailed,
                "memory mutation completed but its result could not be finalized",
            )
        });
        let failure = AgentFailure::new(
            FailureCode::ResolutionRequired,
            "memory state changed but post-effect processing failed",
        )
        .with_detail("cause", cause.message)
        .with_detail("causeCode", cause.code.as_str());
        return resolution_required(task, call, failure, &[], machine, journal);
    }
    complete_tool_result(
        artifacts,
        task,
        call,
        &result,
        Vec::new(),
        ToolCompletionEffects {
            recovery_required: durable_effect,
            source_mutation: false,
        },
        machine,
        journal,
    )
}

fn handle_extension_result(
    artifacts: &ArtifactStore,
    task: &TaskEnvelope,
    call: &ToolCall,
    mut value: ModuleExecution,
    side_effect: ToolSideEffect,
    machine: &mut SessionMachine,
    journal: &mut SessionJournal,
) -> ToolProgress {
    let recovery_required = side_effect.recovery_required_after_record_failure();
    // Workspace mutation is a closed RAX boundary. Trusted extensions may be
    // read-only or external, but cannot register a second workspace writer.
    let source_mutation = false;
    machine.receipts.extend(value.receipts.iter().cloned());
    machine.irreversible_effect_observed |= side_effect.requires_approval();
    let stored_artifacts =
        match persist_module_artifacts(artifacts, std::mem::take(&mut value.artifacts)) {
            Ok(artifacts) => artifacts,
            Err(failure) => {
                return post_effect_failure(task, failure, recovery_required, machine, journal);
            }
        };
    if !stored_artifacts.is_empty() {
        let encoded = match serde_json::to_value(&stored_artifacts) {
            Ok(value) => value,
            Err(error) => {
                let failure = AgentFailure::new(
                    FailureCode::InternalInvariant,
                    "extension artifact references could not be encoded",
                )
                .with_detail("cause", error.to_string());
                return post_effect_failure(task, failure, recovery_required, machine, journal);
            }
        };
        value.result.value = match value.result.value {
            Value::Object(mut object) => {
                object.insert("_artifacts".to_owned(), encoded);
                Value::Object(object)
            }
            other => json!({"value":other,"_artifacts":encoded}),
        };
        // The terminal ToolCompleted event registers these exact references
        // with their call identity before the journal append.
    }
    if value.receipts.iter().any(|receipt| !receipt.verified) {
        let failure = value.result.failure.clone().unwrap_or_else(|| {
            AgentFailure::new(
                FailureCode::ResolutionRequired,
                "extension tool returned an unverified effect receipt",
            )
        });
        return resolution_required(task, call, failure, &stored_artifacts, machine, journal);
    }
    complete_tool_result(
        artifacts,
        task,
        call,
        &value.result,
        stored_artifacts,
        ToolCompletionEffects {
            recovery_required,
            source_mutation,
        },
        machine,
        journal,
    )
}

fn post_effect_failure(
    task: &TaskEnvelope,
    failure: AgentFailure,
    recovery_required: bool,
    machine: &mut SessionMachine,
    journal: &mut SessionJournal,
) -> ToolProgress {
    if recovery_required {
        ToolProgress::Finished(resolution_after_effect_failure(
            task, machine, journal, failure,
        ))
    } else {
        ToolProgress::Finished(finish_failure(task, machine, journal, failure))
    }
}

#[allow(clippy::too_many_arguments)]
fn complete_tool_result(
    artifacts: &ArtifactStore,
    task: &TaskEnvelope,
    call: &ToolCall,
    result: &ToolResult,
    additional_artifacts: Vec<ArtifactRef>,
    effects: ToolCompletionEffects,
    machine: &mut SessionMachine,
    journal: &mut SessionJournal,
) -> ToolProgress {
    let image_artifacts = additional_artifacts.clone();
    let (message, message_artifacts) =
        match prepare_tool_message(artifacts, result, &additional_artifacts) {
            Ok(message) => message,
            Err(failure) => {
                return post_effect_failure(
                    task,
                    failure,
                    effects.recovery_required,
                    machine,
                    journal,
                );
            }
        };
    let semantic_mask_event = match machine.semantic_mask_event_value(call, result) {
        Ok(value) => value,
        Err(failure) => {
            return post_effect_failure(task, failure, effects.recovery_required, machine, journal);
        }
    };
    if let Err(error) = machine.record(
        journal,
        KernelEvent::ToolCompleted,
        json!({
            "callId":call.id.as_str(),
            "name":call.name.as_str(),
            "isError":result.is_error,
            "requirementId":verification_requirement(call),
            "artifacts":additional_artifacts
                .into_iter()
                .chain(message_artifacts)
                .collect::<Vec<_>>(),
            "semanticMask": semantic_mask_event,
        }),
    ) {
        if effects.recovery_required {
            return ToolProgress::Finished(undurable_resolution(
                task,
                machine,
                machine.receipts.clone(),
                error,
            ));
        }
        return ToolProgress::Finished(finish_failure(task, machine, journal, error));
    }
    if let Err(error) = machine.record_image_tool_result(call, result, &image_artifacts) {
        return ToolProgress::Finished(finish_failure(task, machine, journal, error));
    }
    if !result.is_error {
        machine.mark_tool_success(
            &call.name,
            effects.source_mutation,
            verification_requirement(call),
        );
    }
    ToolProgress::Continue(message)
}

fn verification_requirement(call: &ToolCall) -> Option<&str> {
    call.arguments
        .get("requirementId")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
}
