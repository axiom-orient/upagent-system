use std::collections::BTreeMap;
use std::path::{Component, Path};

use serde::Serialize;

use super::backends::AdaptiveMergeEvidence;
use super::request::{SvgProfile, UnitScore, VectorPresetSelection};

pub const VECTOR_ANALYSIS_SCHEMA: &str = "perfectpixel.vector-analysis/1";
pub const VECTOR_EVALUATION_SCHEMA: &str = "perfectpixel.vector-evaluation/3";

/// Whether a requested predicate was evaluated and, when evaluated, its result.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum PredicateAvailability {
    Passed,
    Failed,
    NotEvaluated,
}
/// Comparator used to evaluate one measured gate value against its effective threshold.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum GateComparator {
    Equal,
    GreaterThanOrEqual,
    LessThanOrEqual,
}

/// The type of an observed gate value. It keeps count, score, and boolean facts distinct.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", tag = "kind", content = "value")]
pub enum GateActualValue {
    Boolean(bool),
    Count(u64),
    Dimensions { width: u32, height: u32 },
    Score(UnitScore),
}

/// The type of a threshold used by a gate. Boolean gates intentionally have no threshold.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", tag = "kind", content = "value")]
pub enum GateThreshold {
    Count(u64),
    Dimensions { width: u32, height: u32 },
    Score(UnitScore),
}
/// Immutable measured evidence for one gate. It records a result but cannot authorize output.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GateMeasurement {
    actual: Option<GateActualValue>,
    comparator: GateComparator,
    approved_threshold: Option<GateThreshold>,
    requested_threshold: Option<GateThreshold>,
    effective_threshold: Option<GateThreshold>,
    applicability: PredicateAvailability,
    reason: Option<String>,
}
pub(crate) trait IntoGateActual {
    fn into_gate_actual(self) -> Option<GateActualValue>;
}

impl IntoGateActual for GateActualValue {
    fn into_gate_actual(self) -> Option<GateActualValue> {
        Some(self)
    }
}

impl IntoGateActual for Option<GateActualValue> {
    fn into_gate_actual(self) -> Option<GateActualValue> {
        self
    }
}

impl GateMeasurement {
    pub(crate) fn new(
        actual: impl IntoGateActual,
        comparator: GateComparator,
        approved_threshold: Option<GateThreshold>,
        requested_threshold: Option<GateThreshold>,
        effective_threshold: Option<GateThreshold>,
        applicability: PredicateAvailability,
        reason: Option<String>,
    ) -> Self {
        Self {
            actual: actual.into_gate_actual(),
            comparator,
            approved_threshold,
            requested_threshold,
            effective_threshold,
            applicability,
            reason,
        }
    }

    pub const fn actual(&self) -> Option<GateActualValue> {
        self.actual
    }
    pub const fn comparator(&self) -> GateComparator {
        self.comparator
    }
    pub const fn approved_threshold(&self) -> Option<GateThreshold> {
        self.approved_threshold
    }
    pub const fn requested_threshold(&self) -> Option<GateThreshold> {
        self.requested_threshold
    }
    pub const fn effective_threshold(&self) -> Option<GateThreshold> {
        self.effective_threshold
    }
    pub const fn applicability(&self) -> PredicateAvailability {
        self.applicability
    }
    pub fn reason(&self) -> Option<&str> {
        self.reason.as_deref()
    }
}

/// Measured gate evidence grouped by the same fixed policy families as evaluation.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GateMeasurementFamilies {
    fixed: BTreeMap<String, GateMeasurement>,
    protected: BTreeMap<String, GateMeasurement>,
    calibrated: BTreeMap<String, GateMeasurement>,
    resource: BTreeMap<String, GateMeasurement>,
    output_profile: BTreeMap<String, GateMeasurement>,
}

