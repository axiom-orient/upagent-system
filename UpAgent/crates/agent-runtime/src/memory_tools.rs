use std::collections::BTreeSet;

use agent_core::{
    AgentFailure, EffectReceiptRef, FailureCode, ReceiptId, ToolCall, ToolDefinition, ToolResult,
};
use agent_memory::{
    MemoryConsent, MemoryError, MemoryKind, MemoryRetention, MemoryStore, NewMemory,
};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use uuid::Uuid;

#[derive(Debug)]
pub struct MemoryToolExecution {
    pub result: ToolResult,
    pub receipt: Option<EffectReceiptRef>,
    pub durable_effect: bool,
    pub resolution_required: bool,
}

pub fn definitions() -> Vec<ToolDefinition> {
    vec![
        ToolDefinition {
            name: "remember".to_owned(),
            description:
                "Store one explicitly approved, provenance-aware memory in the current Twin scope."
                    .to_owned(),
            input_schema: json!({
                "type":"object",
                "properties":{
                    "kind":{"type":"string","enum":["fact","preference","decision","goal","routine","relationship","project"]},
                    "content":{"type":"string","minLength":1},
                    "confidenceBasisPoints":{"type":"integer","minimum":0,"maximum":10000},
                    "retention":{"type":"string","enum":["project","persistent"]},
                    "supersedes":{"type":["string","null"]}
                },
                "required":["kind","content","confidenceBasisPoints","retention","supersedes"],
                "additionalProperties":false
            }),
            strict: true,
        },
        ToolDefinition {
            name: "forget_memory".to_owned(),
            description:
                "Permanently remove one memory from the current Twin scope using revision-CAS."
                    .to_owned(),
            input_schema: json!({
                "type":"object",
                "properties":{"id":{"type":"string","minLength":1}},
                "required":["id"],
                "additionalProperties":false
            }),
            strict: true,
        },
    ]
}

pub fn execute(
    store: &MemoryStore,
    scope: &str,
    call: &ToolCall,
    task_id: &str,
    allow_write: bool,
) -> MemoryToolExecution {
    if !allow_write {
        return denied(
            call,
            "durable memory writes were not approved for this task",
        );
    }
    match execute_inner(store, scope, call, task_id) {
        Ok(value) => value,
        Err(error) => MemoryToolExecution {
            result: ToolResult {
                call_id: call.id.clone(),
                name: call.name.clone(),
                value: Value::Null,
                is_error: true,
                failure: Some(error.failure),
            },
            receipt: None,
            durable_effect: error.durable_effect,
            resolution_required: error.resolution_required,
        },
    }
}

fn execute_inner(
    store: &MemoryStore,
    scope: &str,
    call: &ToolCall,
    task_id: &str,
) -> Result<MemoryToolExecution, MemoryExecutionFailure> {
    match call.name.as_str() {
        "remember" => {
            let args: RememberArgs = serde_json::from_value(call.arguments.clone())
                .map_err(|error| memory_failure("remember arguments are invalid", error))?;
            let snapshot = store
                .snapshot(scope)
                .map_err(|error| memory_store_failure("memory snapshot failed", error))?;
            let previous_ids = snapshot.entries.keys().cloned().collect::<BTreeSet<_>>();
            let updated = store
                .append(
                    scope,
                    snapshot.revision,
                    NewMemory {
                        kind: args.kind,
                        content: args.content,
                        provenance: format!("task:{task_id}"),
                        confidence_basis_points: args.confidence_basis_points,
                        consent: MemoryConsent::ExplicitlyApproved,
                        retention: args.retention.into(),
                        supersedes: args.supersedes,
                    },
                )
                .map_err(|error| memory_store_failure("memory append failed", error))?;
            let entry = updated
                .entries
                .values()
                .find(|entry| !previous_ids.contains(&entry.id))
                .ok_or_else(|| {
                    AgentFailure::new(
                        FailureCode::InternalInvariant,
                        "memory append returned no new persisted entry",
                    )
                })
                .map_err(MemoryExecutionFailure::after_effect)?;
            let value = serde_json::to_value(entry)
                .map_err(|error| memory_failure("memory result serialization failed", error))
                .map_err(MemoryExecutionFailure::after_effect)?;
            success(call, "memory.append", value).map_err(MemoryExecutionFailure::after_effect)
        }
        "forget_memory" => {
            let args: ForgetArgs = serde_json::from_value(call.arguments.clone())
                .map_err(|error| memory_failure("forget arguments are invalid", error))?;
            let snapshot = store
                .snapshot(scope)
                .map_err(|error| memory_store_failure("memory snapshot failed", error))?;
            let updated = store
                .forget(scope, snapshot.revision, &args.id)
                .map_err(|error| memory_store_failure("memory forget failed", error))?;
            success(
                call,
                "memory.forget",
                json!({"id":args.id,"revision":updated.revision}),
            )
            .map_err(MemoryExecutionFailure::after_effect)
        }
        _ => Err(AgentFailure::new(FailureCode::CapabilityDenied, "unknown memory tool").into()),
    }
}

