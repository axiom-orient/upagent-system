use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};

use serde::Serialize;

use crate::{
    compose_bundle_with_packing, normalize_sprite, validate_normalize_plan_contract, BundlePlan,
    NormalizePlan, NormalizeRequest, NormalizeStateImages, NormalizeStateSource, PngEncoder,
    PpError, PpResult, SpriteBundleRequest, StateFrames,
};

use super::super::{
    generation::GenerationWorkflow,
    generation_adapter::{
        validate_generation_artifact_count, validate_input_snapshot_count,
        validate_input_snapshot_set, InputSnapshot,
    },
    path::{reject_same_path, resolve_request_frame_path},
    shared::{
        account_decoded_raster_bytes, decode_raster_snapshot, publish_generated_artifacts,
        read_json_request_snapshot, serialize_json, verify_request_digest, BundleArtifact,
    },
};

pub(crate) fn normalize(
    request_path: PathBuf,
    out_dir: PathBuf,
    request_sha256: Option<&str>,
) -> PpResult<String> {
    let (request, request_snapshot): (NormalizeRequest, _) =
        read_json_request_snapshot(&request_path)?;
    verify_request_digest(&request_snapshot, request_sha256)?;
    validate_input_snapshot_count(normalize_input_snapshot_count(&request)?)?;
    validate_generation_artifact_count(normalize_max_artifact_count(&request)?)?;

    let request_base = request_path.parent().unwrap_or_else(|| Path::new("."));
    let (sources, mut input_snapshots) = load_normalize_sources(request_base, &request)?;
    input_snapshots.insert(0, request_snapshot);
    validate_input_snapshot_set(&input_snapshots)?;

    let plan = normalize_sprite(&request, sources)?;
    validate_generation_artifact_count(normalize_output_relative_paths(&plan).len())?;
    reject_normalize_output_collisions(&out_dir, &plan, &input_snapshots)?;
    if plan.report.ok {
        validate_normalize_plan_contract(&request, &plan)?;
    }
    write_normalize_outputs(&out_dir, &plan, input_snapshots)?;

    if !plan.report.ok {
        let message = if plan.report.errors.is_empty() {
            "normalize quality gate failed".to_string()
        } else {
            plan.report.errors.join("; ")
        };
        return Err(PpError::QualityGate {
            gate: "normalize".to_string(),
            message,
        });
    }

    serialize_json(
        &NormalizeSummary {
            ok: true,
            out_dir: out_dir.display().to_string(),
            character: request.character,
            states: plan.states.len(),
            frames: plan.states.iter().map(|state| state.frames.len()).sum(),
            failed_gates: 0,
            files: normalize_output_files(&plan),
        },
        "<normalize-summary>",
    )
}

pub(crate) fn bundle(
    request_path: PathBuf,
    out_dir: PathBuf,
    request_sha256: Option<&str>,
) -> PpResult<String> {
    let (request, request_snapshot): (SpriteBundleRequest, _) =
        read_json_request_snapshot(&request_path)?;
    verify_request_digest(&request_snapshot, request_sha256)?;
    validate_input_snapshot_count(bundle_input_snapshot_count(&request)?)?;
    validate_generation_artifact_count(bundle_min_artifact_count(&request)?)?;

    let request_base = request_path.parent().unwrap_or_else(|| Path::new("."));
    let (states, mut input_snapshots) = load_states(request_base, &request)?;
    input_snapshots.insert(0, request_snapshot);
    validate_input_snapshot_set(&input_snapshots)?;

    let plan = compose_bundle_with_packing(
        &request.character,
        &request.sheet_image,
        states,
        request.cell_width,
        request.cell_height,
        request.packing.clone(),
    )?;
    validate_generation_artifact_count(bundle_output_relative_paths(&plan).len())?;
    reject_bundle_output_collisions(&out_dir, &plan, &input_snapshots)?;
    write_bundle(&out_dir, &plan, input_snapshots)?;

    serialize_json(
        &BundleSummary {
            ok: true,
            out_dir: out_dir.display().to_string(),
            character: request.character,
            animations: plan.manifest.animations.len(),
            sheets: plan.sheets.len(),
            files: bundle_output_files(&plan),
        },
        "<bundle-summary>",
    )
}

fn normalize_input_snapshot_count(request: &NormalizeRequest) -> PpResult<usize> {
    request.states.iter().try_fold(1usize, |total, state| {
        let state_count = if state.frames.is_empty() {
            usize::from(state.strip.is_some())
        } else {
            state.frames.len()
        };
        total
            .checked_add(state_count)
            .ok_or_else(|| PpError::InvalidRequest("normalize input count overflow".to_string()))
    })
}

