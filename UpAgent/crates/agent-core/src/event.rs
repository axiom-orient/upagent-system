use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::AgentFailure;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "snake_case")]
pub enum TaskState {
    #[default]
    Created,
    Ready,
    Running,
    Suspended,
    AwaitingInput,
    AwaitingApproval,
    Succeeded,
    Failed,
    Cancelled,
    ResolutionRequired,
}

impl TaskState {
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Succeeded | Self::Failed | Self::Cancelled | Self::ResolutionRequired
        )
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum KernelEvent {
    Validated,
    Started,
    ModelRequested,
    ModelCompleted,
    ToolRequested,
    ToolCompleted,
    Suspended,
    Resumed,
    InputRequired,
    ApprovalRequired,
    Completed,
    Failed(AgentFailure),
    Cancelled(AgentFailure),
    ResolutionRequired(AgentFailure),
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum TransitionError {
    #[error("event {event} is invalid while task is in {state:?}")]
    Invalid {
        state: TaskState,
        event: &'static str,
    },
}

pub fn transition(state: TaskState, event: &KernelEvent) -> Result<TaskState, TransitionError> {
    let next = match (state, event) {
        (TaskState::Created, KernelEvent::Validated) => TaskState::Ready,
        (TaskState::Ready, KernelEvent::Started)
        | (TaskState::Suspended, KernelEvent::Resumed)
        | (
            TaskState::Running,
            KernelEvent::ModelRequested
            | KernelEvent::ModelCompleted
            | KernelEvent::ToolRequested
            | KernelEvent::ToolCompleted,
        ) => TaskState::Running,
        (TaskState::Running, KernelEvent::Suspended) => TaskState::Suspended,
        (TaskState::Running, KernelEvent::InputRequired) => TaskState::AwaitingInput,
        (TaskState::Running, KernelEvent::ApprovalRequired) => TaskState::AwaitingApproval,
        (TaskState::Running, KernelEvent::Completed) => TaskState::Succeeded,
        (
            TaskState::Created
            | TaskState::Ready
            | TaskState::Running
            | TaskState::Suspended
            | TaskState::AwaitingInput
            | TaskState::AwaitingApproval,
            KernelEvent::Failed(_),
        ) => TaskState::Failed,
        (
            TaskState::Created
            | TaskState::Ready
            | TaskState::Running
            | TaskState::Suspended
            | TaskState::AwaitingInput
            | TaskState::AwaitingApproval,
            KernelEvent::Cancelled(_),
        ) => TaskState::Cancelled,
        (
            TaskState::Ready
            | TaskState::Running
            | TaskState::Suspended
            | TaskState::AwaitingInput
            | TaskState::AwaitingApproval,
            KernelEvent::ResolutionRequired(_),
        ) => TaskState::ResolutionRequired,
        _ => {
            return Err(TransitionError::Invalid {
                state,
                event: event_name(event),
            });
        }
    };
    Ok(next)
}

#[must_use]
pub const fn event_name(event: &KernelEvent) -> &'static str {
    match event {
        KernelEvent::Validated => "validated",
        KernelEvent::Started => "started",
        KernelEvent::ModelRequested => "model_requested",
        KernelEvent::ModelCompleted => "model_completed",
        KernelEvent::ToolRequested => "tool_requested",
        KernelEvent::ToolCompleted => "tool_completed",
        KernelEvent::Suspended => "suspended",
        KernelEvent::Resumed => "resumed",
        KernelEvent::InputRequired => "input_required",
        KernelEvent::ApprovalRequired => "approval_required",
        KernelEvent::Completed => "completed",
        KernelEvent::Failed(_) => "failed",
        KernelEvent::Cancelled(_) => "cancelled",
        KernelEvent::ResolutionRequired(_) => "resolution_required",
    }
}
