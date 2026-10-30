use agent_core::ToolDefinition;
use serde_json::{Value, json};

use super::{
    IMAGE_COMPARE_TOOL, IMAGE_EXTRACT_TOOL, IMAGE_GENERATE_TOOL, IMAGE_INSPECT_TOOL,
    IMAGE_RENDER_TOOL, MAX_FONT_BYTES, MAX_IMAGE_BYTES, MAX_TOOL_PROMPT_BYTES,
};

pub(super) fn generate_definition() -> ToolDefinition {
    ToolDefinition {
        name: IMAGE_GENERATE_TOOL.to_owned(),
        description: "Generate exactly one bounded PNG image through the provider sealed by composition; the result is returned as a durable artifact. A compiled prompt artifact fixes dimensions and background. Its detail value is prompt intent, not native provider quality; quality may be supplied separately when the selected provider supports it.".to_owned(),
        input_schema: json!({
            "type": "object",
            "additionalProperties": false,
            "oneOf": [
                {"required":["prompt"],"not":{"anyOf":[{"required":["promptArtifact"]},{"required":["promptSha256"]}]}},
                {"required":["promptArtifact","promptSha256"],"not":{"required":["prompt"]}}
            ],
            "properties": {
                "promptArtifact": prompt_artifact_ref_schema(),
                "promptSha256": {"type":"string","pattern":"^[a-f0-9]{64}$"},
                "prompt": {"type": "string", "minLength": 1, "maxLength": MAX_TOOL_PROMPT_BYTES},
                "maximumBytes": {"type": "integer", "minimum": 1, "maximum": MAX_IMAGE_BYTES},
                "background": {"type": "string", "enum": ["auto", "transparent", "opaque"]},
                "size": {"type": "string", "enum": ["auto", "1024x1024", "1536x1024", "1024x1536"]},
                "quality": {"type": "string", "enum": ["auto", "low", "medium", "high"]}
            }
        }),
        strict: true,
    }
}

pub(super) fn inspect_definition() -> ToolDefinition {
    ToolDefinition {
        name: IMAGE_INSPECT_TOOL.to_owned(),
        description: "Inspect a validated image artifact through PerfectPixel, return bounded numeric evidence, and attach the same content-addressed PNG as a multimodal observation for the next model turn.".to_owned(),
        input_schema: json!({
            "type": "object",
            "additionalProperties": false,
            "required": ["artifact"],
            "properties": {
                "artifact": artifact_ref_schema()
            }
        }),
        strict: true,
    }
}

