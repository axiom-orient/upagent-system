use std::{collections::BTreeSet, fmt};

use agent_core::{AgentFailure, FailureCode};
use agent_profile_api::{ExecutionClass, ExecutionTopology};
use serde_json::json;

/// Model-independent evidence supplied to completion verifiers.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CompletionEvidence {
    pub execution_class: ExecutionClass,
    pub topology: ExecutionTopology,
    pub required_successful_tools: BTreeSet<String>,
    pub successful_tools: BTreeSet<String>,
    pub required_checks: BTreeSet<String>,
    pub successful_checks: BTreeSet<String>,
    pub required_successful_tools_after_mutation: BTreeSet<String>,
    pub successful_tools_after_mutation: BTreeSet<String>,
    pub successful_checks_after_mutation: BTreeSet<String>,
    pub successful_source_mutation: bool,
}

/// Typed verifier contribution. It receives observations, never model claims.
pub trait CompletionVerifier: Send + Sync + fmt::Debug {
    fn id(&self) -> &str;
    fn verify(&self, evidence: &CompletionEvidence) -> Result<(), AgentFailure>;
}

#[derive(Debug)]
pub(super) struct HostCompletionVerifier;

impl CompletionVerifier for HostCompletionVerifier {
    #[allow(clippy::unnecessary_literal_bound)]
    fn id(&self) -> &str {
        "upagent.evidence-closure"
    }

    fn verify(&self, evidence: &CompletionEvidence) -> Result<(), AgentFailure> {
        let missing_tools = evidence
            .required_successful_tools
            .difference(&evidence.successful_tools)
            .cloned()
            .collect::<Vec<_>>();
        if !missing_tools.is_empty() {
            return Err(AgentFailure::new(
                FailureCode::VerificationFailed,
                "final answer was rejected because required tools did not succeed",
            )
            .with_detail("executionClass", evidence.execution_class.as_str())
            .with_detail("missingTools", json!(missing_tools)));
        }

        let current_checks = if evidence.successful_source_mutation {
            &evidence.successful_checks_after_mutation
        } else {
            &evidence.successful_checks
        };
        let missing_checks = evidence
            .required_checks
            .difference(current_checks)
            .cloned()
            .collect::<Vec<_>>();
        if !missing_checks.is_empty() {
            return Err(AgentFailure::new(
                FailureCode::VerificationFailed,
                "final answer was rejected because required checks lack current PASS evidence",
            )
            .with_detail("executionClass", evidence.execution_class.as_str())
            .with_detail("missingChecks", json!(missing_checks))
            .with_detail("observedChecks", json!(current_checks)));
        }

        let missing_after_mutation = if evidence.successful_source_mutation {
            evidence
                .required_successful_tools_after_mutation
                .difference(&evidence.successful_tools_after_mutation)
                .cloned()
                .collect::<Vec<_>>()
        } else {
            Vec::new()
        };
        if !missing_after_mutation.is_empty() {
            return Err(AgentFailure::new(
                FailureCode::VerificationFailed,
                "source mutation requires successful post-mutation evidence",
            )
            .with_detail("executionClass", evidence.execution_class.as_str())
            .with_detail("requiredTools", json!(missing_after_mutation)));
        }

        if evidence.execution_class == ExecutionClass::Verified
            && evidence.required_checks.is_empty()
            && evidence.required_successful_tools.is_empty()
            && evidence.required_successful_tools_after_mutation.is_empty()
        {
            return Err(AgentFailure::new(
                FailureCode::VerificationFailed,
                "verified execution requires an explicit evidence contract",
            ));
        }
        Ok(())
    }
}