impl GateMeasurementFamilies {
    pub(crate) fn new(
        fixed: BTreeMap<String, GateMeasurement>,
        protected: BTreeMap<String, GateMeasurement>,
        calibrated: BTreeMap<String, GateMeasurement>,
        resource: BTreeMap<String, GateMeasurement>,
        output_profile: BTreeMap<String, GateMeasurement>,
    ) -> Self {
        Self {
            fixed,
            protected,
            calibrated,
            resource,
            output_profile,
        }
    }

    pub fn fixed(&self) -> &BTreeMap<String, GateMeasurement> {
        &self.fixed
    }
    pub fn protected(&self) -> &BTreeMap<String, GateMeasurement> {
        &self.protected
    }
    pub fn calibrated(&self) -> &BTreeMap<String, GateMeasurement> {
        &self.calibrated
    }
    pub fn resource(&self) -> &BTreeMap<String, GateMeasurement> {
        &self.resource
    }
    pub fn output_profile(&self) -> &BTreeMap<String, GateMeasurement> {
        &self.output_profile
    }
}

/// A requested constraint alongside the embedded approved value and normalized outcome.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConstraintValues<T> {
    requested: Option<T>,
    approved: Option<T>,
    effective: Option<T>,
    applied: Option<T>,
    applicable: PredicateAvailability,
    attempted_relaxation: bool,
}

impl<T> ConstraintValues<T> {
    pub(crate) fn new(
        requested: Option<T>,
        approved: Option<T>,
        effective: Option<T>,
        applied: Option<T>,
        applicable: PredicateAvailability,
        attempted_relaxation: bool,
    ) -> Self {
        Self {
            requested,
            approved,
            effective,
            applied,
            applicable,
            attempted_relaxation,
        }
    }
    pub fn requested(&self) -> Option<&T> {
        self.requested.as_ref()
    }
    pub fn approved(&self) -> Option<&T> {
        self.approved.as_ref()
    }
    pub fn effective(&self) -> Option<&T> {
        self.effective.as_ref()
    }
    pub fn applied(&self) -> Option<&T> {
        self.applied.as_ref()
    }
    pub const fn applicability(&self) -> PredicateAvailability {
        self.applicable
    }
    pub const fn attempted_relaxation(&self) -> bool {
        self.attempted_relaxation
    }
}

/// Factorized profiling evidence. Numeric values are evidence, never executable thresholds.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProfileEvidence {
    geometry: BTreeMap<String, f64>,
    paint: BTreeMap<String, f64>,
    alpha: BTreeMap<String, f64>,
    complexity: BTreeMap<String, f64>,
    source_noise: BTreeMap<String, f64>,
    confidence: UnitScore,
    conflicts: Vec<String>,
    abstained: bool,
    expected_support: PredicateAvailability,
    recommended_presets: Vec<VectorPresetSelection>,
}

impl ProfileEvidence {
    // Parameters are exactly this value type's fields and the body is a pure move, so
    // clippy's argument-count heuristic does not apply: grouping them would mean a second
    // struct with identical fields. Fields stay private so construction runs through here.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        geometry: BTreeMap<String, f64>,
        paint: BTreeMap<String, f64>,
        alpha: BTreeMap<String, f64>,
        complexity: BTreeMap<String, f64>,
        source_noise: BTreeMap<String, f64>,
        confidence: UnitScore,
        conflicts: Vec<String>,
        abstained: bool,
        expected_support: PredicateAvailability,
        recommended_presets: Vec<VectorPresetSelection>,
    ) -> Self {
        Self {
            geometry,
            paint,
            alpha,
            complexity,
            source_noise,
            confidence,
            conflicts,
            abstained,
            expected_support,
            recommended_presets,
        }
    }
    pub fn geometry(&self) -> &BTreeMap<String, f64> {
        &self.geometry
    }
    pub fn paint(&self) -> &BTreeMap<String, f64> {
        &self.paint
    }
    pub fn alpha(&self) -> &BTreeMap<String, f64> {
        &self.alpha
    }
    pub fn complexity(&self) -> &BTreeMap<String, f64> {
        &self.complexity
    }
    pub fn source_noise(&self) -> &BTreeMap<String, f64> {
        &self.source_noise
    }
    pub const fn confidence(&self) -> UnitScore {
        self.confidence
    }
    pub fn conflicts(&self) -> &[String] {
        &self.conflicts
    }
    pub const fn abstained(&self) -> bool {
        self.abstained
    }
    pub const fn expected_support(&self) -> PredicateAvailability {
        self.expected_support
    }
    pub fn recommended_presets(&self) -> &[VectorPresetSelection] {
        &self.recommended_presets
    }
}

