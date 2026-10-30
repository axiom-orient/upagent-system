use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{error::CoreInvariantError, operation::Operation, policy::EffectDecision};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct WorkspaceBinding {
    pub display_path: String,
    pub identity: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PathKind {
    File,
    Directory,
    Symlink,
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct PathPrecondition {
    pub path: String,
    pub exists: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<PathKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Plan {
    pub schema_version: String,
    pub plan_id: String,
    pub request_id: String,
    pub workspace: WorkspaceBinding,
    pub operation: Operation,
    pub effects: Vec<EffectDecision>,
    #[serde(default)]
    pub preconditions: Vec<PathPrecondition>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub observations: BTreeMap<String, serde_json::Value>,
    pub created_at: String,
    pub expires_at: String,
    pub fingerprint: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct FingerprintPayload<'a> {
    schema_version: &'a str,
    request_id: &'a str,
    workspace: &'a WorkspaceBinding,
    operation: &'a Operation,
    effects: &'a [EffectDecision],
    preconditions: &'a [PathPrecondition],
    observations: &'a BTreeMap<String, serde_json::Value>,
    expires_at: &'a str,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ApprovalSubjectPayload<'a> {
    schema_version: &'a str,
    workspace: &'a WorkspaceBinding,
    operation: &'a Operation,
    effects: &'a [EffectDecision],
    preconditions: &'a [PathPrecondition],
}

impl Plan {
    /// Digest of what an operator actually decides about.
    ///
    /// The fingerprint seals one plan instance and therefore includes the
    /// request identifier and expiry, which differ on every run. An approval
    /// must instead survive until the same operation is requested again, so the
    /// subject covers only the workspace, the operation, its effect classes, and
    /// the path preconditions. Preconditions carry the existence, kind, size and
    /// hash of every touched path, so a subject match means the approved
    /// operation is being replayed against the state it was approved for, and
    /// `apply` revalidates those same preconditions before mutating.
    ///
    /// Observations are excluded: they are derived readings used for reporting,
    /// not the authority that `apply` revalidates.
    pub fn approval_subject(&self) -> Result<String, CoreInvariantError> {
        let payload = ApprovalSubjectPayload {
            schema_version: &self.schema_version,
            workspace: &self.workspace,
            operation: &self.operation,
            effects: &self.effects,
            preconditions: &self.preconditions,
        };
        let encoded = serde_json::to_vec(&payload)?;
        Ok(format!("{:x}", Sha256::digest(encoded)))
    }

    pub fn compute_fingerprint(&self) -> Result<String, CoreInvariantError> {
        let payload = FingerprintPayload {
            schema_version: &self.schema_version,
            request_id: &self.request_id,
            workspace: &self.workspace,
            operation: &self.operation,
            effects: &self.effects,
            preconditions: &self.preconditions,
            observations: &self.observations,
            expires_at: &self.expires_at,
        };
        let encoded = serde_json::to_vec(&payload)?;
        Ok(format!("{:x}", Sha256::digest(encoded)))
    }

    pub fn seal(mut self) -> Result<Self, CoreInvariantError> {
        self.fingerprint = self.compute_fingerprint()?;
        Ok(self)
    }

    pub fn verify_fingerprint(&self) -> Result<bool, CoreInvariantError> {
        Ok(self.fingerprint == self.compute_fingerprint()?)
    }
}
