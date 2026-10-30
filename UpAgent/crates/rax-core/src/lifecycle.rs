use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::error::CoreInvariantError;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RunState {
    Received,
    Validated,
    Planned,
    AwaitingApproval,
    Ready,
    Running,
    Succeeded,
    Failed,
    Cancelled,
    Denied,
    Expired,
    PartiallyApplied,
    Abandoned,
}

impl RunState {
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Succeeded
                | Self::Failed
                | Self::Cancelled
                | Self::Denied
                | Self::Expired
                | Self::PartiallyApplied
                | Self::Abandoned
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    Validated,
    Planned,
    ApprovalRequested,
    Authorized,
    Started,
    Succeeded,
    Failed,
    Cancelled,
    Denied,
    Expired,
    PartiallyApplied,
    Abandoned,
}

pub fn transition(state: RunState, event: EventKind) -> Result<RunState, CoreInvariantError> {
    use EventKind as E;
    use RunState as S;

    let next = match (state, event) {
        (S::Received, E::Validated) => S::Validated,
        (S::Validated, E::Planned) => S::Planned,
        (S::Planned, E::ApprovalRequested) => S::AwaitingApproval,
        (S::Planned | S::AwaitingApproval, E::Authorized) => S::Ready,
        (S::Ready, E::Started) => S::Running,
        (S::Running, E::Succeeded) => S::Succeeded,
        (
            S::Received | S::Validated | S::Planned | S::AwaitingApproval | S::Ready | S::Running,
            E::Failed,
        ) => S::Failed,
        (S::Planned | S::AwaitingApproval | S::Ready | S::Running, E::Cancelled) => S::Cancelled,
        (S::Planned | S::AwaitingApproval, E::Denied) => S::Denied,
        (S::Planned | S::AwaitingApproval, E::Expired) => S::Expired,
        (S::Running, E::PartiallyApplied) => S::PartiallyApplied,
        (S::Running, E::Abandoned) => S::Abandoned,
        _ => return Err(CoreInvariantError::InvalidTransition { state, event }),
    };
    Ok(next)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mutation_happy_path_is_explicit() {
        let mut state = RunState::Received;
        for event in [
            EventKind::Validated,
            EventKind::Planned,
            EventKind::ApprovalRequested,
            EventKind::Authorized,
            EventKind::Started,
            EventKind::Succeeded,
        ] {
            state = transition(state, event).expect("valid transition");
        }
        assert_eq!(state, RunState::Succeeded);
        assert!(state.is_terminal());
    }

    #[test]
    fn terminal_state_rejects_more_events() {
        assert!(transition(RunState::Succeeded, EventKind::Failed).is_err());
    }
}