fn bundle_input_snapshot_count(request: &SpriteBundleRequest) -> PpResult<usize> {
    request.states.iter().try_fold(1usize, |total, state| {
        total
            .checked_add(state.frames.len())
            .ok_or_else(|| PpError::InvalidRequest("bundle input count overflow".to_string()))
    })
}

fn normalize_max_artifact_count(request: &NormalizeRequest) -> PpResult<usize> {
    let frame_count = request.states.iter().try_fold(0usize, |total, state| {
        let count = if state.frames.is_empty() {
            match state.frame_count {
                Some(value) => usize::try_from(value).map_err(|_| {
                    PpError::InvalidRequest(
                        "normalize frame count does not fit this platform".to_string(),
                    )
                })?,
                None => 0,
            }
        } else {
            state.frames.len()
        };
        total.checked_add(count).ok_or_else(|| {
            PpError::InvalidRequest("normalize output artifact count overflow".to_string())
        })
    })?;
    frame_count.checked_add(2).ok_or_else(|| {
        PpError::InvalidRequest("normalize output artifact count overflow".to_string())
    })
}

fn bundle_min_artifact_count(request: &SpriteBundleRequest) -> PpResult<usize> {
    let frame_count = request.states.iter().try_fold(0usize, |total, state| {
        total.checked_add(state.frames.len()).ok_or_else(|| {
            PpError::InvalidRequest("bundle output artifact count overflow".to_string())
        })
    })?;
    frame_count
        .checked_add(3)
        .ok_or_else(|| PpError::InvalidRequest("bundle output artifact count overflow".to_string()))
}

fn load_states(
    base_dir: &Path,
    request: &SpriteBundleRequest,
) -> PpResult<(Vec<StateFrames>, Vec<InputSnapshot>)> {
    let mut states = Vec::with_capacity(request.states.len());
    let mut snapshots = Vec::new();
    let mut decoded_raster_bytes = 0usize;
    for state in &request.states {
        let mut frames = Vec::with_capacity(state.frames.len());
        for frame in &state.frames {
            let path = resolve_request_frame_path(base_dir, frame)?;
            let (image, snapshot) = decode_raster_snapshot(&path)?;
            account_decoded_raster_bytes(&mut decoded_raster_bytes, &image)?;
            frames.push(image);
            snapshots.push(snapshot);
        }
        states.push(StateFrames {
            name: state.name.clone(),
            fps: state.fps,
            looped: state.looped,
            frames,
        });
    }
    Ok((states, snapshots))
}

fn load_normalize_sources(
    base_dir: &Path,
    request: &NormalizeRequest,
) -> PpResult<(Vec<NormalizeStateImages>, Vec<InputSnapshot>)> {
    let mut sources = Vec::with_capacity(request.states.len());
    let mut snapshots = Vec::new();
    let mut decoded_raster_bytes = 0usize;
    for state in &request.states {
        let source = if !state.frames.is_empty() {
            let mut frames = Vec::with_capacity(state.frames.len());
            for frame in &state.frames {
                let path = resolve_request_frame_path(base_dir, frame)?;
                let (image, snapshot) = decode_raster_snapshot(&path)?;
                account_decoded_raster_bytes(&mut decoded_raster_bytes, &image)?;
                frames.push(image);
                snapshots.push(snapshot);
            }
            NormalizeStateSource::Frames(frames)
        } else if let Some(strip) = state.strip.as_deref() {
            let path = resolve_request_frame_path(base_dir, strip)?;
            let (image, snapshot) = decode_raster_snapshot(&path)?;
            account_decoded_raster_bytes(&mut decoded_raster_bytes, &image)?;
            snapshots.push(snapshot);
            NormalizeStateSource::Strip {
                image,
                frame_count: state.frame_count.unwrap_or(0),
            }
        } else {
            NormalizeStateSource::Frames(Vec::new())
        };
        sources.push(NormalizeStateImages {
            name: state.name.clone(),
            fps: state.fps,
            looped: state.looped,
            source,
        });
    }
    Ok((sources, snapshots))
}

fn reject_normalize_output_collisions(
    out_dir: &Path,
    plan: &NormalizePlan,
    input_snapshots: &[InputSnapshot],
) -> PpResult<()> {
    for relative in normalize_output_relative_paths(plan) {
        let output_path = out_dir.join(relative);
        for input in input_snapshots {
            reject_same_path(
                input.source_path(),
                &output_path,
                "normalize output must not overwrite request or frame inputs",
            )?;
        }
    }
    Ok(())
}

fn normalize_output_files(plan: &NormalizePlan) -> Vec<String> {
    normalize_output_relative_paths(plan).into_iter().collect()
}

fn normalize_output_relative_paths(plan: &NormalizePlan) -> BTreeSet<String> {
    let mut paths = BTreeSet::new();
    paths.insert("normalize-report.json".to_string());
    if plan.report.ok {
        paths.insert("sprite-request.json".to_string());
        for state in &plan.states {
            for index in 0..state.frames.len() {
                paths.insert(format!("frames/{}/frame-{index:02}.png", state.name));
            }
        }
    }
    paths
}

