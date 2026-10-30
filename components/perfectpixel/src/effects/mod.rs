mod apple_vision;
mod external;
mod texture;

pub use apple_vision::*;
pub use external::*;
pub use texture::*;

use serde::{Deserialize, Serialize};

use crate::Sha256Digest;

/// Identity assigned before asynchronous/external work starts. The caller owns `generation`;
/// Effects cannot invent time- or randomness-based authority.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EffectIdentity {
    pub generation: u64,
    pub operation_digest: Sha256Digest,
    pub input_digest: Sha256Digest,
}

impl EffectIdentity {
    pub fn new(
        generation: u64,
        operation_digest: Sha256Digest,
        input_digest: Sha256Digest,
    ) -> Self {
        Self {
            generation,
            operation_digest,
            input_digest,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EffectFailure {
    pub code: EffectFailureCode,
    pub operation: String,
    pub dependency: String,
    pub retryable: bool,
    pub cause: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub context: Vec<EffectFailureContext>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EffectFailureContext {
    pub key: String,
    pub value: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EffectFailureCode {
    InvalidArgument,
    Unsupported,
    NotFound,
    Conflict,
    PreconditionFailed,
    ResourceLimit,
    Timeout,
    DependencyFailed,
    VerificationFailed,
    Internal,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum EffectCompletion<T> {
    Succeeded { value: T },
    Failed { error: EffectFailure },
    Cancelled,
}

/// External work may return only an Event. The owner compares identity before changing state or
/// publishing an artifact, so stale/duplicate completion cannot mutate the latest generation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EffectResult<T> {
    pub identity: EffectIdentity,
    pub completion: EffectCompletion<T>,
}

impl<T> EffectResult<T> {
    pub fn is_current_for(&self, current: &EffectIdentity) -> bool {
        self.identity == *current
    }

    pub fn accept_current(self, current: &EffectIdentity) -> Option<EffectCompletion<T>> {
        if self.is_current_for(current) {
            Some(self.completion)
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity(generation: u64, input: &[u8]) -> EffectIdentity {
        EffectIdentity::new(
            generation,
            Sha256Digest::from_bytes(b"operation"),
            Sha256Digest::from_bytes(input),
        )
    }

    #[test]
    fn stale_result_cannot_be_accepted() {
        let current = identity(2, b"new");
        let result = EffectResult {
            identity: identity(1, b"old"),
            completion: EffectCompletion::Succeeded { value: 42u32 },
        };
        assert!(result.accept_current(&current).is_none());
    }

    #[test]
    fn cancellation_is_distinct_from_failure() {
        let current = identity(1, b"input");
        let result: EffectResult<()> = EffectResult {
            identity: current.clone(),
            completion: EffectCompletion::Cancelled,
        };
        assert_eq!(
            result.accept_current(&current),
            Some(EffectCompletion::Cancelled)
        );
    }
}
