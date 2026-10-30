use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

use crate::lifecycle::{EventKind, RunState};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    InvalidRequest,
    UnsupportedSchemaVersion,
    InvalidPath,
    PathOutsideWorkspace,
    SymlinkNotAllowed,
    NotFound,
    AlreadyExists,
    NotAFile,
    NotADirectory,
    BinaryFile,
    LimitExceeded,
    PreconditionFailed,
    ApprovalRequired,
    ApprovalDenied,
    GrantExpired,
    GrantConsumed,
    GrantMismatch,
    ProcessSpawnFailed,
    ProcessTimedOut,
    ProcessCancelled,
    GitUnavailable,
    GitNotRepository,
    GitConflict,
    GitAuthentication,
    GitNetwork,
    OperationFailed,
    JournalError,
    RecoveryRequired,
    ProtocolError,
    InvariantViolation,
    Unsupported,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ErrorPhase {
    Parse,
    Validate,
    Normalize,
    Plan,
    Authorize,
    Apply,
    Verify,
    Record,
    Recover,
    Protocol,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, Error)]
#[error("{code:?} during {phase:?}: {message}")]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct RaxError {
    pub code: ErrorCode,
    pub phase: ErrorPhase,
    pub retryable: bool,
    pub message: String,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub details: BTreeMap<String, Value>,
}

impl RaxError {
    #[must_use]
    pub fn new(
        code: ErrorCode,
        phase: ErrorPhase,
        retryable: bool,
        message: impl Into<String>,
    ) -> Self {
        Self {
            code,
            phase,
            retryable,
            message: message.into(),
            details: BTreeMap::new(),
        }
    }

    #[must_use]
    pub fn with_detail(mut self, key: impl Into<String>, value: impl Into<Value>) -> Self {
        self.details.insert(key.into(), value.into());
        self
    }
}

#[derive(Debug, Error)]
pub enum CoreInvariantError {
    #[error("invalid lifecycle transition from {state:?} using {event:?}")]
    InvalidTransition { state: RunState, event: EventKind },
    #[error("failed to serialize a canonical core value: {0}")]
    Serialization(#[from] serde_json::Error),
}
