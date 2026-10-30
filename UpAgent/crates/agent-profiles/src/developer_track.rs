use std::collections::BTreeSet;

use agent_core::{ReasoningEffort, TrackId};
use agent_profile_api::{ExecutionClass, ExecutionTopology, ProfileError};

#[derive(Debug, Clone, PartialEq)]
pub(super) struct TrackPolicy {
    pub(super) reasoning: ReasoningEffort,
    pub(super) execution_class: ExecutionClass,
    pub(super) required_successful_tools_after_mutation: BTreeSet<String>,
    pub(super) maximum_workers: u16,
    pub(super) topology: ExecutionTopology,
    pub(super) maximum_transitions: u32,
    pub(super) maximum_model_calls: u32,
    pub(super) maximum_tool_calls: u32,
    pub(super) instruction: &'static str,
}

pub(super) fn track_policy(track: &TrackId) -> Result<TrackPolicy, ProfileError> {
    match track.as_str() {
        "prototyper" => Ok(TrackPolicy {
            reasoning: ReasoningEffort::Medium,
            execution_class: ExecutionClass::Guided,
            required_successful_tools_after_mutation: BTreeSet::new(),
            maximum_workers: 2,
            topology: ExecutionTopology::ParallelRead,
            maximum_transitions: 96,
            maximum_model_calls: 6,
            maximum_tool_calls: 20,
            instruction: "Generate independent hypotheses, isolate experiments, use one explicit metric, and do not promote unverified prototypes.",
        }),
        "builder" => Ok(TrackPolicy {
            reasoning: ReasoningEffort::High,
            execution_class: ExecutionClass::Grounded,
            required_successful_tools_after_mutation: BTreeSet::from(["inspect_diff".to_owned()]),
            maximum_workers: 1,
            topology: ExecutionTopology::SingleWithIndependentVerifier,
            maximum_transitions: 192,
            maximum_model_calls: 12,
            maximum_tool_calls: 48,
            instruction: "Convert the intended product into production-grade code with explicit failure, security, recovery, test, and packaging criteria.",
        }),
        "sweeper" => Ok(TrackPolicy {
            reasoning: ReasoningEffort::High,
            execution_class: ExecutionClass::Grounded,
            required_successful_tools_after_mutation: BTreeSet::from([
                "inspect_diff".to_owned(),
                "run_check".to_owned(),
            ]),
            maximum_workers: 1,
            topology: ExecutionTopology::SingleWithIndependentVerifier,
            maximum_transitions: 128,
            maximum_model_calls: 8,
            maximum_tool_calls: 24,
            instruction: "Preserve behavior while removing waste, duplication, accidental complexity, and measured bottlenecks. Do not add product features.",
        }),
        "grower" => Ok(TrackPolicy {
            reasoning: ReasoningEffort::High,
            execution_class: ExecutionClass::Grounded,
            required_successful_tools_after_mutation: BTreeSet::from(["inspect_diff".to_owned()]),
            maximum_workers: 2,
            topology: ExecutionTopology::ParallelRead,
            maximum_transitions: 192,
            maximum_model_calls: 12,
            maximum_tool_calls: 40,
            instruction: "Tie every product change to an explicit user or market hypothesis and measurable outcome; feature volume is not growth.",
        }),
        "maintainer" => Ok(TrackPolicy {
            reasoning: ReasoningEffort::High,
            execution_class: ExecutionClass::Verified,
            required_successful_tools_after_mutation: BTreeSet::from([
                "inspect_diff".to_owned(),
                "run_check".to_owned(),
            ]),
            maximum_workers: 1,
            topology: ExecutionTopology::SingleWithIndependentVerifier,
            maximum_transitions: 320,
            maximum_model_calls: 20,
            maximum_tool_calls: 80,
            instruction: "Prefer containment, root-cause analysis, regression coverage, rollback, observability, and conservative changes.",
        }),
        _ => Err(ProfileError::Invalid(format!(
            "unknown developer track {track}"
        ))),
    }
}