fn success(
    call: &ToolCall,
    operation: &str,
    value: Value,
) -> Result<MemoryToolExecution, AgentFailure> {
    let encoded = serde_json::to_vec(&value)
        .map_err(|error| memory_failure("memory receipt serialization failed", error))?;
    let digest = format!("{:x}", Sha256::digest(&encoded));
    let id = ReceiptId::parse(format!("memory-{}", Uuid::now_v7())).map_err(|error| {
        AgentFailure::new(
            FailureCode::InternalInvariant,
            "generated memory receipt identifier is invalid",
        )
        .with_detail("cause", error.to_string())
    })?;
    Ok(MemoryToolExecution {
        result: ToolResult {
            call_id: call.id.clone(),
            name: call.name.clone(),
            value,
            is_error: false,
            failure: None,
        },
        receipt: Some(EffectReceiptRef {
            id,
            digest,
            operation: operation.to_owned(),
            verified: true,
        }),
        durable_effect: true,
        resolution_required: false,
    })
}

fn denied(call: &ToolCall, message: &str) -> MemoryToolExecution {
    MemoryToolExecution {
        result: ToolResult {
            call_id: call.id.clone(),
            name: call.name.clone(),
            value: Value::Null,
            is_error: true,
            failure: Some(AgentFailure::new(FailureCode::CapabilityDenied, message)),
        },
        receipt: None,
        durable_effect: false,
        resolution_required: false,
    }
}

#[derive(Debug)]
struct MemoryExecutionFailure {
    failure: AgentFailure,
    durable_effect: bool,
    resolution_required: bool,
}

impl MemoryExecutionFailure {
    fn after_effect(failure: AgentFailure) -> Self {
        Self {
            failure,
            durable_effect: true,
            resolution_required: true,
        }
    }
}

impl From<AgentFailure> for MemoryExecutionFailure {
    fn from(failure: AgentFailure) -> Self {
        Self {
            failure,
            durable_effect: false,
            resolution_required: false,
        }
    }
}

fn memory_failure(message: &str, error: impl std::fmt::Display) -> AgentFailure {
    AgentFailure::new(FailureCode::MemoryFailed, message).with_detail("cause", error.to_string())
}

fn memory_store_failure(message: &str, error: MemoryError) -> MemoryExecutionFailure {
    let may_have_committed = error.may_have_committed();
    let failure = memory_failure(message, error);
    if may_have_committed {
        MemoryExecutionFailure::after_effect(failure)
    } else {
        failure.into()
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct RememberArgs {
    kind: MemoryKind,
    content: String,
    confidence_basis_points: u16,
    retention: RequestedRetention,
    #[serde(default)]
    supersedes: Option<String>,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum RequestedRetention {
    Project,
    Persistent,
}

impl From<RequestedRetention> for MemoryRetention {
    fn from(value: RequestedRetention) -> Self {
        match value {
            RequestedRetention::Project => Self::Project,
            RequestedRetention::Persistent => Self::Persistent,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct ForgetArgs {
    id: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn post_rename_failure_requires_resolution_without_a_success_receipt() {
        let failure = memory_store_failure(
            "memory append failed",
            MemoryError::CommitUncertain(std::io::Error::other("directory sync failed")),
        );
        assert!(failure.durable_effect);
        assert!(failure.resolution_required);
        assert_eq!(failure.failure.code, FailureCode::MemoryFailed);
        let before = memory_store_failure(
            "memory append failed",
            MemoryError::Io(std::io::Error::other("before rename")),
        );
        assert!(!before.durable_effect);
        assert!(!before.resolution_required);
    }

    #[test]
    fn strict_memory_schema_requires_nullable_supersedes() {
        let remember = definitions()
            .into_iter()
            .find(|tool| tool.name == "remember")
            .expect("remember definition");
        assert_eq!(
            remember.input_schema["required"],
            serde_json::json!([
                "kind",
                "content",
                "confidenceBasisPoints",
                "retention",
                "supersedes"
            ])
        );
        assert_eq!(
            remember.input_schema["properties"]["supersedes"]["type"],
            serde_json::json!(["string", "null"])
        );
    }
}