/// Hashes identifying the embedded authority and the exact evaluated state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EvaluationDigests {
    backend: Option<String>,
    route: Option<String>,
    registry: String,
    registry_entry: Option<String>,
    threshold_bundle: Option<String>,
    normalized_request: Option<String>,
    policy: String,
    profile: String,
    foundation: String,
    report: String,
    motion: Option<String>,
    candidate_bytes: Option<String>,
}

impl EvaluationDigests {
    // Parameters are exactly this value type's fields and the body is a pure move, so
    // clippy's argument-count heuristic does not apply: grouping them would mean a second
    // struct with identical fields. Fields stay private so construction runs through here.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        backend: Option<String>,
        route: Option<String>,
        registry: String,
        registry_entry: Option<String>,
        threshold_bundle: Option<String>,
        normalized_request: Option<String>,
        policy: String,
        profile: String,
        foundation: String,
        report: String,
        motion: Option<String>,
        candidate_bytes: Option<String>,
    ) -> Self {
        Self {
            backend,
            route,
            registry,
            registry_entry,
            threshold_bundle,
            normalized_request,
            policy,
            profile,
            foundation,
            report,
            motion,
            candidate_bytes,
        }
    }
    pub fn backend(&self) -> Option<&str> {
        self.backend.as_deref()
    }
    pub fn route(&self) -> Option<&str> {
        self.route.as_deref()
    }
    pub fn registry(&self) -> &str {
        &self.registry
    }
    pub fn registry_entry(&self) -> Option<&str> {
        self.registry_entry.as_deref()
    }
    pub fn threshold_bundle(&self) -> Option<&str> {
        self.threshold_bundle.as_deref()
    }
    pub fn normalized_request(&self) -> Option<&str> {
        self.normalized_request.as_deref()
    }
    pub fn policy(&self) -> &str {
        &self.policy
    }
    pub fn profile(&self) -> &str {
        &self.profile
    }
    pub fn foundation(&self) -> &str {
        &self.foundation
    }
    pub fn report(&self) -> &str {
        &self.report
    }
    pub fn motion(&self) -> Option<&str> {
        self.motion.as_deref()
    }
    pub fn candidate_bytes(&self) -> Option<&str> {
        self.candidate_bytes.as_deref()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TopologyFacts {
    source_components: usize,
    candidate_components: usize,
    source_holes: usize,
    candidate_holes: usize,
    mismatched_mask_pixels: usize,
}

impl TopologyFacts {
    pub(crate) fn new(
        source_components: usize,
        candidate_components: usize,
        source_holes: usize,
        candidate_holes: usize,
        mismatched_mask_pixels: usize,
    ) -> Self {
        Self {
            source_components,
            candidate_components,
            source_holes,
            candidate_holes,
            mismatched_mask_pixels,
        }
    }
}
/// Deterministic metadata for the exact candidate and render-back evaluated by generation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CandidateFacts {
    candidate_sha256: String,
    candidate_byte_count: usize,
    render_back_sha256: String,
    width: u32,
    height: u32,
    path_count: usize,
    color_count: usize,
    node_count: usize,
    curve_segment_count: usize,
    closed_path_count: usize,
    adaptive: Option<AdaptiveMergeEvidence>,
    topology: TopologyFacts,
}

