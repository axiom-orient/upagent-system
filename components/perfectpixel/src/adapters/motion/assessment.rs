use serde::{Deserialize, Serialize};

use super::{aggregate_path_data_bytes, parse_scene_from_ir, TransformPolicy};
use crate::core::SvgContract;

pub(super) use crate::core::sha256::sha256_hex;
/// Maximum source SVG size accepted by the opt-in structural assessment.
pub const MOTION_ASSESSMENT_MAX_SOURCE_BYTES: usize = 1_048_576;
/// Maximum number of paths assessed and scaffolded by the shared motion path cap.
pub const MOTION_ASSESSMENT_MAX_PATHS: usize = super::MOTION_MAX_SCAFFOLD_PATHS;
/// Maximum aggregate path-data size accepted by the opt-in structural assessment.
pub const MOTION_ASSESSMENT_MAX_GEOMETRY_BYTES: usize = 524_288;
/// Version of the motion assessment binding contract.
pub const MOTION_ASSESSMENT_CONTRACT_VERSION: &str = "perfectpixel.motion-assessment/1";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum MotionAssessmentStatus {
    NotEvaluated,
    Passed,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum MotionAssessmentNotEvaluatedReason {
    Unrequested,
    PrerequisiteUnavailable,
    ProfileNotPromoted,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MotionAssessmentRequest {
    /// Assessment is opt-in; false never parses or scaffolds the source.
    pub requested: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MotionAssessmentBinding {
    pub source_scene_sha256: String,
    pub scaffold_sha256: String,
    pub assessed_scene_sha256: String,
    pub motion_contract_version: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MotionAssessmentEvidence {
    pub messages: Vec<String>,
    pub binding: Option<MotionAssessmentBinding>,
}

/// Non-forgeable motion readiness evidence. Only the crate's authorized vector route can
/// construct an assessment that carries approved scene bytes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MotionAssessment {
    pub(crate) status: MotionAssessmentStatus,
    not_evaluated_reason: Option<MotionAssessmentNotEvaluatedReason>,
    evidence: MotionAssessmentEvidence,
    #[serde(skip_serializing)]
    approved_motion_output_bytes: Option<Vec<u8>>,
}

impl MotionAssessment {
    pub fn status(&self) -> MotionAssessmentStatus {
        self.status.clone()
    }

    pub fn not_evaluated_reason(&self) -> Option<MotionAssessmentNotEvaluatedReason> {
        self.not_evaluated_reason.clone()
    }

    pub fn evidence(&self) -> &MotionAssessmentEvidence {
        &self.evidence
    }

    pub(crate) fn approved_motion_output_bytes(&self) -> Option<&[u8]> {
        self.approved_motion_output_bytes.as_deref()
    }
}

/// External requests are intentionally incapable of granting promotion authority.
/// Only the Vectorizer's embedded-route flow can call the crate-private assessor.
pub fn assess_motion_structure(request: &MotionAssessmentRequest) -> MotionAssessment {
    if !request.requested {
        return not_evaluated(MotionAssessmentNotEvaluatedReason::Unrequested);
    }
    not_evaluated(MotionAssessmentNotEvaluatedReason::PrerequisiteUnavailable)
}

pub(crate) fn assess_authorized_motion_structure(
    requested: bool,
    profile_promoted: bool,
    source: &[u8],
) -> MotionAssessment {
    if !requested {
        return not_evaluated(MotionAssessmentNotEvaluatedReason::Unrequested);
    }
    if !profile_promoted {
        return not_evaluated(MotionAssessmentNotEvaluatedReason::ProfileNotPromoted);
    }
    if source.len() > MOTION_ASSESSMENT_MAX_SOURCE_BYTES {
        return failed(
            vec![format!(
                "source scene exceeds {MOTION_ASSESSMENT_MAX_SOURCE_BYTES} byte assessment limit"
            )],
            None,
        );
    }

    let source_hash = sha256_hex(source);
    let binding = |scaffold: Option<&[u8]>, assessed: Option<&[u8]>| MotionAssessmentBinding {
        source_scene_sha256: source_hash.clone(),
        scaffold_sha256: scaffold.map(sha256_hex).unwrap_or_default(),
        assessed_scene_sha256: assessed.map(sha256_hex).unwrap_or_default(),
        motion_contract_version: MOTION_ASSESSMENT_CONTRACT_VERSION.to_string(),
    };
    let source_svg = match std::str::from_utf8(source) {
        Ok(svg) => svg,
        Err(_) => {
            return failed(
                vec!["source scene is not UTF-8 SVG bytes".to_string()],
                Some(binding(None, None)),
            );
        }
    };
    let (source_report, source_ir) = match SvgContract::parse(source_svg) {
        Ok(parsed) => parsed,
        Err(error) => {
            return failed(
                vec![format!("neutral shared SVG IR rejected source: {error}")],
                Some(binding(None, None)),
            );
        }
    };
    let path_count = source_ir
        .elements
        .iter()
        .filter(|element| element.local_name == "path")
        .count();
    if path_count > MOTION_ASSESSMENT_MAX_PATHS {
        return failed(
            vec![format!(
                "source scene exceeds {MOTION_ASSESSMENT_MAX_PATHS} path assessment limit"
            )],
            Some(binding(None, None)),
        );
    }
    let geometry_bytes = aggregate_path_data_bytes(&source_ir);
    if geometry_bytes > MOTION_ASSESSMENT_MAX_GEOMETRY_BYTES {
        return failed(
            vec![format!(
                "source scene exceeds {MOTION_ASSESSMENT_MAX_GEOMETRY_BYTES} geometry byte assessment limit"
            )],
            Some(binding(None, None)),
        );
    }
    if let Err(error) = parse_scene_from_ir(
        source_svg,
        source_report,
        source_ir,
        false,
        TransformPolicy::ScaffoldInput,
    ) {
        return failed(
            vec![format!("motion scene extraction rejected source: {error}")],
            Some(binding(None, None)),
        );
    }

    let scaffold = match super::MotionCompiler::scaffold(source_svg) {
        Ok(scaffold) => scaffold,
        Err(error) => {
            return failed(
                vec![format!(
                    "motion scaffold structural preflight rejected source: {error}"
                )],
                Some(binding(None, None)),
            );
        }
    };
    let assessed = scaffold.scene_svg.into_bytes();
    let assessed_svg = std::str::from_utf8(&assessed).expect("motion scaffold emits UTF-8 text");
    let (report, ir) = match SvgContract::parse(assessed_svg) {
        Ok(parsed) => parsed,
        Err(error) => {
            return failed(
                vec![format!(
                    "motion scaffold scene reparse rejected bytes: {error}"
                )],
                Some(binding(Some(&assessed), None)),
            );
        }
    };
    if let Err(error) =
        parse_scene_from_ir(assessed_svg, report, ir, false, TransformPolicy::BuildInput)
    {
        return failed(
            vec![format!(
                "motion scaffold scene binding rejected bytes: {error}"
            )],
            Some(binding(Some(&assessed), None)),
        );
    }
    let completed_binding = binding(Some(&assessed), Some(&assessed));
    MotionAssessment {
        status: MotionAssessmentStatus::Passed,
        not_evaluated_reason: None,
        evidence: MotionAssessmentEvidence {
            messages: vec![
                "neutral shared SVG IR parsed with bounded geometry".to_string(),
                "motion scaffold structural preflight and reparse succeeded".to_string(),
                "approved motion output bytes are exactly the reparsed scaffold scene bytes"
                    .to_string(),
            ],
            binding: Some(completed_binding),
        },
        approved_motion_output_bytes: Some(assessed),
    }
}

fn not_evaluated(reason: MotionAssessmentNotEvaluatedReason) -> MotionAssessment {
    MotionAssessment {
        status: MotionAssessmentStatus::NotEvaluated,
        not_evaluated_reason: Some(reason),
        evidence: MotionAssessmentEvidence {
            messages: Vec::new(),
            binding: None,
        },
        approved_motion_output_bytes: None,
    }
}

fn failed(messages: Vec<String>, binding: Option<MotionAssessmentBinding>) -> MotionAssessment {
    MotionAssessment {
        status: MotionAssessmentStatus::Failed,
        not_evaluated_reason: None,
        evidence: MotionAssessmentEvidence { messages, binding },
        approved_motion_output_bytes: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SVG: &[u8] = br#"<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16" viewBox="0 0 16 16"><path id="pp-path-0001" d="M0 0 L16 0 L16 16 Z"/></svg>"#;
    const ARC_SVG: &[u8] = br#"<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16"><path id="pp-path-0001" d="M0 0 A4 4 0 0 1 8 8"/></svg>"#;

    #[test]
    fn authorized_assessment_binds_the_reparsed_scaffold_scene_bytes() {
        let original = assess_authorized_motion_structure(true, true, SVG);
        let mut mutated = SVG.to_vec();
        mutated.insert(mutated.len() - "</svg>".len(), b' ');
        let changed = assess_authorized_motion_structure(true, true, &mutated);

        assert_eq!(original.status(), MotionAssessmentStatus::Passed);
        assert_eq!(changed.status(), MotionAssessmentStatus::Passed);
        assert_eq!(original.approved_motion_output_bytes(), Some(SVG));
        assert_eq!(
            changed.approved_motion_output_bytes(),
            Some(mutated.as_slice())
        );
        assert_ne!(
            original
                .evidence()
                .binding
                .as_ref()
                .unwrap()
                .source_scene_sha256,
            changed
                .evidence()
                .binding
                .as_ref()
                .unwrap()
                .source_scene_sha256
        );
    }
    #[test]
    fn authorized_assessment_rejects_geometry_the_motion_exporter_cannot_emit() {
        let assessment = assess_authorized_motion_structure(true, true, ARC_SVG);
        assert_eq!(assessment.status(), MotionAssessmentStatus::Failed);
        assert!(assessment.approved_motion_output_bytes().is_none());
    }
}
