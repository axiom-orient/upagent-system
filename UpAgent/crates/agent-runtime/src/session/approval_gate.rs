use std::collections::BTreeSet;

use agent_core::{
    AgentFailure, ApprovalRequest, EffectApprovalMode, FailureCode, TaskEnvelope, ToolCall,
};

use crate::{RegisteredTool, extension_approval};

use super::tool_dispatch::ToolExecution;
use super::{AgentRuntime, denied_tool};

/// Whether a side-effecting extension may run before it is dispatched.
pub(super) enum ExtensionGate {
    /// The call is authorised, either by a redeemed decision or a task grant.
    Allowed,
    /// The call stops here with this outcome.
    Stopped(Box<ToolExecution>),
}

impl AgentRuntime {
    /// Persist a pending decision so an operator has something to act on.
    pub(super) async fn record_approval_request(
        &self,
        request: &ApprovalRequest,
    ) -> Result<(), AgentFailure> {
        let approvals = self.inner.approvals.clone();
        let request = request.clone();
        tokio::task::spawn_blocking(move || approvals.record_request(&request))
            .await
            .map_err(|error| {
                AgentFailure::new(
                    FailureCode::InternalInvariant,
                    "approval recording did not complete",
                )
                .with_detail("cause", error.to_string())
            })?
            .map_err(|error| {
                AgentFailure::new(FailureCode::StorageFailed, "approval could not be recorded")
                    .with_detail("cause", error.to_string())
            })
    }

    /// Decide whether a side-effecting extension tool may run.
    ///
    /// A decision already redeemed for this run authorises the call directly:
    /// its subject binds the profile, track, tool, arguments and effect class,
    /// so it covers exactly the call the operator was shown.
    pub(super) async fn gate_extension(
        &self,
        task: &TaskEnvelope,
        call: &ToolCall,
        registered: &RegisteredTool,
        approved: &BTreeSet<String>,
    ) -> Result<ExtensionGate, AgentFailure> {
        if !registered.side_effect.requires_approval() {
            return Ok(ExtensionGate::Allowed);
        }
        if task.effect_approval == EffectApprovalMode::Deny {
            return Ok(ExtensionGate::Stopped(Box::new(ToolExecution::Plain(
                denied_tool(
                    call,
                    "extension tool requires explicit task effect authorization",
                ),
            ))));
        }
        let request = extension_approval(task, call, registered.side_effect)?;
        if approved.contains(&request.subject) {
            return Ok(ExtensionGate::Allowed);
        }
        match task.effect_approval {
            EffectApprovalMode::PlanOnly => {
                self.record_approval_request(&request).await?;
                Ok(ExtensionGate::Stopped(Box::new(
                    ToolExecution::ExtensionApproval(request),
                )))
            }
            EffectApprovalMode::TaskGrant => Ok(ExtensionGate::Allowed),
            EffectApprovalMode::Deny => unreachable!("deny was handled before redemption"),
        }
    }
}