#[allow(clippy::unreadable_literal)]
pub(super) fn extract_definition() -> ToolDefinition {
    ToolDefinition {
        name: IMAGE_EXTRACT_TOOL.to_owned(),
        description: "Extract an editable object from an existing image without generative pixel synthesis. Supports alpha, chroma key, color range, alpha-connected component, or an explicit mask; returns object/mask/remainder artifacts.".to_owned(),
        input_schema: json!({
            "type":"object","additionalProperties":false,"required":["artifact","selector"],
            "properties":{
                "artifact": artifact_ref_schema(),
                "maskArtifact": artifact_ref_schema(),
                "selector":{
                    "oneOf":[
                        {"type":"object","additionalProperties":false,"required":["type"],"properties":{"type":{"const":"existing_alpha"},"minimumAlpha":{"type":"integer","minimum":0,"maximum":255}}},
                        {"type":"object","additionalProperties":false,"required":["type","keys","tolerance"],"properties":{"type":{"const":"chroma_key"},"keys":{"type":"array","minItems":1,"maxItems":16,"items":{"type":"array","minItems":3,"maxItems":3,"items":{"type":"integer","minimum":0,"maximum":255}}},"tolerance":{"type":"integer","minimum":0,"maximum":255}}},
                        {"type":"object","additionalProperties":false,"required":["type","minimum","maximum"],"properties":{"type":{"const":"color_range"},"minimum":{"type":"array","minItems":3,"maxItems":3,"items":{"type":"integer","minimum":0,"maximum":255}},"maximum":{"type":"array","minItems":3,"maxItems":3,"items":{"type":"integer","minimum":0,"maximum":255}}}},
                        {"type":"object","additionalProperties":false,"required":["type","point"],"properties":{"type":{"const":"alpha_component"},"point":{"type":"object","additionalProperties":false,"required":["xMillionths","yMillionths"],"properties":{"xMillionths":{"type":"integer","minimum":0,"maximum":1000000},"yMillionths":{"type":"integer","minimum":0,"maximum":1000000}}},"minimumAlpha":{"type":"integer","minimum":1,"maximum":255}}},
                        {"type":"object","additionalProperties":false,"required":["type"],"properties":{"type":{"const":"provided_mask"}}}
                        ,{"type":"object","additionalProperties":false,"required":["type","sourceRevision","requestRevision","helperIdentity","target"],"properties":{"type":{"const":"semantic"},"sourceRevision":{"type":"integer","minimum":1},"requestRevision":{"type":"integer","minimum":1},"helperIdentity":{"type":"string","minLength":1,"maxLength":256},"target":{"oneOf":[{"type":"object","additionalProperties":false,"required":["type","xMillionths","yMillionths"],"properties":{"type":{"const":"point"},"xMillionths":{"type":"integer","minimum":0,"maximum":1000000},"yMillionths":{"type":"integer","minimum":0,"maximum":1000000}}},{"type":"object","additionalProperties":false,"required":["type","xMillionths","yMillionths","widthMillionths","heightMillionths"],"properties":{"type":{"const":"box"},"xMillionths":{"type":"integer","minimum":0,"maximum":1000000},"yMillionths":{"type":"integer","minimum":0,"maximum":1000000},"widthMillionths":{"type":"integer","minimum":1,"maximum":1000000},"heightMillionths":{"type":"integer","minimum":1,"maximum":1000000}}},{"type":"object","additionalProperties":false,"required":["type","points"],"properties":{"type":{"const":"scribble"},"points":{"type":"array","minItems":1,"maxItems":1024,"items":{"type":"array","minItems":2,"maxItems":2,"items":{"type":"integer","minimum":0,"maximum":1000000}}}}}]}}}
                    ]
                },
                "refinement":{"type":"object","additionalProperties":false,"properties":{"featherRadius":{"type":"integer","minimum":0,"maximum":32},"threshold":{"type":["integer","null"],"minimum":0,"maximum":255},"requireSoftAlpha":{"type":"boolean"}}},
                "includeRemainder":{"type":"boolean"}
            }
        }),
        strict: true,
    }
}

pub(super) fn render_definition() -> ToolDefinition {
    ToolDefinition {
        name: IMAGE_RENDER_TOOL.to_owned(),
        description: "Render a bounded, stateless composition DAG through PerfectPixel from explicit raster/object artifacts. This is the canonical deterministic repair/edit surface: crop, rotate, flip, resize, keyed/automatic background removal, z-order, 3x3 affine transforms, opacity, text nodes, and source-over compositing. No hidden canvas state or generative fill is used.".to_owned(),
        input_schema: json!({
            "type":"object","additionalProperties":false,"required":["canvas","nodes"],
            "properties":{
                "canvas":{"type":"object","additionalProperties":false,"required":["width","height"],"properties":{"width":{"type":"integer","minimum":1,"maximum":8192},"height":{"type":"integer","minimum":1,"maximum":8192},"background":{"type":"array","minItems":4,"maxItems":4,"items":{"type":"integer","minimum":0,"maximum":255}}}},
                "nodes":{"type":"array","minItems":1,"maxItems":16,"items":{"type":"object","additionalProperties":false,"required":["id","kind","transform"],"properties":{"id":{"type":"string","minLength":1,"maxLength":128},"z":{"type":"integer"},"kind":{"type":"string","enum":["raster","object","text"]},"artifact":artifact_ref_schema(),"text":text_node_schema(),"transform":{"type":"object","additionalProperties":false,"required":["matrix"],"properties":{"matrix":{"type":"array","minItems":9,"maxItems":9,"items":{"type":"number"}}}},"opacity":{"type":"integer","minimum":0,"maximum":255},"operations":{"type":"array","maxItems":64,"items":raster_operation_schema()}},"oneOf":[{"required":["artifact"],"properties":{"kind":{"enum":["raster","object"]}},"not":{"required":["text"]}},{"required":["text"],"properties":{"kind":{"const":"text"}},"not":{"required":["artifact"]}}]}}
            }
        }),
        strict: true,
    }
}

