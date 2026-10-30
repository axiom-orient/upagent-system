use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{AgentFailure, ToolCallId};

pub use agent_contract::model::{
    MessageRole, ModelContentPart, ModelMessage, ModelTurnResult, ModelUsage, ReasoningEffort,
    ToolCall, ToolDefinition,
};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ModelTurn {
    pub messages: Vec<ModelMessage>,
    #[serde(default)]
    pub tools: Vec<ToolDefinition>,
    #[serde(default)]
    pub reasoning: ReasoningEffort,
    pub timeout_ms: u64,
    pub maximum_response_bytes: usize,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub metadata: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ToolResult {
    pub call_id: ToolCallId,
    pub name: String,
    pub value: Value,
    #[serde(default)]
    pub is_error: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure: Option<AgentFailure>,
}
