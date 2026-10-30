use serde::Deserialize;

use super::{digest, is_sha256_hex, is_sha256_identity, AuthorityError, AuthorityResult};

const EMBEDDED_THRESHOLDS: &str = include_str!("approved_thresholds/foundation-baseline.json");
const MIN_QUALITY_SCORE: f64 = 0.995;
const MIN_LOCAL_LUMA_SSIM: f64 = 0.990;
const MIN_WORST_BLOCK_LUMA_SSIM: f64 = 0.950;
const MAX_PATHS: usize = 64;
const FOUNDATION_BUNDLE_ID: &str = "foundation-baseline/1";
const FOUNDATION_ID: &str = "perfectpixel.vector-foundation/1";
const FOUNDATION_BUNDLE_DIGEST: &str =
    "sha256-4caba650d3102ed76b40549872fad900cd45b013fb1f91bbb62a1e76ba37e526";
const EMBEDDED_FAMILY_CALIBRATIONS: &str =
    include_str!("approved_thresholds/family-calibration.json");
const FAMILY_CALIBRATION_REGISTRY_ID: &str = "family-calibration/2";
const REPORT_SCHEMA: &str = "perfectpixel.vector-evaluation/3";
const FAMILY_CALIBRATIONS: [&str; 4] =
    ["pixel-art", "flat-icon", "line-art", "bounded-illustration"];

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct ThresholdBundle {
    bundle_id: String,
    foundation_id: String,
    bundle_digest: String,
    thresholds: ThresholdDefaults,
    fixed_gates: FixedGates,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct ThresholdDefaults {
    pub(crate) minimum_quality_score: f64,
    pub(crate) minimum_local_luma_ssim: f64,
    pub(crate) minimum_worst_block_luma_ssim: f64,
    pub(crate) maximum_paths: usize,
    pub(crate) maximum_input_pixels: u64,
    pub(crate) maximum_svg_bytes: u64,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct FixedGates {
    pub(crate) require_security_gate: bool,
    pub(crate) reject_unsupported_content: bool,
    pub(crate) preserve_protected_features: bool,
    pub(crate) enforce_resource_limits: bool,
}
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct FamilyCalibrationRegistry {
    registry_id: String,
    registry_digest: String,
    bundles: Vec<FamilyCalibrationBundle>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct FamilyCalibrationBundle {
    schema_version: u8,
    kind: String,
    bundle_digest: String,
    digest_closure: serde_json::Value,
    thresholds_changed: bool,
    no_waivers: bool,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct BackendIdentity {
    id: String,
    version: String,
    source_digest: String,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct VerifiedFamilyBundle {
    pub(crate) digest: String,
    pub(crate) family: String,
    pub(crate) backend_id: String,
    pub(crate) backend_version: String,
    pub(crate) backend_source_digest: String,
    pub(crate) report_schema: String,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct FamilyCalibrationClosure {
    family: String,
    backend: BackendIdentity,
    report_schema: String,
    adaptive_policy: Option<AdaptivePolicyClosure>,
    effective_thresholds: ThresholdDefaults,
    fixed_gates: FixedGates,
    corpus_lock_identity_sha256: String,
    corpus_manifest_sha256: String,
    corpus_generator_source_sha256: String,
    corpus_schema_sha256: String,
    corpus_license_inventory_sha256: String,
    corpus_unique_artifact_inventory_sha256: String,
    calibration_split_sha256: String,
    holdout_split_sha256: String,
    holdout_identity_sha256: String,
    corpus_source_family_sha256: String,
    corpus_defect_sha256: String,
    calibration_verifier: CalibrationVerifierClosure,
    bootstrap: BootstrapIdentity,
    source_inventory: Vec<FixtureInventory>,
    defect_inventory: Vec<DefectInventory>,
    no_waivers: bool,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CalibrationVerifierClosure {
    calibration_verifier_source_sha256: String,
    cargo_lock_sha256: String,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct AdaptivePolicyClosure {
    policy_identity: String,
    model_identity: String,
    selector_identity: String,
    trace_identity: String,
    trace_encoding_version: u8,
    fragment_pixels: usize,
    protected_thin_pixels: usize,
    minimum_plateau_coverage_percent: usize,
    maximum_cost_q: u64,
    selector_merge_credit_q: u64,
    maximum_source_colors: usize,
    maximum_fine_regions: usize,
    maximum_adjacency_edges: usize,
    maximum_merge_evaluations: usize,
    maximum_retained_snapshots: usize,
    maximum_queue_entries: usize,
    maximum_emitted_paths: usize,
}

#[derive(Debug, Clone, Deserialize, serde::Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct BootstrapIdentity {
    run_digest: String,
    evaluation_sha256: String,
    review_sha256: String,
    result_matrix_digest: String,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct FixtureInventory {
    id: String,
    ancestry: String,
    input_sha256: String,
    split: String,
    source_family: String,
    expected_support: String,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct DefectInventory {
    #[serde(flatten)]
    fixture: FixtureInventory,
    defect: String,
    expected_rejection: Rejection,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Rejection {
    code: String,
    category: String,
}

impl FamilyCalibrationClosure {
    fn from_value(value: &serde_json::Value) -> Result<Self, serde_json::Error> {
        serde_json::from_value(value.clone())
    }
}

impl ThresholdBundle {
    pub(crate) fn embedded() -> AuthorityResult<Self> {
        let bundle: Self = serde_json::from_str(EMBEDDED_THRESHOLDS)
            .map_err(|error| AuthorityError::ThresholdRegistry(error.to_string()))?;
        bundle.verify()?;
        Ok(bundle)
    }

    pub(crate) fn id(&self) -> &str {
        &self.bundle_id
    }

    pub(crate) fn foundation_id(&self) -> &str {
        &self.foundation_id
    }

    pub(crate) fn digest(&self) -> &str {
        &self.bundle_digest
    }

    pub(crate) fn defaults(&self) -> &ThresholdDefaults {
        &self.thresholds
    }

    pub(crate) fn fixed_gates(&self) -> &FixedGates {
        &self.fixed_gates
    }
    /// Resolves the exact verified threshold bundle named by a route.
    pub(crate) fn resolve_digest(
        &self,
        digest: &str,
    ) -> AuthorityResult<(ThresholdDefaults, FixedGates)> {
        if digest == self.digest() {
            return Ok((self.thresholds.clone(), self.fixed_gates.clone()));
        }
        let registry = FamilyCalibrationRegistry::embedded()?;
        registry.resolve_digest(digest).ok_or_else(|| {
            AuthorityError::ThresholdRegistry(
                "route references an unknown verified threshold bundle".to_owned(),
            )
        })
    }

    pub(crate) fn verify(&self) -> AuthorityResult<()> {
        if self.bundle_id != FOUNDATION_BUNDLE_ID
            || self.foundation_id != FOUNDATION_ID
            || self.bundle_digest != FOUNDATION_BUNDLE_DIGEST
        {
            return Err(AuthorityError::ThresholdRegistry(
                "embedded bundle does not match the generated SHA-256 foundation identity"
                    .to_owned(),
            ));
        }
        if !self.thresholds.minimum_quality_score.is_finite()
            || !self.thresholds.minimum_local_luma_ssim.is_finite()
            || !self.thresholds.minimum_worst_block_luma_ssim.is_finite()
            || !is_milliquantized(self.thresholds.minimum_quality_score)
            || !is_milliquantized(self.thresholds.minimum_local_luma_ssim)
            || !is_milliquantized(self.thresholds.minimum_worst_block_luma_ssim)
            || self.thresholds.minimum_quality_score < MIN_QUALITY_SCORE
            || self.thresholds.minimum_local_luma_ssim < MIN_LOCAL_LUMA_SSIM
            || self.thresholds.minimum_worst_block_luma_ssim < MIN_WORST_BLOCK_LUMA_SSIM
            || self.thresholds.minimum_quality_score > 1.0
            || self.thresholds.minimum_local_luma_ssim > 1.0
            || self.thresholds.minimum_worst_block_luma_ssim > 1.0
            || self.thresholds.maximum_paths == 0
            || self.thresholds.maximum_paths > MAX_PATHS
            || self.thresholds.maximum_input_pixels == 0
            || self.thresholds.maximum_svg_bytes == 0
        {
            return Err(AuthorityError::ThresholdRegistry(
                "foundation thresholds contain invalid values or resource limits".to_owned(),
            ));
        }
        if !self.fixed_gates.require_security_gate
            || !self.fixed_gates.reject_unsupported_content
            || !self.fixed_gates.preserve_protected_features
            || !self.fixed_gates.enforce_resource_limits
        {
            return Err(AuthorityError::ThresholdRegistry(
                "all fixed security, support, protected-feature, and resource gates are required"
                    .to_owned(),
            ));
        }
        if self.bundle_digest != self.computed_digest() {
            return Err(AuthorityError::ThresholdRegistry(
                "bundle digest does not match canonical threshold contents".to_owned(),
            ));
        }
        Ok(())
    }

    fn computed_digest(&self) -> String {
        digest(&format!(
            "{}\n{}\n{:.3}\n{:.3}\n{:.3}\n{}\n{}\n{}\n{}\n{}\n{}\n{}",
            self.bundle_id,
            self.foundation_id,
            self.thresholds.minimum_quality_score,
            self.thresholds.minimum_local_luma_ssim,
            self.thresholds.minimum_worst_block_luma_ssim,
            self.thresholds.maximum_paths,
            self.thresholds.maximum_input_pixels,
            self.thresholds.maximum_svg_bytes,
            self.fixed_gates.require_security_gate,
            self.fixed_gates.reject_unsupported_content,
            self.fixed_gates.preserve_protected_features,
            self.fixed_gates.enforce_resource_limits,
        ))
    }
}

impl FamilyCalibrationRegistry {
    pub(crate) fn embedded() -> AuthorityResult<Self> {
        let registry: Self = serde_json::from_str(EMBEDDED_FAMILY_CALIBRATIONS)
            .map_err(|error| AuthorityError::ThresholdRegistry(error.to_string()))?;
        registry.verify()?;
        Ok(registry)
    }

    pub(crate) fn verified_bundle(&self, family: &str) -> Option<VerifiedFamilyBundle> {
        self.bundles.iter().find_map(|bundle| {
            let closure = FamilyCalibrationClosure::from_value(&bundle.digest_closure).ok()?;
            (closure.family == family).then(|| VerifiedFamilyBundle {
                digest: bundle.bundle_digest.clone(),
                family: closure.family,
                backend_id: closure.backend.id,
                backend_version: closure.backend.version,
                backend_source_digest: closure.backend.source_digest,
                report_schema: closure.report_schema,
            })
        })
    }
    fn resolve_digest(&self, digest: &str) -> Option<(ThresholdDefaults, FixedGates)> {
        self.bundles.iter().find_map(|bundle| {
            (bundle.bundle_digest == digest)
                .then(|| FamilyCalibrationClosure::from_value(&bundle.digest_closure).ok())
                .flatten()
                .map(|closure| (closure.effective_thresholds, closure.fixed_gates))
        })
    }

    pub(crate) fn threshold_set_id(family: &str) -> Option<String> {
        FAMILY_CALIBRATIONS
            .iter()
            .find(|expected| **expected == family)
            .map(|_| format!("family-calibration/{family}/1"))
    }

    fn verify(&self) -> AuthorityResult<()> {
        if self.registry_id != FAMILY_CALIBRATION_REGISTRY_ID
            || self.bundles.len() > FAMILY_CALIBRATIONS.len()
            || !is_sha256_identity(&self.registry_digest)
        {
            return Err(AuthorityError::ThresholdRegistry(
                "family calibration registry identity or coverage is invalid".to_owned(),
            ));
        }
        let foundation = ThresholdBundle::embedded()?;
        if self.bundles.is_empty() {
            if self.registry_digest != self.computed_digest() {
                return Err(AuthorityError::ThresholdRegistry(
                    "empty family calibration registry digest is invalid".to_owned(),
                ));
            }
            return Ok(());
        }
        let mut families = std::collections::BTreeSet::new();
        let mut digests = std::collections::BTreeSet::new();
        let mut bootstraps = std::collections::BTreeSet::new();
        for bundle in &self.bundles {
            let closure = FamilyCalibrationClosure::from_value(&bundle.digest_closure)
                .map_err(|error| AuthorityError::ThresholdRegistry(error.to_string()))?;
            let backend_digest = crate::vector::backends::backend_source_digest(
                &closure.backend.id,
                &closure.backend.version,
            );
            let valid_inventory = |fixture: &FixtureInventory| {
                let valid_source_family = match closure.family.as_str() {
                    "bounded-illustration" => matches!(
                        fixture.source_family.as_str(),
                        "color-illustration" | "transparent-illustration"
                    ),
                    family => fixture.source_family == family,
                };
                !fixture.id.is_empty()
                    && !fixture.ancestry.is_empty()
                    && valid_source_family
                    && is_sha256_hex(&fixture.input_sha256)
                    && matches!(fixture.split.as_str(), "calibration" | "holdout")
                    && matches!(
                        fixture.expected_support.as_str(),
                        "supported" | "unsupported" | "conditional" | "uncertain" | "reject"
                    )
            };
            let calibration_ancestries: std::collections::BTreeSet<&str> = closure
                .source_inventory
                .iter()
                .filter(|fixture| fixture.split == "calibration")
                .map(|fixture| fixture.ancestry.as_str())
                .collect();
            let holdout_ancestries: std::collections::BTreeSet<&str> = closure
                .source_inventory
                .iter()
                .filter(|fixture| fixture.split == "holdout")
                .map(|fixture| fixture.ancestry.as_str())
                .collect();
            bootstraps.insert(
                serde_json::to_string(&closure.bootstrap)
                    .map_err(|error| AuthorityError::ThresholdRegistry(error.to_string()))?,
            );
            let expected_bundle_digest = digest(
                &(serde_json::to_string(&bundle.digest_closure)
                    .map_err(|error| AuthorityError::ThresholdRegistry(error.to_string()))?
                    + "\n"),
            );
            let mut invalid = Vec::new();
            macro_rules! invalid_if {
                ($condition:expr, $reason:literal) => {
                    if $condition {
                        invalid.push($reason.to_owned());
                    }
                };
            }
            invalid_if!(bundle.schema_version != 2, "schemaVersion");
            invalid_if!(bundle.kind != "family-calibration-bundle", "kind");
            invalid_if!(!bundle.no_waivers || !closure.no_waivers, "noWaivers");
            invalid_if!(bundle.thresholds_changed, "thresholdsChanged");
            invalid_if!(
                bundle.bundle_digest != expected_bundle_digest,
                "bundleDigest"
            );
            invalid_if!(
                !FAMILY_CALIBRATIONS.contains(&closure.family.as_str()),
                "family"
            );
            invalid_if!(
                expected_backend(&closure.family) != Some(closure.backend.id.as_str()),
                "backend"
            );
            invalid_if!(closure.report_schema != REPORT_SCHEMA, "reportSchema");
            invalid_if!(
                !(match closure.family.as_str() {
                    "bounded-illustration" => closure
                        .adaptive_policy
                        .as_ref()
                        .is_some_and(is_expected_adaptive_policy),
                    _ => closure.adaptive_policy.is_none(),
                }),
                "adaptivePolicy"
            );
            invalid_if!(
                backend_digest.as_deref() != Some(closure.backend.source_digest.as_str()),
                "backendSourceDigest"
            );
            invalid_if!(
                closure.effective_thresholds != *foundation.defaults(),
                "effectiveThresholds"
            );
            invalid_if!(
                closure.fixed_gates != *foundation.fixed_gates(),
                "fixedGates"
            );
            invalid_if!(
                ![
                    &closure.corpus_lock_identity_sha256,
                    &closure.corpus_manifest_sha256,
                    &closure.corpus_generator_source_sha256,
                    &closure.corpus_schema_sha256,
                    &closure.corpus_license_inventory_sha256,
                    &closure.corpus_unique_artifact_inventory_sha256,
                    &closure.calibration_split_sha256,
                    &closure.holdout_split_sha256,
                    &closure.holdout_identity_sha256,
                    &closure.corpus_source_family_sha256,
                    &closure.corpus_defect_sha256,
                    &closure
                        .calibration_verifier
                        .calibration_verifier_source_sha256,
                    &closure.calibration_verifier.cargo_lock_sha256,
                ]
                .into_iter()
                .all(|value| is_sha256_hex(value)),
                "corpusDigestIdentity"
            );
            invalid_if!(
                !is_sha256_identity(&closure.bootstrap.run_digest)
                    || !is_sha256_hex(&closure.bootstrap.evaluation_sha256)
                    || !is_sha256_hex(&closure.bootstrap.review_sha256)
                    || !is_sha256_identity(&closure.bootstrap.result_matrix_digest),
                "bootstrapIdentity"
            );
            invalid_if!(closure.source_inventory.len() < 4, "sourceCoverage");
            invalid_if!(
                calibration_ancestries.len() < 2
                    || holdout_ancestries.len() < 2
                    || !calibration_ancestries.is_disjoint(&holdout_ancestries),
                "splitIndependence"
            );
            invalid_if!(closure.defect_inventory.is_empty(), "defectCoverage");
            invalid_if!(
                !closure.source_inventory.iter().all(valid_inventory),
                "sourceInventory"
            );
            invalid_if!(
                !closure.defect_inventory.iter().all(|defect| {
                    valid_inventory(&defect.fixture)
                        && !defect.defect.is_empty()
                        && !defect.expected_rejection.code.is_empty()
                        && !defect.expected_rejection.category.is_empty()
                        && defect.fixture.expected_support == "reject"
                        && defect.fixture.split == "calibration"
                }),
                "defectInventory"
            );
            invalid_if!(!families.insert(closure.family.clone()), "duplicateFamily");
            invalid_if!(
                !digests.insert(bundle.bundle_digest.clone()),
                "duplicateBundleDigest"
            );
            if !invalid.is_empty() {
                return Err(AuthorityError::ThresholdRegistry(format!(
                    "family calibration bundle is not a complete local corpus-v2 closure: {}",
                    invalid.join(", ")
                )));
            }
        }
        if bootstraps.len() != 1 {
            return Err(AuthorityError::ThresholdRegistry(
                "family calibration bundles must share one bootstrap identity".to_owned(),
            ));
        }
        if digests.len() != self.bundles.len() || self.registry_digest != self.computed_digest() {
            return Err(AuthorityError::ThresholdRegistry(
                "family calibration registry digest does not close over unique bundles".to_owned(),
            ));
        }
        Ok(())
    }

    fn computed_digest(&self) -> String {
        let mut bundle_digests: Vec<&str> = self
            .bundles
            .iter()
            .map(|bundle| bundle.bundle_digest.as_str())
            .collect();
        bundle_digests.sort_unstable();
        digest(&format!(
            "{}\n{}",
            self.registry_id,
            bundle_digests.join("\n")
        ))
    }
}
fn expected_backend(family: &str) -> Option<&'static str> {
    match family {
        "pixel-art" => Some("logical-grid-pixel-art"),
        "flat-icon" => Some("bounded-flat-regions"),
        "line-art" => Some("polarity-network-line-art"),
        "bounded-illustration" => Some("stable-solid-illustration"),
        _ => None,
    }
}
fn is_expected_adaptive_policy(policy: &AdaptivePolicyClosure) -> bool {
    policy.policy_identity == "adaptive-fragment-merge/2"
        && policy.model_identity == "stable-solid-rgba/2"
        && policy.selector_identity == "minimum-score-with-merge-credit/1"
        && policy.trace_identity == "perfectpixel.adaptive-trace/1"
        && policy.trace_encoding_version == 1
        && policy.fragment_pixels == 4
        && policy.protected_thin_pixels == 5
        && policy.minimum_plateau_coverage_percent == 60
        && policy.maximum_cost_q == 100_000
        && policy.selector_merge_credit_q == 1_000
        && policy.maximum_source_colors == 1_024
        && policy.maximum_fine_regions == 4_096
        && policy.maximum_adjacency_edges == 16_384
        && policy.maximum_merge_evaluations == 262_144
        && policy.maximum_retained_snapshots == 2
        && policy.maximum_queue_entries == 65_536
        && policy.maximum_emitted_paths == 32_768
}

fn is_milliquantized(value: f64) -> bool {
    value.is_finite() && (0.0..=1.0).contains(&value) && (value * 1_000.0).fract() == 0.0
}

#[cfg(test)]
mod tests {
    use super::{is_milliquantized, is_sha256_hex, is_sha256_identity};

    #[test]
    fn canonical_threshold_precision_is_collision_free() {
        assert!(is_milliquantized(0.995));
        assert!(is_milliquantized(0.990));
        assert!(!is_milliquantized(0.9950000000000001));
        assert!(!is_milliquantized(9.950000000000001e-1));
    }
    #[test]
    fn digest_identities_reject_zero_and_noncanonical_hex() {
        assert!(!is_sha256_hex(&"0".repeat(64)));
        assert!(!is_sha256_hex(&"A".repeat(64)));
        assert!(!is_sha256_identity(&format!("sha256-{}", "0".repeat(64))));
        assert!(is_sha256_identity(&format!("sha256-{}", "a".repeat(64))));
    }
}
