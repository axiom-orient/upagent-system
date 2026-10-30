use agent_core::{
    AgentFailure, ArtifactRef, FailureCode, ImageObservation, ImageObservationDetail,
    ImageObservationRole, ModelContentPart, ModelMessage, ToolResult,
};
use serde_json::{Value, json};

use crate::{
    ArtifactStore, ModuleArtifact,
    image_tools::{
        IMAGE_COMPARE_TOOL, IMAGE_EDIT_SEMANTIC_TOOL, IMAGE_EXTRACT_TOOL, IMAGE_GENERATE_TOOL,
        IMAGE_INSPECT_TOOL, IMAGE_RENDER_TOOL,
    },
};

const MAXIMUM_INLINE_TOOL_RESULT_BYTES: usize = 192 * 1024;
const MAXIMUM_TOOL_RESULT_PREVIEW_BYTES: usize = 8 * 1024;

pub(super) fn prepare_tool_message(
    store: &ArtifactStore,
    result: &ToolResult,
    additional_artifacts: &[ArtifactRef],
) -> Result<(ModelMessage, Vec<ArtifactRef>), AgentFailure> {
    #[derive(serde::Serialize)]
    #[serde(rename_all = "camelCase")]
    struct ErrorProjection<'a> {
        value: &'a Value,
        failure: &'a Option<AgentFailure>,
    }

    let provider_value = image_result_projection(result, additional_artifacts)?;
    let encoded = if result.is_error {
        serde_json::to_vec(&ErrorProjection {
            value: &provider_value,
            failure: &result.failure,
        })
    } else {
        serde_json::to_vec(&provider_value)
    }
    .map_err(|error| {
        AgentFailure::new(
            FailureCode::InternalInvariant,
            "tool result could not be encoded for the provider",
        )
        .with_detail("cause", error.to_string())
    })?;

    if encoded.len() <= MAXIMUM_INLINE_TOOL_RESULT_BYTES {
        let content = String::from_utf8(encoded).map_err(|error| {
            AgentFailure::new(
                FailureCode::InternalInvariant,
                "JSON tool result was not valid UTF-8",
            )
            .with_detail("cause", error.to_string())
        })?;
        return Ok((
            tool_result_message(result, content, additional_artifacts)?,
            Vec::new(),
        ));
    }

    let artifact = store
        .put(&encoded, "application/json", "json")
        .map_err(|error| {
            AgentFailure::new(
                FailureCode::StorageFailed,
                "oversized tool result could not be persisted",
            )
            .with_detail("cause", error.to_string())
        })?;
    let original_bytes = encoded.len();
    let preview_end = encoded.len().min(MAXIMUM_TOOL_RESULT_PREVIEW_BYTES);
    let preview = String::from_utf8_lossy(&encoded[..preview_end]);
    let projection = json!({
        "oversized": true,
        "artifact": &artifact,
        "originalBytes": original_bytes,
        "isError": result.is_error,
        "preview": preview,
        "failureCode": result.failure.as_ref().map(|failure| failure.code.as_str()),
        "failureMessage": result.failure.as_ref().map(|failure| failure.message.as_str()),
        "message": "The complete tool result is stored as an artifact. Request a narrower result when more detail is required."
    });
    let content = serde_json::to_string(&projection).map_err(|error| {
        AgentFailure::new(
            FailureCode::InternalInvariant,
            "oversized tool result reference could not be encoded",
        )
        .with_detail("cause", error.to_string())
    })?;
    Ok((
        tool_result_message(result, content, additional_artifacts)?,
        vec![artifact],
    ))
}

