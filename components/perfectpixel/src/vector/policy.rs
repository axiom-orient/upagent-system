//! Private request normalization shared by the library and CLI entry points.
//! Authority is supplied only by the embedded registry integration, never by a request or policy.

use std::collections::BTreeMap;

use super::report::{AnalysisNormalization, PredicateAvailability};
use super::request::{VectorAnalysisRequest, VectorPresetSelection};
use serde::{Deserialize, Serialize};

use super::profile::{AutoDisposition, RasterEvidenceProfile};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum PresetFamily {
    PixelArt,
    ExactGrid,
    Icon,
    LineArt,
    Color,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum OutputProfile {
    Compact,
    MotionStructureReady,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum SourceRepresentation {
    Rgba8Raster,
    IndexedRaster,
    Other,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct VectorRequest {
    pub preset: Option<PresetFamily>,
    pub profile: Option<OutputProfile>,
    pub detail: Option<u8>,
    pub minimum_quality: Option<f64>,
    pub maximum_quality_loss: Option<f64>,
    pub maximum_paths: Option<usize>,
    pub representation: SourceRepresentation,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PolicyConstraints {
    pub allowed_families: Option<Vec<PresetFamily>>,
    pub allowed_profiles: Option<Vec<OutputProfile>>,
    pub maximum_detail: Option<u8>,
    pub minimum_quality: Option<f64>,
    pub maximum_quality_loss: Option<f64>,
    pub maximum_paths: Option<usize>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct EmbeddedRouteLimits {
    pub minimum_quality: f64,
    pub maximum_quality_loss: f64,
    pub maximum_paths: Option<usize>,
    pub maximum_detail: u8,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub(crate) enum NormalizationRejection {
    PresetRequired,
    UnsupportedRepresentation,
    UnsupportedContent,
    PolicyInfeasible,
    InvalidRequest,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RelaxationAttempt {
    pub field: &'static str,
    pub requested: String,
    pub approved: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct NormalizedRequest {
    pub family: PresetFamily,
    pub profile: OutputProfile,
    pub detail: Option<u8>,
    pub minimum_quality: f64,
    pub maximum_quality_loss: f64,
    pub maximum_paths: Option<usize>,
    pub relaxation_attempts: Vec<RelaxationAttempt>,
}

pub(crate) fn normalize_request(
    request: &VectorRequest,
    policy: &PolicyConstraints,
    route: EmbeddedRouteLimits,
    profile: &RasterEvidenceProfile,
) -> Result<NormalizedRequest, NormalizationRejection> {
    if !matches!(
        request.representation,
        SourceRepresentation::Rgba8Raster | SourceRepresentation::IndexedRaster
    ) {
        return Err(NormalizationRejection::UnsupportedRepresentation);
    }
    if !request_valid(request) {
        return Err(NormalizationRejection::InvalidRequest);
    }
    if !policy_valid(policy) {
        return Err(NormalizationRejection::PolicyInfeasible);
    }
    if unsupported_content(profile) {
        return Err(NormalizationRejection::UnsupportedContent);
    }
    let family = match request.preset {
        Some(family) => family,
        None => auto_family(profile)?,
    };
    let output_profile = request.profile.unwrap_or(OutputProfile::Compact);
    if !allowed(policy.allowed_families.as_deref(), family)
        || !allowed(policy.allowed_profiles.as_deref(), output_profile)
    {
        return Err(NormalizationRejection::PolicyInfeasible);
    }
    let mut relaxation_attempts = Vec::new();
    let minimum_quality = tighten_minimum(
        "minimumQuality",
        request.minimum_quality,
        policy.minimum_quality,
        route.minimum_quality,
        &mut relaxation_attempts,
    );
    let maximum_quality_loss = tighten_maximum(
        "maximumQualityLoss",
        request.maximum_quality_loss,
        policy.maximum_quality_loss,
        route.maximum_quality_loss,
        &mut relaxation_attempts,
    );
    let maximum_paths = tighten_optional_maximum(
        "maximumPaths",
        request.maximum_paths,
        policy.maximum_paths,
        route.maximum_paths,
        &mut relaxation_attempts,
    );
    let detail_cap = policy.maximum_detail.map_or(route.maximum_detail, |limit| {
        limit.min(route.maximum_detail)
    });
    let detail = request.detail.map(|detail| {
        if detail > detail_cap {
            relaxation_attempts.push(RelaxationAttempt {
                field: "detail",
                requested: detail.to_string(),
                approved: detail_cap.to_string(),
            });
            detail_cap
        } else {
            detail
        }
    });
    Ok(NormalizedRequest {
        family,
        profile: output_profile,
        detail,
        minimum_quality,
        maximum_quality_loss,
        maximum_paths,
        relaxation_attempts,
    })
}
/// Produces candidate-free normalization facts for analysis reports.
pub(crate) fn analysis_normalization(
    request: &VectorAnalysisRequest,
    normalized: Result<&NormalizedRequest, &NormalizationRejection>,
) -> AnalysisNormalization {
    let mut applicability = BTreeMap::new();
    let mut reasons = Vec::new();
    match normalized {
        Ok(normalized) => {
            applicability.insert("policy".to_owned(), PredicateAvailability::Passed);
            applicability.insert("routeSelection".to_owned(), PredicateAvailability::Passed);
            applicability.insert("outputProfile".to_owned(), PredicateAvailability::Passed);
            reasons.push(format!("family:{}", family_name(normalized.family)));
            reasons.push(format!("profile:{}", profile_name(normalized.profile)));
        }
        Err(rejection) => {
            let (policy, route_selection) = match rejection {
                NormalizationRejection::PolicyInfeasible => (
                    PredicateAvailability::Failed,
                    PredicateAvailability::NotEvaluated,
                ),
                NormalizationRejection::PresetRequired
                | NormalizationRejection::UnsupportedRepresentation
                | NormalizationRejection::UnsupportedContent => {
                    (PredicateAvailability::Passed, PredicateAvailability::Failed)
                }
                NormalizationRejection::InvalidRequest => (
                    PredicateAvailability::NotEvaluated,
                    PredicateAvailability::NotEvaluated,
                ),
            };
            applicability.insert("policy".to_owned(), policy);
            applicability.insert("routeSelection".to_owned(), route_selection);
            applicability.insert(
                "outputProfile".to_owned(),
                PredicateAvailability::NotEvaluated,
            );
            reasons.push(format!("normalization:{}", rejection_name(rejection)));
        }
    }
    AnalysisNormalization::new(
        request.policy().schema().to_owned(),
        request.policy().version().to_owned(),
        applicability,
        reasons,
    )
}

/// Returns one explicit recommendation only when factorized profiling classified the input.
pub(crate) fn recommended_presets(profile: &RasterEvidenceProfile) -> Vec<VectorPresetSelection> {
    match profile.auto_disposition {
        AutoDisposition::Classified(super::profile::RasterSignalClass::PixelArt) => {
            vec![VectorPresetSelection::PixelArt]
        }
        AutoDisposition::Classified(super::profile::RasterSignalClass::FlatIcon) => {
            vec![VectorPresetSelection::FlatIcon]
        }
        AutoDisposition::Classified(
            super::profile::RasterSignalClass::TransparentIllustration
            | super::profile::RasterSignalClass::ColorIllustration,
        ) => vec![VectorPresetSelection::BoundedIllustration],
        AutoDisposition::Classified(super::profile::RasterSignalClass::ContinuousTone)
        | AutoDisposition::PresetRequired
        | AutoDisposition::Unsupported => Vec::new(),
    }
}

fn unsupported_content(profile: &RasterEvidenceProfile) -> bool {
    matches!(
        profile.auto_disposition,
        AutoDisposition::Unsupported
            | AutoDisposition::Classified(super::profile::RasterSignalClass::ContinuousTone)
    )
}

fn auto_family(profile: &RasterEvidenceProfile) -> Result<PresetFamily, NormalizationRejection> {
    match profile.auto_disposition {
        AutoDisposition::PresetRequired => Err(NormalizationRejection::PresetRequired),
        AutoDisposition::Unsupported
        | AutoDisposition::Classified(super::profile::RasterSignalClass::ContinuousTone) => {
            Err(NormalizationRejection::UnsupportedContent)
        }
        AutoDisposition::Classified(class) => Ok(match class {
            super::profile::RasterSignalClass::PixelArt => PresetFamily::PixelArt,
            super::profile::RasterSignalClass::FlatIcon => PresetFamily::Icon,
            super::profile::RasterSignalClass::TransparentIllustration
            | super::profile::RasterSignalClass::ColorIllustration => PresetFamily::Color,
            super::profile::RasterSignalClass::ContinuousTone => {
                unreachable!("continuous-tone content is rejected before route selection")
            }
        }),
    }
}

fn family_name(family: PresetFamily) -> &'static str {
    match family {
        PresetFamily::PixelArt => "pixel-art",
        PresetFamily::ExactGrid => "exact-grid",
        PresetFamily::Icon => "icon",
        PresetFamily::LineArt => "line-art",
        PresetFamily::Color => "color",
    }
}

fn profile_name(profile: OutputProfile) -> &'static str {
    match profile {
        OutputProfile::Compact => "compact",
        OutputProfile::MotionStructureReady => "motion-structure-ready",
    }
}

fn rejection_name(rejection: &NormalizationRejection) -> &'static str {
    match rejection {
        NormalizationRejection::PresetRequired => "PRESET_REQUIRED",
        NormalizationRejection::UnsupportedRepresentation => "UNSUPPORTED_REPRESENTATION",
        NormalizationRejection::UnsupportedContent => "UNSUPPORTED_CONTENT",
        NormalizationRejection::PolicyInfeasible => "POLICY_INFEASIBLE",
        NormalizationRejection::InvalidRequest => "INVALID_REQUEST",
    }
}

fn allowed<T: Copy + PartialEq>(allowed: Option<&[T]>, selected: T) -> bool {
    allowed.is_none_or(|values| values.contains(&selected))
}

fn request_valid(request: &VectorRequest) -> bool {
    request.detail.is_none_or(|value| (1..=5).contains(&value))
        && request.minimum_quality.is_none_or(valid_unit)
        && request.maximum_quality_loss.is_none_or(valid_unit)
}

fn policy_valid(policy: &PolicyConstraints) -> bool {
    policy
        .maximum_detail
        .is_none_or(|value| (1..=5).contains(&value))
        && policy.minimum_quality.is_none_or(valid_unit)
        && policy.maximum_quality_loss.is_none_or(valid_unit)
}

fn valid_unit(value: f64) -> bool {
    value.is_finite() && (0.0..=1.0).contains(&value)
}

fn tighten_minimum(
    field: &'static str,
    requested: Option<f64>,
    policy: Option<f64>,
    approved: f64,
    attempts: &mut Vec<RelaxationAttempt>,
) -> f64 {
    let requested = requested.unwrap_or(approved);
    if requested < approved {
        attempts.push(RelaxationAttempt {
            field,
            requested: requested.to_string(),
            approved: approved.to_string(),
        });
    }
    policy.unwrap_or(approved).max(requested).max(approved)
}

fn tighten_maximum(
    field: &'static str,
    requested: Option<f64>,
    policy: Option<f64>,
    approved: f64,
    attempts: &mut Vec<RelaxationAttempt>,
) -> f64 {
    let requested = requested.unwrap_or(approved);
    if requested > approved {
        attempts.push(RelaxationAttempt {
            field,
            requested: requested.to_string(),
            approved: approved.to_string(),
        });
    }
    policy.unwrap_or(approved).min(requested).min(approved)
}

fn tighten_optional_maximum(
    field: &'static str,
    requested: Option<usize>,
    policy: Option<usize>,
    approved: Option<usize>,
    attempts: &mut Vec<RelaxationAttempt>,
) -> Option<usize> {
    match (requested, policy, approved) {
        (None, None, limit) => limit,
        (requested, policy, Some(approved)) => {
            if requested.is_some_and(|value| value > approved)
                || policy.is_some_and(|value| value > approved)
            {
                attempts.push(RelaxationAttempt {
                    field,
                    requested: requested.or(policy).unwrap().to_string(),
                    approved: approved.to_string(),
                });
            }
            Some(
                requested
                    .into_iter()
                    .chain(policy)
                    .chain(Some(approved))
                    .min()
                    .unwrap(),
            )
        }
        (requested, policy, None) => requested.into_iter().chain(policy).min(),
    }
}