fn raster_operation_schema() -> Value {
    json!({
        "oneOf": [
            {"type":"object","additionalProperties":false,"required":["op","x","y","width","height"],"properties":{"op":{"const":"crop"},"x":{"type":"integer","minimum":0},"y":{"type":"integer","minimum":0},"width":{"type":"integer","minimum":1},"height":{"type":"integer","minimum":1}}},
            {"type":"object","additionalProperties":false,"required":["op","quarterTurns"],"properties":{"op":{"const":"rotate"},"quarterTurns":{"type":"integer","enum":[1,2,3]}}},
            {"type":"object","additionalProperties":false,"required":["op","axis"],"properties":{"op":{"const":"flip"},"axis":{"type":"string","enum":["horizontal","vertical"]}}},
            {"type":"object","additionalProperties":false,"required":["op","width","height","filter"],"properties":{"op":{"const":"resize"},"width":{"type":"integer","minimum":1},"height":{"type":"integer","minimum":1},"filter":{"type":"string","enum":["nearest","lanczos3"]}}},
            {"type":"object","additionalProperties":false,"required":["op","keys","tolerance","feather"],"properties":{"op":{"const":"remove_background"},"keys":{"type":"array","minItems":1,"maxItems":16,"uniqueItems":true,"items":{"type":"array","minItems":3,"maxItems":3,"items":{"type":"integer","minimum":0,"maximum":255}}},"tolerance":{"type":"integer","minimum":0,"maximum":255},"feather":{"type":"integer","minimum":0,"maximum":255}}},
            {"type":"object","additionalProperties":false,"required":["op","maxKeys","minEdgeCoverageBasisPoints","tolerance","feather"],"properties":{"op":{"const":"remove_background_auto"},"maxKeys":{"type":"integer","minimum":1,"maximum":16},"minEdgeCoverageBasisPoints":{"type":"integer","minimum":1,"maximum":10000},"tolerance":{"type":"integer","minimum":0,"maximum":255},"feather":{"type":"integer","minimum":0,"maximum":255}}}
        ]
    })
}

