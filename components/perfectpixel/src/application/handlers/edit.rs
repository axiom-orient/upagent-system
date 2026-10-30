use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::{
    apply_raster_edits_with_evidence, AtomicFileWriter, FilePrecondition, PngEncoder, PpError,
    PpResult,
};

use super::super::{
    path::{reject_same_path, validate_file_extension, validate_raster_input_path},
    shared::{decode_raster_snapshot, read_json_request_snapshot, serialize_json},
};

const EDIT_SCHEMA: &str = "perfectpixel.image-edit/2";

pub(crate) fn execute(request_path: PathBuf) -> PpResult<String> {
    validate_file_extension(&request_path, &["json"], "edit request")?;
    let (request, _request_snapshot): (EditRequest, _) = read_json_request_snapshot(&request_path)?;
    if request.schema_version != 1 || request.operation != "edit" {
        return Err(PpError::InvalidRequest(
            "edit request schemaVersion must be 1 and operation must be 'edit'".to_string(),
        ));
    }
    if request.steps.is_empty() || request.steps.len() > 64 {
        return Err(PpError::InvalidRequest(
            "edit request steps must contain 1..=64 entries".to_string(),
        ));
    }

    let base_dir = request_path.parent().unwrap_or_else(|| Path::new("."));
    let input = resolve_path(base_dir, &request.input, "input")?;
    let output = resolve_path(base_dir, &request.output, "output")?;
    validate_raster_input_path(&input)?;
    validate_file_extension(&output, &["png"], "edit output")?;
    reject_same_path(&input, &output, "edit input and output must not collide")?;
    reject_same_path(
        &request_path,
        &output,
        "edit output must not overwrite its request",
    )?;

    // Output revision is captured before decode and edit work. Publication must observe this exact
    // revision again immediately before rename, otherwise the operation fails closed.
    let output_precondition = FilePrecondition::capture(&output)?;
    let (source, _input_snapshot) = decode_raster_snapshot(&input)?;
    let edits = request
        .steps
        .iter()
        .map(EditStep::to_raster_edit)
        .collect::<Vec<_>>();
    let (result, auto_plans) = apply_raster_edits_with_evidence(&source, &edits)?;
    let encoded = PngEncoder::encode_rgba(&result)?;
    let output_artifact = crate::ArtifactRef::from_bytes("image/png", &encoded)?;
    AtomicFileWriter::write_bytes_checked(&output, &output_precondition, &encoded)?;

    serialize_json(
        &EditSummary {
            schema: EDIT_SCHEMA,
            ok: true,
            operation: "edit",
            input: input.display().to_string(),
            output: output.display().to_string(),
            input_width: source.width(),
            input_height: source.height(),
            output_width: result.width(),
            output_height: result.height(),
            steps: request.steps.iter().map(EditStep::evidence_name).collect(),
            output_artifact,
            bytes_written: u64::try_from(encoded.len()).map_err(|_| {
                PpError::InvalidRequest("edit output byte count overflow".to_string())
            })?,
            auto_background: auto_plans
                .into_iter()
                .map(|plan| AutoBackgroundEvidence {
                    selected_keys: plan.selected_keys,
                    edge_coverage_basis_points: plan.edge_coverage_basis_points,
                })
                .collect(),
        },
        "<edit-summary>",
    )
}

fn resolve_path(base_dir: &Path, value: &str, label: &str) -> PpResult<PathBuf> {
    if value.trim().is_empty() || value != value.trim() || value.contains('\0') {
        return Err(PpError::InvalidRequest(format!(
            "edit {label} path must be a non-empty printable path"
        )));
    }
    let path = Path::new(value);
    Ok(if path.is_absolute() {
        path.to_path_buf()
    } else {
        base_dir.join(path)
    })
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct EditRequest {
    schema_version: u32,
    operation: String,
    input: String,
    output: String,
    steps: Vec<EditStep>,
}

#[derive(Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
enum EditStep {
    Crop {
        x: u32,
        y: u32,
        width: u32,
        height: u32,
    },
    Rotate {
        #[serde(rename = "quarterTurns")]
        quarter_turns: u8,
    },
    Flip {
        axis: EditFlipAxis,
    },
    Resize {
        width: u32,
        height: u32,
        filter: EditFilter,
    },
    RemoveBackground {
        keys: Vec<[u8; 3]>,
        tolerance: u8,
        feather: u8,
    },
    RemoveBackgroundAuto {
        #[serde(rename = "maxKeys")]
        max_keys: u8,
        #[serde(rename = "minEdgeCoverageBasisPoints")]
        min_edge_coverage_basis_points: u16,
        tolerance: u8,
        feather: u8,
    },
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum EditFlipAxis {
    Horizontal,
    Vertical,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum EditFilter {
    Nearest,
    Lanczos3,
}

impl EditStep {
    fn to_raster_edit(&self) -> crate::RasterEdit {
        match self {
            Self::Crop {
                x,
                y,
                width,
                height,
            } => crate::RasterEdit::Crop {
                x: *x,
                y: *y,
                width: *width,
                height: *height,
            },
            Self::Rotate { quarter_turns } => crate::RasterEdit::RotateQuarterTurns {
                quarter_turns: *quarter_turns,
            },
            Self::Flip { axis } => match axis {
                EditFlipAxis::Horizontal => crate::RasterEdit::FlipHorizontal,
                EditFlipAxis::Vertical => crate::RasterEdit::FlipVertical,
            },
            Self::Resize {
                width,
                height,
                filter,
            } => crate::RasterEdit::Resize {
                width: *width,
                height: *height,
                filter: match filter {
                    EditFilter::Nearest => crate::ResampleFilter::Nearest,
                    EditFilter::Lanczos3 => crate::ResampleFilter::Lanczos3,
                },
            },
            Self::RemoveBackground {
                keys,
                tolerance,
                feather,
            } => crate::RasterEdit::RemoveBackground {
                keys: keys.clone(),
                tolerance: *tolerance,
                feather: *feather,
            },
            Self::RemoveBackgroundAuto {
                max_keys,
                min_edge_coverage_basis_points,
                tolerance,
                feather,
            } => crate::RasterEdit::RemoveBackgroundAuto {
                max_keys: *max_keys,
                min_edge_coverage_basis_points: *min_edge_coverage_basis_points,
                tolerance: *tolerance,
                feather: *feather,
            },
        }
    }

    fn evidence_name(&self) -> &'static str {
        match self {
            Self::Crop { .. } => "crop",
            Self::Rotate { .. } => "rotate",
            Self::Flip { .. } => "flip",
            Self::Resize { .. } => "resize",
            Self::RemoveBackground { .. } => "remove_background",
            Self::RemoveBackgroundAuto { .. } => "remove_background_auto",
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct EditSummary {
    schema: &'static str,
    ok: bool,
    operation: &'static str,
    input: String,
    output: String,
    input_width: u32,
    input_height: u32,
    output_width: u32,
    output_height: u32,
    steps: Vec<&'static str>,
    output_artifact: crate::ArtifactRef,
    bytes_written: u64,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    auto_background: Vec<AutoBackgroundEvidence>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct AutoBackgroundEvidence {
    selected_keys: Vec<[u8; 3]>,
    edge_coverage_basis_points: u16,
}
