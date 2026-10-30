use std::collections::BTreeMap;
use std::fmt;

use crate::Raster;

pub(crate) use crate::core::sha256::sha256_hex;

use super::report::{DiagnosticArtifactSet, EvaluationReport, PredicateAvailability};

/// Immutable identities that bind an evaluated SVG to the authority used to produce it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CandidateDigests {
    backend: String,
    backend_id: String,
    backend_version: String,
    backend_source: String,
    route: String,
    threshold_bundle: String,
    foundation: String,
    registry: String,
    report: String,
    profile: String,
    normalized_request: String,
    motion: Option<String>,
}

impl CandidateDigests {
    // Parameters are exactly this value type's fields and the body is a pure move, so
    // clippy's argument-count heuristic does not apply: grouping them would mean a second
    // struct with identical fields. Fields stay private so construction runs through here.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        backend: String,
        backend_id: String,
        backend_version: String,
        backend_source: String,
        route: String,
        threshold_bundle: String,
        foundation: String,
        registry: String,
        report: String,
        profile: String,
        normalized_request: String,
        motion: Option<String>,
    ) -> Result<Self, CandidateConstructionError> {
        let digests = Self {
            backend,
            backend_id,
            backend_version,
            backend_source,
            route,
            threshold_bundle,
            foundation,
            registry,
            report,
            profile,
            normalized_request,
            motion,
        };
        digests.validate()?;
        Ok(digests)
    }

    fn validate(&self) -> Result<(), CandidateConstructionError> {
        for (name, value) in [
            ("backend digest", &self.backend),
            ("route digest", &self.route),
            ("threshold bundle digest", &self.threshold_bundle),
            ("foundation digest", &self.foundation),
            ("report digest", &self.report),
            ("profile digest", &self.profile),
            ("normalized request digest", &self.normalized_request),
            ("registry digest", &self.registry),
        ] {
            require_digest(name, value)?;
        }
        if self.backend_id.trim().is_empty() || self.backend_version.trim().is_empty() {
            return Err(CandidateConstructionError::IncompleteFacts(
                "backend identity",
            ));
        }
        require_digest("backend source digest", &self.backend_source)?;
        if super::backends::backend_source_digest(&self.backend_id, &self.backend_version)
            .as_deref()
            != Some(self.backend_source.as_str())
        {
            return Err(CandidateConstructionError::BackendLifecycleMismatch);
        }
        if let Some(motion) = &self.motion {
            require_digest("motion digest", motion)?;
        }
        Ok(())
    }
}

/// Digest-bound evidence produced by request normalization.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NormalizedRequestEvidence {
    input: String,
    request: String,
    policy: String,
}

impl NormalizedRequestEvidence {
    pub(crate) fn new(
        input: String,
        normalized: &super::policy::NormalizedRequest,
        policy: String,
    ) -> Result<Self, CandidateConstructionError> {
        let request = sha256_identity(
            &serde_json::to_vec(normalized).expect("normalized request is serializable"),
        );
        let evidence = Self {
            input,
            request,
            policy,
        };
        require_digest("input digest", &evidence.input)?;
        require_digest("normalized request digest", &evidence.request)?;
        require_digest("policy digest", &evidence.policy)?;
        Ok(evidence)
    }
}

/// Shared SVG facts and IR identity for the exact rendered candidate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SvgCandidateFacts {
    svg_ir: String,
    facts: String,
    source_svg_sha256: String,
    source: String,
}

impl SvgCandidateFacts {
    pub(crate) fn new(
        svg_ir: String,
        facts: String,
        source: String,
    ) -> Result<Self, CandidateConstructionError> {
        let facts = Self {
            svg_ir,
            facts,
            source_svg_sha256: sha256_identity(source.as_bytes()),
            source,
        };
        require_digest("SVG IR digest", &facts.svg_ir)?;
        require_digest("SVG facts digest", &facts.facts)?;
        require_digest("SVG facts source digest", &facts.source_svg_sha256)?;
        if facts.svg_ir != sha256_identity(facts.source.as_bytes()) {
            return Err(CandidateConstructionError::IncompleteFacts(
                "canonical SVG IR identity",
            ));
        }
        Ok(facts)
    }
}

