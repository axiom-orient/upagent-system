use crate::core::{PpError, PpResult};
use std::fmt;

use super::candidate::{EvaluatedCandidate, GateFamily};
use super::outcome::{RejectedVectorOutput, VectorOutcome, VectorRejectionCode};
use super::report::{DiagnosticArtifactSet, EvaluationReport};

/// The only public capability that can expose final SVG bytes.
///
/// This type has no public constructor and deliberately does not implement `Deserialize`.
#[derive(PartialEq)]
pub struct ApprovedVectorOutput {
    svg_bytes: Vec<u8>,
    svg_sha256: String,
    report: EvaluationReport,
    artifacts: DiagnosticArtifactSet,
}

impl fmt::Debug for ApprovedVectorOutput {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ApprovedVectorOutput")
            .field("svg_sha256", &self.svg_sha256)
            .field("report", &self.report)
            .field("artifacts", &self.artifacts)
            .finish()
    }
}

impl ApprovedVectorOutput {
    pub fn exact_svg_bytes(&self) -> &[u8] {
        &self.svg_bytes
    }
    pub fn svg_sha256(&self) -> &str {
        &self.svg_sha256
    }
    pub fn report(&self) -> &EvaluationReport {
        &self.report
    }
    pub fn artifacts(&self) -> &DiagnosticArtifactSet {
        &self.artifacts
    }
}

/// Consumes exact-byte evaluated state and performs the universal publication gate.
///
/// Reports are checked as evidence only. They cannot call this function or create an
/// `ApprovedVectorOutput` without the consumed candidate.
pub(crate) fn approve(candidate: EvaluatedCandidate) -> PpResult<VectorOutcome> {
    match candidate.into_approval_transition() {
        Ok(approved) => {
            let (svg_bytes, svg_sha256, report, artifacts) = approved.into_parts();
            Ok(VectorOutcome::Approved(ApprovedVectorOutput {
                svg_bytes,
                svg_sha256,
                report,
                artifacts,
            }))
        }
        Err(rejected) => {
            let (report, artifacts, diagnostics, failed_gates, invariant_failure) =
                (*rejected).into_parts();
            let codes: Vec<_> = failed_gates.into_iter().map(rejection_code).collect();
            if invariant_failure {
                return Err(PpError::Vectorizer(
                    "candidate digest or report decision changed after evaluated-state construction"
                        .to_owned(),
                ));
            }
            Ok(VectorOutcome::Rejected(RejectedVectorOutput::new(
                report,
                artifacts,
                diagnostics,
                codes,
            )))
        }
    }
}

fn rejection_code(family: GateFamily) -> VectorRejectionCode {
    match family {
        GateFamily::Fixed => VectorRejectionCode::FixedGateFailed,
        GateFamily::Protected => VectorRejectionCode::ProtectedGateFailed,
        GateFamily::Calibrated => VectorRejectionCode::CalibratedGateFailed,
        GateFamily::Resource => VectorRejectionCode::ResourceGateFailed,
        GateFamily::OutputProfile => VectorRejectionCode::OutputProfileGateFailed,
    }
}