impl CandidateFacts {
    // Parameters are exactly this value type's fields and the body is a pure move, so
    // clippy's argument-count heuristic does not apply: grouping them would mean a second
    // struct with identical fields. Fields stay private so construction runs through here.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        candidate_sha256: String,
        candidate_byte_count: usize,
        render_back_sha256: String,
        width: u32,
        height: u32,
        path_count: usize,
        color_count: usize,
        node_count: usize,
        curve_segment_count: usize,
        closed_path_count: usize,
        adaptive: Option<AdaptiveMergeEvidence>,
        topology: TopologyFacts,
    ) -> Self {
        Self {
            candidate_sha256,
            candidate_byte_count,
            render_back_sha256,
            width,
            height,
            path_count,
            color_count,
            node_count,
            curve_segment_count,
            closed_path_count,
            adaptive,
            topology,
        }
    }

    pub fn candidate_sha256(&self) -> &str {
        &self.candidate_sha256
    }

    pub const fn candidate_byte_count(&self) -> usize {
        self.candidate_byte_count
    }

    pub fn render_back_sha256(&self) -> &str {
        &self.render_back_sha256
    }

    pub const fn dimensions(&self) -> (u32, u32) {
        (self.width, self.height)
    }
}

/// Serializable metadata for one bounded diagnostic artifact. Exact bytes remain
/// outcome-owned and are never retained by an evaluation report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiagnosticArtifactMetadata {
    relative_path: String,
    digest: String,
    media_type: String,
    byte_count: usize,
}

impl DiagnosticArtifactMetadata {
    fn from_artifact(artifact: &DiagnosticArtifact) -> Self {
        Self {
            relative_path: artifact.relative_path.clone(),
            digest: artifact.digest.clone(),
            media_type: artifact.media_type.clone(),
            byte_count: artifact.byte_count,
        }
    }

    pub fn relative_path(&self) -> &str {
        &self.relative_path
    }
    pub fn digest(&self) -> &str {
        &self.digest
    }
    pub fn media_type(&self) -> &str {
        &self.media_type
    }
    pub const fn byte_count(&self) -> usize {
        self.byte_count
    }
}

/// Exact diagnostic bytes owned exclusively by an outcome artifact set.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiagnosticArtifact {
    relative_path: String,
    digest: String,
    media_type: String,
    byte_count: usize,
    #[serde(skip)]
    exact_bytes: Vec<u8>,
}

impl DiagnosticArtifact {
    pub(crate) fn new(
        relative_path: String,
        media_type: String,
        exact_bytes: Vec<u8>,
    ) -> Result<Self, String> {
        validate_artifact_contents(&relative_path, &media_type, &exact_bytes)?;
        let digest = super::candidate::sha256_hex(&exact_bytes);
        Ok(Self {
            relative_path,
            digest,
            media_type,
            byte_count: exact_bytes.len(),
            exact_bytes,
        })
    }

    pub fn relative_path(&self) -> &str {
        &self.relative_path
    }
    pub fn digest(&self) -> &str {
        &self.digest
    }
    pub fn media_type(&self) -> &str {
        &self.media_type
    }
    pub const fn byte_count(&self) -> usize {
        self.byte_count
    }
    pub fn exact_bytes(&self) -> &[u8] {
        &self.exact_bytes
    }
}

fn validate_artifact_contents(
    relative_path: &str,
    media_type: &str,
    exact_bytes: &[u8],
) -> Result<(), String> {
    validate_artifact_path(relative_path)?;
    if media_type.is_empty()
        || media_type.len() > 128
        || !media_type
            .bytes()
            .all(|byte| byte.is_ascii_graphic() || byte == b' ')
    {
        return Err("diagnostic artifact media type is invalid".to_owned());
    }
    if exact_bytes.len() > DiagnosticArtifactSet::MAX_ARTIFACT_BYTES {
        return Err("diagnostic artifact exceeds byte limit".to_owned());
    }
    Ok(())
}

