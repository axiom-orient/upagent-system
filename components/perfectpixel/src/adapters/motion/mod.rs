mod assessment;
pub(crate) use assessment::assess_authorized_motion_structure;
pub use assessment::{
    assess_motion_structure, MotionAssessment, MotionAssessmentBinding, MotionAssessmentEvidence,
    MotionAssessmentNotEvaluatedReason, MotionAssessmentRequest, MotionAssessmentStatus,
    MOTION_ASSESSMENT_CONTRACT_VERSION, MOTION_ASSESSMENT_MAX_GEOMETRY_BYTES,
    MOTION_ASSESSMENT_MAX_PATHS, MOTION_ASSESSMENT_MAX_SOURCE_BYTES,
};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use svgtypes::PathSegment;

use crate::{PpError, PpResult, SvgContract};

pub const MOTION_SCHEMA: &str = "perfectpixel.motion/1";
/// Prefix a dotLottie `generator`/`meta.g` field must start with to be recognized as this
/// product's own output, regardless of which version wrote it.
pub const GENERATOR_PREFIX: &str = concat!(env!("CARGO_PKG_NAME"), "@");
const MOTION_LAYERS_SCHEMA: &str = "perfectpixel.motion-layers/1";
/// Schema identity written into every `motion-report.json`.
pub const MOTION_REPORT_SCHEMA: &str = "perfectpixel.motion-report/1";
const MOTION_MAX_SOURCE_BYTES: usize = 1_048_576;
const MOTION_MAX_OUTPUT_BYTES: usize = 4_194_304;
const MOTION_MAX_REQUEST_ITEMS: usize = 4_096;
const MOTION_MAX_TOTAL_KEYFRAMES: usize = 16_384;
const MOTION_MAX_TOTAL_PATH_REFERENCES: usize = 16_384;
const MOTION_MAX_AUTHORED_PATH_BYTES: usize = 524_288;
const MOTION_MAX_REQUEST_BYTES: usize = 1_048_576;
/// Shared cap for scaffold relationship ranking and structural assessment.
pub const MOTION_MAX_SCAFFOLD_PATHS: usize = 512;
const MOTION_MAX_RELATED_PATH_ID_BYTES: usize = 524_288;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MotionRequest {
    pub schema: String,
    pub name: String,
    pub source_svg: String,
    pub source_svg_sha256: String,
    pub fps: u32,
    pub duration_ms: u32,
    #[serde(rename = "loop")]
    pub looped: bool,
    #[serde(default)]
    pub authored_paths: Vec<MotionAuthoredPath>,
    pub parts: Vec<MotionPart>,
    pub tracks: Vec<MotionTrack>,
    #[serde(default)]
    pub markers: Vec<MotionMarker>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MotionAuthoredPath {
    pub id: String,
    pub d: String,
    pub fill: String,
    #[serde(default = "default_opacity")]
    pub fill_opacity: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MotionPart {
    pub id: String,
    pub path_ids: Vec<String>,
    pub anchor: [f64; 2],
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MotionTrack {
    pub part: String,
    #[serde(default)]
    pub interpolation: MotionInterpolation,
    pub keyframes: Vec<MotionKeyframe>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MotionInterpolation {
    #[default]
    Linear,
    Hold,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MotionKeyframe {
    pub at_ms: u32,
    pub translate: [f64; 2],
    pub rotate_deg: f64,
    pub scale: [f64; 2],
    pub opacity: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MotionMarker {
    pub name: String,
    pub from_ms: u32,
    pub to_ms: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MotionLayer {
    pub id: String,
    pub order: usize,
    pub fill: String,
    pub fill_opacity: f64,
    pub bounds: [f64; 4],
    pub shape_count: usize,
    pub related_path_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MotionLayersDocument {
    pub schema: String,
    pub width: u32,
    pub height: u32,
    pub layers: Vec<MotionLayer>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MotionScaffoldOutput {
    pub scene_svg: String,
    pub layers: MotionLayersDocument,
    pub inspector_html: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MotionReport {
    pub schema: String,
    pub name: String,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub duration_ms: u32,
    pub total_frames: f64,
    pub path_count: usize,
    pub authored_path_count: usize,
    pub part_count: usize,
    pub track_count: usize,
    pub marker_count: usize,
    pub assigned_path_count: usize,
    pub unassigned_path_count: usize,
    pub lottie_shape_count: usize,
    pub animated_svg: bool,
    pub lottie_json: bool,
    pub dotlottie_v2_layout: bool,
    pub dotlottie_archive_created: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MotionBuildOutput {
    pub animated_svg: String,
    pub lottie_json: String,
    pub dotlottie_manifest_json: String,
    pub report: MotionReport,
    pub preview_html: String,
}

#[derive(Debug, Clone)]
struct SvgScene {
    svg: String,
    root_opening_tag: std::ops::Range<usize>,
    root_closing_tag: std::ops::Range<usize>,
    element_prefix: String,
    width: u32,
    height: u32,
    paths: Vec<SvgPath>,
    transform_attributes: Vec<std::ops::Range<usize>>,
}

#[derive(Debug, Clone)]
struct SvgPath {
    id: String,
    has_id: bool,
    segments: Vec<PathSegment>,
    opening_tag: std::ops::Range<usize>,
    element_span: std::ops::Range<usize>,
    fill: String,
    fill_opacity: f64,
    translate: [f64; 2],
    path_data: std::ops::Range<usize>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TransformPolicy {
    ScaffoldInput,
    BuildInput,
}

#[derive(Debug, Clone, Serialize)]
struct LottieBezier {
    c: bool,
    i: Vec<[f64; 2]>,
    o: Vec<[f64; 2]>,
    v: Vec<[f64; 2]>,
}

#[derive(Debug)]
struct BezierBuilder {
    closed: bool,
    incoming: Vec<[f64; 2]>,
    outgoing: Vec<[f64; 2]>,
    vertices: Vec<[f64; 2]>,
}

impl BezierBuilder {
    fn new(point: [f64; 2]) -> Self {
        Self {
            closed: false,
            incoming: vec![[0.0, 0.0]],
            outgoing: vec![[0.0, 0.0]],
            vertices: vec![point],
        }
    }

    fn current(&self) -> [f64; 2] {
        *self.vertices.last().unwrap_or(&[0.0, 0.0])
    }

    fn line_to(&mut self, point: [f64; 2]) {
        self.vertices.push(point);
        self.incoming.push([0.0, 0.0]);
        self.outgoing.push([0.0, 0.0]);
    }

    fn curve_to(&mut self, control_1: [f64; 2], control_2: [f64; 2], point: [f64; 2]) {
        let current = self.current();
        if let Some(outgoing) = self.outgoing.last_mut() {
            *outgoing = sub(control_1, current);
        }
        self.vertices.push(point);
        self.incoming.push(sub(control_2, point));
        self.outgoing.push([0.0, 0.0]);
    }

    fn close(&mut self) {
        self.closed = true;
        if self.vertices.len() > 1 {
            let first = self.vertices[0];
            let last = *self.vertices.last().unwrap_or(&first);
            if nearly_equal(first, last) {
                if let Some(last_in) = self.incoming.last().copied() {
                    self.incoming[0] = last_in;
                }
                self.vertices.pop();
                self.incoming.pop();
                self.outgoing.pop();
            }
        }
    }

    fn finish(self) -> Option<LottieBezier> {
        if self.vertices.len() < 2 {
            return None;
        }
        Some(LottieBezier {
            c: self.closed,
            i: self.incoming,
            o: self.outgoing,
            v: self.vertices,
        })
    }
}

pub struct MotionCompiler;

impl MotionCompiler {
    pub fn scene_sha256(svg: &str) -> String {
        assessment::sha256_hex(svg.as_bytes())
    }

    pub fn scaffold(svg: &str) -> PpResult<MotionScaffoldOutput> {
        ensure_motion_source_bound(svg)?;
        // Bound path count before missing-ID annotation or all-pairs relationship work.
        let source_scene = parse_scene(svg, true, TransformPolicy::ScaffoldInput)?;
        let flattened = flatten_scene_translations(&source_scene)?;
        let scene = parse_scene(&flattened, false, TransformPolicy::BuildInput)?;
        let mut motion_layers = scene
            .paths
            .iter()
            .enumerate()
            .map(|(order, path)| motion_layer(path, order))
            .collect::<PpResult<Vec<_>>>()?;
        populate_related_paths(&mut motion_layers, scene.width, scene.height)?;
        let layers = MotionLayersDocument {
            schema: MOTION_LAYERS_SCHEMA.to_string(),
            width: scene.width,
            height: scene.height,
            layers: motion_layers,
        };
        let inspector_html = inspector_html(&scene, &layers)?;
        Ok(MotionScaffoldOutput {
            scene_svg: scene.svg,
            layers,
            inspector_html,
        })
    }

    pub fn build(svg: &str, request: &MotionRequest) -> PpResult<MotionBuildOutput> {
        ensure_motion_source_bound(svg)?;
        ensure_motion_request_bounds(request)?;
        let source_scene = parse_scene(svg, false, TransformPolicy::BuildInput)?;
        if assessment::sha256_hex(source_scene.svg.as_bytes()) != request.source_svg_sha256 {
            return Err(PpError::InvalidRequest(
                "motion request sourceSvgSha256 does not match the exact reparsed scene bytes"
                    .to_string(),
            ));
        }
        let scene = if request.authored_paths.is_empty() {
            source_scene
        } else {
            let source = inject_authored_paths(&source_scene, &request.authored_paths)?;
            parse_scene(&source, false, TransformPolicy::BuildInput)?
        };
        let validated = validate_request(&scene, request)?;
        let animated_svg = build_animated_svg(&scene, request, &validated.tracks_by_part)?;
        let (lottie, lottie_shape_count) = build_lottie(&scene, request, &validated.track_by_path)?;
        let lottie_json =
            serde_json::to_string_pretty(&lottie).map_err(|source| PpError::Json {
                path: "<motion-lottie>".into(),
                message: source.to_string(),
            })?;
        ensure_motion_output_bound("Lottie JSON", lottie_json.len())?;
        let manifest = json!({
            "version": "2",
            "generator": format!("{GENERATOR_PREFIX}{}", env!("CARGO_PKG_VERSION")),
            "initial": { "animation": request.name },
            "animations": [{ "id": request.name }]
        });
        let dotlottie_manifest_json =
            serde_json::to_string_pretty(&manifest).map_err(|source| PpError::Json {
                path: "<dotlottie-manifest>".into(),
                message: source.to_string(),
            })?;
        ensure_motion_output_bound("dotLottie manifest", dotlottie_manifest_json.len())?;
        let report = MotionReport {
            schema: MOTION_REPORT_SCHEMA.to_string(),
            name: request.name.clone(),
            width: scene.width,
            height: scene.height,
            fps: request.fps,
            duration_ms: request.duration_ms,
            total_frames: frames(request.duration_ms, request.fps),
            path_count: scene.paths.len(),
            authored_path_count: request.authored_paths.len(),
            part_count: request.parts.len(),
            track_count: request.tracks.len(),
            marker_count: request.markers.len(),
            assigned_path_count: validated.track_by_path.len(),
            unassigned_path_count: scene.paths.len() - validated.track_by_path.len(),
            lottie_shape_count,
            animated_svg: true,
            lottie_json: true,
            dotlottie_v2_layout: true,
            dotlottie_archive_created: false,
        };
        Ok(MotionBuildOutput {
            animated_svg,
            lottie_json,
            dotlottie_manifest_json,
            preview_html: preview_html(request),
            report,
        })
    }
}

fn default_opacity() -> f64 {
    1.0
}

fn inject_authored_paths(
    scene: &SvgScene,
    authored_paths: &[MotionAuthoredPath],
) -> PpResult<String> {
    let insert_at = scene.root_closing_tag.start;
    if scene.root_closing_tag.is_empty()
        || scene.root_closing_tag.end > scene.svg.len()
        || !scene.svg[scene.root_closing_tag.clone()].starts_with("</")
    {
        return Err(PpError::InvalidRequest(
            "motion SVG root closing-tag span is invalid".to_string(),
        ));
    }
    let mut authored = String::new();
    let mut ids = BTreeSet::new();
    for path in authored_paths {
        validate_id(&path.id, "authored path id")?;
        if !ids.insert(path.id.as_str()) {
            return Err(PpError::InvalidRequest(format!(
                "duplicate authored path id '{}'",
                path.id
            )));
        }
        if path.d.trim().is_empty()
            || path.d != path.d.trim()
            || path.d.contains(['<', '>', '"', '\'', '&', '\0'])
        {
            return Err(PpError::InvalidRequest(format!(
                "authored path '{}' contains unsafe or empty path data",
                path.id
            )));
        }
        parse_hex_color(&path.fill)?;
        validate_finite(path.fill_opacity, "authored path fillOpacity")?;
        if !(0.0..=1.0).contains(&path.fill_opacity) {
            return Err(PpError::InvalidRequest(format!(
                "authored path '{}' fillOpacity must be from 0 through 1",
                path.id
            )));
        }
        writeln!(
            authored,
            "<{}path id=\"{}\" d=\"{}\" fill=\"{}\" fill-opacity=\"{}\"/>",
            scene.element_prefix,
            path.id,
            path.d,
            path.fill,
            format_number(path.fill_opacity)
        )
        .map_err(|_| PpError::InvalidRequest("cannot build authored SVG paths".to_string()))?;
    }
    let mut output = String::with_capacity(scene.svg.len() + authored.len());
    output.push_str(&scene.svg[..insert_at]);
    output.push_str(&authored);
    output.push_str(&scene.svg[insert_at..]);
    Ok(output)
}

fn motion_layer(path: &SvgPath, order: usize) -> PpResult<MotionLayer> {
    let beziers = parse_lottie_beziers(path)?;
    let bounds = bezier_bounds(&beziers).ok_or_else(|| {
        PpError::InvalidRequest(format!("path '{}' has no measurable geometry", path.id))
    })?;
    Ok(MotionLayer {
        id: path.id.clone(),
        order,
        fill: path.fill.clone(),
        fill_opacity: path.fill_opacity,
        bounds,
        shape_count: beziers.len(),
        related_path_ids: Vec::new(),
    })
}

fn populate_related_paths(layers: &mut [MotionLayer], width: u32, height: u32) -> PpResult<()> {
    let ids = layers
        .iter()
        .map(|layer| layer.id.clone())
        .collect::<Vec<_>>();
    let related = layers
        .iter()
        .enumerate()
        .map(|(index, layer)| {
            let mut candidates = layers
                .iter()
                .enumerate()
                .filter_map(|(other_index, other)| {
                    if index == other_index
                        || is_full_canvas_layer(layer, width, height)
                        || is_full_canvas_layer(other, width, height)
                    {
                        return None;
                    }
                    related_layer_score(layer, other).map(|score| (other_index, score))
                })
                .collect::<Vec<_>>();
            candidates.sort_by(|(left_index, left_score), (right_index, right_score)| {
                right_score
                    .total_cmp(left_score)
                    .then_with(|| ids[*left_index].cmp(&ids[*right_index]))
            });
            candidates
                .into_iter()
                .take(16)
                .map(|(other_index, _)| other_index)
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    let related_bytes = related
        .iter()
        .flatten()
        .try_fold(0usize, |total, index| total.checked_add(ids[*index].len()))
        .ok_or_else(|| {
            PpError::InvalidRequest("motion related-path ID bytes overflow".to_string())
        })?;
    if related_bytes > MOTION_MAX_RELATED_PATH_ID_BYTES {
        return Err(PpError::InvalidRequest(format!(
            "motion related-path IDs exceed {MOTION_MAX_RELATED_PATH_ID_BYTES} byte limit"
        )));
    }
    for (layer, related_indices) in layers.iter_mut().zip(related) {
        layer.related_path_ids = related_indices
            .into_iter()
            .map(|index| ids[index].clone())
            .collect();
    }
    Ok(())
}

fn is_full_canvas_layer(layer: &MotionLayer, width: u32, height: u32) -> bool {
    let epsilon = f64::from(width.max(height)) * 0.001 + 0.01;
    layer.bounds[0].abs() <= epsilon
        && layer.bounds[1].abs() <= epsilon
        && (layer.bounds[2] - f64::from(width)).abs() <= epsilon
        && (layer.bounds[3] - f64::from(height)).abs() <= epsilon
}

fn related_layer_score(left: &MotionLayer, right: &MotionLayer) -> Option<f64> {
    let left_area = left.bounds[2] * left.bounds[3];
    let right_area = right.bounds[2] * right.bounds[3];
    if left_area <= 0.0 || right_area <= 0.0 {
        return None;
    }
    let area_ratio = left_area.min(right_area) / left_area.max(right_area);
    if area_ratio < 0.2 {
        return None;
    }
    let intersection_width = (left.bounds[0] + left.bounds[2])
        .min(right.bounds[0] + right.bounds[2])
        - left.bounds[0].max(right.bounds[0]);
    let intersection_height = (left.bounds[1] + left.bounds[3])
        .min(right.bounds[1] + right.bounds[3])
        - left.bounds[1].max(right.bounds[1]);
    if intersection_width <= 0.0 || intersection_height <= 0.0 {
        return None;
    }
    let coverage = intersection_width * intersection_height / left_area.min(right_area);
    if coverage < 0.6 {
        return None;
    }
    let left_color = parse_hex_color(&left.fill).ok()?;
    let right_color = parse_hex_color(&right.fill).ok()?;
    let color_distance = ((left_color[0] - right_color[0]).powi(2)
        + (left_color[1] - right_color[1]).powi(2)
        + (left_color[2] - right_color[2]).powi(2))
    .sqrt();
    (color_distance <= 0.55).then_some(coverage + area_ratio - color_distance * 0.25)
}

fn bezier_bounds(beziers: &[LottieBezier]) -> Option<[f64; 4]> {
    let mut min = [f64::INFINITY, f64::INFINITY];
    let mut max = [f64::NEG_INFINITY, f64::NEG_INFINITY];
    for bezier in beziers {
        for (index, vertex) in bezier.v.iter().enumerate() {
            for point in [
                *vertex,
                add(*vertex, bezier.i.get(index).copied().unwrap_or([0.0, 0.0])),
                add(*vertex, bezier.o.get(index).copied().unwrap_or([0.0, 0.0])),
            ] {
                min[0] = min[0].min(point[0]);
                min[1] = min[1].min(point[1]);
                max[0] = max[0].max(point[0]);
                max[1] = max[1].max(point[1]);
            }
        }
    }
    min[0].is_finite().then_some([
        min[0],
        min[1],
        (max[0] - min[0]).max(0.0),
        (max[1] - min[1]).max(0.0),
    ])
}

struct ValidatedMotion<'a> {
    tracks_by_part: BTreeMap<&'a str, &'a MotionTrack>,
    track_by_path: BTreeMap<&'a str, (&'a MotionPart, Option<&'a MotionTrack>)>,
}

#[cfg(test)]
fn default_keyframe(at_ms: u32, translate: [f64; 2]) -> MotionKeyframe {
    MotionKeyframe {
        at_ms,
        translate,
        rotate_deg: 0.0,
        scale: [1.0, 1.0],
        opacity: 1.0,
    }
}

fn parse_scene(
    svg: &str,
    assign_missing_ids: bool,
    transform_policy: TransformPolicy,
) -> PpResult<SvgScene> {
    let (report, ir) = SvgContract::parse(svg)?;
    parse_scene_from_ir(svg, report, ir, assign_missing_ids, transform_policy)
}

fn parse_scene_from_ir(
    svg: &str,
    report: crate::core::SvgReport,
    ir: crate::core::SvgIr,
    assign_missing_ids: bool,
    transform_policy: TransformPolicy,
) -> PpResult<SvgScene> {
    if ir.source != svg {
        return Err(PpError::InvalidRequest(
            "motion SVG source does not match the parser-produced IR".to_string(),
        ));
    }
    if ir.root.width != Some(report.width) || ir.root.height != Some(report.height) {
        return Err(PpError::InvalidRequest(
            "motion SVG report does not match the parser-produced IR".to_string(),
        ));
    }
    if report.width == 0 || report.height == 0 {
        return Err(PpError::InvalidRequest(
            "motion SVG must declare positive integer width and height".to_string(),
        ));
    }
    if assign_missing_ids && aggregate_path_data_bytes(&ir) > MOTION_ASSESSMENT_MAX_GEOMETRY_BYTES {
        return Err(PpError::InvalidRequest(format!(
            "motion scaffold path data exceed {MOTION_ASSESSMENT_MAX_GEOMETRY_BYTES} byte limit"
        )));
    }

    let mut transform_attributes = Vec::new();
    for element in &ir.elements {
        let Some(value) = element.attributes.get("transform") else {
            continue;
        };
        let element_name = element.attributes.get("id").map_or_else(
            || element.local_name.clone(),
            |id| format!("{} '{id}'", element.local_name),
        );
        match transform_policy {
            TransformPolicy::BuildInput => {
                return Err(PpError::InvalidRequest(format!(
                    "motion-build source contains transform on {element_name}; rerun motion-scaffold"
                )));
            }
            TransformPolicy::ScaffoldInput if !element.transform.translation_only => {
                return Err(PpError::InvalidRequest(format!(
                    "motion scaffold {element_name} uses unsupported transform '{value}'; motion v1 supports only translate(x,y)"
                )));
            }
            TransformPolicy::ScaffoldInput => {}
        }
        let range = element.attribute_ranges.get("transform").ok_or_else(|| {
            PpError::InvalidRequest(format!(
                "motion SVG {element_name} transform has no parser-owned byte range"
            ))
        })?;
        transform_attributes.push(range.attribute.clone());
    }

    let mut paths = Vec::new();
    let root_opening_tag = ir
        .elements
        .first()
        .filter(|element| element.local_name == "svg")
        .map(|element| element.opening_tag.clone())
        .ok_or_else(|| {
            PpError::InvalidRequest("motion SVG has no parsed root element".to_string())
        })?;
    let element_prefix = svg_element_prefix(svg, &root_opening_tag)?;
    let mut reserved_ids = ir.ids.clone();
    let mut next_generated_id = 1usize;
    for element in &ir.elements {
        if element.local_name != "path" {
            continue;
        }
        let id = match element.attributes.get("id") {
            Some(id) => id.clone(),
            None if assign_missing_ids => loop {
                let generated = format!("pp-path-{next_generated_id:04}");
                next_generated_id += 1;
                if reserved_ids.insert(generated.clone()) {
                    break generated;
                }
            },
            None => {
                return Err(PpError::InvalidRequest(
                    "motion-build source paths need IDs; run motion-scaffold first".to_string(),
                ))
            }
        };
        validate_id(&id, "path id")?;
        if !element.attributes.contains_key("d") {
            return Err(PpError::InvalidRequest(format!(
                "path '{id}' has no d attribute"
            )));
        }
        let path = element.path.as_ref().ok_or_else(|| {
            PpError::InvalidRequest(format!("path '{id}' has no parsed geometry"))
        })?;
        let paint = element.paint.as_ref().ok_or_else(|| {
            PpError::InvalidRequest(format!("path '{id}' has no rendered paint facts"))
        })?;
        let fill = paint.fill.clone().ok_or_else(|| {
            PpError::InvalidRequest(format!("path '{id}' has no resolved rendered fill"))
        })?;
        let matrix = element.transform.matrix;
        let path_data = element
            .attribute_ranges
            .get("d")
            .ok_or_else(|| PpError::InvalidRequest(format!("path '{id}' has no d byte range")))?
            .value
            .clone();
        paths.push(SvgPath {
            id,
            has_id: element.attributes.contains_key("id"),
            segments: path.segments.clone(),
            opening_tag: element.opening_tag.clone(),
            element_span: element.range.clone(),
            fill,
            fill_opacity: paint.opacity,
            translate: [matrix[4], matrix[5]],
            path_data,
        });
    }
    if paths.is_empty() {
        return Err(PpError::InvalidRequest(
            "motion SVG contains no path geometry".to_string(),
        ));
    }
    if assign_missing_ids && paths.len() > MOTION_MAX_SCAFFOLD_PATHS {
        return Err(PpError::InvalidRequest(format!(
            "motion scaffold supports at most {MOTION_MAX_SCAFFOLD_PATHS} paths"
        )));
    }
    if assign_missing_ids && paths.iter().any(|path| !path.has_id) {
        return parse_scene(&annotate_path_ids(svg, &paths)?, false, transform_policy);
    }
    Ok(SvgScene {
        svg: svg.to_string(),
        root_opening_tag,
        root_closing_tag: ir.root.closing_tag,
        element_prefix,
        width: report.width,
        height: report.height,
        paths,
        transform_attributes,
    })
}

fn aggregate_path_data_bytes(ir: &crate::core::SvgIr) -> usize {
    ir.elements
        .iter()
        .filter_map(|element| element.attributes.get("d"))
        .try_fold(0usize, |total, data| total.checked_add(data.len()))
        .unwrap_or(usize::MAX)
}

fn annotate_path_ids(svg: &str, paths: &[SvgPath]) -> PpResult<String> {
    let insertions = paths
        .iter()
        .filter(|path| !path.has_id)
        .map(|path| {
            if path.opening_tag.is_empty() || path.opening_tag.end > svg.len() {
                return Err(PpError::InvalidRequest(
                    "motion SVG path IR does not contain a valid opening-tag span".to_string(),
                ));
            }
            let insert_at = path.opening_tag.end
                - usize::from(
                    svg.as_bytes().get(path.opening_tag.end.saturating_sub(2)) == Some(&b'/'),
                )
                - 1;
            Ok((insert_at, path.id.as_str()))
        })
        .collect::<PpResult<Vec<_>>>()?;
    let added_bytes = insertions.iter().map(|(_, id)| id.len() + 6).sum::<usize>();
    let mut output = String::with_capacity(svg.len() + added_bytes);
    let mut cursor = 0usize;
    for (insert_at, id) in insertions {
        if insert_at < cursor || insert_at > svg.len() {
            return Err(PpError::InvalidRequest(
                "motion SVG path IR does not contain ordered opening-tag spans".to_string(),
            ));
        }
        output.push_str(&svg[cursor..insert_at]);
        write!(output, " id=\"{id}\"")
            .map_err(|_| PpError::InvalidRequest("cannot annotate motion path IDs".to_string()))?;
        cursor = insert_at;
    }
    output.push_str(&svg[cursor..]);
    Ok(output)
}

fn flatten_scene_translations(scene: &SvgScene) -> PpResult<String> {
    let mut edits = scene
        .paths
        .iter()
        .map(|path| {
            serialize_translated_path(path).map(|data| (path.path_data.clone(), Some(data)))
        })
        .collect::<PpResult<Vec<_>>>()?;
    edits.extend(
        scene
            .transform_attributes
            .iter()
            .cloned()
            .map(|range| (range, None)),
    );
    edits.sort_by_key(|(range, _)| range.start);

    let mut output = String::with_capacity(scene.svg.len());
    let mut cursor = 0usize;
    for (range, replacement) in edits {
        if range.start < cursor || range.end > scene.svg.len() {
            return Err(PpError::InvalidRequest(
                "motion SVG attribute byte ranges overlap or exceed the source".to_string(),
            ));
        }
        output.push_str(&scene.svg[cursor..range.start]);
        if let Some(replacement) = replacement {
            output.push_str(&replacement);
        }
        cursor = range.end;
    }
    output.push_str(&scene.svg[cursor..]);
    Ok(output)
}

fn serialize_translated_path(path: &SvgPath) -> PpResult<String> {
    let mut output = String::new();
    let mut current = [0.0, 0.0];
    let mut subpath_start = [0.0, 0.0];
    for segment in &path.segments {
        if !output.is_empty() {
            output.push(' ');
        }
        match *segment {
            PathSegment::MoveTo { abs, x, y } => {
                current = absolute_point(abs, current, x, y);
                subpath_start = current;
                write_path_point(&mut output, 'M', add(current, path.translate))?;
            }
            PathSegment::LineTo { abs, x, y } => {
                current = absolute_point(abs, current, x, y);
                write_path_point(&mut output, 'L', add(current, path.translate))?;
            }
            PathSegment::HorizontalLineTo { abs, x } => {
                current[0] = if abs { x } else { current[0] + x };
                write_path_point(&mut output, 'L', add(current, path.translate))?;
            }
            PathSegment::VerticalLineTo { abs, y } => {
                current[1] = if abs { y } else { current[1] + y };
                write_path_point(&mut output, 'L', add(current, path.translate))?;
            }
            PathSegment::CurveTo {
                abs,
                x1,
                y1,
                x2,
                y2,
                x,
                y,
            } => {
                let control_1 = add(absolute_point(abs, current, x1, y1), path.translate);
                let control_2 = add(absolute_point(abs, current, x2, y2), path.translate);
                current = absolute_point(abs, current, x, y);
                write!(
                    output,
                    "C{} {} {} {} {} {}",
                    format_number(control_1[0]),
                    format_number(control_1[1]),
                    format_number(control_2[0]),
                    format_number(control_2[1]),
                    format_number(current[0] + path.translate[0]),
                    format_number(current[1] + path.translate[1])
                )
                .map_err(|_| PpError::InvalidRequest("cannot flatten motion path".to_string()))?;
            }
            PathSegment::SmoothCurveTo { abs, x2, y2, x, y } => {
                let control_2 = add(absolute_point(abs, current, x2, y2), path.translate);
                current = absolute_point(abs, current, x, y);
                write!(
                    output,
                    "S{} {} {} {}",
                    format_number(control_2[0]),
                    format_number(control_2[1]),
                    format_number(current[0] + path.translate[0]),
                    format_number(current[1] + path.translate[1])
                )
                .map_err(|_| PpError::InvalidRequest("cannot flatten motion path".to_string()))?;
            }
            PathSegment::Quadratic { abs, x1, y1, x, y } => {
                let control = add(absolute_point(abs, current, x1, y1), path.translate);
                current = absolute_point(abs, current, x, y);
                write!(
                    output,
                    "Q{} {} {} {}",
                    format_number(control[0]),
                    format_number(control[1]),
                    format_number(current[0] + path.translate[0]),
                    format_number(current[1] + path.translate[1])
                )
                .map_err(|_| PpError::InvalidRequest("cannot flatten motion path".to_string()))?;
            }
            PathSegment::SmoothQuadratic { abs, x, y } => {
                current = absolute_point(abs, current, x, y);
                write_path_point(&mut output, 'T', add(current, path.translate))?;
            }
            PathSegment::EllipticalArc { .. } => return Err(unsupported_arc(path)),
            PathSegment::ClosePath { .. } => {
                output.push('Z');
                current = subpath_start;
            }
        }
    }
    Ok(output)
}

fn write_path_point(output: &mut String, command: char, point: [f64; 2]) -> PpResult<()> {
    write!(
        output,
        "{command}{} {}",
        format_number(point[0]),
        format_number(point[1])
    )
    .map_err(|_| PpError::InvalidRequest("cannot flatten motion path".to_string()))
}

fn unsupported_arc(path: &SvgPath) -> PpError {
    PpError::InvalidRequest(format!(
        "path '{}' uses an elliptical arc; motion v1 Lottie export supports line/cubic/quadratic paths",
        path.id
    ))
}

fn svg_element_prefix(svg: &str, opening_tag: &std::ops::Range<usize>) -> PpResult<String> {
    let opening = svg.get(opening_tag.clone()).ok_or_else(|| {
        PpError::InvalidRequest("motion SVG root opening-tag span is invalid".to_string())
    })?;
    let name = opening
        .strip_prefix('<')
        .and_then(|tag| {
            tag.split(|ch: char| ch.is_whitespace() || matches!(ch, '>' | '/'))
                .next()
        })
        .ok_or_else(|| {
            PpError::InvalidRequest("motion SVG root opening tag is invalid".to_string())
        })?;
    match name.split_once(':') {
        Some((prefix, "svg")) if !prefix.is_empty() => Ok(format!("{prefix}:")),
        Some(_) => Err(PpError::InvalidRequest(
            "motion SVG root opening tag is invalid".to_string(),
        )),
        None if name == "svg" => Ok(String::new()),
        None => Err(PpError::InvalidRequest(
            "motion SVG root opening tag is invalid".to_string(),
        )),
    }
}

fn validate_request<'a>(
    scene: &'a SvgScene,
    request: &'a MotionRequest,
) -> PpResult<ValidatedMotion<'a>> {
    if request.schema != MOTION_SCHEMA {
        return Err(PpError::InvalidRequest(format!(
            "motion schema must be '{MOTION_SCHEMA}'"
        )));
    }
    validate_id(&request.name, "motion name")?;
    validate_safe_relative_svg(&request.source_svg)?;
    if !(1..=120).contains(&request.fps) {
        return Err(PpError::InvalidRequest(
            "motion fps must be from 1 through 120".to_string(),
        ));
    }
    if !(1..=600_000).contains(&request.duration_ms) {
        return Err(PpError::InvalidRequest(
            "motion durationMs must be from 1 through 600000".to_string(),
        ));
    }
    if request.parts.is_empty() {
        return Err(PpError::InvalidRequest(
            "motion request needs at least one caller-authored part".to_string(),
        ));
    }
    let available = scene
        .paths
        .iter()
        .map(|path| path.id.as_str())
        .collect::<BTreeSet<_>>();
    let mut parts = BTreeMap::new();
    let mut path_owner = BTreeMap::new();
    for part in &request.parts {
        validate_id(&part.id, "part id")?;
        validate_finite_pair(part.anchor, "part anchor")?;
        if part.path_ids.is_empty() {
            return Err(PpError::InvalidRequest(format!(
                "part '{}' needs at least one path id",
                part.id
            )));
        }
        if parts.insert(part.id.as_str(), part).is_some() {
            return Err(PpError::InvalidRequest(format!(
                "duplicate part id '{}'",
                part.id
            )));
        }
        for path_id in &part.path_ids {
            validate_id(path_id, "part path id")?;
            if !available.contains(path_id.as_str()) {
                return Err(PpError::InvalidRequest(format!(
                    "part '{}' references missing path '{}'",
                    part.id, path_id
                )));
            }
            if let Some(previous) = path_owner.insert(path_id.as_str(), part) {
                return Err(PpError::InvalidRequest(format!(
                    "path '{}' belongs to both '{}' and '{}'",
                    path_id, previous.id, part.id
                )));
            }
        }
    }
    let mut tracks_by_part = BTreeMap::new();
    for track in &request.tracks {
        let Some(_part) = parts.get(track.part.as_str()) else {
            return Err(PpError::InvalidRequest(format!(
                "track references missing part '{}'",
                track.part
            )));
        };
        if tracks_by_part.insert(track.part.as_str(), track).is_some() {
            return Err(PpError::InvalidRequest(format!(
                "part '{}' has more than one track",
                track.part
            )));
        }
        validate_keyframes(track, request.duration_ms)?;
    }
    let mut marker_names = BTreeSet::new();
    for marker in &request.markers {
        validate_id(&marker.name, "marker name")?;
        if !marker_names.insert(marker.name.as_str()) {
            return Err(PpError::InvalidRequest(format!(
                "duplicate marker '{}'",
                marker.name
            )));
        }
        if marker.from_ms >= marker.to_ms || marker.to_ms > request.duration_ms {
            return Err(PpError::InvalidRequest(format!(
                "marker '{}' must have 0 <= fromMs < toMs <= durationMs",
                marker.name
            )));
        }
    }
    let track_by_path = path_owner
        .into_iter()
        .map(|(path_id, part)| {
            (
                path_id,
                (part, tracks_by_part.get(part.id.as_str()).copied()),
            )
        })
        .collect();
    Ok(ValidatedMotion {
        tracks_by_part,
        track_by_path,
    })
}

fn validate_keyframes(track: &MotionTrack, duration_ms: u32) -> PpResult<()> {
    if track.keyframes.len() < 2 {
        return Err(PpError::InvalidRequest(format!(
            "track '{}' needs at least two keyframes",
            track.part
        )));
    }
    if track.keyframes[0].at_ms != 0 {
        return Err(PpError::InvalidRequest(format!(
            "track '{}' first keyframe must start at 0ms",
            track.part
        )));
    }
    let mut previous = None;
    for keyframe in &track.keyframes {
        if keyframe.at_ms > duration_ms
            || previous.is_some_and(|previous| keyframe.at_ms <= previous)
        {
            return Err(PpError::InvalidRequest(format!(
                "track '{}' keyframes must be strictly increasing within durationMs",
                track.part
            )));
        }
        validate_finite_pair(keyframe.translate, "keyframe translate")?;
        validate_finite(keyframe.rotate_deg, "keyframe rotateDeg")?;
        validate_finite_pair(keyframe.scale, "keyframe scale")?;
        if keyframe.scale[0] <= 0.0 || keyframe.scale[1] <= 0.0 {
            return Err(PpError::InvalidRequest(
                "keyframe scale values must be positive".to_string(),
            ));
        }
        validate_finite(keyframe.opacity, "keyframe opacity")?;
        if !(0.0..=1.0).contains(&keyframe.opacity) {
            return Err(PpError::InvalidRequest(
                "keyframe opacity must be from 0 through 1".to_string(),
            ));
        }
        previous = Some(keyframe.at_ms);
    }
    Ok(())
}

fn build_animated_svg(
    scene: &SvgScene,
    request: &MotionRequest,
    tracks_by_part: &BTreeMap<&str, &MotionTrack>,
) -> PpResult<String> {
    let mut css = String::new();
    let mut part_by_path = BTreeMap::new();
    for part in &request.parts {
        let Some(track) = tracks_by_part.get(part.id.as_str()) else {
            continue;
        };
        for path_id in &part.path_ids {
            part_by_path.insert(path_id.as_str(), part.id.as_str());
        }
        writeln!(
            css,
            ".pp-motion-{}{{transform-box:view-box;transform-origin:{}px {}px;animation:pp-motion-{} {}ms {} {};}}",
            part.id,
            format_number(part.anchor[0]),
            format_number(part.anchor[1]),
            part.id,
            request.duration_ms,
            match track.interpolation {
                MotionInterpolation::Linear => "linear",
                MotionInterpolation::Hold => "steps(1,end)",
            },
            if request.looped { "infinite" } else { "1 forwards" }
        )
        .map_err(|_| PpError::InvalidRequest("cannot build motion CSS".to_string()))?;
        writeln!(css, "@keyframes pp-motion-{}{{", part.id)
            .map_err(|_| PpError::InvalidRequest("cannot build motion CSS".to_string()))?;
        for keyframe in &track.keyframes {
            let percent = f64::from(keyframe.at_ms) / f64::from(request.duration_ms) * 100.0;
            writeln!(
                css,
                "{}%{{transform:translate({}px,{}px) rotate({}deg) scale({},{});opacity:{};}}",
                format_number(percent),
                format_number(keyframe.translate[0]),
                format_number(keyframe.translate[1]),
                format_number(keyframe.rotate_deg),
                format_number(keyframe.scale[0]),
                format_number(keyframe.scale[1]),
                format_number(keyframe.opacity)
            )
            .map_err(|_| PpError::InvalidRequest("cannot build motion CSS".to_string()))?;
        }
        css.push_str("}\n");
    }
    let wrapped_svg = wrap_motion_paths(
        &scene.svg,
        &part_by_path,
        &scene.paths,
        &scene.element_prefix,
    )?;
    let root_end = scene.root_opening_tag.end;
    if root_end > wrapped_svg.len() {
        return Err(PpError::InvalidRequest(
            "motion SVG root opening-tag span is invalid".to_string(),
        ));
    }
    let mut animated = String::with_capacity(wrapped_svg.len() + css.len() + 64);
    animated.push_str(&wrapped_svg[..root_end]);
    writeln!(animated, "\n<{}style>", scene.element_prefix)
        .map_err(|_| PpError::InvalidRequest("cannot build motion SVG stylesheet".to_string()))?;
    animated.push_str(&css);
    write!(animated, "</{}style>", scene.element_prefix)
        .map_err(|_| PpError::InvalidRequest("cannot build motion SVG stylesheet".to_string()))?;
    animated.push_str(&wrapped_svg[root_end..]);
    SvgContract::validate_generated_motion(&animated)?;
    ensure_motion_output_bound("animated SVG", animated.len())?;
    Ok(animated)
}

fn wrap_motion_paths(
    svg: &str,
    part_by_path: &BTreeMap<&str, &str>,
    paths: &[SvgPath],
    element_prefix: &str,
) -> PpResult<String> {
    let mut output = String::with_capacity(svg.len() + part_by_path.len() * 40);
    let mut cursor = 0usize;
    for path in paths {
        if path.element_span.start < cursor || path.element_span.end > svg.len() {
            return Err(PpError::InvalidRequest(
                "motion SVG path element span is invalid".to_string(),
            ));
        }
        output.push_str(&svg[cursor..path.element_span.start]);
        let element = &svg[path.element_span.clone()];
        if let Some(part_id) = part_by_path.get(path.id.as_str()) {
            write!(
                output,
                "<{}g class=\"pp-motion-{part_id}\">{element}</{}g>",
                element_prefix, element_prefix
            )
            .map_err(|_| PpError::InvalidRequest("cannot wrap motion SVG paths".to_string()))?;
        } else {
            output.push_str(element);
        }
        cursor = path.element_span.end;
    }
    output.push_str(&svg[cursor..]);
    Ok(output)
}

fn build_lottie(
    scene: &SvgScene,
    request: &MotionRequest,
    track_by_path: &BTreeMap<&str, (&MotionPart, Option<&MotionTrack>)>,
) -> PpResult<(Value, usize)> {
    let total_frames = frames(request.duration_ms, request.fps);
    let mut layers = Vec::with_capacity(scene.paths.len());
    let mut shape_count = 0;
    for (reverse_index, path) in scene.paths.iter().rev().enumerate() {
        let (anchor, track) = track_by_path
            .get(path.id.as_str())
            .map(|(part, track)| (part.anchor, *track))
            .unwrap_or(([0.0, 0.0], None));
        let mut shapes = parse_lottie_beziers(path)?
            .into_iter()
            .enumerate()
            .map(|(index, bezier)| {
                json!({
                    "ty": "sh",
                    "nm": format!("{}-shape-{}", path.id, index + 1),
                    "d": 1,
                    "ks": { "a": 0, "k": bezier }
                })
            })
            .collect::<Vec<_>>();
        shape_count += shapes.len();
        let color = parse_hex_color(&path.fill)?;
        shapes.push(json!({
            "ty": "fl",
            "nm": format!("{}-fill", path.id),
            "c": { "a": 0, "k": color },
            "o": { "a": 0, "k": path.fill_opacity * 100.0 },
            "r": 2
        }));
        layers.push(json!({
            "ddd": 0,
            "ind": reverse_index + 1,
            "ty": 4,
            "nm": path.id,
            "sr": 1,
            "ks": lottie_transform(anchor, track, request.fps),
            "ao": 0,
            "shapes": shapes,
            "ip": 0,
            "op": total_frames,
            "st": 0,
            "bm": 0
        }));
    }
    let markers = request
        .markers
        .iter()
        .map(|marker| {
            json!({
                "cm": marker.name,
                "tm": frames(marker.from_ms, request.fps),
                "dr": frames(marker.to_ms - marker.from_ms, request.fps)
            })
        })
        .collect::<Vec<_>>();
    Ok((
        json!({
            "v": "5.12.2",
            "ver": 10000,
            "nm": request.name,
            "ddd": 0,
            "h": scene.height,
            "w": scene.width,
            "meta": { "g": format!("{GENERATOR_PREFIX}{}", env!("CARGO_PKG_VERSION")) },
            "fr": request.fps,
            "ip": 0,
            "op": total_frames,
            "assets": [],
            "layers": layers,
            "markers": markers
        }),
        shape_count,
    ))
}

fn lottie_transform(anchor: [f64; 2], track: Option<&MotionTrack>, fps: u32) -> Value {
    let Some(track) = track else {
        return json!({
            "o": { "a": 0, "k": 100 },
            "r": { "a": 0, "k": 0 },
            "p": { "a": 0, "k": [0, 0, 0] },
            "a": { "a": 0, "k": [0, 0, 0] },
            "s": { "a": 0, "k": [100, 100, 100] }
        });
    };
    json!({
        "o": animated_scalar(track, fps, |keyframe| keyframe.opacity * 100.0),
        "r": animated_scalar(track, fps, |keyframe| keyframe.rotate_deg),
        "p": animated_vector(track, fps, |keyframe| vec![
            anchor[0] + keyframe.translate[0],
            anchor[1] + keyframe.translate[1],
            0.0
        ]),
        "a": { "a": 0, "k": [anchor[0], anchor[1], 0.0] },
        "s": animated_vector(track, fps, |keyframe| vec![
            keyframe.scale[0] * 100.0,
            keyframe.scale[1] * 100.0,
            100.0
        ])
    })
}

fn animated_scalar(track: &MotionTrack, fps: u32, value: impl Fn(&MotionKeyframe) -> f64) -> Value {
    animated_vector(track, fps, |keyframe| vec![value(keyframe)])
}

fn animated_vector(
    track: &MotionTrack,
    fps: u32,
    value: impl Fn(&MotionKeyframe) -> Vec<f64>,
) -> Value {
    let mut keyframes = Vec::with_capacity(track.keyframes.len());
    for (index, keyframe) in track.keyframes.iter().enumerate() {
        let mut item = json!({
            "t": frames(keyframe.at_ms, fps),
            "s": value(keyframe)
        });
        if index + 1 < track.keyframes.len() {
            match track.interpolation {
                MotionInterpolation::Linear => {
                    item["o"] = json!({
                        "x": [1.0 / 3.0],
                        "y": [1.0 / 3.0]
                    });
                    item["i"] = json!({
                        "x": [2.0 / 3.0],
                        "y": [2.0 / 3.0]
                    });
                }
                MotionInterpolation::Hold => item["h"] = json!(1),
            }
        }
        keyframes.push(item);
    }
    json!({ "a": 1, "k": keyframes })
}

fn parse_lottie_beziers(path: &SvgPath) -> PpResult<Vec<LottieBezier>> {
    let mut output = Vec::new();
    let mut builder: Option<BezierBuilder> = None;
    let mut current = [0.0, 0.0];
    let mut last_cubic_control: Option<[f64; 2]> = None;
    let mut last_quadratic_control: Option<[f64; 2]> = None;
    for segment in &path.segments {
        match *segment {
            PathSegment::MoveTo { abs, x, y } => {
                if let Some(previous) = builder.take().and_then(BezierBuilder::finish) {
                    output.push(previous);
                }
                current = absolute_point(abs, current, x, y);
                builder = Some(BezierBuilder::new(current));
                last_cubic_control = None;
                last_quadratic_control = None;
            }
            PathSegment::LineTo { abs, x, y } => {
                current = absolute_point(abs, current, x, y);
                builder_mut(&mut builder, path)?.line_to(current);
                last_cubic_control = None;
                last_quadratic_control = None;
            }
            PathSegment::HorizontalLineTo { abs, x } => {
                current[0] = if abs { x } else { current[0] + x };
                builder_mut(&mut builder, path)?.line_to(current);
                last_cubic_control = None;
                last_quadratic_control = None;
            }
            PathSegment::VerticalLineTo { abs, y } => {
                current[1] = if abs { y } else { current[1] + y };
                builder_mut(&mut builder, path)?.line_to(current);
                last_cubic_control = None;
                last_quadratic_control = None;
            }
            PathSegment::CurveTo {
                abs,
                x1,
                y1,
                x2,
                y2,
                x,
                y,
            } => {
                let control_1 = absolute_point(abs, current, x1, y1);
                let control_2 = absolute_point(abs, current, x2, y2);
                let point = absolute_point(abs, current, x, y);
                builder_mut(&mut builder, path)?.curve_to(control_1, control_2, point);
                current = point;
                last_cubic_control = Some(control_2);
                last_quadratic_control = None;
            }
            PathSegment::SmoothCurveTo { abs, x2, y2, x, y } => {
                let control_1 = last_cubic_control
                    .map(|control| reflect(control, current))
                    .unwrap_or(current);
                let control_2 = absolute_point(abs, current, x2, y2);
                let point = absolute_point(abs, current, x, y);
                builder_mut(&mut builder, path)?.curve_to(control_1, control_2, point);
                current = point;
                last_cubic_control = Some(control_2);
                last_quadratic_control = None;
            }
            PathSegment::Quadratic { abs, x1, y1, x, y } => {
                let quadratic = absolute_point(abs, current, x1, y1);
                let point = absolute_point(abs, current, x, y);
                let control_1 = add(current, scale(sub(quadratic, current), 2.0 / 3.0));
                let control_2 = add(point, scale(sub(quadratic, point), 2.0 / 3.0));
                builder_mut(&mut builder, path)?.curve_to(control_1, control_2, point);
                current = point;
                last_cubic_control = None;
                last_quadratic_control = Some(quadratic);
            }
            PathSegment::SmoothQuadratic { abs, x, y } => {
                let quadratic = last_quadratic_control
                    .map(|control| reflect(control, current))
                    .unwrap_or(current);
                let point = absolute_point(abs, current, x, y);
                let control_1 = add(current, scale(sub(quadratic, current), 2.0 / 3.0));
                let control_2 = add(point, scale(sub(quadratic, point), 2.0 / 3.0));
                builder_mut(&mut builder, path)?.curve_to(control_1, control_2, point);
                current = point;
                last_cubic_control = None;
                last_quadratic_control = Some(quadratic);
            }
            PathSegment::EllipticalArc { .. } => {
                return Err(unsupported_arc(path));
            }
            PathSegment::ClosePath { .. } => {
                let active = builder_mut(&mut builder, path)?;
                let subpath_start = active.vertices[0];
                active.close();
                if let Some(previous) = builder.take().and_then(BezierBuilder::finish) {
                    output.push(previous);
                }
                current = subpath_start;
                builder = Some(BezierBuilder::new(current));
                last_cubic_control = None;
                last_quadratic_control = None;
            }
        }
    }
    if let Some(last) = builder.take().and_then(BezierBuilder::finish) {
        output.push(last);
    }
    if output.is_empty() {
        return Err(PpError::InvalidRequest(format!(
            "path '{}' has no exportable Lottie shape",
            path.id
        )));
    }
    Ok(output)
}

fn builder_mut<'a>(
    builder: &'a mut Option<BezierBuilder>,
    path: &SvgPath,
) -> PpResult<&'a mut BezierBuilder> {
    builder.as_mut().ok_or_else(|| {
        PpError::InvalidRequest(format!("path '{}' must start with a move command", path.id))
    })
}

fn inspector_html(scene: &SvgScene, layers: &MotionLayersDocument) -> PpResult<String> {
    SvgContract::validate(&scene.svg)?;
    let layer_id_bytes = layers
        .layers
        .iter()
        .map(|layer| layer.id.len())
        .sum::<usize>();
    let related_id_bytes = layers
        .layers
        .iter()
        .flat_map(|layer| &layer.related_path_ids)
        .map(String::len)
        .sum::<usize>();
    let inspector_budget = scene
        .svg
        .len()
        .checked_add(layer_id_bytes.saturating_mul(2))
        .and_then(|bytes| bytes.checked_add(related_id_bytes.saturating_mul(2)))
        .and_then(|bytes| bytes.checked_add(layers.layers.len().saturating_mul(512)))
        .and_then(|bytes| bytes.checked_add(4_096))
        .ok_or_else(|| {
            PpError::InvalidRequest("motion layer inspector byte budget overflow".to_string())
        })?;
    ensure_motion_output_bound("motion layer inspector", inspector_budget)?;
    let mut layer_rows = String::new();
    for layer in &layers.layers {
        let related = layer.related_path_ids.join(",");
        write!(
            layer_rows,
            "<button data-path=\"{}\" data-related=\"{}\" title=\"bounds: {:.1}, {:.1}, {:.1}, {:.1}; related: {}\"><span style=\"background:{}\"></span>{}<small>#{:04} · {:.0}×{:.0} · +{}</small></button>",
            layer.id,
            related,
            layer.bounds[0],
            layer.bounds[1],
            layer.bounds[2],
            layer.bounds[3],
            related,
            html_escape(&layer.fill),
            layer.id,
            layer.order + 1,
            layer.bounds[2],
            layer.bounds[3],
            layer.related_path_ids.len()
        )
        .map_err(|_| PpError::InvalidRequest("cannot build motion layer inspector".to_string()))?;
    }
    let inspector = format!(
        "<!doctype html><html lang=\"ko\"><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width,initial-scale=1\"><title>Motion layer inspector</title><style>body{{margin:0;background:#0b1020;color:#eef2ff;font:14px system-ui;display:grid;grid-template-columns:minmax(0,1fr) 360px;height:100vh}}main{{display:grid;place-items:center;overflow:auto;background:#fff}}main svg{{max-width:94%;max-height:94%}}aside{{padding:16px;overflow:auto}}button{{display:flex;width:100%;gap:8px;align-items:center;background:#172033;color:#eef2ff;border:1px solid #334155;border-radius:7px;padding:7px;margin:5px 0;text-align:left}}button span{{width:14px;height:14px;border:1px solid #fff;flex:0 0 auto}}button small{{margin-left:auto;color:#94a3b8}}path:hover,path.selected{{stroke:#ff2bd6!important;stroke-width:3!important}}textarea{{width:100%;height:120px;background:#050816;color:#7dd3fc}}</style></head><body><main>{}</main><aside><h1>Path IDs</h1><p>클릭은 한 path, Shift+클릭은 같은 위치·비슷한 크기·인접 색의 related paths까지 함께 고릅니다. layers.json에는 색, 순서, bounds, shape 수와 relatedPathIds가 있습니다.</p><textarea id=\"selected\" readonly></textarea>{}</aside><script>const chosen=new Set(),out=document.querySelector('#selected');function render(){{document.querySelectorAll('path[id]').forEach(p=>p.classList.toggle('selected',chosen.has(p.id)));out.value=[...chosen].sort().join('\\n')}}function toggle(id){{chosen.has(id)?chosen.delete(id):chosen.add(id)}}function choose(id,related){{toggle(id);if(related)related.split(',').filter(Boolean).forEach(value=>chosen.add(value));render()}}document.querySelectorAll('[data-path]').forEach(b=>b.onclick=e=>choose(b.dataset.path,e.shiftKey?b.dataset.related:''));document.querySelectorAll('path[id]').forEach(p=>p.onclick=e=>{{e.stopPropagation();const b=document.querySelector(`[data-path=\"${{p.id}}\"]`);choose(p.id,e.shiftKey?b?.dataset.related:'')}})</script></body></html>",
        scene.svg, layer_rows
    );
    ensure_motion_output_bound("motion layer inspector", inspector.len())?;
    Ok(inspector)
}

fn ensure_motion_source_bound(svg: &str) -> PpResult<()> {
    if svg.len() > MOTION_MAX_SOURCE_BYTES {
        return Err(PpError::InvalidRequest(format!(
            "motion SVG exceeds {MOTION_MAX_SOURCE_BYTES} byte limit"
        )));
    }
    Ok(())
}

fn ensure_motion_output_bound(kind: &str, bytes: usize) -> PpResult<()> {
    if bytes > MOTION_MAX_OUTPUT_BYTES {
        return Err(PpError::InvalidRequest(format!(
            "{kind} exceeds {MOTION_MAX_OUTPUT_BYTES} byte limit"
        )));
    }
    Ok(())
}

fn ensure_motion_request_bounds(request: &MotionRequest) -> PpResult<()> {
    if request.authored_paths.len() > MOTION_MAX_REQUEST_ITEMS
        || request.parts.len() > MOTION_MAX_REQUEST_ITEMS
        || request.tracks.len() > MOTION_MAX_REQUEST_ITEMS
        || request.markers.len() > MOTION_MAX_REQUEST_ITEMS
        || request
            .parts
            .iter()
            .any(|part| part.path_ids.len() > MOTION_MAX_REQUEST_ITEMS)
        || request
            .tracks
            .iter()
            .any(|track| track.keyframes.len() > MOTION_MAX_REQUEST_ITEMS)
    {
        return Err(PpError::InvalidRequest(format!(
            "motion request exceeds {MOTION_MAX_REQUEST_ITEMS} item limit"
        )));
    }
    let total_keyframes = request
        .tracks
        .iter()
        .map(|track| track.keyframes.len())
        .sum::<usize>();
    let total_path_references = request
        .parts
        .iter()
        .map(|part| part.path_ids.len())
        .sum::<usize>();
    let authored_path_bytes = request
        .authored_paths
        .iter()
        .map(|path| path.d.len())
        .sum::<usize>();
    let request_bytes = serde_json::to_vec(request)
        .map_err(|error| {
            PpError::InvalidRequest(format!("cannot serialize motion request: {error}"))
        })?
        .len();
    let projected_output_bytes = authored_path_bytes
        .saturating_add(total_keyframes.saturating_mul(192))
        .saturating_add(total_path_references.saturating_mul(96))
        .saturating_add(request_bytes.saturating_mul(2));
    if total_keyframes > MOTION_MAX_TOTAL_KEYFRAMES
        || total_path_references > MOTION_MAX_TOTAL_PATH_REFERENCES
        || authored_path_bytes > MOTION_MAX_AUTHORED_PATH_BYTES
        || request_bytes > MOTION_MAX_REQUEST_BYTES
        || projected_output_bytes > MOTION_MAX_OUTPUT_BYTES
    {
        return Err(PpError::InvalidRequest(
            "motion request aggregate limit exceeded".to_string(),
        ));
    }
    Ok(())
}

fn preview_html(request: &MotionRequest) -> String {
    format!(
        "<!doctype html><html lang=\"ko\"><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width,initial-scale=1\"><title>{0} motion preview</title><style>body{{margin:0;background:#090d14;color:#eef2f7;font:15px system-ui}}main{{max-width:1200px;margin:auto;padding:24px}}.grid{{display:grid;grid-template-columns:1fr 1fr;gap:16px}}section{{background:#111827;border:1px solid #334155;border-radius:14px;padding:14px}}object,canvas{{width:100%;aspect-ratio:1;background:#fff;border-radius:9px}}.note{{color:#94a3b8}}@media(max-width:760px){{.grid{{grid-template-columns:1fr}}}}</style></head><body><main><h1>{0}</h1><p class=\"note\">왼쪽은 dependency-free animated SVG, 오른쪽은 dotLottie Web이 animation.json을 재생합니다.</p><div class=\"grid\"><section><h2>Animated SVG</h2><object data=\"animated.svg\" type=\"image/svg+xml\"></object></section><section><h2>Lottie JSON</h2><canvas id=\"lottie\"></canvas><p id=\"status\" class=\"note\">dotLottie Web 로딩 중…</p></section></div></main><script type=\"module\">try{{const{{DotLottie}}=await import('https://cdn.jsdelivr.net/npm/@lottiefiles/dotlottie-web@0.77.0/+esm');new DotLottie({{canvas:document.querySelector('#lottie'),src:'animation.json',autoplay:true,loop:{1}}});document.querySelector('#status').textContent='dotLottie Web 재생 중';}}catch(error){{document.querySelector('#status').textContent='외부 플레이어를 불러오지 못했습니다. animated.svg 미리보기는 계속 동작합니다.';}}</script></body></html>",
        html_escape(&request.name),
        if request.looped { "true" } else { "false" }
    )
}

fn validate_safe_relative_svg(value: &str) -> PpResult<()> {
    if value.is_empty()
        || value.contains('\0')
        || value.contains('\\')
        || value.starts_with('/')
        || value
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
        || !value.to_ascii_lowercase().ends_with(".svg")
    {
        return Err(PpError::InvalidRequest(
            "motion sourceSvg must be a safe relative .svg path".to_string(),
        ));
    }
    Ok(())
}

fn validate_id(value: &str, label: &str) -> PpResult<()> {
    let bytes = value.as_bytes();
    let valid = !bytes.is_empty()
        && bytes[0].is_ascii_alphanumeric()
        && bytes[bytes.len() - 1].is_ascii_alphanumeric()
        && bytes
            .iter()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || *byte == b'-')
        && !value.contains("--");
    if !valid {
        return Err(PpError::InvalidRequest(format!(
            "{label} '{value}' must use lowercase ASCII letters, digits, and single hyphens"
        )));
    }
    Ok(())
}

fn validate_finite_pair(value: [f64; 2], label: &str) -> PpResult<()> {
    validate_finite(value[0], label)?;
    validate_finite(value[1], label)
}

fn validate_finite(value: f64, label: &str) -> PpResult<()> {
    if !value.is_finite() {
        return Err(PpError::InvalidRequest(format!("{label} must be finite")));
    }
    Ok(())
}

fn parse_hex_color(raw: &str) -> PpResult<[f64; 4]> {
    let hex = raw.strip_prefix('#').ok_or_else(|| {
        PpError::InvalidRequest(format!(
            "motion v1 Lottie export needs hexadecimal fills, found '{raw}'"
        ))
    })?;
    let invalid_fill = || PpError::InvalidRequest(format!("invalid hexadecimal fill '{raw}'"));
    let digits = hex.as_bytes();
    if !matches!(digits.len(), 3 | 6) || !digits.iter().all(u8::is_ascii_hexdigit) {
        return Err(invalid_fill());
    }
    let byte = |high: u8, low: u8| {
        let nibble = |value: u8| match value {
            b'0'..=b'9' => value - b'0',
            b'a'..=b'f' => value - b'a' + 10,
            b'A'..=b'F' => value - b'A' + 10,
            _ => unreachable!("validated hexadecimal digit"),
        };
        (nibble(high) << 4) | nibble(low)
    };
    let [red, green, blue] = match digits.len() {
        3 => [
            byte(digits[0], digits[0]),
            byte(digits[1], digits[1]),
            byte(digits[2], digits[2]),
        ],
        6 => [
            byte(digits[0], digits[1]),
            byte(digits[2], digits[3]),
            byte(digits[4], digits[5]),
        ],
        _ => unreachable!("validated hexadecimal length"),
    };
    Ok([
        f64::from(red) / 255.0,
        f64::from(green) / 255.0,
        f64::from(blue) / 255.0,
        1.0,
    ])
}

fn frames(milliseconds: u32, fps: u32) -> f64 {
    f64::from(milliseconds) * f64::from(fps) / 1_000.0
}

fn absolute_point(abs: bool, current: [f64; 2], x: f64, y: f64) -> [f64; 2] {
    if abs {
        [x, y]
    } else {
        [current[0] + x, current[1] + y]
    }
}

fn add(left: [f64; 2], right: [f64; 2]) -> [f64; 2] {
    [left[0] + right[0], left[1] + right[1]]
}

fn sub(left: [f64; 2], right: [f64; 2]) -> [f64; 2] {
    [left[0] - right[0], left[1] - right[1]]
}

fn scale(value: [f64; 2], amount: f64) -> [f64; 2] {
    [value[0] * amount, value[1] * amount]
}

fn reflect(control: [f64; 2], around: [f64; 2]) -> [f64; 2] {
    [2.0 * around[0] - control[0], 2.0 * around[1] - control[1]]
}

fn nearly_equal(left: [f64; 2], right: [f64; 2]) -> bool {
    (left[0] - right[0]).abs() <= 1e-6 && (left[1] - right[1]).abs() <= 1e-6
}

fn format_number(value: f64) -> String {
    let text = value.to_string();
    if text == "-0" {
        "0".to_string()
    } else {
        text
    }
}

fn html_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

#[cfg(test)]
mod tests {
    use super::*;

    const SVG: &str = "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"32\" height=\"32\" viewBox=\"0 0 32 32\"><path fill=\"#ff0000\" d=\"M4 4 C10 0 22 0 28 4 L28 28 L4 28 Z\"/></svg>";
    fn authored_request(scene_svg: &str, path_id: &str) -> MotionRequest {
        MotionRequest {
            schema: MOTION_SCHEMA.to_string(),
            name: "motion".to_string(),
            source_svg: "scene.svg".to_string(),
            source_svg_sha256: MotionCompiler::scene_sha256(scene_svg),
            fps: 30,
            duration_ms: 1_200,
            looped: true,
            authored_paths: Vec::new(),
            parts: vec![MotionPart {
                id: "part".to_string(),
                path_ids: vec![path_id.to_string()],
                anchor: [16.0, 16.0],
            }],
            tracks: vec![MotionTrack {
                part: "part".to_string(),
                interpolation: MotionInterpolation::Linear,
                keyframes: vec![
                    default_keyframe(0, [0.0, 0.0]),
                    default_keyframe(1_200, [0.0, 0.0]),
                ],
            }],
            markers: Vec::new(),
        }
    }

    #[test]
    fn scaffold_assigns_stable_ids_without_generated_motion_intent() -> PpResult<()> {
        let output = MotionCompiler::scaffold(SVG)?;
        assert!(output.scene_svg.contains("id=\"pp-path-0001\""));
        assert_eq!(output.layers.layers.len(), 1);
        assert!(output.inspector_html.contains("Path IDs"));
        assert!(!output.inspector_html.contains("subject"));
        Ok(())
    }

    #[test]
    fn scaffold_links_overlapping_boundary_color_paths() -> PpResult<()> {
        let svg = "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"32\" height=\"32\" viewBox=\"0 0 32 32\"><path fill=\"#000000\" d=\"M8 8 L24 8 L24 24 L8 24 Z\"/><path fill=\"#202020\" d=\"M9 9 L23 9 L23 23 L9 23 Z\"/></svg>";
        let output = MotionCompiler::scaffold(svg)?;
        assert_eq!(output.layers.layers[0].related_path_ids, ["pp-path-0002"]);
        assert_eq!(output.layers.layers[1].related_path_ids, ["pp-path-0001"]);
        Ok(())
    }

    #[test]
    fn build_emits_animated_svg_lottie_and_v2_manifest() -> PpResult<()> {
        let scaffold = MotionCompiler::scaffold(SVG)?;
        let request = authored_request(&scaffold.scene_svg, "pp-path-0001");
        let output = MotionCompiler::build(&scaffold.scene_svg, &request)?;
        let lottie: Value = serde_json::from_str(&output.lottie_json).unwrap();
        assert!(output.animated_svg.contains("@keyframes pp-motion-part"));
        assert_eq!(lottie["layers"].as_array().map(Vec::len), Some(1));
        assert_eq!(lottie["layers"][0]["ty"], 4);
        for property in ["o", "r", "p", "s"] {
            let first = &lottie["layers"][0]["ks"][property]["k"][0];
            assert_eq!(first["o"]["x"][0], json!(1.0 / 3.0));
            assert_eq!(first["o"]["y"][0], json!(1.0 / 3.0));
            assert_eq!(first["i"]["x"][0], json!(2.0 / 3.0));
            assert_eq!(first["i"]["y"][0], json!(2.0 / 3.0));
        }
        assert!(output
            .dotlottie_manifest_json
            .contains("\"version\": \"2\""));
        assert!(!output.report.dotlottie_archive_created);
        Ok(())
    }

    #[test]
    fn build_rejects_missing_caller_authored_path() -> PpResult<()> {
        let scaffold = MotionCompiler::scaffold(SVG)?;
        let request = authored_request(&scaffold.scene_svg, "pp-path-9999");
        let error = MotionCompiler::build(&scaffold.scene_svg, &request).unwrap_err();
        assert!(matches!(error, PpError::InvalidRequest(_)));
        Ok(())
    }

    #[test]
    fn build_injects_caller_authored_path() -> PpResult<()> {
        let scaffold = MotionCompiler::scaffold(SVG)?;
        let mut request = authored_request(&scaffold.scene_svg, "pp-path-0001");
        request.authored_paths.push(MotionAuthoredPath {
            id: "tear-left".to_string(),
            d: "M12 12 C10 16 11 20 12 21 C13 20 14 16 12 12 Z".to_string(),
            fill: "#4bb6ff".to_string(),
            fill_opacity: 1.0,
        });
        request.parts.push(MotionPart {
            id: "tear".to_string(),
            path_ids: vec!["tear-left".to_string()],
            anchor: [12.0, 16.0],
        });
        request.tracks.push(MotionTrack {
            part: "tear".to_string(),
            interpolation: MotionInterpolation::Linear,
            keyframes: vec![
                MotionKeyframe {
                    opacity: 0.0,
                    ..default_keyframe(0, [0.0, 0.0])
                },
                MotionKeyframe {
                    opacity: 1.0,
                    ..default_keyframe(600, [0.0, 2.0])
                },
                MotionKeyframe {
                    opacity: 0.0,
                    ..default_keyframe(1_200, [0.0, 5.0])
                },
            ],
        });
        let output = MotionCompiler::build(&scaffold.scene_svg, &request)?;
        assert!(output.animated_svg.contains("id=\"tear-left\""));
        assert!(output.lottie_json.contains("tear-left"));
        assert_eq!(output.report.authored_path_count, 1);
        Ok(())
    }

    #[test]
    fn hold_tracks_emit_discrete_svg_and_lottie_keyframes() -> PpResult<()> {
        let scaffold = MotionCompiler::scaffold(SVG)?;
        let mut request = authored_request(&scaffold.scene_svg, "pp-path-0001");
        request.tracks[0].interpolation = MotionInterpolation::Hold;
        request.tracks[0].keyframes[1] = MotionKeyframe {
            at_ms: 1_200,
            translate: [3.0, 4.0],
            rotate_deg: 45.0,
            scale: [2.0, 3.0],
            opacity: 0.25,
        };
        let output = MotionCompiler::build(&scaffold.scene_svg, &request)?;
        assert!(output.animated_svg.contains("steps(1,end)"));
        assert!(output
            .animated_svg
            .contains("100%{transform:translate(3px,4px) rotate(45deg) scale(2,3);opacity:0.25;}"));
        let lottie: Value = serde_json::from_str(&output.lottie_json).unwrap();
        let transform = &lottie["layers"][0]["ks"];
        for property in ["o", "r", "p", "s"] {
            assert_eq!(transform[property]["k"][0]["h"], 1);
            assert!(transform[property]["k"][1].get("h").is_none());
        }
        assert_eq!(transform["o"]["k"][1]["s"], json!([25.0]));
        assert_eq!(transform["r"]["k"][1]["s"], json!([45.0]));
        assert_eq!(transform["p"]["k"][1]["s"], json!([19.0, 20.0, 0.0]));
        assert_eq!(transform["s"]["k"][1]["s"], json!([200.0, 300.0, 100.0]));
        Ok(())
    }
}
