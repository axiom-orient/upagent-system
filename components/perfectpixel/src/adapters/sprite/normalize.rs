use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use self::chroma::{
    default_chroma_adjacent_pixel_threshold, default_chroma_adjacent_threshold,
    default_key_threshold, default_spill_max_fraction, default_unmix_reach,
};
use self::pipeline::{
    default_fps, default_fringe_delta, default_fringe_threshold, default_loop,
    default_min_used_pixels, default_outline_strength, default_palette_size,
    default_registration_drift_px, default_sheet_image, default_true,
};
use self::quality::{default_content_height_variance_px, default_content_height_variance_ratio};
use self::registration::{default_align_x, default_align_y, default_ground_y_variance_px};
use self::resize::default_resample;
use super::{PackingRequest, SpriteBundleRequest, StateRequest};
use crate::core::{content_bbox, FrameRect, PpError, PpResult, Raster};

mod chroma;
mod component;
mod pipeline;
mod pixel_art;
mod quality;
mod raster;
mod registration;
mod resize;
mod validation;

const HARD_ALPHA_THRESHOLD: u8 = 16;

const OPAQUE_ALPHA_THRESHOLD: u8 = 128;

const EDGE_DELTA_THRESHOLD: u32 = 96;

const IN_BAND_UNMIX_KEY_DEPTH: u8 = 2;

