use serde::Serialize;

use super::approval::ApprovedVectorOutput;
use super::report::{DiagnosticArtifactSet, EvaluationReport, ProfileEvidence};

/// The complete domain result of vector generation.
#[derive(Debug, PartialEq)]
pub enum VectorOutcome {
    Approved(ApprovedVectorOutput),
    Rejected(RejectedVectorOutput),
}

/// Stable, machine-readable reasons that generation did not produce publishable SVG bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum VectorRejectionCode {
    PresetRequired,
    Unsupported,
    ProfileNotPromoted,
    FixedGateFailed,
    ProtectedGateFailed,
    CalibratedGateFailed,
    ResourceGateFailed,
    OutputProfileGateFailed,
}

impl VectorRejectionCode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::PresetRequired => "PRESET_REQUIRED",
            Self::Unsupported => "UNSUPPORTED",
            Self::ProfileNotPromoted => "PROFILE_NOT_PROMOTED",
            Self::FixedGateFailed => "FIXED_GATE_FAILED",
            Self::ProtectedGateFailed => "PROTECTED_GATE_FAILED",
            Self::CalibratedGateFailed => "CALIBRATED_GATE_FAILED",
            Self::ResourceGateFailed => "RESOURCE_GATE_FAILED",
            Self::OutputProfileGateFailed => "OUTPUT_PROFILE_GATE_FAILED",
        }
    }
}

/// A reportable rejection. It intentionally contains no final SVG bytes or accessor.
#[derive(Debug, Clone, PartialEq)]
pub struct RejectedVectorOutput {
    report: EvaluationReport,
    artifacts: DiagnosticArtifactSet,
    diagnostics: Vec<String>,
    codes: Vec<VectorRejectionCode>,
}

impl RejectedVectorOutput {
    pub(crate) fn new(
        report: EvaluationReport,
        artifacts: DiagnosticArtifactSet,
        diagnostics: Vec<String>,
        mut codes: Vec<VectorRejectionCode>,
    ) -> Self {
        codes.sort_unstable();
        codes.dedup();
        Self {
            report,
            artifacts,
            diagnostics,
            codes,
        }
    }

    pub fn report(&self) -> &EvaluationReport {
        &self.report
    }
    pub fn evidence(&self) -> &ProfileEvidence {
        self.report.evidence()
    }
    pub fn artifacts(&self) -> &DiagnosticArtifactSet {
        &self.artifacts
    }
    pub fn diagnostics(&self) -> &[String] {
        &self.diagnostics
    }
    pub fn codes(&self) -> &[VectorRejectionCode] {
        &self.codes
    }
}