fn image_result_projection(
    result: &ToolResult,
    additional_artifacts: &[ArtifactRef],
) -> Result<Value, AgentFailure> {
    let mut value = result.value.clone();
    if !matches!(
        result.name.as_str(),
        IMAGE_GENERATE_TOOL
            | IMAGE_EXTRACT_TOOL
            | IMAGE_RENDER_TOOL
            | IMAGE_EDIT_SEMANTIC_TOOL
            | IMAGE_COMPARE_TOOL
    ) {
        return Ok(value);
    }
    let Some(object) = value.as_object_mut() else {
        return Err(AgentFailure::new(
            FailureCode::InternalInvariant,
            "image tool result must be a JSON object",
        ));
    };
    let dependencies = result
        .value
        .get("receipt")
        .cloned()
        .map(serde_json::from_value::<agent_core::OperationReceiptV2>)
        .transpose()
        .map_err(|error| {
            AgentFailure::new(
                FailureCode::InternalInvariant,
                "image receipt could not be decoded",
            )
            .with_detail("cause", error.to_string())
        })?
        .map_or_else(Vec::new, |receipt| receipt.dependencies);
    let roles = result
        .value
        .get("artifactRoles")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let descriptors = additional_artifacts
        .iter()
        .filter(|artifact| artifact.media_type == "image/png")
        .enumerate()
        .map(|(index, artifact)| {
            let role = roles.get(index).and_then(Value::as_str).unwrap_or("render");
            let kind = match role {
                "object" => agent_core::ArtifactKindV2::Object,
                "mask" => agent_core::ArtifactKindV2::Mask,
                _ => agent_core::ArtifactKindV2::WorkingRaster,
            };
            agent_core::ArtifactDescriptorV2::new(
                artifact.clone(),
                kind,
                Some(agent_core::PixelSpec::rgba8_srgb_straight()),
                dependencies.clone(),
                agent_core::ArtifactRetention::TaskScoped,
            )
            .map_err(|error| {
                AgentFailure::new(
                    FailureCode::InternalInvariant,
                    "image artifact descriptor could not be encoded",
                )
                .with_detail("cause", error)
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    object.insert(
        "artifactDescriptors".to_owned(),
        serde_json::to_value(descriptors).map_err(|error| {
            AgentFailure::new(
                FailureCode::InternalInvariant,
                "image artifact descriptors could not be encoded",
            )
            .with_detail("cause", error.to_string())
        })?,
    );
    Ok(value)
}

fn tool_result_message(
    result: &ToolResult,
    content: String,
    additional_artifacts: &[ArtifactRef],
) -> Result<ModelMessage, AgentFailure> {
    let mut parts = Vec::new();
    let observations: Vec<ArtifactRef> = match result.name.as_str() {
        IMAGE_GENERATE_TOOL | IMAGE_EXTRACT_TOOL | IMAGE_RENDER_TOOL | IMAGE_EDIT_SEMANTIC_TOOL => {
            additional_artifacts
                .iter()
                .filter(|artifact| artifact.media_type == "image/png")
                .cloned()
                .collect()
        }
        IMAGE_INSPECT_TOOL => result
            .value
            .get("artifact")
            .cloned()
            .map(serde_json::from_value::<ArtifactRef>)
            .transpose()
            .map_err(|error| {
                AgentFailure::new(
                    FailureCode::InternalInvariant,
                    "image inspection artifact reference could not be decoded",
                )
                .with_detail("cause", error.to_string())
            })?
            .into_iter()
            .collect(),
        _ => Vec::new(),
    };
    for artifact in observations {
        parts.push(ModelContentPart::ImageObservation {
            image: ImageObservation {
                artifact,
                role: if result.name == IMAGE_EXTRACT_TOOL {
                    ImageObservationRole::Crop
                } else {
                    ImageObservationRole::Full
                },
                detail: ImageObservationDetail::Auto,
                caption: Some(
                    match result.name.as_str() {
                        IMAGE_GENERATE_TOOL => "Generated image artifact",
                        IMAGE_EXTRACT_TOOL => "Extracted object artifact",
                        IMAGE_RENDER_TOOL => "Rendered composition artifact",
                        IMAGE_EDIT_SEMANTIC_TOOL => "Validated semantic edit artifact",
                        IMAGE_INSPECT_TOOL => "Inspected source image artifact",
                        _ => "Image artifact",
                    }
                    .to_owned(),
                ),
            },
        });
    }
    if result.name == IMAGE_COMPARE_TOOL {
        let roles = result
            .value
            .get("artifactRoles")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        for (index, artifact) in additional_artifacts
            .iter()
            .filter(|artifact| artifact.media_type == "image/png")
            .take(2)
            .enumerate()
        {
            let role = roles
                .get(index)
                .and_then(Value::as_str)
                .unwrap_or("difference");
            parts.push(ModelContentPart::ImageObservation {
                image: ImageObservation {
                    artifact: artifact.clone(),
                    role: if role == "difference" {
                        ImageObservationRole::Diff
                    } else {
                        ImageObservationRole::Mask
                    },
                    detail: ImageObservationDetail::Auto,
                    caption: Some(
                        match role {
                            "difference" => "Deterministic difference preview",
                            "mask_overlay" => "Deterministic mask overlay preview",
                            _ => "Deterministic comparison preview",
                        }
                        .to_owned(),
                    ),
                },
            });
        }
    }
    Ok(ModelMessage::tool_result(
        result.call_id.clone(),
        result.name.clone(),
        content,
        result.is_error,
    )
    .with_content_parts(parts))
}

pub(super) fn persist_module_artifacts(
    store: &ArtifactStore,
    artifacts: Vec<ModuleArtifact>,
) -> Result<Vec<ArtifactRef>, AgentFailure> {
    if artifacts.len() > 16 {
        return Err(AgentFailure::new(
            FailureCode::StorageFailed,
            "extension returned more than 16 artifacts",
        ));
    }
    let mut total = 0_usize;
    let mut stored = Vec::with_capacity(artifacts.len());
    for artifact in artifacts {
        total = total.checked_add(artifact.bytes.len()).ok_or_else(|| {
            AgentFailure::new(
                FailureCode::StorageFailed,
                "extension artifact byte length overflowed",
            )
        })?;
        if total > 64 * 1024 * 1024 {
            return Err(AgentFailure::new(
                FailureCode::StorageFailed,
                "extension artifacts exceed the 64 MiB aggregate bound",
            ));
        }
        stored.push(
            store
                .put(&artifact.bytes, &artifact.media_type, &artifact.extension)
                .map_err(|error| {
                    AgentFailure::new(
                        FailureCode::StorageFailed,
                        "extension artifact could not be persisted",
                    )
                    .with_detail("cause", error.to_string())
                })?,
        );
    }
    Ok(stored)
}
