use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::operation::{MAX_CONTROLLED_TEXT_BYTES, MAX_EDIT_COUNT, Operation};

pub const SCHEMA_VERSION: &str = "1";

/// Maximum serialized request size accepted by machine adapters.
///
/// JSON can expand each controlled-text byte to a six-byte `\u00XX` escape.
/// The remaining allowance covers the bounded `TextEdit` envelope and fixed
/// request fields without making adapters silently reject a core-valid request.
pub const MAX_REQUEST_WIRE_BYTES: u64 =
    6 * MAX_CONTROLLED_TEXT_BYTES as u64 + 128 * MAX_EDIT_COUNT as u64 + 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct RequestEnvelope {
    pub schema_version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    pub operation: Operation,
    #[serde(default)]
    pub record: bool,
}

impl RequestEnvelope {
    pub fn validate(&self) -> Result<(), crate::error::RaxError> {
        if self.schema_version != SCHEMA_VERSION {
            return Err(crate::error::RaxError::new(
                crate::error::ErrorCode::UnsupportedSchemaVersion,
                crate::error::ErrorPhase::Validate,
                false,
                format!(
                    "unsupported schemaVersion {}; expected {SCHEMA_VERSION}",
                    self.schema_version
                ),
            ));
        }
        if let Some(request_id) = &self.request_id
            && (request_id.is_empty() || request_id.len() > 128 || request_id.contains('\0'))
        {
            return Err(crate::error::RaxError::new(
                crate::error::ErrorCode::InvalidRequest,
                crate::error::ErrorPhase::Validate,
                false,
                "requestId must be 1..128 characters and contain no NUL",
            ));
        }
        self.operation.validate()
    }
}
