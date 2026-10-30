use std::sync::Arc;

use agent_core::{AgentFailure, ApprovalRequest, FailureCode, TaskEnvelope, ToolCall, ToolResult};
use agent_effects::{EffectCancellationToken, EffectExecution};
use serde_json::json;

use crate::tool_authority::is_mutating_tool;
use crate::{
    ModuleExecution, RegisteredTool, TaskCancellation, ToolModuleError, ToolSideEffect,
    execute_extension, memory_tools,
};

use super::approval_gate::ExtensionGate;
use super::{
    AgentRuntime, PreparedTask, denied_tool, failed_tool, is_effect_tool, is_interaction_tool,
    is_memory_tool,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EffectInterruption {
    None,
    Cancelled,
    Deadline,
}

pub(super) enum ToolExecution {
    InputRequest(crate::interaction_tools::InputRequest),
    Effect(EffectExecution),
    Memory(memory_tools::MemoryToolExecution),
    Extension(ModuleExecution, ToolSideEffect),
    ExtensionInterrupted(AgentFailure, ToolSideEffect),
    ExtensionApproval(ApprovalRequest),
    Plain(ToolResult),
}

fn interruption_failure(interruption: EffectInterruption) -> Option<AgentFailure> {
    match interruption {
        EffectInterruption::None => None,
        EffectInterruption::Cancelled => Some(AgentFailure::new(
            FailureCode::Cancelled,
            "task was cancelled during effect execution",
        )),
        EffectInterruption::Deadline => Some(AgentFailure::new(
            FailureCode::BudgetExceeded,
            "effect execution exceeded the remaining task deadline",
        )),
    }
}

impl AgentRuntime {
    pub(super) async fn dispatch_tool(
        &self,
        task: &TaskEnvelope,
        prepared: &PreparedTask,
        call: &ToolCall,
        deadline: tokio::time::Instant,
        cancellation: &TaskCancellation,
        semantic_mask_operation: Option<agent_core::SemanticMaskOperation>,
    ) -> Result<ToolExecution, AgentFailure> {
        if is_interaction_tool(&call.name) {
            return Ok(match crate::interaction_tools::decode(call) {
                Ok(value) => ToolExecution::InputRequest(value),
                Err(failure) => ToolExecution::Plain(failed_tool(call, failure)),
            });
        }
        if is_effect_tool(&call.name) {
            validate_bound_check_call(prepared, call)?;
            return self
                .dispatch_effect(task, prepared, call, deadline, cancellation)
                .await;
        }
        if is_memory_tool(&call.name) {
            let scope = prepared.plan.memory_scope.as_deref().ok_or_else(|| {
                AgentFailure::new(
                    FailureCode::InternalInvariant,
                    "memory tool has no profile scope",
                )
            })?;
            // Memory mutation takes a file lock and rewrites the durable
            // snapshot, so it never runs on a runtime worker.
            let memory = self.inner.memory.clone().ok_or_else(|| {
                AgentFailure::new(
                    FailureCode::MemoryFailed,
                    "memory store is not enabled by the selected composition",
                )
            })?;
            let scope = scope.to_owned();
            let call = call.clone();
            let task_id = task.task_id.as_str().to_owned();
            let allow_write = task.allow_memory_write;
            let execution = tokio::task::spawn_blocking(move || {
                memory_tools::execute(&memory, &scope, &call, &task_id, allow_write)
            })
            .await
            .map_err(|error| {
                AgentFailure::new(
                    FailureCode::InternalInvariant,
                    "memory tool worker did not complete",
                )
                .with_detail("cause", error.to_string())
            })?;
            return Ok(ToolExecution::Memory(execution));
        }
        if let Some(registered) = self.inner.extension_tools.get(&call.name).cloned() {
            return self
                .dispatch_extension(
                    task,
                    call,
                    registered,
                    deadline,
                    cancellation,
                    prepared,
                    semantic_mask_operation,
                )
                .await;
        }
        Ok(ToolExecution::Plain(denied_tool(
            call,
            "tool has no execution adapter",
        )))
    }

    async fn dispatch_effect(
        &self,
        task: &TaskEnvelope,
        prepared: &PreparedTask,
        call: &ToolCall,
        deadline: tokio::time::Instant,
        cancellation: &TaskCancellation,
    ) -> Result<ToolExecution, AgentFailure> {
        let plane = prepared.effect_plane.as_ref().ok_or_else(|| {
            AgentFailure::new(
                FailureCode::InternalInvariant,
                "effect tool has no effect plane",
            )
        })?;
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return Err(AgentFailure::new(
                FailureCode::BudgetExceeded,
                "task deadline elapsed before the effect started",
            ));
        }
        let maximum_process_timeout_ms =
            u64::try_from(remaining.as_millis().max(1)).map_err(|_| {
                AgentFailure::new(
                    FailureCode::InternalInvariant,
                    "remaining effect timeout does not fit u64",
                )
            })?;
        let plane = Arc::clone(plane);
        let mode = task.effect_approval;
        let mutating = is_mutating_tool(&call.name);
        let call = call.clone();
        let approved = prepared.approved_subjects.clone();
        let effect_cancellation = EffectCancellationToken::default();
        let worker_cancellation = effect_cancellation.clone();
        let mut worker = tokio::task::spawn_blocking(move || {
            plane.execute_with_grants(
                call,
                mode,
                maximum_process_timeout_ms,
                &worker_cancellation,
                &approved,
            )
        });
        let (execution, interruption) = tokio::select! {
            biased;
            () = cancellation.cancelled() => {
                effect_cancellation.cancel();
                (worker.await, EffectInterruption::Cancelled)
            }
            () = tokio::time::sleep_until(deadline) => {
                effect_cancellation.cancel();
                (worker.await, EffectInterruption::Deadline)
            }
            result = &mut worker => (result, EffectInterruption::None),
        };
        let execution = execution.map_err(|error| {
            AgentFailure::new(FailureCode::InternalInvariant, "effect worker failed")
                .with_detail("cause", error.to_string())
        })?;
        let execution = match execution {
            Ok(value) => value,
            Err(error) => {
                return Err(interruption_failure(interruption).unwrap_or_else(|| {
                    AgentFailure::new(FailureCode::EffectFailed, error.to_string())
                }));
            }
        };
        let must_preserve = match &execution {
            EffectExecution::ResolutionRequired { .. } => true,
            EffectExecution::Completed { result, .. } => mutating && !result.is_error,
            EffectExecution::ApprovalRequired(_) => false,
        };
        if !must_preserve && let Some(failure) = interruption_failure(interruption) {
            return Err(failure);
        }
        if let EffectExecution::ApprovalRequired(request) = &execution {
            self.record_approval_request(request).await?;
        }
        Ok(ToolExecution::Effect(execution))
    }

    #[allow(clippy::too_many_arguments)]
    async fn dispatch_extension(
        &self,
        task: &TaskEnvelope,
        call: &ToolCall,
        registered: RegisteredTool,
        deadline: tokio::time::Instant,
        cancellation: &TaskCancellation,
        prepared: &PreparedTask,
        semantic_mask_operation: Option<agent_core::SemanticMaskOperation>,
    ) -> Result<ToolExecution, AgentFailure> {
        match self
            .gate_extension(task, call, &registered, &prepared.approved_subjects)
            .await?
        {
            ExtensionGate::Allowed => {}
            ExtensionGate::Stopped(execution) => return Ok(*execution),
        }
        let result = execute_extension(
            &registered,
            task,
            call,
            prepared.plan.reasoning,
            deadline.saturating_duration_since(tokio::time::Instant::now()),
            cancellation.token(),
            self.inner.artifacts.reader(),
            semantic_mask_operation,
        )
        .await;
        match result {
            Ok(value) => Ok(ToolExecution::Extension(value, registered.side_effect)),
            Err(ToolModuleError::Contract(message)) => {
                Err(AgentFailure::new(FailureCode::InternalInvariant, message))
            }
            Err(ToolModuleError::Failure(failure))
                if matches!(
                    failure.code,
                    FailureCode::Cancelled | FailureCode::BudgetExceeded
                ) =>
            {
                Err(failure)
            }
            Err(ToolModuleError::Failure(failure)) => {
                Ok(ToolExecution::Plain(failed_tool(call, failure)))
            }
            Err(ToolModuleError::TimedOut(failure) | ToolModuleError::Cancelled(failure)) => Ok(
                ToolExecution::ExtensionInterrupted(failure, registered.side_effect),
            ),
        }
    }
}

