use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::operation::Operation;

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum EffectClass {
    ReadWorkspace,
    WriteWorkspace,
    DeleteWorkspace,
    ExecuteProcess,
    ExecuteShell,
    GitIndexMutation,
    GitWorktreeMutation,
    Network,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PolicyDecision {
    Permit,
    ApprovalRequired,
    Deny,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct EffectDecision {
    pub effect: EffectClass,
    pub decision: PolicyDecision,
    pub reason: String,
}

#[derive(Debug, Clone, Default)]
pub struct DefaultPolicy;

impl DefaultPolicy {
    #[must_use]
    pub fn evaluate(operation: &Operation) -> Vec<EffectDecision> {
        effects_for(operation)
            .into_iter()
            .map(|effect| EffectDecision {
                effect,
                decision: match effect {
                    EffectClass::ReadWorkspace => PolicyDecision::Permit,
                    EffectClass::WriteWorkspace
                    | EffectClass::DeleteWorkspace
                    | EffectClass::ExecuteProcess
                    | EffectClass::ExecuteShell
                    | EffectClass::GitIndexMutation
                    | EffectClass::GitWorktreeMutation
                    | EffectClass::Network => PolicyDecision::ApprovalRequired,
                },
                reason: reason_for(effect).to_string(),
            })
            .collect()
    }
}

#[must_use]
pub fn effects_for(operation: &Operation) -> Vec<EffectClass> {
    use EffectClass as E;
    use Operation as O;

    match operation {
        O::FsRead(_) | O::FsStat(_) | O::FsList(_) | O::FsGlob(_) | O::FsGrep(_) => {
            vec![E::ReadWorkspace]
        }
        O::FsWrite(_) | O::FsPatch(_) | O::FsMkdir(_) | O::FsCopy(_) => {
            vec![E::WriteWorkspace]
        }
        O::FsMove(_) => vec![E::WriteWorkspace, E::DeleteWorkspace],
        O::FsRemove(_) => vec![E::DeleteWorkspace],
        O::ProcessRun(_) => vec![E::ExecuteProcess],
        O::ProcessShell(_) => vec![E::ExecuteShell],
        O::GitStatus(_)
        | O::GitDiff(_)
        | O::GitLog(_)
        | O::GitShow(_)
        | O::GitRemote(_)
        | O::GitBranches(_) => vec![E::ReadWorkspace],
        O::GitStage(_) | O::GitUnstage(_) | O::GitCommit(_) => vec![E::GitIndexMutation],
        O::GitDiscard(_) | O::GitCheckout(_) => vec![E::GitWorktreeMutation],
        O::GitFetch(_) | O::GitPush(_) => vec![E::Network],
        O::GitPullFf(_) => vec![E::Network, E::GitWorktreeMutation],
    }
}

const fn reason_for(effect: EffectClass) -> &'static str {
    match effect {
        EffectClass::ReadWorkspace => "bounded read-only workspace effect",
        EffectClass::WriteWorkspace => "workspace content may be created or changed",
        EffectClass::DeleteWorkspace => "workspace content may be permanently removed",
        EffectClass::ExecuteProcess => {
            "a child process executes with the current OS user authority"
        }
        EffectClass::ExecuteShell => "a shell interprets a command string",
        EffectClass::GitIndexMutation => "the Git index or history may change",
        EffectClass::GitWorktreeMutation => "the Git worktree or branch may change",
        EffectClass::Network => "Git may contact a remote service",
    }
}

#[cfg(test)]
mod tests {
    use crate::{FsReadInput, FsWriteInput, Operation, PolicyDecision};

    use super::{DefaultPolicy, EffectClass};

    #[test]
    fn bounded_reads_are_permitted_without_approval() {
        let decisions = DefaultPolicy::evaluate(&Operation::FsRead(FsReadInput {
            path: "README.md".to_string(),
            max_bytes: 1024,
        }));
        assert_eq!(decisions.len(), 1);
        assert_eq!(decisions[0].effect, EffectClass::ReadWorkspace);
        assert_eq!(decisions[0].decision, PolicyDecision::Permit);
    }

    #[test]
    fn workspace_writes_require_approval() {
        let decisions = DefaultPolicy::evaluate(&Operation::FsWrite(FsWriteInput {
            path: "output.txt".to_string(),
            content: "value".to_string(),
            encoding: crate::ContentEncoding::Utf8,
            create_only: true,
            expected_sha256: None,
        }));
        assert_eq!(decisions.len(), 1);
        assert_eq!(decisions[0].effect, EffectClass::WriteWorkspace);
        assert_eq!(decisions[0].decision, PolicyDecision::ApprovalRequired);
    }
}
