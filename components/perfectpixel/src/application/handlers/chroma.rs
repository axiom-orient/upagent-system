use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::{PpError, PpResult};

use super::super::{
    path::validate_file_extension,
    shared::{read_json_request_snapshot, serialize_json},
};

const CHROMA_PLAN_OPERATION: &str = "chroma_plan";

pub(crate) fn plan(request_path: PathBuf) -> PpResult<String> {
    validate_file_extension(&request_path, &["json"], "chroma plan request")?;
    let (request, _snapshot): (ChromaPlanRequest, _) = read_json_request_snapshot(&request_path)?;
    if request.schema_version != 1 || request.operation != CHROMA_PLAN_OPERATION {
        return Err(PpError::InvalidRequest(
            "chroma plan request schemaVersion must be 1 and operation must be 'chroma_plan'"
                .to_string(),
        ));
    }

    let plan = crate::plan_chroma(&request.subject_rgb_colors)?;
    let candidate_scores = plan
        .candidates
        .iter()
        .map(|candidate| ChromaCandidateScorePayload {
            rgb: candidate.rgb,
            hex: rgb_hex(candidate.rgb),
            score: candidate.min_distance,
        })
        .collect();

    serialize_json(
        &ChromaPlanResponse {
            schema: crate::CHROMA_PLAN_SCHEMA,
            schema_version: 1,
            ok: true,
            operation: CHROMA_PLAN_OPERATION,
            subject_rgb_colors: request.subject_rgb_colors,
            metric: crate::CHROMA_PLAN_METRIC,
            selected_rgb: plan.selected_rgb,
            selected_hex: rgb_hex(plan.selected_rgb),
            min_distance: plan.min_distance,
            candidate_scores,
        },
        "<chroma-plan>",
    )
}

fn rgb_hex(rgb: [u8; 3]) -> String {
    format!("#{:02X}{:02X}{:02X}", rgb[0], rgb[1], rgb[2])
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ChromaPlanRequest {
    schema_version: u32,
    operation: String,
    subject_rgb_colors: Vec<[u8; 3]>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ChromaPlanResponse {
    schema: &'static str,
    schema_version: u32,
    ok: bool,
    operation: &'static str,
    subject_rgb_colors: Vec<[u8; 3]>,
    metric: &'static str,
    selected_rgb: [u8; 3],
    selected_hex: String,
    min_distance: f64,
    candidate_scores: Vec<ChromaCandidateScorePayload>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ChromaCandidateScorePayload {
    rgb: [u8; 3],
    hex: String,
    score: f64,
}
