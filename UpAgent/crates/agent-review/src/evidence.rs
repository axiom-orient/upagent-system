use std::collections::{BTreeMap, BTreeSet};

use super::{
    CandidateFinding, ConsensusVerdict, DiscoveryReport, EvidenceVerdict, FactCheckRecord,
    FactCheckReport, FindingConsensus, LaneRecipe, LaneRecord, LaneReport, LaneStatus,
    MAX_FINDINGS, MAX_LANE_OUTPUT_BYTES, ReviewError, ReviewPhase, bound_error, bound_text,
    deterministic_child_id, sha256, validate_text,
};

// A lane parse always returns terminal evidence: malformed model JSON is a
// lane failure, not an orchestration/storage error.
#[allow(clippy::unnecessary_wraps)]
pub(super) fn parse_lane_output(
    lane: LaneRecipe,
    input_digest: String,
    recipe_digest: &str,
    raw: &str,
    candidates: Option<&[CandidateFinding]>,
) -> Result<LaneRecord, ReviewError> {
    if raw.trim().is_empty() || raw.len() > MAX_LANE_OUTPUT_BYTES {
        return Ok(LaneRecord {
            lane,
            input_digest,
            recipe_digest: recipe_digest.to_owned(),
            status: LaneStatus::Failed,
            raw_output: Some(bound_text(raw, MAX_LANE_OUTPUT_BYTES)),
            output_digest: Some(sha256(raw.as_bytes())),
            report: None,
            error: Some("reviewer output is empty or exceeds its byte bound".to_owned()),
        });
    }
    let report = match lane.phase {
        ReviewPhase::Discovery => {
            let value: DiscoveryReport = match serde_json::from_str(raw) {
                Ok(value) => value,
                Err(error) => {
                    return Ok(failed_lane_record(
                        lane,
                        input_digest,
                        recipe_digest,
                        raw,
                        &format!("strict discovery report rejected: {error}"),
                    ));
                }
            };
            if let Err(error) = validate_discovery(&value) {
                return Ok(failed_lane_record(
                    lane,
                    input_digest,
                    recipe_digest,
                    raw,
                    &error.to_string(),
                ));
            }
            LaneReport::Discovery(value)
        }
        ReviewPhase::FactCheck => {
            let value: FactCheckReport = match serde_json::from_str(raw) {
                Ok(value) => value,
                Err(error) => {
                    return Ok(failed_lane_record(
                        lane,
                        input_digest,
                        recipe_digest,
                        raw,
                        &format!("strict fact-check report rejected: {error}"),
                    ));
                }
            };
            if let Err(error) = validate_fact_check(&value, candidates.unwrap_or_default()) {
                return Ok(failed_lane_record(
                    lane,
                    input_digest,
                    recipe_digest,
                    raw,
                    &error.to_string(),
                ));
            }
            let validator_route_id = lane.route_id.clone();
            let mut value = value;
            for check in &mut value.checks {
                check.validator_route_id = Some(validator_route_id.clone());
            }
            value
                .checks
                .sort_by(|left, right| left.finding_id.cmp(&right.finding_id));
            LaneReport::FactCheck(value)
        }
    };
    Ok(LaneRecord {
        lane,
        input_digest,
        recipe_digest: recipe_digest.to_owned(),
        status: LaneStatus::Succeeded,
        raw_output: Some(raw.to_owned()),
        output_digest: Some(sha256(raw.as_bytes())),
        report: Some(report),
        error: None,
    })
}

fn failed_lane_record(
    lane: LaneRecipe,
    input_digest: String,
    recipe_digest: &str,
    raw: &str,
    error: &str,
) -> LaneRecord {
    LaneRecord {
        lane,
        input_digest,
        recipe_digest: recipe_digest.to_owned(),
        status: LaneStatus::Failed,
        raw_output: Some(bound_text(raw, MAX_LANE_OUTPUT_BYTES)),
        output_digest: Some(sha256(raw.as_bytes())),
        report: None,
        error: Some(bound_error(error)),
    }
}

fn validate_discovery(report: &DiscoveryReport) -> Result<(), ReviewError> {
    validate_text(&report.summary, "discovery summary", 8 * 1024)?;
    if report.findings.len() > MAX_FINDINGS {
        return Err(ReviewError::Contract(
            "discovery findings exceed the bound".to_owned(),
        ));
    }
    for finding in &report.findings {
        validate_text(&finding.title, "finding title", 512)?;
        validate_text(&finding.problem, "finding problem", 8 * 1024)?;
        validate_text(&finding.impact, "finding impact", 8 * 1024)?;
        validate_text(&finding.evidence, "finding evidence", 16 * 1024)?;
        if let Some(location) = &finding.location {
            validate_text(location, "finding location", 2 * 1024)?;
        }
    }
    Ok(())
}

