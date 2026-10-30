use std::{
    collections::BTreeSet,
    path::{Component, Path, PathBuf},
};

use serde::{Deserialize, Serialize};

use crate::{
    agent_capability_manifest, apply_raster_edits, compare_images, dependency_closure_sha256,
    difference_preview, extract_object, inspect_raster, mask_overlay_preview, render_composition,
    AffineTransform, AgentArtifactDependency, AgentArtifactDescriptor, AgentArtifactKind,
    AgentArtifactRetention, AgentDeterminismClass, AgentInputArtifact, AgentOperationReceipt,
    AgentOperationStatus, AgentPixelSpec, AtomicDirectoryEntry, AtomicDirectoryWriter,
    CompareAssertionResult, CompareMetrics, CompareSeverity, DecodeLimits, ExtractSelector,
    ImageCodec, LineBreakMode, MatteRefinement, ObjectBounds, PngEncoder, PpError, PpResult,
    Raster, RasterEdit, RasterInspection, RenderCanvas, RenderFilter, RenderNode, ResampleFilter,
    TextAlignment, TextDirection, TextLayoutSnapshot, TextNode, AGENT_BEHAVIOR_VERSION,
    AGENT_COMPARE_REQUEST_SCHEMA, AGENT_COMPARE_RESULT_SCHEMA, AGENT_EXTRACT_REQUEST_SCHEMA,
    AGENT_EXTRACT_RESULT_SCHEMA, AGENT_INSPECT_REQUEST_SCHEMA, AGENT_INSPECT_RESULT_SCHEMA,
    AGENT_PROTOCOL_VERSION, AGENT_RECEIPT_SCHEMA, AGENT_RENDER_REQUEST_SCHEMA,
    AGENT_RENDER_RESULT_SCHEMA, MAX_FONT_BYTES,
};

use super::super::{
    generation_adapter::InputSnapshot,
    path::{validate_file_extension, validate_raster_input_path},
    shared::{read_json_request_snapshot, serialize_json, MAX_RASTER_READ_BYTES},
};

mod compare;

use compare::{bounded_preview, prepare_compare_assertion};

fn validate_options(args: &[String], allowed: &[&str]) -> PpResult<()> {
    let mut seen = BTreeSet::new();
    let mut index = 0usize;
    while index < args.len() {
        let key = args[index].as_str();
        if !key.starts_with("--") {
            return Err(PpError::InvalidOption(format!(
                "unexpected positional argument '{key}'"
            )));
        }
        if !allowed.contains(&key) {
            return Err(PpError::InvalidOption(format!("unknown option '{key}'")));
        }
        if !seen.insert(key.to_owned()) {
            return Err(PpError::InvalidOption(format!("duplicate option '{key}'")));
        }
        let Some(value) = args.get(index + 1) else {
            return Err(PpError::InvalidOption(format!(
                "missing value for option '{key}'"
            )));
        };
        if value.starts_with("--") {
            return Err(PpError::InvalidOption(format!(
                "missing value for option '{key}'"
            )));
        }
        index += 2;
    }
    Ok(())
}

