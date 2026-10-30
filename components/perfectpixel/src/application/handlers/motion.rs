use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::{MotionCompiler, MotionRequest, PpError, PpResult, MOTION_SCHEMA};

use super::super::{
    generation::GenerationWorkflow,
    generation_adapter::validate_input_snapshot_set,
    path::{managed_relative_path, validate_file_extension},
    shared::{
        publish_generated_artifacts, read_json_request_snapshot, read_utf8_snapshot,
        reject_generated_output_collisions, serialize_json, text_artifact, verify_request_digest,
        MAX_SVG_READ_BYTES,
    },
};

pub(crate) fn scaffold(input: PathBuf, out_dir: PathBuf) -> PpResult<String> {
    validate_file_extension(&input, &["svg"], "motion SVG input")?;
    let (source, input_snapshot) = read_utf8_snapshot(&input, MAX_SVG_READ_BYTES)?;
    validate_input_snapshot_set(std::slice::from_ref(&input_snapshot))?;

    let scaffold = MotionCompiler::scaffold(&source)?;
    let layers_json = serialize_json(&scaffold.layers, "<motion-layers>")?;
    let starter_request = MotionRequest {
        schema: MOTION_SCHEMA.to_string(),
        name: derived_motion_name(&input),
        source_svg: "scene.svg".to_string(),
        source_svg_sha256: MotionCompiler::scene_sha256(&scaffold.scene_svg),
        fps: 30,
        duration_ms: 1_000,
        looped: true,
        authored_paths: Vec::new(),
        parts: Vec::new(),
        tracks: Vec::new(),
        markers: Vec::new(),
    };
    let request_json = serialize_json(&starter_request, "<motion-request>")?;
    let files = vec![
        "scene.svg".to_string(),
        "layers.json".to_string(),
        "motion-request.json".to_string(),
        "layer-inspector.html".to_string(),
    ];
    reject_generated_output_collisions(&out_dir, &files, std::slice::from_ref(&input_snapshot))?;

    publish_generated_artifacts(
        &out_dir,
        GenerationWorkflow::MotionScaffold,
        vec![
            text_artifact("scene.svg", scaffold.scene_svg),
            text_artifact("layers.json", layers_json),
            text_artifact("motion-request.json", request_json),
            text_artifact("layer-inspector.html", scaffold.inspector_html),
        ],
        vec![GenerationWorkflow::MotionBuild],
        vec![input_snapshot],
    )?;

    serialize_json(
        &MotionScaffoldSummary {
            ok: true,
            out_dir: out_dir.display().to_string(),
            paths: scaffold.layers.layers.len(),
            files,
        },
        "<motion-scaffold-summary>",
    )
}

pub(crate) fn build(
    request_path: PathBuf,
    out_dir: PathBuf,
    request_sha256: Option<&str>,
) -> PpResult<String> {
    validate_file_extension(&request_path, &["json"], "motion request")?;
    let (request, request_snapshot): (MotionRequest, _) =
        read_json_request_snapshot(&request_path)?;
    verify_request_digest(&request_snapshot, request_sha256)?;
    let request_base = request_path.parent().unwrap_or_else(|| Path::new("."));
    let source_path = resolve_motion_source_path(request_base, &request.source_svg)?;
    let (source, source_snapshot) = read_utf8_snapshot(&source_path, MAX_SVG_READ_BYTES)?;
    let build = MotionCompiler::build(&source, &request)?;
    let report_json = serialize_json(&build.report, "<motion-report>")?;
    let animation_relative = format!("dotlottie/a/{}.json", request.name);
    let files = vec![
        "animated.svg".to_string(),
        "animation.json".to_string(),
        "motion-report.json".to_string(),
        "preview.html".to_string(),
        "dotlottie/manifest.json".to_string(),
        animation_relative.clone(),
    ];
    let motion_inputs = vec![request_snapshot, source_snapshot];
    validate_input_snapshot_set(&motion_inputs)?;
    reject_generated_output_collisions(&out_dir, &files, &motion_inputs)?;

    publish_generated_artifacts(
        &out_dir,
        GenerationWorkflow::MotionBuild,
        vec![
            text_artifact("animated.svg", build.animated_svg),
            text_artifact("animation.json", build.lottie_json.clone()),
            text_artifact("motion-report.json", report_json),
            text_artifact("preview.html", build.preview_html),
            text_artifact("dotlottie/manifest.json", build.dotlottie_manifest_json),
            text_artifact(&animation_relative, build.lottie_json),
        ],
        Vec::new(),
        motion_inputs,
    )?;

    serialize_json(
        &MotionBuildSummary {
            ok: true,
            out_dir: out_dir.display().to_string(),
            name: request.name,
            paths: build.report.path_count,
            parts: build.report.part_count,
            tracks: build.report.track_count,
            lottie_shapes: build.report.lottie_shape_count,
            dotlottie_archive_created: build.report.dotlottie_archive_created,
            files,
        },
        "<motion-build-summary>",
    )
}

fn resolve_motion_source_path(base_dir: &Path, value: &str) -> PpResult<PathBuf> {
    let relative = managed_relative_path(value).map_err(|_| {
        PpError::InvalidRequest(format!(
            "motion sourceSvg '{value}' must be a safe path relative to the request file"
        ))
    })?;
    validate_file_extension(&relative, &["svg"], "motion sourceSvg")?;
    Ok(base_dir.join(relative))
}

fn derived_motion_name(input: &Path) -> String {
    let stem = input
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or_default();
    let mut name = String::with_capacity(stem.len());
    let mut pending_hyphen = false;
    for byte in stem.bytes() {
        if byte.is_ascii_alphanumeric() {
            if pending_hyphen && !name.is_empty() {
                name.push('-');
            }
            name.push((byte as char).to_ascii_lowercase());
            pending_hyphen = false;
        } else {
            pending_hyphen = true;
        }
    }
    if name.is_empty() {
        "motion".to_string()
    } else {
        name
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct MotionScaffoldSummary {
    ok: bool,
    out_dir: String,
    paths: usize,
    files: Vec<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct MotionBuildSummary {
    ok: bool,
    out_dir: String,
    name: String,
    paths: usize,
    parts: usize,
    tracks: usize,
    lottie_shapes: usize,
    dotlottie_archive_created: bool,
    files: Vec<String>,
}
