use agent_core::{
    AgentFailure, ApprovalRequest, EffectReceiptRef, FailureCode, ReceiptId, ToolCall, ToolResult,
};
use rax_core::{Outcome, ResultEnvelope};
use serde_json::Value;
use sha2::{Digest, Sha256};

use super::{EffectError, EffectExecution};

pub(super) fn execution_from_result(
    call: ToolCall,
    result: ResultEnvelope,
) -> Result<EffectExecution, EffectError> {
    let receipt = receipt_from(&result)?;
    match result.outcome {
        Outcome::Succeeded { output, .. } => Ok(EffectExecution::Completed {
            result: ToolResult {
                call_id: call.id,
                name: call.name,
                value: serde_json::to_value(output)?,
                is_error: false,
                failure: None,
            },
            receipt,
        }),
        Outcome::PartiallyApplied { error, .. } => {
            let failure = failure_from_rax(error, FailureCode::ResolutionRequired);
            Ok(EffectExecution::ResolutionRequired {
                result: ToolResult {
                    call_id: call.id,
                    name: call.name,
                    value: Value::Null,
                    is_error: true,
                    failure: Some(failure),
                },
                receipt,
            })
        }
        Outcome::Cancelled { error, .. } => {
            let failure = failure_from_rax(error, FailureCode::Cancelled);
            Ok(EffectExecution::Completed {
                result: ToolResult {
                    call_id: call.id,
                    name: call.name,
                    value: Value::Null,
                    is_error: true,
                    failure: Some(failure),
                },
                receipt,
            })
        }
        Outcome::Failed { error, .. } | Outcome::Denied { error } | Outcome::Expired { error } => {
            let failure = failure_from_rax(error, FailureCode::EffectFailed);
            Ok(EffectExecution::Completed {
                result: ToolResult {
                    call_id: call.id,
                    name: call.name,
                    value: Value::Null,
                    is_error: true,
                    failure: Some(failure),
                },
                receipt,
            })
        }
        Outcome::ApprovalRequired { plan } => {
            let subject = plan.approval_subject().map_err(|error| {
                EffectError::Contract(format!(
                    "plan approval subject could not be computed: {error}"
                ))
            })?;
            Ok(EffectExecution::ApprovalRequired(approval_request(
                &plan, subject,
            )))
        }
    }
}

fn receipt_from(result: &ResultEnvelope) -> Result<Option<EffectReceiptRef>, EffectError> {
    let has_receipt = matches!(
        &result.outcome,
        Outcome::Succeeded {
            receipt_path: Some(_),
            ..
        } | Outcome::Failed {
            receipt_path: Some(_),
            ..
        } | Outcome::Cancelled {
            receipt_path: Some(_),
            ..
        } | Outcome::PartiallyApplied {
            receipt_path: Some(_),
            ..
        }
    );
    if !has_receipt {
        return Ok(None);
    }
    let encoded = serde_json::to_vec(result)?;
    Ok(Some(EffectReceiptRef {
        id: ReceiptId::parse(result.run_id.clone())
            .map_err(|error| EffectError::Contract(error.to_string()))?,
        digest: format!("{:x}", Sha256::digest(encoded)),
        operation: result.operation.clone(),
        verified: matches!(&result.outcome, Outcome::Succeeded { .. }),
    }))
}

pub(super) fn approval_request(plan: &rax_core::Plan, subject: String) -> ApprovalRequest {
    ApprovalRequest {
        plan_id: plan.plan_id.clone(),
        fingerprint: plan.fingerprint.clone(),
        subject,
        expires_at: plan.expires_at.clone(),
        effects: plan
            .effects
            .iter()
            .map(|decision| effect_class_name(decision.effect).to_owned())
            .collect(),
        operation: plan.operation.operation_name().to_owned(),
    }
}

const fn effect_class_name(effect: rax_core::EffectClass) -> &'static str {
    match effect {
        rax_core::EffectClass::ReadWorkspace => "read_workspace",
        rax_core::EffectClass::WriteWorkspace => "write_workspace",
        rax_core::EffectClass::DeleteWorkspace => "delete_workspace",
        rax_core::EffectClass::ExecuteProcess => "execute_process",
        rax_core::EffectClass::ExecuteShell => "execute_shell",
        rax_core::EffectClass::GitIndexMutation => "git_index_mutation",
        rax_core::EffectClass::GitWorktreeMutation => "git_worktree_mutation",
        rax_core::EffectClass::Network => "network",
    }
}

pub(super) fn denied_tool_result(call: &ToolCall, plan: &rax_core::Plan) -> ToolResult {
    let failure = AgentFailure::new(
        FailureCode::CapabilityDenied,
        "effect execution is disabled for this task",
    )
    .with_detail("planId", plan.plan_id.clone())
    .with_detail("fingerprint", plan.fingerprint.clone());
    ToolResult {
        call_id: call.id.clone(),
        name: call.name.clone(),
        value: Value::Null,
        is_error: true,
        failure: Some(failure),
    }
}

fn failure_from_rax(error: rax_core::RaxError, code: FailureCode) -> AgentFailure {
    let rax_core::RaxError {
        code: rax_code,
        phase,
        message,
        retryable,
        details,
    } = error;
    let mut failure = AgentFailure::new(code, message)
        .retryable(retryable)
        .with_detail("raxCode", format!("{rax_code:?}").to_lowercase())
        .with_detail("raxPhase", format!("{phase:?}").to_lowercase());
    for (key, value) in details {
        failure = failure.with_detail(format!("rax.{key}"), value);
    }
    failure
}