fn validate_artifact_path(relative_path: &str) -> Result<(), String> {
    let path = Path::new(relative_path);
    let depth = path.components().count();
    if relative_path.is_empty()
        || relative_path.len() > 256
        || depth == 0
        || depth > 8
        || relative_path.contains('\0')
        || relative_path.contains('\\')
        || path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        Err(format!(
            "diagnostic artifact '{relative_path}' is not a safe normalized relative path"
        ))
    } else {
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiagnosticArtifactSet {
    artifacts: Vec<DiagnosticArtifact>,
}

impl DiagnosticArtifactSet {
    pub(crate) const MAX_ARTIFACTS: usize = 16;
    pub(crate) const MAX_ARTIFACT_BYTES: usize = 8 * 1024 * 1024;
    pub(crate) const MAX_TOTAL_BYTES: usize = 32 * 1024 * 1024;

    pub(crate) fn new(artifacts: Vec<DiagnosticArtifact>) -> Result<Self, String> {
        if artifacts.len() > Self::MAX_ARTIFACTS {
            return Err("diagnostic artifact count exceeds limit".to_owned());
        }

        let mut destinations = std::collections::BTreeSet::new();
        let mut total_bytes = 0usize;
        for artifact in &artifacts {
            if !destinations.insert(artifact.relative_path.as_str()) {
                return Err(format!(
                    "duplicate diagnostic artifact destination '{}'",
                    artifact.relative_path
                ));
            }
            total_bytes = total_bytes
                .checked_add(artifact.byte_count)
                .ok_or_else(|| "diagnostic artifact byte count overflow".to_owned())?;
            if total_bytes > Self::MAX_TOTAL_BYTES {
                return Err("diagnostic artifacts exceed total byte limit".to_owned());
            }
            let verified = Self::verify(artifact)?;
            if !verified {
                return Err(format!(
                    "diagnostic artifact '{}' has inconsistent bytes",
                    artifact.relative_path
                ));
            }
        }
        Ok(Self { artifacts })
    }

    fn verify(artifact: &DiagnosticArtifact) -> Result<bool, String> {
        validate_artifact_contents(
            &artifact.relative_path,
            &artifact.media_type,
            &artifact.exact_bytes,
        )?;
        Ok(
            super::candidate::sha256_hex(&artifact.exact_bytes) == artifact.digest
                && artifact.exact_bytes.len() == artifact.byte_count,
        )
    }

    pub fn artifacts(&self) -> &[DiagnosticArtifact] {
        &self.artifacts
    }
}

/// Requested diagnostic output intent. It has no transaction or publication state,
/// and embeds only report-safe artifact metadata.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ArtifactIntent {
    requested: bool,
    artifacts: Vec<DiagnosticArtifactMetadata>,
}

impl ArtifactIntent {
    pub(crate) fn new(requested: bool, artifacts: DiagnosticArtifactSet) -> Self {
        Self {
            requested,
            artifacts: artifacts
                .artifacts
                .iter()
                .map(DiagnosticArtifactMetadata::from_artifact)
                .collect(),
        }
    }
    pub(crate) fn matches_artifacts(&self, artifacts: &DiagnosticArtifactSet) -> bool {
        self.artifacts.len() == artifacts.artifacts.len()
            && self
                .artifacts
                .iter()
                .zip(&artifacts.artifacts)
                .all(|(metadata, artifact)| {
                    metadata.relative_path == artifact.relative_path
                        && metadata.digest == artifact.digest
                        && metadata.media_type == artifact.media_type
                        && metadata.byte_count == artifact.byte_count
                })
    }
    pub const fn requested(&self) -> bool {
        self.requested
    }
    pub fn artifacts(&self) -> &[DiagnosticArtifactMetadata] {
        &self.artifacts
    }
}

/// Runtime measurements are intentionally excluded from deterministic product evidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentMeasurements {
    #[serde(serialize_with = "serialize_null")]
    render_time_ms: (),
    #[serde(serialize_with = "serialize_null")]
    peak_memory_bytes: (),
    #[serde(serialize_with = "serialize_null")]
    throughput_pixels_per_second: (),
    #[serde(serialize_with = "serialize_null")]
    machine_protocol_id: (),
}

