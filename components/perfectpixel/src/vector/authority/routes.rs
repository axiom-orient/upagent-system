use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

#[cfg(test)]
use super::is_sha256_hex;
use super::{
    digest, is_nonzero_sha256_hex, is_sha256_identity,
    thresholds::{FamilyCalibrationRegistry, ThresholdBundle},
    AuthorityError, AuthorityResult,
};

const EMBEDDED_ROUTES: &str = include_str!("approved_routes/foundation.json");
const EMBEDDED_PROMOTION: &str = include_str!("approved_routes/promotion.json");
const POLICY_VERSION: &str = "perfectpixel.vector-policy/1";
const PROFILE_VERSION: &str = "perfectpixel.profile/1";
const EDITABLE_PROFILE: &str = "perfectpixel.svg-editable/1";
const MOTION_PROFILE: &str = "perfectpixel.svg-motion-structure/1";
const REPORT_SCHEMA: &str = "perfectpixel.vector-evaluation/3";
const REPORT_DIGEST: &str =
    "sha256-962c423b7aa6e6de99010847b38943e3c0c9c9683e84d53ec264d9bb7152bcef";
const FOUNDATION_DIGEST: &str =
    "sha256-68f9f730e7578760a5cc5e026db19133318bac0bed5c81b86b4001061a1175b7";

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct RouteKey {
    pub(crate) input_family: String,
    pub(crate) policy_version: String,
    pub(crate) profile_version: String,
    pub(crate) output_profile: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub(crate) enum RouteState {
    #[serde(rename = "foundationActive")]
    FoundationActive,
    #[serde(rename = "candidateShadow")]
    CandidateShadow,
    #[serde(rename = "promoted")]
    Promoted,
    #[serde(rename = "unsupported")]
    Unsupported,
}

#[cfg(test)]
impl RouteState {
    pub(crate) const fn may_publish(self) -> bool {
        matches!(self, Self::FoundationActive | Self::Promoted)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct RouteEntry {
    #[serde(flatten)]
    pub(crate) key: RouteKey,
    pub(crate) state: RouteState,
    pub(crate) backend: String,
    pub(crate) backend_version: String,
    pub(crate) backend_digest: String,
    pub(crate) threshold_set_id: String,
    pub(crate) foundation_id: String,
    pub(crate) threshold_bundle_digest: String,
    pub(crate) foundation_digest: String,
    pub(crate) report_schema: String,
    pub(crate) report_digest: String,
    pub(crate) predecessor_entry_digest: Option<String>,
    pub(crate) entry_digest: String,
}
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PromotionManifest {
    schema_version: u8,
    kind: String,
    manifest_digest: String,
    registry_digest: String,
    promotions: Vec<PromotionEvidence>,
}

#[derive(Debug, Clone, Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PromotionEvidence {
    family: String,
    input_family: String,
    policy_version: String,
    profile_version: String,
    output_profile: String,
    family_threshold_bundle_digest: String,
    predecessor_entry_digest: String,
    promoted_entry_digest: String,
    evaluation_sha256: String,
    review_sha256: String,
    decision: String,
    machine_pass: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    provisional: bool,
}

impl PromotionManifest {
    fn embedded() -> AuthorityResult<Self> {
        let manifest: Self = serde_json::from_str(EMBEDDED_PROMOTION)
            .map_err(|error| AuthorityError::RouteRegistry(error.to_string()))?;
        let closure = serde_json::json!({
            "schemaVersion": manifest.schema_version,
            "kind": &manifest.kind,
            "registryDigest": &manifest.registry_digest,
            "promotions": &manifest.promotions,
        });
        if manifest.schema_version != 1
            || manifest.kind != "family-calibration-promotion-manifest"
            || !is_sha256_identity(&manifest.manifest_digest)
            || !is_sha256_identity(&manifest.registry_digest)
            || manifest.manifest_digest
                != digest(
                    &(serde_json::to_string(&closure)
                        .map_err(|error| AuthorityError::RouteRegistry(error.to_string()))?
                        + "\n"),
                )
        {
            return Err(AuthorityError::RouteRegistry(
                "promotion manifest does not close over its canonical contents".to_owned(),
            ));
        }
        Ok(manifest)
    }

    fn verifies_exact_coverage(&self, registry: &RouteRegistry) -> bool {
        let promoted: BTreeSet<RouteKey> = registry
            .routes
            .iter()
            .filter(|route| route.state == RouteState::Promoted)
            .map(|route| route.key.clone())
            .collect();
        let evidenced: BTreeSet<RouteKey> = self
            .promotions
            .iter()
            .map(|evidence| RouteKey {
                input_family: evidence.input_family.clone(),
                policy_version: evidence.policy_version.clone(),
                profile_version: evidence.profile_version.clone(),
                output_profile: evidence.output_profile.clone(),
            })
            .collect();
        promoted == evidenced && self.promotions.len() == evidenced.len()
    }

    fn authorizes(&self, route: &RouteEntry, calibrations: &FamilyCalibrationRegistry) -> bool {
        self.promotions.iter().any(|evidence| {
            let complete_evidence = !evidence.provisional
                && is_nonzero_sha256_hex(&evidence.evaluation_sha256)
                && is_nonzero_sha256_hex(&evidence.review_sha256);
            #[cfg(test)]
            let complete_evidence =
                complete_evidence || (evidence.provisional && is_exact_calibration_harness());
            evidence.family == route.key.input_family
                && evidence.input_family == route.key.input_family
                && evidence.policy_version == route.key.policy_version
                && evidence.profile_version == route.key.profile_version
                && evidence.output_profile == route.key.output_profile
                && evidence.family_threshold_bundle_digest == route.threshold_bundle_digest
                && evidence.predecessor_entry_digest
                    == route.predecessor_entry_digest.as_deref().unwrap_or("")
                && evidence.promoted_entry_digest == route.entry_digest
                && evidence.decision == "machine-pass"
                && evidence.machine_pass
                && complete_evidence
                && calibrations
                    .verified_bundle(&route.key.input_family)
                    .is_some_and(|bundle| bundle.digest == evidence.family_threshold_bundle_digest)
        })
    }
}

#[cfg(test)]
fn is_exact_calibration_harness() -> bool {
    std::env::var_os("PERFECTPIXEL_FAMILY_CALIBRATION_HARNESS").as_deref()
        == Some(std::ffi::OsStr::new("1"))
        && matches!(
            std::thread::current().name(),
            Some(
                "vector::tests::family_calibration_harness_from_environment"
                    | "vector::quality::tests::controlled_defect_probe_from_environment"
            )
        )
}

fn is_false(value: &bool) -> bool {
    !value
}

impl RouteEntry {
    #[cfg(test)]
    pub(crate) const fn may_publish(&self) -> bool {
        self.state.may_publish()
    }

    fn verify(
        &self,
        thresholds: &ThresholdBundle,
        calibrations: &FamilyCalibrationRegistry,
        promotion: &PromotionManifest,
    ) -> AuthorityResult<()> {
        let (expected_backend, expected_backend_version) = backend_for(&self.key.input_family)
            .ok_or_else(|| {
                AuthorityError::RouteRegistry(
                    "route input family is not supported by the authority".to_owned(),
                )
            })?;
        if self.key.policy_version != POLICY_VERSION
            || self.key.profile_version != PROFILE_VERSION
            || !matches!(
                self.key.output_profile.as_str(),
                EDITABLE_PROFILE | MOTION_PROFILE
            )
            || self.backend != expected_backend
            || self.backend_version != expected_backend_version
            || crate::vector::backends::backend_source_digest(&self.backend, &self.backend_version)
                .as_deref()
                != Some(self.backend_digest.as_str())
            || self.foundation_id != thresholds.foundation_id()
            || self.foundation_digest != FOUNDATION_DIGEST
            || self.report_schema != REPORT_SCHEMA
            || self.report_digest != REPORT_DIGEST
            || {
                let family_bundle = calibrations.verified_bundle(&self.key.input_family);
                let is_promoted_route = self.state == RouteState::Promoted;
                let uses_foundation = self.threshold_set_id == thresholds.id()
                    && self.threshold_bundle_digest == thresholds.digest();
                let uses_family_bundle = family_bundle.as_ref().is_some_and(|bundle| {
                    self.threshold_set_id
                        == FamilyCalibrationRegistry::threshold_set_id(&bundle.family)
                            .as_deref()
                            .unwrap_or("")
                        && self.threshold_bundle_digest == bundle.digest
                        && self.backend == bundle.backend_id
                        && self.backend_version == bundle.backend_version
                        && self.backend_digest == bundle.backend_source_digest
                        && self.report_schema == bundle.report_schema
                });
                (is_promoted_route
                    && (!uses_family_bundle || !promotion.authorizes(self, calibrations)))
                    || (self.state != RouteState::Promoted && !uses_foundation)
            }
        {
            return Err(AuthorityError::RouteRegistry(
                "route does not bind the approved key, backend, threshold, foundation, and report identities".to_owned(),
            ));
        }
        if !is_sha256_identity(&self.entry_digest)
            || self
                .predecessor_entry_digest
                .as_deref()
                .is_some_and(|digest| !is_sha256_identity(digest))
            || self.entry_digest != self.computed_digest()
        {
            return Err(AuthorityError::RouteRegistry(
                "route entry or predecessor digest is not a valid SHA-256 closure identity"
                    .to_owned(),
            ));
        }
        Ok(())
    }

    fn computed_digest(&self) -> String {
        digest(&format!(
            "{}\n{}\n{}\n{}\n{}\n{}\n{}\n{}\n{}\n{}\n{}\n{}\n{}\n{}\n{}",
            self.key.input_family,
            self.key.policy_version,
            self.key.profile_version,
            self.key.output_profile,
            state_name(self.state),
            self.backend,
            self.backend_version,
            self.backend_digest,
            self.threshold_set_id,
            self.foundation_id,
            self.threshold_bundle_digest,
            self.foundation_digest,
            self.report_schema,
            self.report_digest,
            self.predecessor_entry_digest.as_deref().unwrap_or(""),
        ))
    }
}
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct RouteRegistry {
    registry_id: String,
    registry_digest: String,
    routes: Vec<RouteEntry>,
}

impl RouteRegistry {
    pub(crate) fn embedded(thresholds: &ThresholdBundle) -> AuthorityResult<Self> {
        let registry: Self = serde_json::from_str(EMBEDDED_ROUTES)
            .map_err(|error| AuthorityError::RouteRegistry(error.to_string()))?;
        let calibrations = FamilyCalibrationRegistry::embedded()?;
        let promotion = PromotionManifest::embedded()?;
        if promotion.registry_digest != registry.registry_digest {
            return Err(AuthorityError::RouteRegistry(
                "promotion manifest does not bind the current route registry".to_owned(),
            ));
        }
        if !promotion.verifies_exact_coverage(&registry) {
            return Err(AuthorityError::RouteRegistry(
                "promotion manifest coverage does not exactly match promoted routes".to_owned(),
            ));
        }
        registry.verify(thresholds, &calibrations, &promotion)?;
        Ok(registry)
    }

    pub(crate) fn id(&self) -> &str {
        &self.registry_id
    }
    pub(crate) fn digest(&self) -> &str {
        &self.registry_digest
    }
    pub(crate) fn route(&self, key: &RouteKey) -> Option<&RouteEntry> {
        self.routes.iter().find(|entry| entry.key == *key)
    }

    #[cfg(test)]
    pub(crate) fn promote(
        &self,
        intended: &RouteKey,
        candidate: RouteEntry,
        thresholds: &ThresholdBundle,
    ) -> AuthorityResult<Self> {
        let current = self.target(intended)?;
        if candidate.key != *intended || candidate.state != RouteState::Promoted {
            return Err(AuthorityError::Transition(
                "promotion must target the exact promoted route key".to_owned(),
            ));
        }
        if candidate.predecessor_entry_digest.as_deref() != Some(current.entry_digest.as_str()) {
            return Err(AuthorityError::Transition(
                "promotion candidate does not name the current route as its predecessor".to_owned(),
            ));
        }
        let calibrations = FamilyCalibrationRegistry::embedded()?;
        let promotion = PromotionManifest::embedded()?;
        candidate.verify(thresholds, &calibrations, &promotion)?;
        self.replace(intended, candidate, thresholds, &calibrations, &promotion)
    }

    #[cfg(test)]
    pub(crate) fn rollback(
        &self,
        intended: &RouteKey,
        predecessor: RouteEntry,
        thresholds: &ThresholdBundle,
    ) -> AuthorityResult<Self> {
        let current = self.target(intended)?;
        if current.state != RouteState::Promoted
            || predecessor.key != *intended
            || current.predecessor_entry_digest.as_deref()
                != Some(predecessor.entry_digest.as_str())
        {
            return Err(AuthorityError::Transition(
                "rollback predecessor is not the exact predecessor of the intended route"
                    .to_owned(),
            ));
        }
        let calibrations = FamilyCalibrationRegistry::embedded()?;
        let promotion = PromotionManifest::embedded()?;
        predecessor.verify(thresholds, &calibrations, &promotion)?;
        self.replace(intended, predecessor, thresholds, &calibrations, &promotion)
    }

    #[cfg(test)]
    fn target(&self, key: &RouteKey) -> AuthorityResult<&RouteEntry> {
        self.route(key).ok_or_else(|| {
            AuthorityError::Transition("intended route key is not registered".to_owned())
        })
    }

    #[cfg(test)]
    fn replace(
        &self,
        intended: &RouteKey,
        replacement: RouteEntry,
        thresholds: &ThresholdBundle,
        calibrations: &FamilyCalibrationRegistry,
        promotion: &PromotionManifest,
    ) -> AuthorityResult<Self> {
        let mut snapshot = self.clone();
        let index = snapshot
            .routes
            .iter()
            .position(|entry| entry.key == *intended)
            .expect("target was checked before snapshot replacement");
        snapshot.routes[index] = replacement;
        snapshot.registry_digest = snapshot.computed_digest();
        snapshot.verify_snapshot(thresholds, calibrations, promotion)?;
        Ok(snapshot)
    }

    fn verify(
        &self,
        thresholds: &ThresholdBundle,
        calibrations: &FamilyCalibrationRegistry,
        promotion: &PromotionManifest,
    ) -> AuthorityResult<()> {
        self.verify_inner(thresholds, calibrations, promotion, true)
    }
    #[cfg(test)]
    fn verify_snapshot(
        &self,
        thresholds: &ThresholdBundle,
        calibrations: &FamilyCalibrationRegistry,
        promotion: &PromotionManifest,
    ) -> AuthorityResult<()> {
        self.verify_inner(thresholds, calibrations, promotion, false)
    }

    fn verify_inner(
        &self,
        thresholds: &ThresholdBundle,
        calibrations: &FamilyCalibrationRegistry,
        promotion: &PromotionManifest,
        require_predecessor_closure: bool,
    ) -> AuthorityResult<()> {
        if self.registry_id != "foundation-routes/1"
            || self.routes.len() != 10
            || !is_sha256_identity(&self.registry_digest)
        {
            return Err(AuthorityError::RouteRegistry(
                "registry identity or required route cardinality is invalid".to_owned(),
            ));
        }
        let mut keys = BTreeSet::new();
        let mut digests = BTreeSet::new();
        for entry in &self.routes {
            if !keys.insert(entry.key.clone()) || !digests.insert(entry.entry_digest.clone()) {
                return Err(AuthorityError::RouteRegistry(
                    "route keys and SHA entry identities must be unique".to_owned(),
                ));
            }
            entry.verify(thresholds, calibrations, promotion)?;
        }
        if require_predecessor_closure {
            for family in [
                "pixel-art",
                "exact-grid",
                "flat-icon",
                "line-art",
                "bounded-illustration",
            ] {
                for output in [EDITABLE_PROFILE, MOTION_PROFILE] {
                    let key = RouteKey {
                        input_family: family.to_owned(),
                        policy_version: POLICY_VERSION.to_owned(),
                        profile_version: PROFILE_VERSION.to_owned(),
                        output_profile: output.to_owned(),
                    };
                    let entry = self.route(&key).ok_or_else(|| {
                        AuthorityError::RouteRegistry("required route key is missing".to_owned())
                    })?;
                    let state_is_valid = entry.state == RouteState::Unsupported
                        || if family == "exact-grid" && output == EDITABLE_PROFILE {
                            entry.state == RouteState::FoundationActive
                        } else {
                            matches!(
                                entry.state,
                                RouteState::CandidateShadow | RouteState::Promoted
                            )
                        };
                    if !state_is_valid {
                        return Err(AuthorityError::RouteRegistry(
                            "route state is not authorized by foundation or family evidence"
                                .to_owned(),
                        ));
                    }
                    if entry.state == RouteState::Promoted
                        && entry.predecessor_entry_digest.is_none()
                    {
                        return Err(AuthorityError::RouteRegistry(
                            "promoted route must bind its approved predecessor".to_owned(),
                        ));
                    }
                }
            }
        }
        if self.registry_digest != self.computed_digest() {
            return Err(AuthorityError::RouteRegistry(
                "registry digest does not match canonical SHA route contents".to_owned(),
            ));
        }
        Ok(())
    }

    fn computed_digest(&self) -> String {
        let mut route_digests: Vec<&str> = self
            .routes
            .iter()
            .map(|entry| entry.entry_digest.as_str())
            .collect();
        route_digests.sort_unstable();
        digest(&format!(
            "{}\n{}",
            self.registry_id,
            route_digests.join("\n")
        ))
    }
}

fn backend_for(family: &str) -> Option<(&'static str, &'static str)> {
    match family {
        "pixel-art" => Some(("logical-grid-pixel-art", "1")),
        "exact-grid" => Some(("perfectpixel/exact-grid/1", "1")),
        "flat-icon" => Some(("bounded-flat-regions", "1")),
        "line-art" => Some(("polarity-network-line-art", "1")),
        "bounded-illustration" => Some(("stable-solid-illustration", "2")),
        _ => None,
    }
}

fn state_name(state: RouteState) -> &'static str {
    match state {
        RouteState::FoundationActive => "foundationActive",
        RouteState::CandidateShadow => "candidateShadow",
        RouteState::Promoted => "promoted",
        RouteState::Unsupported => "unsupported",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn promotion_fixture() -> (ThresholdBundle, RouteRegistry, RouteEntry, RouteEntry) {
        let thresholds = ThresholdBundle::embedded().unwrap();
        let calibrations = FamilyCalibrationRegistry::embedded().unwrap();
        let registry = RouteRegistry::embedded(&thresholds).unwrap();
        let promoted = registry
            .routes
            .iter()
            .find(|entry| {
                entry.state == RouteState::Promoted && entry.key.output_profile == EDITABLE_PROFILE
            })
            .unwrap()
            .clone();
        let mut predecessor = promoted.clone();
        predecessor.state = RouteState::CandidateShadow;
        predecessor.threshold_set_id = thresholds.id().to_owned();
        predecessor.threshold_bundle_digest = thresholds.digest().to_owned();
        predecessor.predecessor_entry_digest = None;
        predecessor.entry_digest = predecessor.computed_digest();
        assert_eq!(
            promoted.predecessor_entry_digest.as_deref(),
            Some(predecessor.entry_digest.as_str())
        );

        let mut base = registry;
        let index = base
            .routes
            .iter()
            .position(|entry| entry.key == predecessor.key)
            .unwrap();
        base.routes[index] = predecessor.clone();
        base.registry_digest = base.computed_digest();
        base.verify_snapshot(
            &thresholds,
            &calibrations,
            &PromotionManifest::embedded().unwrap(),
        )
        .unwrap();
        (thresholds, base, predecessor, promoted)
    }

    #[test]
    fn promotion_and_rollback_preserve_non_target_routes() {
        let (thresholds, registry, predecessor, candidate) = promotion_fixture();
        let intended = predecessor.key.clone();
        let untouched = registry
            .routes
            .iter()
            .find(|entry| entry.key != intended)
            .unwrap()
            .clone();

        let promoted = registry.promote(&intended, candidate, &thresholds).unwrap();
        assert_eq!(promoted.route(&untouched.key), Some(&untouched));
        assert!(promoted.route(&intended).unwrap().may_publish());
        let rolled_back = promoted
            .rollback(&intended, predecessor.clone(), &thresholds)
            .unwrap();
        assert_eq!(rolled_back.route(&intended), Some(&predecessor));
        assert_eq!(rolled_back.route(&untouched.key), Some(&untouched));
    }

    #[test]
    fn transition_rejects_invalid_predecessor_and_non_target_key() {
        let (thresholds, registry, predecessor, mut candidate) = promotion_fixture();
        let intended = predecessor.key.clone();
        let other = registry
            .routes
            .iter()
            .find(|entry| entry.key != intended)
            .unwrap()
            .key
            .clone();
        candidate.predecessor_entry_digest = Some(
            "sha256-0000000000000000000000000000000000000000000000000000000000000000".to_owned(),
        );
        candidate.entry_digest = candidate.computed_digest();
        assert!(registry
            .promote(&intended, candidate.clone(), &thresholds)
            .is_err());
        candidate.key = other;
        candidate.entry_digest = candidate.computed_digest();
        assert!(registry.promote(&intended, candidate, &thresholds).is_err());
    }
    #[test]
    fn digest_identities_reject_zero_and_noncanonical_hex() {
        assert!(!is_sha256_hex(&"0".repeat(64)));
        assert!(!is_sha256_hex(&"F".repeat(64)));
        assert!(!is_sha256_identity(&format!("sha256-{}", "0".repeat(64))));
        assert!(is_sha256_identity(&format!("sha256-{}", "f".repeat(64))));
    }
}