fn validate_bound_check_call(prepared: &PreparedTask, call: &ToolCall) -> Result<(), AgentFailure> {
    if call.name != "run_check" {
        return Ok(());
    }
    if prepared.plan.required_check_commands.is_empty() {
        if prepared.plan.required_checks.is_empty() {
            return Ok(());
        }
        return Err(AgentFailure::new(
            FailureCode::VerificationFailed,
            "AXD check requirements have no host-authorized command bindings",
        ));
    }
    let requirement_id = call
        .arguments
        .get("requirementId")
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| {
            AgentFailure::new(
                FailureCode::VerificationFailed,
                "bound AXD checks require a non-empty requirementId",
            )
        })?;
    let expected = prepared
        .plan
        .required_check_commands
        .get(requirement_id)
        .ok_or_else(|| {
            AgentFailure::new(
                FailureCode::VerificationFailed,
                "run_check names an unknown AXD check requirement",
            )
            .with_detail("requirementId", requirement_id)
        })?;

    let program = call
        .arguments
        .get("program")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| {
            AgentFailure::new(
                FailureCode::VerificationFailed,
                "bound AXD check is missing its executable",
            )
            .with_detail("requirementId", requirement_id)
        })?;
    let arguments = call
        .arguments
        .get("args")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| {
            AgentFailure::new(
                FailureCode::VerificationFailed,
                "bound AXD check args must be an array",
            )
            .with_detail("requirementId", requirement_id)
        })?;
    let mut actual = Vec::with_capacity(arguments.len().saturating_add(1));
    actual.push(program.to_owned());
    for argument in arguments {
        let argument = argument.as_str().ok_or_else(|| {
            AgentFailure::new(
                FailureCode::VerificationFailed,
                "bound AXD check args must contain strings",
            )
            .with_detail("requirementId", requirement_id)
        })?;
        actual.push(argument.to_owned());
    }
    let cwd_is_root = call
        .arguments
        .get("cwd")
        .is_none_or(|value| value.is_null() || value.as_str() == Some(""));
    if &actual != expected || !cwd_is_root {
        return Err(AgentFailure::new(
            FailureCode::VerificationFailed,
            "run_check does not match the host-authorized AXD check command",
        )
        .with_detail("requirementId", requirement_id)
        .with_detail("expectedArgv", json!(expected))
        .with_detail("actualArgv", json!(actual))
        .with_detail("workspaceRootRequired", true));
    }
    Ok(())
}