fn serialize_null<S, T>(_: &T, serializer: S) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    serializer.serialize_none()
}

impl EnvironmentMeasurements {
    pub(crate) const fn product_report() -> Self {
        Self {
            render_time_ms: (),
            peak_memory_bytes: (),
            throughput_pixels_per_second: (),
            machine_protocol_id: (),
        }
    }
}

/// Candidate-free policy normalization and applicability evidence.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AnalysisNormalization {
    policy_schema: String,
    policy_version: String,
    applicability: BTreeMap<String, PredicateAvailability>,
    reasons: Vec<String>,
}

impl AnalysisNormalization {
    pub(crate) fn new(
        policy_schema: String,
        policy_version: String,
        applicability: BTreeMap<String, PredicateAvailability>,
        reasons: Vec<String>,
    ) -> Self {
        Self {
            policy_schema,
            policy_version,
            applicability,
            reasons,
        }
    }

    pub fn policy_schema(&self) -> &str {
        &self.policy_schema
    }
    pub fn policy_version(&self) -> &str {
        &self.policy_version
    }
    pub fn applicability(&self) -> &BTreeMap<String, PredicateAvailability> {
        &self.applicability
    }
    pub fn reasons(&self) -> &[String] {
        &self.reasons
    }
}

/// A candidate-free analysis evidence document. It has no approval or publication capability.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VectorAnalysis {
    schema: &'static str,
    input_digest: String,
    preset: VectorPresetSelection,
    profile: SvgProfile,
    policy_digest: String,
    evidence: ProfileEvidence,
    route: Option<String>,
    route_reasons: Vec<String>,
    digests: EvaluationDigests,
    predicates: BTreeMap<String, PredicateAvailability>,
    normalization: Option<AnalysisNormalization>,
    environment_measurements: EnvironmentMeasurements,
}

impl VectorAnalysis {
    pub const SCHEMA: &'static str = VECTOR_ANALYSIS_SCHEMA;
    // Parameters are exactly this value type's fields and the body is a pure move, so
    // clippy's argument-count heuristic does not apply: grouping them would mean a second
    // struct with identical fields. Fields stay private so construction runs through here.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        input_digest: String,
        preset: VectorPresetSelection,
        profile: SvgProfile,
        policy_digest: String,
        evidence: ProfileEvidence,
        route: Option<String>,
        route_reasons: Vec<String>,
        digests: EvaluationDigests,
        predicates: BTreeMap<String, PredicateAvailability>,
    ) -> Self {
        Self {
            schema: Self::SCHEMA,
            input_digest,
            preset,
            profile,
            policy_digest,
            evidence,
            route,
            route_reasons,
            digests,
            predicates,
            normalization: None,
            environment_measurements: EnvironmentMeasurements::product_report(),
        }
    }
    pub const fn schema(&self) -> &'static str {
        self.schema
    }
    pub fn input_digest(&self) -> &str {
        &self.input_digest
    }
    pub const fn preset(&self) -> VectorPresetSelection {
        self.preset
    }
    pub const fn profile(&self) -> SvgProfile {
        self.profile
    }
    pub fn policy_digest(&self) -> &str {
        &self.policy_digest
    }
    pub fn evidence(&self) -> &ProfileEvidence {
        &self.evidence
    }
    pub fn route(&self) -> Option<&str> {
        self.route.as_deref()
    }
    pub fn route_reasons(&self) -> &[String] {
        &self.route_reasons
    }
    pub fn digests(&self) -> &EvaluationDigests {
        &self.digests
    }
    pub fn predicates(&self) -> &BTreeMap<String, PredicateAvailability> {
        &self.predicates
    }
    pub fn normalization(&self) -> Option<&AnalysisNormalization> {
        self.normalization.as_ref()
    }
    pub(crate) fn with_normalization(mut self, normalization: AnalysisNormalization) -> Self {
        self.normalization = Some(normalization);
        self
    }
    pub const fn environment_measurements(&self) -> EnvironmentMeasurements {
        self.environment_measurements
    }
}