pub(super) fn compare_definition() -> ToolDefinition {
    ToolDefinition {
        name: IMAGE_COMPARE_TOOL.to_owned(),
        description: "Compare two content-addressed image artifacts with deterministic pixel, region, mask, alpha, and geometry assertions. Required assertion failure is returned as diagnostic evidence rather than hidden as tool success; optional diff/overlay previews are attached for visual review.".to_owned(),
        input_schema: json!({
            "type":"object","additionalProperties":false,
            "required":["beforeArtifact","afterArtifact","assertions","preview"],
            "properties":{
                "beforeArtifact":artifact_ref_schema(),
                "afterArtifact":artifact_ref_schema(),
                "afterKind":{"type":"string","enum":["raster","object","export"],"default":"raster"},
                "assertions":{
                    "type":"array","minItems":1,"maxItems":256,
                    "contains":{"type":"object","required":["severity"],"properties":{"severity":{"const":"required"}}},
                    "minContains":1,
                    "items":{"oneOf":[
                        {"type":"object","additionalProperties":false,"required":["type","id","severity"],"properties":{"type":{"const":"exact_equal"},"id":compare_assertion_id_schema(),"severity":compare_severity_schema()}},
                        {"type":"object","additionalProperties":false,"required":["type","id","severity"],"anyOf":[{"required":["minimum"]},{"required":["maximum"]}],"properties":{"type":{"const":"changed_ratio"},"id":compare_assertion_id_schema(),"severity":compare_severity_schema(),"minimum":{"type":"number","minimum":0.0,"maximum":1.0},"maximum":{"type":"number","minimum":0.0,"maximum":1.0}}},
                        {"type":"object","additionalProperties":false,"required":["type","id","severity","maximum","maskArtifact"],"properties":{"type":{"const":"outside_mask_changed_ratio"},"id":compare_assertion_id_schema(),"severity":compare_severity_schema(),"maximum":{"type":"number","minimum":0.0,"maximum":1.0},"maskArtifact":artifact_ref_schema()}},
                        {"type":"object","additionalProperties":false,"required":["type","id","severity","minimum","maskArtifact"],"properties":{"type":{"const":"inside_mask_changed_ratio"},"id":compare_assertion_id_schema(),"severity":compare_severity_schema(),"minimum":{"type":"number","minimum":0.0,"maximum":1.0},"maximum":{"type":"number","minimum":0.0,"maximum":1.0},"maskArtifact":artifact_ref_schema()}},
                        {"type":"object","additionalProperties":false,"required":["type","id","severity","region"],"properties":{"type":{"const":"unchanged_region_exact"},"id":compare_assertion_id_schema(),"severity":compare_severity_schema(),"region":compare_rect_schema()}},
                        {"type":"object","additionalProperties":false,"required":["type","id","severity","minimum","maskArtifact"],"properties":{"type":{"const":"alpha_iou"},"id":compare_assertion_id_schema(),"severity":compare_severity_schema(),"minimum":{"type":"number","minimum":0.0,"maximum":1.0},"maskArtifact":artifact_ref_schema()}},
                        {"type":"object","additionalProperties":false,"required":["type","id","severity","maximum","maskArtifact"],"properties":{"type":{"const":"mask_leakage_ratio"},"id":compare_assertion_id_schema(),"severity":compare_severity_schema(),"maximum":{"type":"number","minimum":0.0,"maximum":1.0},"maskArtifact":artifact_ref_schema()}},
                        {"type":"object","additionalProperties":false,"required":["type","id","severity","expected","tolerance"],"properties":{"type":{"const":"object_bounds"},"id":compare_assertion_id_schema(),"severity":compare_severity_schema(),"expected":compare_rect_schema(),"tolerance":{"type":"integer","minimum":0,"maximum":8192}}},
                        {"type":"object","additionalProperties":false,"required":["type","id","severity","expected","tolerance"],"properties":{"type":{"const":"object_centroid"},"id":compare_assertion_id_schema(),"severity":compare_severity_schema(),"expected":{"type":"array","minItems":2,"maxItems":2,"items":{"type":"number"}},"tolerance":{"type":"number","minimum":0.0}}},
                        {"type":"object","additionalProperties":false,"required":["type","id","severity","expected","tolerance"],"properties":{"type":{"const":"object_area"},"id":compare_assertion_id_schema(),"severity":compare_severity_schema(),"expected":{"type":"integer","minimum":0},"tolerance":{"type":"integer","minimum":0}}},
                        {"type":"object","additionalProperties":false,"required":["type","id","severity","maximum"],"properties":{"type":{"const":"maximum_channel_error"},"id":compare_assertion_id_schema(),"severity":compare_severity_schema(),"maximum":{"type":"integer","minimum":0,"maximum":255}}},
                        {"type":"object","additionalProperties":false,"required":["type","id","severity","maximum"],"properties":{"type":{"const":"mean_absolute_error"},"id":compare_assertion_id_schema(),"severity":compare_severity_schema(),"maximum":{"type":"number","minimum":0.0,"maximum":1.0}}}
                    ]}
                },
                "preview":{"type":"object","additionalProperties":false,"required":["difference","maximumEdge"],"properties":{"difference":{"type":"boolean"},"maskOverlay":{"type":"boolean"},"maximumEdge":{"type":"integer","minimum":64,"maximum":4096}}}
            }
        }),
        strict: true,
    }
}

fn compare_assertion_id_schema() -> Value {
    json!({"type":"string","pattern":"^[A-Za-z][A-Za-z0-9_-]{0,63}$"})
}

