use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum FailureCode {
    InvalidRequest,
    InvalidProfile,
    InvalidTransition,
    BudgetExceeded,
    ProviderUnavailable,
    ProviderFailed,
    CapabilityDenied,
    ApprovalRequired,
    EffectFailed,
    VerificationFailed,
    MemoryFailed,
    StorageFailed,
    Cancelled,
    ResolutionRequired,
    InternalInvariant,
}

impl FailureCode {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::InvalidRequest => "invalid_request",
            Self::InvalidProfile => "invalid_profile",
            Self::InvalidTransition => "invalid_transition",
            Self::BudgetExceeded => "budget_exceeded",
            Self::ProviderUnavailable => "provider_unavailable",
            Self::ProviderFailed => "provider_failed",
            Self::CapabilityDenied => "capability_denied",
            Self::ApprovalRequired => "approval_required",
            Self::EffectFailed => "effect_failed",
            Self::VerificationFailed => "verification_failed",
            Self::MemoryFailed => "memory_failed",
            Self::StorageFailed => "storage_failed",
            Self::Cancelled => "cancelled",
            Self::ResolutionRequired => "resolution_required",
            Self::InternalInvariant => "internal_invariant",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, Error)]
#[error("{code:?}: {message}")]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct AgentFailure {
    pub code: FailureCode,
    pub message: String,
    #[serde(default)]
    pub retryable: bool,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub details: BTreeMap<String, Value>,
}

impl AgentFailure {
    #[must_use]
    pub fn new(code: FailureCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            retryable: false,
            details: BTreeMap::new(),
        }
    }

    #[must_use]
    pub fn retryable(mut self, value: bool) -> Self {
        self.retryable = value;
        self
    }

    #[must_use]
    pub fn with_detail(mut self, key: impl Into<String>, value: impl Into<Value>) -> Self {
        self.details.insert(key.into(), value.into());
        self
    }
}