const SPILL_MIN_TINT: f64 = 40.0;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NormalizeRequest {
    pub character: String,
    #[serde(default = "default_sheet_image")]
    pub sheet_image: String,
    pub cell_width: u32,
    pub cell_height: u32,
    #[serde(default)]
    pub safe_margin_x: u32,
    #[serde(default)]
    pub safe_margin_y: u32,
    #[serde(default)]
    pub packing: PackingRequest,
    #[serde(default)]
    pub chroma: Option<NormalizeChroma>,
    #[serde(default)]
    pub fit: NormalizeFit,
    #[serde(default)]
    pub quality: NormalizeQuality,
    pub states: Vec<NormalizeStateRequest>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NormalizeStateRequest {
    pub name: String,
    #[serde(default = "default_fps")]
    pub fps: u32,
    #[serde(default = "default_loop", rename = "loop")]
    pub looped: bool,
    #[serde(default)]
    pub frames: Vec<String>,
    #[serde(default)]
    pub strip: Option<String>,
    #[serde(default)]
    pub frame_count: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NormalizeChroma {
    pub rgb: [u8; 3],
    #[serde(default = "default_key_threshold")]
    pub threshold: f64,
    #[serde(default = "default_fringe_threshold")]
    pub fringe_threshold: f64,
    #[serde(default = "default_fringe_delta")]
    pub fringe_delta: f64,
    #[serde(default = "default_unmix_reach")]
    pub unmix_reach: u8,
    #[serde(default = "default_spill_max_fraction")]
    pub spill_max_fraction: f64,
    #[serde(default = "default_chroma_adjacent_threshold")]
    pub adjacent_threshold: f64,
    #[serde(default = "default_chroma_adjacent_pixel_threshold")]
    pub adjacent_pixel_threshold: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NormalizeFit {
    #[serde(default)]
    pub pixel_perfect: bool,
    #[serde(default = "default_resample")]
    pub resample: String,
    #[serde(default = "default_align_x")]
    pub align_x: String,
    #[serde(default = "default_align_y")]
    pub align_y: String,
    #[serde(default)]
    pub logical_height: Option<u32>,
    #[serde(default = "default_true")]
    pub detail_bias: bool,
    #[serde(default = "default_palette_size")]
    pub palette_size: usize,
    #[serde(default)]
    pub outline: NormalizeOutline,
    #[serde(default = "default_true")]
    pub ground_frames: bool,
    #[serde(default)]
    pub pitch_hint: Option<u32>,
    #[serde(default = "default_true")]
    pub scale_conform: bool,
    #[serde(default)]
    pub allow_slot_fallback: bool,
}

impl Default for NormalizeFit {
    fn default() -> Self {
        Self {
            pixel_perfect: false,
            resample: resize::default_resample(),
            align_x: registration::default_align_x(),
            align_y: registration::default_align_y(),
            logical_height: None,
            detail_bias: true,
            palette_size: pipeline::default_palette_size(),
            outline: NormalizeOutline::default(),
            ground_frames: true,
            pitch_hint: None,
            scale_conform: true,
            allow_slot_fallback: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NormalizeOutline {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_outline_strength")]
    pub strength: f64,
}

impl Default for NormalizeOutline {
    fn default() -> Self {
        Self {
            enabled: true,
            strength: pipeline::default_outline_strength(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NormalizeQuality {
    #[serde(default = "default_min_used_pixels")]
    pub min_used_pixels: u32,
    #[serde(default = "default_content_height_variance_px")]
    pub max_content_height_variance_px: f64,
    #[serde(default = "default_content_height_variance_ratio")]
    pub max_content_height_variance_ratio: f64,
    #[serde(default = "default_ground_y_variance_px")]
    pub max_ground_y_variance_px: f64,
    #[serde(default = "default_registration_drift_px")]
    pub max_registration_drift_px: f64,
}

impl Default for NormalizeQuality {
    fn default() -> Self {
        Self {
            min_used_pixels: pipeline::default_min_used_pixels(),
            max_content_height_variance_px: quality::default_content_height_variance_px(),
            max_content_height_variance_ratio: quality::default_content_height_variance_ratio(),
            max_ground_y_variance_px: registration::default_ground_y_variance_px(),
            max_registration_drift_px: pipeline::default_registration_drift_px(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NormalizeStateImages {
    pub name: String,
    pub fps: u32,
    pub looped: bool,
    pub source: NormalizeStateSource,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NormalizeStateSource {
    Frames(Vec<Raster>),
    Strip { image: Raster, frame_count: u32 },
}

#[derive(Debug, Clone, PartialEq)]
pub struct NormalizePlan {
    pub bundle_request: SpriteBundleRequest,
    pub states: Vec<NormalizedStateOutput>,
    pub report: NormalizeReport,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NormalizedStateOutput {
    pub name: String,
    pub frames: Vec<Raster>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NormalizeReport {
    pub ok: bool,
    pub schema: String,
    pub character: String,
    pub cell_width: u32,
    pub cell_height: u32,
    pub states: Vec<NormalizeStateReport>,
    pub gates: Vec<NormalizeGateReport>,
    pub errors: Vec<String>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NormalizeStateReport {
    pub name: String,
    pub method: String,
    pub frames: usize,
    pub pixel_perfect: bool,
    pub pitch: Option<u32>,
    pub scale: u32,
    pub content_height_range: f64,
    pub ground_y_range: f64,
    pub center_x_range: f64,
    pub frame_records: Vec<NormalizeFrameReport>,
    pub ok: bool,
    pub errors: Vec<String>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NormalizeFrameReport {
    pub index: u32,
    pub width: u32,
    pub height: u32,
    pub content_box: Option<FrameRect>,
    pub nontransparent_pixels: u32,
    pub chroma_adjacent_pixels: u32,
    pub ground_y: u32,
    pub center_x: f64,
    pub pitch: Option<u32>,
    pub phase_x: Option<u32>,
    pub phase_y: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NormalizeGateReport {
    pub name: String,
    pub ok: bool,
    pub observed: f64,
    pub limit: f64,
    pub unit: String,
}

#[derive(Debug, Clone)]
struct PreparedState {
    name: String,
    method: String,
    frames: Vec<Raster>,
    pixel_perfect: bool,
    pitch: Option<u32>,
    scale: u32,
    phases: Vec<Option<(u32, u32)>>,
    warnings: Vec<String>,
}

#[derive(Debug, Clone)]
struct Component {
    pixels: Vec<usize>,
    area: usize,
    bbox: FrameRect,
    center_x: f64,
}

pub fn normalize_sprite(
    request: &NormalizeRequest,
    sources: Vec<NormalizeStateImages>,
) -> PpResult<NormalizePlan> {
    let errors = validation::validate_normalize_request(request, &sources);
    if !errors.is_empty() {
        return Err(PpError::InvalidRequest(errors.join("; ")));
    }

    let prepared_states = crate::io::parallel_map_owned(sources, |source| {
        let state_name = source.name.clone();
        pipeline::prepare_state(request, source).map_err(|error| {
            PpError::InvalidRequest(format!("state '{state_name}' preparation failed: {error}"))
        })
    })?;

    let normalized_states = registration::finalize_prepared_states(request, &prepared_states)?;
    let mut state_reports = Vec::new();
    let mut gates = Vec::new();
    let mut warnings = Vec::new();
    let mut errors = Vec::new();
    for prepared in &prepared_states {
        warnings.extend(prepared.warnings.iter().cloned());
    }

    let indexed_states = normalized_states.iter().enumerate().collect::<Vec<_>>();
    let reports_and_gates = crate::io::parallel_map(&indexed_states, |&(state_index, output)| {
        let prepared = &prepared_states[state_index];
        let report = pixel_art::inspect_normalized_state(request, prepared, output);
        let state_gates = quality::state_quality_gates(request, &report);
        Ok::<_, std::convert::Infallible>((report, state_gates))
    })
    .unwrap();
    for (report, state_gates) in reports_and_gates {
        gates.extend(state_gates);
        state_reports.push(report);
    }

    for state in &state_reports {
        for error in &state.errors {
            errors.push(format!("{}: {error}", state.name));
        }
    }
    for gate in &gates {
        if !gate.ok {
            errors.push(format!(
                "gate '{}' failed: observed {:.3} {}, limit {:.3}",
                gate.name, gate.observed, gate.unit, gate.limit
            ));
        }
    }

    let ok = errors.is_empty();
    let bundle_request = pipeline::bundle_request_for(request, &normalized_states);
    Ok(NormalizePlan {
        bundle_request,
        states: if ok { normalized_states } else { Vec::new() },
        report: NormalizeReport {
            ok,
            schema: normalize_schema().to_string(),
            character: request.character.clone(),
            cell_width: request.cell_width,
            cell_height: request.cell_height,
            states: state_reports,
            gates,
            errors,
            warnings,
        },
    })
}

pub fn normalize_schema() -> &'static str {
    super::NORMALIZE_SCHEMA
}

/// Enforces fail-closed postconditions before any bundle-ready normalize output is written.
///
/// A failed quality report cannot be treated as a successful artifact plan. Request and
/// report state order is compared before success files are emitted.
pub fn validate_normalize_plan_contract(
    request: &NormalizeRequest,
    plan: &NormalizePlan,
) -> PpResult<()> {
    if !plan.report.ok {
        return Err(quality::normalize_quality_error(
            "failed normalization plan cannot be emitted as bundle-ready output".to_owned(),
        ));
    }

    let requested_states = request
        .states
        .iter()
        .map(|state| state.name.as_str())
        .collect::<Vec<_>>();
    let reported_states = plan
        .report
        .states
        .iter()
        .map(|state| state.name.as_str())
        .collect::<Vec<_>>();
    let output_states = plan
        .states
        .iter()
        .map(|state| state.name.as_str())
        .collect::<Vec<_>>();
    let bundle_states = plan
        .bundle_request
        .states
        .iter()
        .map(|state| state.name.as_str())
        .collect::<Vec<_>>();

    if requested_states != reported_states
        || requested_states != output_states
        || requested_states != bundle_states
    {
        return Err(validation::normalize_contract_error(format!(
            "normalize state order drifted: request={requested_states:?}, report={reported_states:?}, output={output_states:?}, bundle={bundle_states:?}"
        )));
    }

    Ok(())
}