fn compare_severity_schema() -> Value {
    json!({"type":"string","enum":["required","advisory"]})
}

fn compare_rect_schema() -> Value {
    json!({
        "type":"object","additionalProperties":false,
        "required":["x","y","width","height"],
        "properties":{
            "x":{"type":"integer","minimum":0},
            "y":{"type":"integer","minimum":0},
            "width":{"type":"integer","minimum":1},
            "height":{"type":"integer","minimum":1}
        }
    })
}

pub(crate) fn artifact_ref_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["id", "sha256", "mediaType", "byteLength", "relativePath"],
        "properties": {
            "id": {"type": "string", "minLength": 1, "maxLength": 160},
            "sha256": {"type": "string", "pattern": "^[0-9A-Fa-f]{64}$"},
            "mediaType": {"const": "image/png"},
            "byteLength": {"type": "integer", "minimum": 1, "maximum": MAX_IMAGE_BYTES},
            "relativePath": {"type": "string", "minLength": 1, "maxLength": 512}
        }
    })
}

fn prompt_artifact_ref_schema() -> Value {
    constrained_artifact_ref_schema(
        agent_core::PROMPT_MEDIA_TYPE,
        agent_core::MAX_PROMPT_ARTIFACT_BYTES as u64,
    )
}

pub(crate) fn prompt_input_artifact_ref_schema() -> Value {
    constrained_artifact_ref_schema(
        agent_core::PROMPT_INPUT_MEDIA_TYPE,
        agent_core::MAX_COMPILE_INPUT_BYTES as u64,
    )
}

fn constrained_artifact_ref_schema(media_type: &str, maximum_bytes: u64) -> Value {
    let mut schema = artifact_ref_schema();
    if let Some(properties) = schema.get_mut("properties").and_then(Value::as_object_mut) {
        properties.insert("mediaType".to_owned(), json!({"const":media_type}));
        properties.insert(
            "byteLength".to_owned(),
            json!({"type":"integer","minimum":1,"maximum":maximum_bytes}),
        );
    }
    schema
}

fn text_node_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["content", "direction", "language", "boxWidth", "boxHeight", "alignment", "lineBreak", "fontSize", "color", "pixelSpec", "fontArtifact"],
        "properties": {
            "content": {"type": "string", "minLength": 1, "maxLength": 65536},
            "direction": {"type": "string", "enum": ["ltr", "rtl"]},
            "language": {"type": "string", "minLength": 1, "maxLength": 35, "pattern": "^[A-Za-z0-9-]+$"},
            "boxWidth": {"type": "integer", "minimum": 1, "maximum": 8192},
            "boxHeight": {"type": "integer", "minimum": 1, "maximum": 8192},
            "alignment": {"type": "string", "enum": ["start", "center", "end"]},
            "lineBreak": {"type": "string", "enum": ["no_wrap", "word_wrap"]},
            "fontSize": {"type": "number", "minimum": 0.1, "maximum": 1024},
            "color": {"type": "array", "minItems": 4, "maxItems": 4, "items": {"type": "integer", "minimum": 0, "maximum": 255}},
            "pixelSpec": {
                "type": "object", "additionalProperties": false,
                "required": ["format", "colorSpace", "alphaMode"],
                "properties": {"format": {"const": "rgba8"}, "colorSpace": {"const": "srgb"}, "alphaMode": {"const": "straight"}}
            },
            "fontArtifact": font_artifact_schema()
        }
    })
}

fn font_artifact_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["id", "sha256", "mediaType", "byteLength", "relativePath"],
        "properties": {
            "id": {"type": "string", "minLength": 1, "maxLength": 160},
            "sha256": {"type": "string", "pattern": "^[0-9A-Fa-f]{64}$"},
            "mediaType": {"type": "string", "enum": ["font/ttf", "font/otf"]},
            "byteLength": {"type": "integer", "minimum": 1, "maximum": MAX_FONT_BYTES},
            "relativePath": {"type": "string", "minLength": 1, "maxLength": 512}
        }
    })
}