/// Results for each gate family that universally protects publication.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GateEvidence {
    fixed: BTreeMap<String, PredicateAvailability>,
    protected: BTreeMap<String, PredicateAvailability>,
    calibrated: BTreeMap<String, PredicateAvailability>,
    resource: BTreeMap<String, PredicateAvailability>,
    output_profile: BTreeMap<String, PredicateAvailability>,
}

impl GateEvidence {
    pub(crate) fn new(
        fixed: BTreeMap<String, PredicateAvailability>,
        protected: BTreeMap<String, PredicateAvailability>,
        calibrated: BTreeMap<String, PredicateAvailability>,
        resource: BTreeMap<String, PredicateAvailability>,
        output_profile: BTreeMap<String, PredicateAvailability>,
    ) -> Result<Self, CandidateConstructionError> {
        let gates = Self {
            fixed,
            protected,
            calibrated,
            resource,
            output_profile,
        };
        gates.validate_complete()?;
        Ok(gates)
    }

    fn validate_complete(&self) -> Result<(), CandidateConstructionError> {
        Self::require_exact_gate_keys(
            &self.fixed,
            &["security", "unsupportedContent"],
            "fixed gates",
        )?;
        Self::require_exact_gate_keys(
            &self.protected,
            &[
                "alpha",
                "edges",
                "endpoints",
                "features",
                "interiorTranslucency",
                "junctions",
                "palette",
                "topology",
            ],
            "protected gates",
        )?;
        Self::require_exact_gate_keys(
            &self.calibrated,
            &[
                "localLumaSsim",
                "qualityLoss",
                "qualityScore",
                "worstBlockLumaSsim",
            ],
            "calibrated gates",
        )?;
        Self::require_exact_gate_keys(
            &self.resource,
            &["distinctColors", "inputPixels", "paths", "svgBytes"],
            "resource gates",
        )?;
        if self.output_profile.len() != 1
            || !matches!(
                self.output_profile.keys().next().map(String::as_str),
                Some("compact" | "motionStructure")
            )
        {
            return Err(CandidateConstructionError::IncompleteFacts(
                "output-profile gates",
            ));
        }
        Ok(())
    }
    fn require_exact_gate_keys(
        gates: &BTreeMap<String, PredicateAvailability>,
        expected: &[&str],
        family: &'static str,
    ) -> Result<(), CandidateConstructionError> {
        if gates
            .keys()
            .map(String::as_str)
            .eq(expected.iter().copied())
        {
            Ok(())
        } else {
            Err(CandidateConstructionError::IncompleteFacts(family))
        }
    }

    pub(crate) fn all_passed(&self) -> bool {
        [
            &self.fixed,
            &self.protected,
            &self.calibrated,
            &self.resource,
            &self.output_profile,
        ]
        .into_iter()
        .all(|gates| {
            gates
                .values()
                .all(|result| *result == PredicateAvailability::Passed)
        })
    }