/// Actual decision recorded after private, candidate-bound evaluation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum EvaluationDecision {
    Approved,
    Rejected,
    NotApplicable,
}

/// Immutable evaluation evidence; it is not an approval token and contains no SVG or candidate bytes.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EvaluationReport {
    schema: &'static str,
    input_digest: String,
    preset: VectorPresetSelection,
    profile: SvgProfile,
    evidence: ProfileEvidence,
    constraints: BTreeMap<String, ConstraintValues<UnitScore>>,
    path_limit: ConstraintValues<usize>,
    digests: EvaluationDigests,
    candidate_facts: Option<CandidateFacts>,
    fixed_gates: BTreeMap<String, PredicateAvailability>,
    protected_gates: BTreeMap<String, PredicateAvailability>,
    calibrated_gates: BTreeMap<String, PredicateAvailability>,
    resource_gates: BTreeMap<String, PredicateAvailability>,
    profile_predicates: BTreeMap<String, PredicateAvailability>,
    gate_measurements: GateMeasurementFamilies,
    actual_decision: EvaluationDecision,
    rejection_codes: Vec<String>,
    rejection_reasons: Vec<String>,
    artifact_intent: ArtifactIntent,
    environment_measurements: EnvironmentMeasurements,
}

impl EvaluationReport {
    pub const SCHEMA: &'static str = VECTOR_EVALUATION_SCHEMA;
    // Parameters are exactly this value type's fields and the body is a pure move, so
    // clippy's argument-count heuristic does not apply: grouping them would mean a second
    // struct with identical fields. Fields stay private so construction runs through here.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        input_digest: String,
        preset: VectorPresetSelection,
        profile: SvgProfile,
        evidence: ProfileEvidence,
        constraints: BTreeMap<String, ConstraintValues<UnitScore>>,
        path_limit: ConstraintValues<usize>,
        digests: EvaluationDigests,
        candidate_facts: Option<CandidateFacts>,
        fixed_gates: BTreeMap<String, PredicateAvailability>,
        protected_gates: BTreeMap<String, PredicateAvailability>,
        calibrated_gates: BTreeMap<String, PredicateAvailability>,
        resource_gates: BTreeMap<String, PredicateAvailability>,
        profile_predicates: BTreeMap<String, PredicateAvailability>,
        gate_measurements: GateMeasurementFamilies,
        actual_decision: EvaluationDecision,
        rejection_codes: Vec<String>,
        rejection_reasons: Vec<String>,
        artifact_intent: ArtifactIntent,
    ) -> Self {
        Self {
            schema: Self::SCHEMA,
            input_digest,
            preset,
            profile,
            evidence,
            constraints,
            path_limit,
            digests,
            candidate_facts,
            fixed_gates,
            protected_gates,
            calibrated_gates,
            resource_gates,
            profile_predicates,
            gate_measurements,
            actual_decision,
            rejection_codes,
            rejection_reasons,
            artifact_intent,
            environment_measurements: EnvironmentMeasurements::product_report(),
        }
    }

    pub fn gate_measurements(&self) -> &GateMeasurementFamilies {
        &self.gate_measurements
    }
    pub const fn schema(&self) -> &'static str {
        self.schema
    }
    pub fn input_digest(&self) -> &str {
        &self.input_digest
    }
    pub const fn preset(&self) -> VectorPresetSelection {
        self.preset
    }
    pub const fn profile(&self) -> SvgProfile {
        self.profile
    }
    pub fn evidence(&self) -> &ProfileEvidence {
        &self.evidence
    }
    pub fn constraints(&self) -> &BTreeMap<String, ConstraintValues<UnitScore>> {
        &self.constraints
    }
    pub fn path_limit(&self) -> &ConstraintValues<usize> {
        &self.path_limit
    }
    pub fn digests(&self) -> &EvaluationDigests {
        &self.digests
    }

    pub fn candidate_facts(&self) -> Option<&CandidateFacts> {
        self.candidate_facts.as_ref()
    }
    pub(crate) fn fixed_gates(&self) -> &BTreeMap<String, PredicateAvailability> {
        &self.fixed_gates
    }
    pub(crate) fn protected_gates(&self) -> &BTreeMap<String, PredicateAvailability> {
        &self.protected_gates
    }
    pub(crate) fn calibrated_gates(&self) -> &BTreeMap<String, PredicateAvailability> {
        &self.calibrated_gates
    }
    pub(crate) fn resource_gates(&self) -> &BTreeMap<String, PredicateAvailability> {
        &self.resource_gates
    }
    pub(crate) fn output_profile_gates(&self) -> &BTreeMap<String, PredicateAvailability> {
        &self.profile_predicates
    }
    pub const fn actual_decision(&self) -> EvaluationDecision {
        self.actual_decision
    }
    pub fn rejection_codes(&self) -> &[String] {
        &self.rejection_codes
    }
    pub fn rejection_reasons(&self) -> &[String] {
        &self.rejection_reasons
    }
    pub fn artifact_intent(&self) -> &ArtifactIntent {
        &self.artifact_intent
    }
    pub(crate) fn has_complete_constraints_and_rejection_metadata(&self) -> bool {
        let constraint_keys_are_exact = self
            .constraints
            .keys()
            .map(String::as_str)
            .eq(["maximumQualityLoss", "minimumQuality"]);
        let constraints_complete = self.constraints.values().all(|constraint| {
            constraint.approved.is_some()
                && constraint.effective.is_some()
                && constraint.applied.is_some()
                && constraint.applicable != PredicateAvailability::NotEvaluated
        });
        let path_limit_complete = self.path_limit.approved.is_some()
            && self.path_limit.effective.is_some()
            && self.path_limit.applied.is_some()
            && self.path_limit.applicable != PredicateAvailability::NotEvaluated;
        let metadata_matches_intent =
            self.artifact_intent.requested || self.artifact_intent.artifacts.is_empty();
        let rejection_metadata_is_consistent = match self.actual_decision {
            EvaluationDecision::Approved => {
                self.rejection_codes.is_empty() && self.rejection_reasons.is_empty()
            }
            EvaluationDecision::Rejected => {
                !self.rejection_codes.is_empty()
                    && !self.rejection_reasons.is_empty()
                    && self.rejection_codes.len() == self.rejection_reasons.len()
                    && self
                        .rejection_codes
                        .iter()
                        .chain(&self.rejection_reasons)
                        .all(|value| !value.trim().is_empty())
            }
            EvaluationDecision::NotApplicable => false,
        };

        constraint_keys_are_exact
            && constraints_complete
            && path_limit_complete
            && metadata_matches_intent
            && rejection_metadata_is_consistent
    }
    pub const fn environment_measurements(&self) -> EnvironmentMeasurements {
        self.environment_measurements
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diagnostic_artifact_set_rejects_duplicate_destinations() {
        let first = DiagnosticArtifact::new(
            "candidate.svg".to_owned(),
            "image/svg+xml".to_owned(),
            vec![1],
        )
        .unwrap();
        let second = DiagnosticArtifact::new(
            "candidate.svg".to_owned(),
            "image/svg+xml".to_owned(),
            vec![2],
        )
        .unwrap();

        assert!(DiagnosticArtifactSet::new(vec![first, second]).is_err());
    }

    #[test]
    fn report_intent_does_not_retain_exact_artifact_bytes() {
        let bytes = vec![1, 2, 3];
        let artifacts = DiagnosticArtifactSet::new(vec![DiagnosticArtifact::new(
            "candidate.svg".to_owned(),
            "image/svg+xml".to_owned(),
            bytes,
        )
        .unwrap()])
        .unwrap();

        let intent = ArtifactIntent::new(true, artifacts);
        assert_eq!(intent.artifacts().len(), 1);
        assert_eq!(intent.artifacts()[0].byte_count(), 3);
    }
}