fn option_path(args: &[String], key: &str) -> PpResult<PathBuf> {
    args.windows(2)
        .find(|pair| pair[0] == key)
        .map(|pair| PathBuf::from(&pair[1]))
        .ok_or_else(|| PpError::InvalidOption(format!("{key} is required")))
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct AgentInspectRequest {
    schema: String,
    request_id: String,
    source: AgentInputArtifact,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct AgentInspectResult {
    schema: &'static str,
    request_id: String,
    status: &'static str,
    protocol_version: &'static str,
    behavior_version: &'static str,
    source: AgentArtifactDescriptor,
    inspection: AgentRasterInspection,
    receipt: AgentOperationReceipt,
}

/// Agent image protocol inspection includes the explicit normalized colour
/// space. The general asset inspection intentionally keeps colour provenance
/// unknown when no ICC profile is present, so this projection is kept at the
/// protocol boundary instead of widening that public asset contract.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct AgentRasterInspection {
    #[serde(flatten)]
    raster: RasterInspection,
    color_space: &'static str,
}

pub(crate) fn agent_schema() -> PpResult<String> {
    serialize_json(
        &agent_capability_manifest(env!("CARGO_PKG_VERSION")),
        "<agent-schema>",
    )
}

pub(crate) fn agent_inspect(args: &[String]) -> PpResult<String> {
    validate_options(args, &["--request"])?;
    let request_path = option_path(args, "--request")?;
    validate_file_extension(&request_path, &["json"], "agent inspect request")?;
    let (request, request_snapshot): (AgentInspectRequest, _) =
        read_json_request_snapshot(&request_path)?;
    if request.schema != AGENT_INSPECT_REQUEST_SCHEMA
        || request.request_id.trim().is_empty()
        || request.request_id.len() > 192
    {
        return Err(PpError::InvalidRequest(
            "agent inspect envelope is invalid".to_owned(),
        ));
    }
    if request.source.media_type != "image/png"
        || request.source.kind != AgentArtifactKind::WorkingRaster
    {
        return Err(PpError::InvalidRequest(
            "agent inspect requires a working image/png artifact".to_owned(),
        ));
    }
    crate::validate_canonical_sha256(&request.source.expected_sha256)?;
    let input = PathBuf::from(&request.source.path);
    if !input.is_absolute()
        || input
            .components()
            .any(|component| component == Component::ParentDir)
    {
        return Err(PpError::InvalidRequest(
            "agent inspect source path must be absolute and traversal-free".to_owned(),
        ));
    }
    if let Some(pixel_spec) = request.source.pixel_spec {
        if pixel_spec != AgentPixelSpec::rgba8_srgb_straight() {
            return Err(PpError::InvalidRequest(
                "agent inspect source pixelSpec must be rgba8/srgb/straight".to_owned(),
            ));
        }
    }
    validate_raster_input_path(&input)?;
    let (bytes, input_snapshot) = InputSnapshot::capture(&input, MAX_RASTER_READ_BYTES)?;
    if u64::try_from(bytes.len()).ok() != Some(request.source.byte_length)
        || input_snapshot.sha256() != request.source.expected_sha256
    {
        return Err(PpError::InvalidRequest(
            "agent inspect source changed or does not match its content address".to_owned(),
        ));
    }
    let image = ImageCodec::decode_rgba_bytes(&input, &bytes, DecodeLimits::default())?;
    let inspection = inspect_raster(&image);
    let dependency = AgentArtifactDependency {
        sha256: request.source.expected_sha256.clone(),
        media_type: request.source.media_type.clone(),
        byte_length: request.source.byte_length,
    };
    let dependencies = vec![dependency];
    let closure = dependency_closure_sha256(&dependencies)?;
    let source = AgentArtifactDescriptor {
        sha256: request.source.expected_sha256.clone(),
        media_type: request.source.media_type.clone(),
        byte_length: request.source.byte_length,
        kind: AgentArtifactKind::WorkingRaster,
        pixel_spec: Some(AgentPixelSpec::rgba8_srgb_straight()),
        retention: AgentArtifactRetention::TaskScoped,
        dependencies: Vec::new(),
        dependency_closure_sha256: dependency_closure_sha256(&[])?,
    };
    let receipt = AgentOperationReceipt {
        schema: AGENT_RECEIPT_SCHEMA.to_owned(),
        request_id: request.request_id.clone(),
        operation: "inspect".to_owned(),
        status: AgentOperationStatus::Committed,
        behavior_version: AGENT_BEHAVIOR_VERSION.to_owned(),
        implementation_version: env!("CARGO_PKG_VERSION").to_owned(),
        request_sha256: request_snapshot.sha256().to_owned(),
        dependency_closure_sha256: closure,
        dependencies,
        output_sha256: None,
        determinism: AgentDeterminismClass::BitExact,
    };
    if input_snapshot.sha256() != request.source.expected_sha256 {
        return Err(PpError::InvalidRequest(
            "agent inspect input snapshot does not match its content address".to_owned(),
        ));
    }
    serialize_json(
        &AgentInspectResult {
            schema: AGENT_INSPECT_RESULT_SCHEMA,
            request_id: request.request_id,
            status: "committed",
            protocol_version: AGENT_PROTOCOL_VERSION,
            behavior_version: AGENT_BEHAVIOR_VERSION,
            source,
            inspection: AgentRasterInspection {
                raster: inspection,
                color_space: "srgb",
            },
            receipt,
        },
        "<agent-inspect>",
    )
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct AgentExtractRequest {
    schema: String,
    request_id: String,
    source: AgentInputArtifact,
    selector: ExtractSelector,
    #[serde(default)]
    mask: Option<AgentInputArtifact>,
    #[serde(default)]
    refinement: MatteRefinement,
    #[serde(default = "default_true")]
    include_remainder: bool,
}

const fn default_true() -> bool {
    true
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct AgentExtractResult {
    schema: &'static str,
    request_id: String,
    status: &'static str,
    protocol_version: &'static str,
    behavior_version: &'static str,
    source_canvas: AgentCanvasSize,
    source_bbox: AgentBounds,
    pivot: [f64; 2],
    completeness: &'static str,
    occluded_background: &'static str,
    soft_alpha_pixels: u64,
    object: AgentPublishedArtifact,
    mask: AgentPublishedArtifact,
    #[serde(skip_serializing_if = "Option::is_none")]
    remainder: Option<AgentPublishedArtifact>,
    receipt: AgentOperationReceipt,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct AgentCanvasSize {
    width: u32,
    height: u32,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct AgentBounds {
    x: u32,
    y: u32,
    width: u32,
    height: u32,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct AgentPublishedArtifact {
    relative_path: String,
    descriptor: AgentArtifactDescriptor,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct AgentRenderRequest {
    schema: String,
    request_id: String,
    canvas: AgentRenderCanvas,
    nodes: Vec<AgentRenderNodeRequest>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct AgentRenderCanvas {
    width: u32,
    height: u32,
    #[serde(default)]
    background: [u8; 4],
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct AgentRenderNodeRequest {
    id: String,
    z: i32,
    #[serde(default)]
    source: Option<AgentInputArtifact>,
    #[serde(default)]
    kind: AgentRenderNodeKind,
    #[serde(default)]
    text: Option<AgentTextNodeRequest>,
    transform: AffineTransform,
    #[serde(default = "default_opacity")]
    opacity: u8,
    #[serde(default = "default_render_filter")]
    filter: RenderFilter,
    #[serde(default)]
    operations: Vec<AgentRasterOperationRequest>,
}

#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum AgentRenderNodeKind {
    #[default]
    Raster,
    Object,
    Text,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct AgentTextNodeRequest {
    content: String,
    direction: TextDirection,
    language: String,
    box_width: u32,
    box_height: u32,
    alignment: TextAlignment,
    line_break: LineBreakMode,
    font_size: f32,
    color: [u8; 4],
    pixel_spec: AgentPixelSpec,
    font: AgentFontInput,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct AgentFontInput {
    artifact: AgentFontArtifact,
    path: String,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct AgentFontArtifact {
    id: String,
    sha256: String,
    media_type: String,
    byte_length: u64,
    relative_path: String,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(
    tag = "op",
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
enum AgentRasterOperationRequest {
    Crop {
        x: u32,
        y: u32,
        width: u32,
        height: u32,
    },
    Rotate {
        quarter_turns: u8,
    },
    Flip {
        axis: AgentFlipAxis,
    },
    Resize {
        width: u32,
        height: u32,
        filter: ResampleFilter,
    },
    RemoveBackground {
        keys: Vec<[u8; 3]>,
        tolerance: u8,
        feather: u8,
    },
    RemoveBackgroundAuto {
        max_keys: u8,
        min_edge_coverage_basis_points: u16,
        tolerance: u8,
        feather: u8,
    },
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum AgentFlipAxis {
    Horizontal,
    Vertical,
}

impl AgentRasterOperationRequest {
    fn into_edit(self) -> RasterEdit {
        match self {
            Self::Crop {
                x,
                y,
                width,
                height,
            } => RasterEdit::Crop {
                x,
                y,
                width,
                height,
            },
            Self::Rotate { quarter_turns } => RasterEdit::RotateQuarterTurns { quarter_turns },
            Self::Flip {
                axis: AgentFlipAxis::Horizontal,
            } => RasterEdit::FlipHorizontal,
            Self::Flip {
                axis: AgentFlipAxis::Vertical,
            } => RasterEdit::FlipVertical,
            Self::Resize {
                width,
                height,
                filter,
            } => RasterEdit::Resize {
                width,
                height,
                filter,
            },
            Self::RemoveBackground {
                keys,
                tolerance,
                feather,
            } => RasterEdit::RemoveBackground {
                keys,
                tolerance,
                feather,
            },
            Self::RemoveBackgroundAuto {
                max_keys,
                min_edge_coverage_basis_points,
                tolerance,
                feather,
            } => RasterEdit::RemoveBackgroundAuto {
                max_keys,
                min_edge_coverage_basis_points,
                tolerance,
                feather,
            },
        }
    }
}

const fn default_opacity() -> u8 {
    255
}
const fn default_render_filter() -> RenderFilter {
    RenderFilter::Nearest
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct AgentRenderResult {
    schema: &'static str,
    request_id: String,
    status: &'static str,
    protocol_version: &'static str,
    behavior_version: &'static str,
    output: AgentPublishedArtifact,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    text_nodes: Vec<AgentTextNodeEvidence>,
    receipt: AgentOperationReceipt,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct AgentTextNodeEvidence {
    node_id: String,
    layout: TextLayoutSnapshot,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct AgentCompareRequest {
    schema: String,
    request_id: String,
    before: AgentInputArtifact,
    after: AgentInputArtifact,
    assertions: Vec<AgentCompareAssertionRequest>,
    preview: AgentComparePreviewRequest,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
enum AgentCompareAssertionRequest {
    ExactEqual {
        id: String,
        severity: CompareSeverity,
    },
    ChangedRatio {
        id: String,
        severity: CompareSeverity,
        #[serde(default)]
        minimum: Option<f64>,
        #[serde(default)]
        maximum: Option<f64>,
    },
    OutsideMaskChangedRatio {
        id: String,
        severity: CompareSeverity,
        maximum: f64,
        mask: AgentInputArtifact,
    },
    InsideMaskChangedRatio {
        id: String,
        severity: CompareSeverity,
        minimum: f64,
        #[serde(default)]
        maximum: Option<f64>,
        mask: AgentInputArtifact,
    },
    UnchangedRegionExact {
        id: String,
        severity: CompareSeverity,
        region: ObjectBounds,
    },
    AlphaIou {
        id: String,
        severity: CompareSeverity,
        minimum: f64,
        mask: AgentInputArtifact,
    },
    MaskLeakageRatio {
        id: String,
        severity: CompareSeverity,
        maximum: f64,
        mask: AgentInputArtifact,
    },
    ObjectBounds {
        id: String,
        severity: CompareSeverity,
        expected: ObjectBounds,
        tolerance: u32,
    },
    ObjectCentroid {
        id: String,
        severity: CompareSeverity,
        expected: [f64; 2],
        tolerance: f64,
    },
    ObjectArea {
        id: String,
        severity: CompareSeverity,
        expected: u64,
        tolerance: u64,
    },
    MaximumChannelError {
        id: String,
        severity: CompareSeverity,
        maximum: u8,
    },
    MeanAbsoluteError {
        id: String,
        severity: CompareSeverity,
        maximum: f64,
    },
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct AgentComparePreviewRequest {
    difference: bool,
    #[serde(default)]
    mask_overlay: bool,
    maximum_edge: u32,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct AgentComparePreviewResult {
    role: &'static str,
    relative_path: String,
    descriptor: AgentArtifactDescriptor,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct AgentCompareResult {
    schema: &'static str,
    request_id: String,
    status: &'static str,
    protocol_version: &'static str,
    behavior_version: &'static str,
    all_required_passed: bool,
    metrics: CompareMetrics,
    assertions: Vec<CompareAssertionResult>,
    previews: Vec<AgentComparePreviewResult>,
    receipt: AgentOperationReceipt,
}

pub(crate) fn agent_extract(args: &[String]) -> PpResult<String> {
    validate_options(args, &["--request", "--out-dir"])?;
    let request_path = option_path(args, "--request")?;
    let output_dir = option_path(args, "--out-dir")?;
    validate_file_extension(&request_path, &["json"], "agent extract request")?;
    validate_agent_output_dir(&request_path, &output_dir, &[])?;
    let (request, request_snapshot): (AgentExtractRequest, _) =
        read_json_request_snapshot(&request_path)?;
    validate_agent_request_id(
        &request.schema,
        AGENT_EXTRACT_REQUEST_SCHEMA,
        &request.request_id,
    )?;
    let (source, source_dependency) =
        read_agent_raster(&request.source, AgentArtifactKind::WorkingRaster)?;
    let source_path = PathBuf::from(&request.source.path);
    validate_agent_output_dir(&request_path, &output_dir, &[source_path.as_path()])?;
    let mut dependencies = vec![source_dependency.clone()];
    let mask_raster = if let Some(mask) = &request.mask {
        let (raster, dependency) = read_agent_raster(mask, AgentArtifactKind::Mask)?;
        validate_agent_output_dir(
            &request_path,
            &output_dir,
            &[source_path.as_path(), Path::new(&mask.path)],
        )?;
        dependencies.push(dependency);
        Some(raster)
    } else {
        None
    };
    if matches!(&request.selector, ExtractSelector::ProvidedMask) != mask_raster.is_some() {
        return Err(PpError::InvalidRequest(
            "provided_mask selector requires exactly one mask artifact and other selectors must not provide one".to_owned(),
        ));
    }
    dependencies.sort();
    dependencies.dedup();
    let extracted = extract_object(
        &source,
        &request.selector,
        mask_raster.as_ref(),
        request.refinement,
    )?;
    let object_bytes = PngEncoder::encode_rgba(&extracted.object)?;
    let mask_bytes = PngEncoder::encode_rgba(&extracted.mask)?;
    let remainder_bytes = request
        .include_remainder
        .then(|| PngEncoder::encode_rgba(&extracted.remainder))
        .transpose()?;
    let object_descriptor = agent_output_descriptor(
        &object_bytes,
        AgentArtifactKind::Object,
        dependencies.clone(),
    )?;
    let mask_descriptor =
        agent_output_descriptor(&mask_bytes, AgentArtifactKind::Mask, dependencies.clone())?;
    let remainder_descriptor = remainder_bytes
        .as_ref()
        .map(|bytes| {
            agent_output_descriptor(
                bytes,
                AgentArtifactKind::WorkingRaster,
                dependencies.clone(),
            )
        })
        .transpose()?;

    let mut entries = vec![
        AtomicDirectoryEntry {
            relative_path: Path::new("object.png"),
            bytes: &object_bytes,
            sha256: crate::sha256(&object_bytes),
        },
        AtomicDirectoryEntry {
            relative_path: Path::new("mask.png"),
            bytes: &mask_bytes,
            sha256: crate::sha256(&mask_bytes),
        },
    ];
    if let Some(bytes) = remainder_bytes.as_ref() {
        entries.push(AtomicDirectoryEntry {
            relative_path: Path::new("remainder.png"),
            bytes,
            sha256: crate::sha256(bytes),
        });
    }
    AtomicDirectoryWriter::replace_new_or_empty(&output_dir, &entries)?;
    let closure = dependency_closure_sha256(&dependencies)?;
    let receipt = AgentOperationReceipt {
        schema: AGENT_RECEIPT_SCHEMA.to_owned(),
        request_id: request.request_id.clone(),
        operation: "extract".to_owned(),
        status: AgentOperationStatus::Committed,
        behavior_version: AGENT_BEHAVIOR_VERSION.to_owned(),
        implementation_version: env!("CARGO_PKG_VERSION").to_owned(),
        request_sha256: request_snapshot.sha256().to_owned(),
        dependency_closure_sha256: closure,
        dependencies,
        output_sha256: Some(object_descriptor.sha256.clone()),
        determinism: AgentDeterminismClass::BitExact,
    };
    let bounds = extracted.source_bounds;
    serialize_json(
        &AgentExtractResult {
            schema: AGENT_EXTRACT_RESULT_SCHEMA,
            request_id: request.request_id,
            status: "committed",
            protocol_version: AGENT_PROTOCOL_VERSION,
            behavior_version: AGENT_BEHAVIOR_VERSION,
            source_canvas: AgentCanvasSize {
                width: extracted.source_width,
                height: extracted.source_height,
            },
            source_bbox: AgentBounds {
                x: bounds.x,
                y: bounds.y,
                width: bounds.width,
                height: bounds.height,
            },
            pivot: [0.5, 0.5],
            completeness: "visible_pixels_only",
            occluded_background: "unknown",
            soft_alpha_pixels: extracted.soft_alpha_pixels,
            object: AgentPublishedArtifact {
                relative_path: "object.png".to_owned(),
                descriptor: object_descriptor,
            },
            mask: AgentPublishedArtifact {
                relative_path: "mask.png".to_owned(),
                descriptor: mask_descriptor,
            },
            remainder: remainder_descriptor.map(|descriptor| AgentPublishedArtifact {
                relative_path: "remainder.png".to_owned(),
                descriptor,
            }),
            receipt,
        },
        "<agent-extract>",
    )
}

pub(crate) fn agent_render(args: &[String]) -> PpResult<String> {
    validate_options(args, &["--request", "--out-dir"])?;
    let request_path = option_path(args, "--request")?;
    let output_dir = option_path(args, "--out-dir")?;
    validate_file_extension(&request_path, &["json"], "agent render request")?;
    let (request, request_snapshot): (AgentRenderRequest, _) =
        read_json_request_snapshot(&request_path)?;
    let AgentRenderRequest {
        schema,
        request_id,
        canvas,
        nodes,
    } = request;
    validate_agent_request_id(&schema, AGENT_RENDER_REQUEST_SCHEMA, &request_id)?;
    if nodes.is_empty() || nodes.len() > 64 {
        return Err(PpError::InvalidRequest(
            "render nodes must contain 1..=64 entries".to_owned(),
        ));
    }

    let mut dependencies = Vec::with_capacity(nodes.len());
    let mut inputs = Vec::with_capacity(nodes.len());
    let mut input_paths = Vec::with_capacity(nodes.len());
    let mut text_nodes = Vec::new();
    let mut ids = BTreeSet::new();

    for node in nodes {
        if !ids.insert(node.id.clone()) {
            return Err(PpError::InvalidRequest(
                "render node ids must be unique".to_owned(),
            ));
        }
        if node.filter != RenderFilter::Nearest {
            return Err(PpError::InvalidRequest(
                "affine composition exposes nearest sampling only; resize filtering belongs to node operations".to_owned(),
            ));
        }

        let source = match node.kind {
            AgentRenderNodeKind::Raster | AgentRenderNodeKind::Object => {
                if node.text.is_some() {
                    return Err(PpError::InvalidRequest(
                        "raster/object render nodes cannot contain text".to_owned(),
                    ));
                }
                let source = node.source.ok_or_else(|| {
                    PpError::InvalidRequest("raster/object render nodes require source".to_owned())
                })?;
                let expected_kind = if node.kind == AgentRenderNodeKind::Object {
                    AgentArtifactKind::Object
                } else {
                    AgentArtifactKind::WorkingRaster
                };
                if source.kind != expected_kind {
                    return Err(PpError::InvalidRequest(
                        "render node kind and source artifact kind disagree".to_owned(),
                    ));
                }
                let path = PathBuf::from(&source.path);
                input_paths.push(path);
                let (raster, dependency) = read_agent_raster(&source, expected_kind)?;
                dependencies.push(dependency);
                let edits = node
                    .operations
                    .into_iter()
                    .map(AgentRasterOperationRequest::into_edit)
                    .collect::<Vec<_>>();
                apply_raster_edits(&raster, &edits)?
            }
            AgentRenderNodeKind::Text => {
                if node.source.is_some() {
                    return Err(PpError::InvalidRequest(
                        "text render nodes cannot contain raster source".to_owned(),
                    ));
                }
                if !node.operations.is_empty() {
                    return Err(PpError::InvalidRequest(
                        "text render nodes do not accept raster operations".to_owned(),
                    ));
                }
                let text = node.text.ok_or_else(|| {
                    PpError::InvalidRequest("text render nodes require text".to_owned())
                })?;
                let (font_bytes, font_dependency, font_path) = read_agent_font(&text.font)?;
                input_paths.push(font_path);
                dependencies.push(font_dependency);
                let rendered_text = TextNode {
                    content: text.content,
                    direction: text.direction,
                    language: text.language,
                    box_width: text.box_width,
                    box_height: text.box_height,
                    alignment: text.alignment,
                    line_break: text.line_break,
                    font_size: text.font_size,
                    color: text.color,
                    pixel_spec: text.pixel_spec,
                    font_sha256: text.font.artifact.sha256,
                    font_bytes,
                }
                .render()?;
                text_nodes.push(AgentTextNodeEvidence {
                    node_id: node.id.clone(),
                    layout: rendered_text.layout,
                });
                rendered_text.raster
            }
        };

        inputs.push(RenderNode {
            id: node.id,
            z: node.z,
            source,
            transform: node.transform,
            opacity: node.opacity,
            filter: node.filter,
        });
    }

    let refs = input_paths.iter().map(PathBuf::as_path).collect::<Vec<_>>();
    validate_agent_output_dir(&request_path, &output_dir, &refs)?;
    dependencies.sort();
    dependencies.dedup();
    let rendered = render_composition(
        RenderCanvas {
            width: canvas.width,
            height: canvas.height,
            background: canvas.background,
        },
        &inputs,
    )?;
    let output_bytes = PngEncoder::encode_rgba(&rendered)?;
    let output_descriptor = agent_output_descriptor(
        &output_bytes,
        AgentArtifactKind::WorkingRaster,
        dependencies.clone(),
    )?;
    let entries = [AtomicDirectoryEntry {
        relative_path: Path::new("render.png"),
        bytes: &output_bytes,
        sha256: crate::sha256(&output_bytes),
    }];
    AtomicDirectoryWriter::replace_new_or_empty(&output_dir, &entries)?;
    let receipt = AgentOperationReceipt {
        schema: AGENT_RECEIPT_SCHEMA.to_owned(),
        request_id: request_id.clone(),
        operation: "render".to_owned(),
        status: AgentOperationStatus::Committed,
        behavior_version: AGENT_BEHAVIOR_VERSION.to_owned(),
        implementation_version: env!("CARGO_PKG_VERSION").to_owned(),
        request_sha256: request_snapshot.sha256().to_owned(),
        dependency_closure_sha256: dependency_closure_sha256(&dependencies)?,
        dependencies,
        output_sha256: Some(output_descriptor.sha256.clone()),
        determinism: AgentDeterminismClass::BackendDeterministic,
    };
    serialize_json(
        &AgentRenderResult {
            schema: AGENT_RENDER_RESULT_SCHEMA,
            request_id,
            status: "committed",
            protocol_version: AGENT_PROTOCOL_VERSION,
            behavior_version: AGENT_BEHAVIOR_VERSION,
            output: AgentPublishedArtifact {
                relative_path: "render.png".to_owned(),
                descriptor: output_descriptor,
            },
            text_nodes,
            receipt,
        },
        "<agent-render>",
    )
}

pub(crate) fn agent_compare(args: &[String]) -> PpResult<String> {
    validate_options(args, &["--request", "--out-dir"])?;
    let request_path = option_path(args, "--request")?;
    let output_dir = option_path(args, "--out-dir")?;
    validate_file_extension(&request_path, &["json"], "agent compare request")?;
    let (request, request_snapshot): (AgentCompareRequest, _) =
        read_json_request_snapshot(&request_path)?;
    validate_agent_request_id(
        &request.schema,
        AGENT_COMPARE_REQUEST_SCHEMA,
        &request.request_id,
    )?;
    if request.preview.maximum_edge < 64 || request.preview.maximum_edge > 4096 {
        return Err(PpError::InvalidRequest(
            "compare preview maximumEdge must be within 64..=4096".to_owned(),
        ));
    }
    let (before, before_dependency) =
        read_agent_raster(&request.before, AgentArtifactKind::WorkingRaster)?;
    let (after, after_dependency) = read_agent_raster_any(&request.after)?;
    if !matches!(
        request.after.kind,
        AgentArtifactKind::WorkingRaster
            | AgentArtifactKind::Object
            | AgentArtifactKind::ExportRaster
    ) {
        return Err(PpError::InvalidRequest(
            "compare after artifact kind must be working_raster, export_raster, or object"
                .to_owned(),
        ));
    }
    let mut dependencies = vec![before_dependency, after_dependency];
    let mut masks = Vec::<Raster>::new();
    let mut mask_paths = Vec::<PathBuf>::new();
    let mut assertions = Vec::with_capacity(request.assertions.len());
    for assertion in request.assertions {
        assertions.push(prepare_compare_assertion(
            assertion,
            &mut masks,
            &mut dependencies,
            &mut mask_paths,
        )?);
    }
    let mut input_paths = vec![
        PathBuf::from(&request.before.path),
        PathBuf::from(&request.after.path),
    ];
    input_paths.extend(mask_paths);
    let refs = input_paths.iter().map(PathBuf::as_path).collect::<Vec<_>>();
    validate_agent_output_dir(&request_path, &output_dir, &refs)?;
    dependencies.sort();
    dependencies.dedup();
    let outcome = compare_images(&before, &after, &masks, &assertions)?;

    let mut preview_payloads = Vec::<(&'static str, &'static str, Vec<u8>)>::new();
    if request.preview.difference {
        let preview = bounded_preview(
            difference_preview(&before, &after)?,
            request.preview.maximum_edge,
        )?;
        preview_payloads.push((
            "difference",
            "difference.png",
            PngEncoder::encode_rgba(&preview)?,
        ));
    }
    if request.preview.mask_overlay {
        let Some(mask) = masks.first() else {
            return Err(PpError::InvalidRequest(
                "maskOverlay preview requires at least one mask assertion".to_owned(),
            ));
        };
        let preview = bounded_preview(
            mask_overlay_preview(&after, mask)?,
            request.preview.maximum_edge,
        )?;
        preview_payloads.push((
            "mask_overlay",
            "mask-overlay.png",
            PngEncoder::encode_rgba(&preview)?,
        ));
    }

    let mut published_previews = Vec::with_capacity(preview_payloads.len());
    let mut entries = Vec::with_capacity(preview_payloads.len());
    for (role, relative_path, bytes) in &preview_payloads {
        let descriptor = agent_output_descriptor(
            bytes,
            AgentArtifactKind::WorkingRaster,
            dependencies.clone(),
        )?;
        published_previews.push(AgentComparePreviewResult {
            role,
            relative_path: (*relative_path).to_owned(),
            descriptor,
        });
        entries.push(AtomicDirectoryEntry {
            relative_path: Path::new(*relative_path),
            bytes,
            sha256: crate::sha256(bytes),
        });
    }
    // Even a diagnostic-free compare owns an output-directory contract: an
    // existing non-empty directory must not be silently accepted as if this
    // operation had published it.  The new-or-empty check is performed by
    // the locked publisher, closing the preflight TOCTOU window for both
    // preview and no-preview requests.
    AtomicDirectoryWriter::replace_new_or_empty(&output_dir, &entries)?;
    let status = if outcome.all_required_passed {
        AgentOperationStatus::Committed
    } else {
        AgentOperationStatus::Rejected
    };
    let receipt = AgentOperationReceipt {
        schema: AGENT_RECEIPT_SCHEMA.to_owned(),
        request_id: request.request_id.clone(),
        operation: "compare".to_owned(),
        status,
        behavior_version: AGENT_BEHAVIOR_VERSION.to_owned(),
        implementation_version: env!("CARGO_PKG_VERSION").to_owned(),
        request_sha256: request_snapshot.sha256().to_owned(),
        dependency_closure_sha256: dependency_closure_sha256(&dependencies)?,
        dependencies,
        output_sha256: None,
        determinism: AgentDeterminismClass::BitExact,
    };
    serialize_json(
        &AgentCompareResult {
            schema: AGENT_COMPARE_RESULT_SCHEMA,
            request_id: request.request_id,
            status: if outcome.all_required_passed {
                "committed"
            } else {
                "rejected"
            },
            protocol_version: AGENT_PROTOCOL_VERSION,
            behavior_version: AGENT_BEHAVIOR_VERSION,
            all_required_passed: outcome.all_required_passed,
            metrics: outcome.metrics,
            assertions: outcome.assertions,
            previews: published_previews,
            receipt,
        },
        "<agent-compare>",
    )
}

fn validate_agent_request_id(
    schema: &str,
    expected_schema: &str,
    request_id: &str,
) -> PpResult<()> {
    if schema != expected_schema || request_id.trim().is_empty() || request_id.len() > 192 {
        return Err(PpError::InvalidRequest(
            "agent operation envelope is invalid".to_owned(),
        ));
    }
    Ok(())
}

fn validate_agent_output_dir(
    request_path: &Path,
    output_dir: &Path,
    inputs: &[&Path],
) -> PpResult<()> {
    if !request_path.is_absolute()
        || request_path
            .components()
            .any(|component| component == Component::ParentDir)
        || !output_dir.is_absolute()
        || output_dir
            .components()
            .any(|component| component == Component::ParentDir)
    {
        return Err(PpError::InvalidRequest(
            "agent request/output paths must be absolute and traversal-free".to_owned(),
        ));
    }
    if request_path.starts_with(output_dir)
        || inputs.iter().any(|path| path.starts_with(output_dir))
    {
        return Err(PpError::InvalidRequest(
            "agent output directory must not contain the request or any input artifact".to_owned(),
        ));
    }
    Ok(())
}

fn read_agent_raster(
    input: &AgentInputArtifact,
    expected_kind: AgentArtifactKind,
) -> PpResult<(Raster, AgentArtifactDependency)> {
    if input.kind != expected_kind {
        return Err(PpError::InvalidRequest(
            "agent input artifact kind is invalid".to_owned(),
        ));
    }
    read_agent_raster_any(input)
}

fn read_agent_raster_any(
    input: &AgentInputArtifact,
) -> PpResult<(Raster, AgentArtifactDependency)> {
    crate::validate_canonical_sha256(&input.expected_sha256)?;
    if input.media_type != "image/png" || input.byte_length == 0 {
        return Err(PpError::InvalidRequest(
            "agent raster input must be a non-empty image/png".to_owned(),
        ));
    }
    if let Some(pixel_spec) = input.pixel_spec {
        if pixel_spec != AgentPixelSpec::rgba8_srgb_straight() {
            return Err(PpError::InvalidRequest(
                "agent raster pixelSpec must be rgba8/srgb/straight".to_owned(),
            ));
        }
    }
    let path = PathBuf::from(&input.path);
    if !path.is_absolute()
        || path
            .components()
            .any(|component| component == Component::ParentDir)
    {
        return Err(PpError::InvalidRequest(
            "agent raster source path must be absolute and traversal-free".to_owned(),
        ));
    }
    validate_raster_input_path(&path)?;
    let (bytes, snapshot) = InputSnapshot::capture(&path, MAX_RASTER_READ_BYTES)?;
    if u64::try_from(bytes.len()).ok() != Some(input.byte_length)
        || snapshot.sha256() != input.expected_sha256
    {
        return Err(PpError::InvalidRequest(
            "agent raster input changed or does not match its content address".to_owned(),
        ));
    }
    let raster = ImageCodec::decode_rgba_bytes(&path, &bytes, DecodeLimits::default())?;
    let dependency = AgentArtifactDependency {
        sha256: input.expected_sha256.clone(),
        media_type: input.media_type.clone(),
        byte_length: input.byte_length,
    };
    Ok((raster, dependency))
}

fn read_agent_font(
    input: &AgentFontInput,
) -> PpResult<(Vec<u8>, AgentArtifactDependency, PathBuf)> {
    crate::validate_canonical_sha256(&input.artifact.sha256)?;
    if input.artifact.id.trim().is_empty()
        || input.artifact.id.len() > 160
        || input.artifact.byte_length == 0
        || input.artifact.byte_length > MAX_FONT_BYTES as u64
        || input.artifact.relative_path.is_empty()
        || input.artifact.relative_path.len() > 512
        || !matches!(input.artifact.media_type.as_str(), "font/ttf" | "font/otf")
    {
        return Err(PpError::InvalidRequest(
            "text font artifact metadata is invalid".to_owned(),
        ));
    }
    let relative = Path::new(&input.artifact.relative_path);
    if relative.is_absolute()
        || relative
            .components()
            .any(|component| component == Component::ParentDir)
    {
        return Err(PpError::InvalidRequest(
            "text font relativePath must be relative and traversal-free".to_owned(),
        ));
    }
    let path = PathBuf::from(&input.path);
    if !path.is_absolute()
        || path
            .components()
            .any(|component| component == Component::ParentDir)
    {
        return Err(PpError::InvalidRequest(
            "text font path must be absolute and traversal-free".to_owned(),
        ));
    }
    let extension = path
        .extension()
        .and_then(std::ffi::OsStr::to_str)
        .unwrap_or_default()
        .to_ascii_lowercase();
    let expected_extension = if input.artifact.media_type == "font/ttf" {
        "ttf"
    } else {
        "otf"
    };
    if extension != expected_extension {
        return Err(PpError::InvalidRequest(
            "text font path extension and mediaType disagree".to_owned(),
        ));
    }
    let (bytes, snapshot) = InputSnapshot::capture(&path, MAX_FONT_BYTES)?;
    if u64::try_from(bytes.len()).ok() != Some(input.artifact.byte_length)
        || snapshot.sha256() != input.artifact.sha256
    {
        return Err(PpError::InvalidRequest(
            "text font changed or does not match its content address".to_owned(),
        ));
    }
    let dependency = AgentArtifactDependency {
        sha256: input.artifact.sha256.clone(),
        media_type: input.artifact.media_type.clone(),
        byte_length: input.artifact.byte_length,
    };
    Ok((bytes, dependency, path))
}

fn agent_output_descriptor(
    bytes: &[u8],
    kind: AgentArtifactKind,
    dependencies: Vec<AgentArtifactDependency>,
) -> PpResult<AgentArtifactDescriptor> {
    AgentArtifactDescriptor::new(
        crate::sha256_hex(bytes),
        "image/png".to_owned(),
        u64::try_from(bytes.len())
            .map_err(|_| PpError::InvalidRequest("artifact byte length overflowed".to_owned()))?,
        kind,
        Some(AgentPixelSpec::rgba8_srgb_straight()),
        AgentArtifactRetention::TaskScoped,
        dependencies,
    )
}