fn validate_fact_check(
    report: &FactCheckReport,
    candidates: &[CandidateFinding],
) -> Result<(), ReviewError> {
    if report.checks.len() != candidates.len() {
        return Err(ReviewError::Contract(
            "fact-check report must contain exactly one check per candidate".to_owned(),
        ));
    }
    let expected = candidates
        .iter()
        .map(|candidate| candidate.id.as_str())
        .collect::<BTreeSet<_>>();
    let actual = report
        .checks
        .iter()
        .map(|check| check.finding_id.as_str())
        .collect::<BTreeSet<_>>();
    if expected != actual || actual.len() != report.checks.len() {
        return Err(ReviewError::Contract(
            "fact-check candidate identifiers are not an exact set".to_owned(),
        ));
    }
    for check in &report.checks {
        if check.validator_route_id.is_some() {
            return Err(ReviewError::Contract(
                "fact-check validator provenance is host-owned".to_owned(),
            ));
        }
        validate_text(&check.evidence, "fact-check evidence", 16 * 1024)?;
        validate_text(&check.rationale, "fact-check rationale", 16 * 1024)?;
    }
    Ok(())
}

pub(super) fn freeze_candidates(
    discovery: &[LaneRecord],
    maximum: usize,
) -> Result<Vec<CandidateFinding>, ReviewError> {
    let mut output = Vec::new();
    for lane in discovery {
        let Some(LaneReport::Discovery(report)) = &lane.report else {
            return Err(ReviewError::Contract(
                "successful discovery lane has no discovery report".to_owned(),
            ));
        };
        for (index, finding) in report.findings.iter().enumerate() {
            if output.len() >= maximum {
                return Err(ReviewError::Contract(
                    "frozen candidate set exceeds maximumFindings".to_owned(),
                ));
            }
            let canonical = serde_json::to_string(&(lane.lane.lane_id.as_str(), index, finding))
                .map_err(|error| ReviewError::Json(error.to_string()))?;
            output.push(CandidateFinding {
                id: deterministic_child_id("finding", &lane.lane.lane_id, &canonical),
                source_lane_id: lane.lane.lane_id.clone(),
                source_route_id: lane.lane.route_id.clone(),
                source_role: lane.lane.role,
                title: finding.title.clone(),
                problem: finding.problem.clone(),
                impact: finding.impact.clone(),
                evidence: finding.evidence.clone(),
                location: finding.location.clone(),
                severity: finding.severity,
            });
        }
    }
    output.sort_by(|left, right| left.id.cmp(&right.id));
    Ok(output)
}

pub(super) fn reduce_consensus(
    candidates: &[CandidateFinding],
    lanes: &[LaneRecord],
    required_confirmations: usize,
) -> Result<Vec<FindingConsensus>, ReviewError> {
    let mut checks_by_id: BTreeMap<String, Vec<FactCheckRecord>> = BTreeMap::new();
    for lane in lanes {
        let Some(LaneReport::FactCheck(report)) = &lane.report else {
            continue;
        };
        for check in &report.checks {
            checks_by_id
                .entry(check.finding_id.clone())
                .or_default()
                .push(check.clone());
        }
    }
    let mut output = Vec::with_capacity(candidates.len());
    for candidate in candidates {
        let checks = checks_by_id.remove(&candidate.id).unwrap_or_default();
        if checks.is_empty() {
            return Err(ReviewError::Contract(format!(
                "candidate {} has no fact-check evidence",
                candidate.id
            )));
        }
        if checks
            .iter()
            .any(|check| check.validator_route_id.is_none())
        {
            return Err(ReviewError::Contract(
                "fact-check evidence is missing host validator provenance".to_owned(),
            ));
        }
        let eligible = checks
            .iter()
            .filter(|check| {
                check
                    .validator_route_id
                    .as_deref()
                    .is_some_and(|route| route != candidate.source_route_id)
            })
            .collect::<Vec<_>>();
        let has_refutation = eligible
            .iter()
            .any(|check| check.verdict == EvidenceVerdict::Refuted);
        let confirmation_routes = eligible
            .iter()
            .filter(|check| check.verdict == EvidenceVerdict::Confirmed)
            .filter_map(|check| check.validator_route_id.as_deref())
            .collect::<BTreeSet<_>>();
        let verdict = if has_refutation {
            ConsensusVerdict::Disputed
        } else if confirmation_routes.len() >= required_confirmations {
            ConsensusVerdict::Confirmed
        } else {
            ConsensusVerdict::Indeterminate
        };
        output.push(FindingConsensus {
            candidate: candidate.clone(),
            verdict,
            checks,
        });
    }
    if let Some((unknown, _)) = checks_by_id.first_key_value() {
        return Err(ReviewError::Contract(format!(
            "fact-check evidence references unknown candidate {unknown}"
        )));
    }
    Ok(output)
}