fn write_normalize_outputs(
    out_dir: &Path,
    plan: &NormalizePlan,
    input_snapshots: Vec<InputSnapshot>,
) -> PpResult<()> {
    let mut artifacts = Vec::new();
    artifacts.push(BundleArtifact {
        relative_path: "normalize-report.json".to_string(),
        bytes: serde_json::to_vec_pretty(&plan.report).map_err(|source| PpError::Json {
            path: out_dir.join("normalize-report.json"),
            message: source.to_string(),
        })?,
    });
    if plan.report.ok {
        artifacts.push(BundleArtifact {
            relative_path: "sprite-request.json".to_string(),
            bytes: serde_json::to_vec_pretty(&plan.bundle_request).map_err(|source| {
                PpError::Json {
                    path: out_dir.join("sprite-request.json"),
                    message: source.to_string(),
                }
            })?,
        });
        for state in &plan.states {
            for (index, frame) in state.frames.iter().enumerate() {
                artifacts.push(BundleArtifact {
                    relative_path: format!("frames/{}/frame-{index:02}.png", state.name),
                    bytes: PngEncoder::encode_rgba(frame)?,
                });
            }
        }
    }
    publish_generated_artifacts(
        out_dir,
        GenerationWorkflow::Normalize,
        artifacts,
        Vec::new(),
        input_snapshots,
    )
}

fn reject_bundle_output_collisions(
    out_dir: &Path,
    plan: &BundlePlan,
    input_snapshots: &[InputSnapshot],
) -> PpResult<()> {
    for relative in bundle_output_relative_paths(plan) {
        let output_path = out_dir.join(relative);
        for input in input_snapshots {
            reject_same_path(
                input.source_path(),
                &output_path,
                "bundle output must not overwrite request or frame inputs",
            )?;
        }
    }
    Ok(())
}

fn bundle_output_files(plan: &BundlePlan) -> Vec<String> {
    let mut files = Vec::with_capacity(1 + plan.sheets.len() * 2 + 1);
    for sheet in &plan.sheets {
        files.push(sheet.relative_path.clone());
    }
    files.push("manifest.json".to_string());
    for sheet in &plan.sheets {
        files.push(sheet.aseprite_json_path.clone());
    }
    files.push("frames/".to_string());
    files
}

fn write_bundle(
    out_dir: &Path,
    plan: &BundlePlan,
    input_snapshots: Vec<InputSnapshot>,
) -> PpResult<()> {
    publish_generated_artifacts(
        out_dir,
        GenerationWorkflow::Bundle,
        bundle_artifacts(out_dir, plan)?,
        Vec::new(),
        input_snapshots,
    )
}

fn bundle_output_relative_paths(plan: &BundlePlan) -> BTreeSet<String> {
    let mut paths = BTreeSet::new();
    paths.insert("manifest.json".to_string());
    for sheet in &plan.sheets {
        paths.insert(sheet.relative_path.clone());
        paths.insert(sheet.aseprite_json_path.clone());
    }
    for frame in &plan.frame_outputs {
        paths.insert(frame.relative_path.clone());
    }
    paths
}

fn bundle_artifacts(out_dir: &Path, plan: &BundlePlan) -> PpResult<Vec<BundleArtifact>> {
    let mut artifacts = Vec::with_capacity(1 + plan.sheets.len() * 2 + plan.frame_outputs.len());
    for sheet in &plan.sheets {
        artifacts.push(BundleArtifact {
            relative_path: sheet.relative_path.clone(),
            bytes: PngEncoder::encode_rgba(&sheet.image)?,
        });
        artifacts.push(BundleArtifact {
            relative_path: sheet.aseprite_json_path.clone(),
            bytes: sheet.aseprite_json.as_bytes().to_vec(),
        });
    }
    artifacts.push(BundleArtifact {
        relative_path: "manifest.json".to_string(),
        bytes: serde_json::to_vec_pretty(&plan.manifest).map_err(|source| PpError::Json {
            path: out_dir.join("manifest.json"),
            message: source.to_string(),
        })?,
    });
    for frame in &plan.frame_outputs {
        artifacts.push(BundleArtifact {
            relative_path: frame.relative_path.clone(),
            bytes: PngEncoder::encode_rgba(&frame.image)?,
        });
    }
    Ok(artifacts)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct NormalizeSummary {
    ok: bool,
    out_dir: String,
    character: String,
    states: usize,
    frames: usize,
    failed_gates: usize,
    files: Vec<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct BundleSummary {
    ok: bool,
    out_dir: String,
    character: String,
    animations: usize,
    sheets: usize,
    files: Vec<String>,
}
