use crate::core::Raster;

use super::{
    authority, candidate, output_profile, policy, EvaluationDigests, OutputProfile, VectorPolicy,
    VectorRequest, VECTOR_EVALUATION_SCHEMA,
};

pub(super) fn raster_digest(image: &Raster) -> String {
    let mut identity = Vec::with_capacity(48 + image.pixels().len());
    identity.extend_from_slice(b"perfectpixel.raster-rgba8/1\0");
    identity.extend_from_slice(&image.width().to_be_bytes());
    identity.extend_from_slice(&image.height().to_be_bytes());
    identity.extend_from_slice(&(image.pixels().len() as u64).to_be_bytes());
    identity.extend_from_slice(image.pixels());
    candidate::sha256_identity(&identity)
}

pub(super) fn evaluation_digests(
    authority: &authority::VerifiedAuthority,
    route: Option<&authority::RouteEntry>,
    request: &VectorRequest,
    normalized: Option<&policy::NormalizedRequest>,
    motion: Option<String>,
    candidate_bytes: Option<String>,
) -> EvaluationDigests {
    let invariants = authority.invariants();
    let threshold_bundle = route
        .map(|entry| entry.threshold_bundle_digest.clone())
        .unwrap_or(invariants.threshold_bundle_digest);
    let foundation = route
        .map(|entry| entry.foundation_digest.clone())
        .unwrap_or_else(|| candidate::sha256_identity(invariants.foundation_id.as_bytes()));
    let report = route
        .map(|entry| entry.report_digest.clone())
        .unwrap_or_else(|| candidate::sha256_identity(VECTOR_EVALUATION_SCHEMA.as_bytes()));
    EvaluationDigests::new(
        route.map(|entry| candidate::sha256_identity(entry.backend.as_bytes())),
        route.map(|entry| entry.entry_digest.clone()),
        invariants.route_registry_digest,
        route.map(|entry| entry.entry_digest.clone()),
        Some(threshold_bundle),
        normalized.map(|value| {
            candidate::sha256_identity(
                &serde_json::to_vec(value).expect("normalized request is serializable"),
            )
        }),
        policy_digest(request.policy()),
        profile_digest(output_profile(request.profile())),
        foundation,
        report,
        motion,
        candidate_bytes,
    )
}

pub(super) fn policy_digest(policy: &VectorPolicy) -> String {
    candidate::sha256_identity(
        &serde_json::to_vec(policy).expect("validated vector policy is serializable"),
    )
}

pub(super) fn profile_digest(profile: OutputProfile) -> String {
    let canonical = match profile {
        OutputProfile::Compact => "compact",
        OutputProfile::MotionStructureReady => "motion-structure-ready",
    };
    candidate::sha256_identity(format!("perfectpixel.profile/1:{canonical}").as_bytes())
}