    pub(crate) fn failed_families(&self) -> impl Iterator<Item = GateFamily> + '_ {
        [
            (GateFamily::Fixed, &self.fixed),
            (GateFamily::Protected, &self.protected),
            (GateFamily::Calibrated, &self.calibrated),
            (GateFamily::Resource, &self.resource),
            (GateFamily::OutputProfile, &self.output_profile),
        ]
        .into_iter()
        .filter_map(|(family, gates)| {
            (!gates
                .values()
                .all(|result| *result == PredicateAvailability::Passed))
            .then_some(family)
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GateFamily {
    Fixed,
    Protected,
    Calibrated,
    Resource,
    OutputProfile,
}

/// Candidate-bound state. It cannot be cloned or reconstructed from a report.
#[derive(Debug)]
pub(crate) struct EvaluatedCandidate {
    svg_bytes: Vec<u8>,
    svg_sha256: String,
    _render_back: Raster,
    _svg_facts: SvgCandidateFacts,
    _request: NormalizedRequestEvidence,
    _digests: CandidateDigests,
    gates: GateEvidence,
    report: EvaluationReport,
    artifacts: DiagnosticArtifactSet,
    diagnostics: Vec<String>,
}

impl EvaluatedCandidate {
    // Parameters are exactly this value type's fields and the body is a pure move, so
    // clippy's argument-count heuristic does not apply: grouping them would mean a second
    // struct with identical fields. Fields stay private so construction runs through here.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        svg_bytes: Vec<u8>,
        expected_svg_sha256: String,
        render_back: Raster,
        svg_facts: SvgCandidateFacts,
        request: NormalizedRequestEvidence,
        digests: CandidateDigests,
        gates: GateEvidence,
        report: EvaluationReport,
        artifacts: DiagnosticArtifactSet,
        diagnostics: Vec<String>,
    ) -> Result<Self, CandidateConstructionError> {
        if svg_bytes.is_empty() {
            return Err(CandidateConstructionError::IncompleteFacts(
                "exact SVG bytes",
            ));
        }
        let svg_sha256 = sha256_identity(&svg_bytes);
        if expected_svg_sha256 != svg_sha256 {
            return Err(CandidateConstructionError::SvgDigestMismatch {
                expected: expected_svg_sha256,
                actual: svg_sha256,
            });
        }
        if svg_facts.source_svg_sha256 != expected_svg_sha256
            || svg_facts.source.as_bytes() != svg_bytes.as_slice()
        {
            return Err(CandidateConstructionError::SvgFactsNotBoundToCandidate);
        }
        let report_facts = report
            .candidate_facts()
            .ok_or(CandidateConstructionError::ReportNotBoundToCandidate)?;
        if report.digests().candidate_bytes() != Some(expected_svg_sha256.as_str())
            || report_facts.candidate_sha256() != expected_svg_sha256
            || report_facts.candidate_byte_count() != svg_bytes.len()
            || report_facts.render_back_sha256() != super::raster_digest(&render_back)
            || report_facts.dimensions() != (render_back.width(), render_back.height())
            || svg_facts.facts
                != sha256_identity(
                    &serde_json::to_vec(report_facts)
                        .expect("candidate facts contain only serializable values"),
                )
        {
            return Err(CandidateConstructionError::ReportNotBoundToCandidate);
        }
        let authority_mismatch = [
            (
                report.schema() != super::report::VECTOR_EVALUATION_SCHEMA,
                "report schema",
            ),
            (
                report.input_digest() != request.input.as_str(),
                "input digest",
            ),
            (
                report.digests().normalized_request() != Some(request.request.as_str())
                    || digests.normalized_request != request.request,
                "normalized request digest",
            ),
            (
                report.digests().policy() != request.policy.as_str(),
                "policy digest",
            ),
            (
                report.digests().backend() != Some(digests.backend.as_str()),
                "backend digest",
            ),
            (
                report.digests().route() != Some(digests.route.as_str()),
                "route digest",
            ),
            (
                report.digests().threshold_bundle() != Some(digests.threshold_bundle.as_str()),
                "threshold bundle digest",
            ),
            (
                report.digests().foundation() != digests.foundation.as_str(),
                "foundation digest",
            ),
            (
                report.digests().report() != digests.report.as_str(),
                "report digest",
            ),
            (
                report.digests().profile() != digests.profile.as_str(),
                "profile digest",
            ),
            (
                report.digests().motion() != digests.motion.as_deref(),
                "motion digest",
            ),
        ]
        .into_iter()
        .find_map(|(mismatch, name)| mismatch.then_some(name));
        if let Some(name) = authority_mismatch {
            return Err(CandidateConstructionError::ReportAuthorityMismatch(name));
        }
        if svg_facts.svg_ir != sha256_identity(svg_facts.source.as_bytes()) {
            return Err(CandidateConstructionError::SvgFactsNotBoundToCandidate);
        }
        if report.digests().registry() != digests.registry.as_str()
            || report
                .digests()
                .registry_entry()
                .filter(|entry| *entry == digests.route.as_str())
                .is_none()
        {
            return Err(CandidateConstructionError::RegistryProvenanceMismatch);
        }
        if !report.has_complete_constraints_and_rejection_metadata() {
            return Err(CandidateConstructionError::ReportConstraintsMismatch);
        }
        if !report.artifact_intent().matches_artifacts(&artifacts) {
            return Err(CandidateConstructionError::ReportArtifactsMismatch);
        }
        let report_profile_gates = report.output_profile_gates();
        let report_profile_keys_are_complete = report_profile_gates
            .keys()
            .map(String::as_str)
            .eq(["compact", "motionStructure"]);
        let selected_profile_matches = gates
            .output_profile
            .iter()
            .all(|(name, result)| report_profile_gates.get(name) == Some(result));
        let unselected_profile_is_not_evaluated =
            report_profile_gates.iter().all(|(name, result)| {
                gates.output_profile.contains_key(name)
                    || *result == PredicateAvailability::NotEvaluated
            });
        if report.fixed_gates() != &gates.fixed
            || report.protected_gates() != &gates.protected
            || report.calibrated_gates() != &gates.calibrated
            || report.resource_gates() != &gates.resource
            || !report_profile_keys_are_complete
            || !selected_profile_matches
            || !unselected_profile_is_not_evaluated
        {
            return Err(CandidateConstructionError::ReportGateMismatch);
        }
        if gates
            .output_profile
            .get("motionStructure")
            .is_some_and(|state| *state == PredicateAvailability::Passed)
            && (digests.motion.as_deref() != Some(expected_svg_sha256.as_str()))
        {
            return Err(CandidateConstructionError::ReportAuthorityMismatch(
                "passed motion candidate bytes",
            ));
        }
        let expected_decision = if gates.all_passed() {
            super::report::EvaluationDecision::Approved
        } else {
            super::report::EvaluationDecision::Rejected
        };
        if report.actual_decision() != expected_decision {
            return Err(CandidateConstructionError::ReportDecisionMismatch);
        }
        Ok(Self {
            svg_bytes,
            svg_sha256: expected_svg_sha256,
            _render_back: render_back,
            _svg_facts: svg_facts,
            _request: request,
            _digests: digests,
            gates,
            report,
            artifacts,
            diagnostics,
        })
    }

    /// Consumes candidate-bound state and releases exact bytes only through the verified branch.
    pub(crate) fn into_approval_transition(
        self,
    ) -> Result<ApprovalReadyCandidate, Box<RejectedCandidateParts>> {
        let digest_matches = sha256_identity(&self.svg_bytes) == self.svg_sha256;
        let failed_gates: Vec<_> = self.gates.failed_families().collect();
        let expected_decision = if failed_gates.is_empty() {
            super::report::EvaluationDecision::Approved
        } else {
            super::report::EvaluationDecision::Rejected
        };
        let report_is_consistent = self.report.actual_decision() == expected_decision;

        if digest_matches && report_is_consistent && failed_gates.is_empty() {
            return Ok(ApprovalReadyCandidate {
                svg_bytes: self.svg_bytes,
                svg_sha256: self.svg_sha256,
                report: self.report,
                artifacts: self.artifacts,
            });
        }

        Err(Box::new(RejectedCandidateParts {
            report: self.report,
            artifacts: self.artifacts,
            diagnostics: self.diagnostics,
            failed_gates,
            invariant_failure: !digest_matches || !report_is_consistent,
        }))
    }
}

pub(crate) struct ApprovalReadyCandidate {
    svg_bytes: Vec<u8>,
    svg_sha256: String,
    report: EvaluationReport,
    artifacts: DiagnosticArtifactSet,
}

impl ApprovalReadyCandidate {
    pub(crate) fn into_parts(self) -> (Vec<u8>, String, EvaluationReport, DiagnosticArtifactSet) {
        (self.svg_bytes, self.svg_sha256, self.report, self.artifacts)
    }
}

pub(crate) struct RejectedCandidateParts {
    report: EvaluationReport,
    artifacts: DiagnosticArtifactSet,
    diagnostics: Vec<String>,
    failed_gates: Vec<GateFamily>,
    invariant_failure: bool,
}

impl RejectedCandidateParts {
    pub(crate) fn into_parts(
        self,
    ) -> (
        EvaluationReport,
        DiagnosticArtifactSet,
        Vec<String>,
        Vec<GateFamily>,
        bool,
    ) {
        (
            self.report,
            self.artifacts,
            self.diagnostics,
            self.failed_gates,
            self.invariant_failure,
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CandidateConstructionError {
    IncompleteFacts(&'static str),
    InvalidDigest { name: &'static str, value: String },
    SvgDigestMismatch { expected: String, actual: String },
    SvgFactsNotBoundToCandidate,
    ReportNotBoundToCandidate,
    ReportAuthorityMismatch(&'static str),
    ReportGateMismatch,
    ReportDecisionMismatch,
    BackendLifecycleMismatch,
    RegistryProvenanceMismatch,
    ReportConstraintsMismatch,
    ReportArtifactsMismatch,
}

impl fmt::Display for CandidateConstructionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::IncompleteFacts(name) => {
                write!(formatter, "incomplete evaluated candidate facts: {name}")
            }
            Self::InvalidDigest { name, value } => write!(formatter, "invalid {name}: {value}"),
            Self::SvgDigestMismatch { expected, actual } => {
                write!(
                    formatter,
                    "SVG SHA-256 mismatch: expected {expected}, got {actual}"
                )
            }
            Self::SvgFactsNotBoundToCandidate => {
                formatter.write_str("SVG facts are not bound to the exact candidate bytes")
            }
            Self::ReportNotBoundToCandidate => {
                formatter.write_str("evaluation report is not bound to the exact candidate bytes")
            }
            Self::ReportAuthorityMismatch(name) => write!(
                formatter,
                "evaluation report does not match the candidate {name}"
            ),
            Self::ReportGateMismatch => {
                formatter.write_str("evaluation report gates do not match candidate gate evidence")
            }
            Self::ReportDecisionMismatch => formatter
                .write_str("evaluation report decision does not match candidate gate evidence"),
            Self::BackendLifecycleMismatch => {
                formatter.write_str("backend lifecycle provenance does not match its source digest")
            }
            Self::RegistryProvenanceMismatch => formatter
                .write_str("evaluation report registry provenance does not match its route"),
            Self::ReportConstraintsMismatch => formatter
                .write_str("evaluation report constraints or rejection metadata are incomplete"),
            Self::ReportArtifactsMismatch => formatter
                .write_str("evaluation report artifact intent does not match candidate artifacts"),
        }
    }
}

impl std::error::Error for CandidateConstructionError {}

fn require_digest(name: &'static str, value: &str) -> Result<(), CandidateConstructionError> {
    let Some(hex) = value.strip_prefix("sha256-") else {
        return Err(CandidateConstructionError::InvalidDigest {
            name,
            value: value.to_owned(),
        });
    };
    if hex.len() == 64
        && hex
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        Ok(())
    } else {
        Err(CandidateConstructionError::InvalidDigest {
            name,
            value: value.to_owned(),
        })
    }
}

pub(crate) fn sha256_identity(bytes: &[u8]) -> String {
    format!("sha256-{}", sha256_hex(bytes))
}
#[cfg(test)]
mod tests {
    use super::sha256_identity;

    #[test]
    fn sha256_identity_matches_known_answer() {
        assert_eq!(
            sha256_identity(b"abc"),
            "sha256-ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
}
