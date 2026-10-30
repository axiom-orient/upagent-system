use std::collections::BTreeMap;

use resvg::{tiny_skia, usvg};

use crate::adapters::motion::{assess_authorized_motion_structure, MotionAssessmentStatus};
use crate::core::{PpError, PpResult, Raster, SvgContract};

mod approval;
mod authority;
mod backends;
mod candidate;
mod identity;
mod outcome;
mod policy;
mod profile;
#[path = "quality/mod.rs"]
mod quality;
mod report;
mod request;

fn canonical_rgba(pixel: &[u8]) -> [u8; 4] {
    if pixel[3] == 0 {
        [0, 0, 0, 0]
    } else {
        [pixel[0], pixel[1], pixel[2], pixel[3]]
    }
}

use identity::{evaluation_digests, policy_digest, profile_digest, raster_digest};
use policy::{
    EmbeddedRouteLimits, NormalizationRejection, OutputProfile, PolicyConstraints, PresetFamily,
    SourceRepresentation,
};
use quality::{
    adaptive_protected_geometry_assessment, compact_structural_assessment, compare_rasters,
    input_resource_gate_assessment, protected_palette_gate, protected_raster_gate_assessment,
};
use report::TopologyFacts;

pub use approval::ApprovedVectorOutput;
pub use outcome::{RejectedVectorOutput, VectorOutcome, VectorRejectionCode};
pub use report::{
    AnalysisNormalization, ArtifactIntent, CandidateFacts, ConstraintValues, DiagnosticArtifact,
    DiagnosticArtifactSet, EvaluationDecision, EvaluationDigests, EvaluationReport,
    GateActualValue, GateComparator, GateMeasurement, GateMeasurementFamilies, GateThreshold,
    PredicateAvailability, ProfileEvidence, VectorAnalysis, VECTOR_ANALYSIS_SCHEMA,
    VECTOR_EVALUATION_SCHEMA,
};
pub use request::{
    DiagnosticsIntent, RequestValidationError, SvgProfile, UnitScore, VectorAnalysisRequest,
    VectorDetail, VectorPolicy, VectorPresetSelection, VectorRequest,
};

fn profile_evidence(profile: &profile::RasterEvidenceProfile) -> PpResult<ProfileEvidence> {
    let unit = UnitScore::new(profile.confidence)
        .map_err(|error| PpError::Vectorizer(error.to_string()))?;
    Ok(ProfileEvidence::new(
        BTreeMap::from([
            (
                "horizontalRunRatio".to_owned(),
                profile.geometry.horizontal_run_ratio,
            ),
            (
                "strongEdgeRatio".to_owned(),
                profile.geometry.strong_edge_ratio,
            ),
            (
                "hardGridLikelihood".to_owned(),
                profile.geometry.hard_grid_likelihood,
            ),
        ]),
        BTreeMap::from([
            (
                "uniqueColorCount".to_owned(),
                profile.paint.unique_color_count as f64,
            ),
            (
                "paletteLikelihood".to_owned(),
                profile.paint.palette_likelihood,
            ),
        ]),
        BTreeMap::from([
            (
                "transparentPixelRatio".to_owned(),
                profile.alpha.transparent_pixel_ratio,
            ),
            (
                "partialAlphaRatio".to_owned(),
                profile.alpha.partial_alpha_ratio,
            ),
        ]),
        BTreeMap::from([
            (
                "pixelCount".to_owned(),
                profile.complexity.pixel_count as f64,
            ),
            (
                "estimatedRegionComplexity".to_owned(),
                profile.complexity.estimated_region_complexity,
            ),
        ]),
        BTreeMap::from([
            (
                "isolatedPixelRatio".to_owned(),
                profile.source_noise.isolated_pixel_ratio,
            ),
            (
                "sourceNoiseLikelihood".to_owned(),
                profile.source_noise.source_noise_likelihood,
            ),
        ]),
        unit,
        profile
            .conflicts
            .iter()
            .map(|conflict| format!("{conflict:?}"))
            .collect(),
        profile.abstains,
        if profile.is_supported_for_auto() {
            PredicateAvailability::Passed
        } else {
            PredicateAvailability::Failed
        },
        policy::recommended_presets(profile),
    ))
}

/// Stable vector runtime. The embedded registry is the sole publication authority.
#[derive(Debug, Clone)]
pub struct Vectorizer {
    authority: authority::VerifiedAuthority,
}

impl Vectorizer {
    pub fn new() -> PpResult<Self> {
        let authority = authority::VerifiedAuthority::embedded().map_err(|error| {
            PpError::Vectorizer(format!(
                "embedded vector authority verification failed: {error}"
            ))
        })?;
        Ok(Self { authority })
    }

    /// Profiles and resolves a candidate-free request. Route and abstention facts are exit-neutral.
    pub fn analyze(
        &self,
        image: &Raster,
        request: &VectorAnalysisRequest,
    ) -> PpResult<VectorAnalysis> {
        let profile = profile::analyze_content(image);
        let evidence = profile_evidence(&profile)?;
        let private = policy::VectorRequest {
            preset: preset_family(request.preset()),
            profile: Some(output_profile(request.profile())),
            detail: None,
            minimum_quality: None,
            maximum_quality_loss: None,
            maximum_paths: None,
            representation: SourceRepresentation::Rgba8Raster,
        };
        let normalized = policy::normalize_request(
            &private,
            &PolicyConstraints::default(),
            route_limits(self.authority.thresholds().defaults()),
            &profile,
        );
        let (route, selected_entry, reasons, predicate) = match &normalized {
            Ok(normalized) => {
                let key = route_key(normalized.family, normalized.profile);
                let entry = self.authority.route(&key).ok_or_else(|| {
                    PpError::Vectorizer(
                        "verified embedded authority lost a required normalized route".to_owned(),
                    )
                })?;
                (
                    Some(entry.backend.clone()),
                    Some(entry),
                    route_reason(entry.state),
                    route_predicate(entry.state),
                )
            }
            Err(reason) => (
                None,
                None,
                vec![normalization_reason((*reason).clone()).to_owned()],
                PredicateAvailability::NotEvaluated,
            ),
        };
        let invariants = self.authority.invariants();
        let policy_digest = policy_digest(request.policy());
        let threshold_bundle = selected_entry
            .map(|entry| entry.threshold_bundle_digest.clone())
            .unwrap_or(invariants.threshold_bundle_digest);
        let foundation = selected_entry
            .map(|entry| entry.foundation_digest.clone())
            .unwrap_or_else(|| candidate::sha256_identity(invariants.foundation_id.as_bytes()));
        let report = selected_entry
            .map(|entry| entry.report_digest.clone())
            .unwrap_or_else(|| candidate::sha256_identity(VECTOR_ANALYSIS_SCHEMA.as_bytes()));
        Ok(VectorAnalysis::new(
            raster_digest(image),
            request.preset(),
            request.profile(),
            policy_digest.clone(),
            evidence,
            route,
            reasons,
            EvaluationDigests::new(
                selected_entry.map(|entry| candidate::sha256_identity(entry.backend.as_bytes())),
                selected_entry.map(|entry| entry.entry_digest.clone()),
                invariants.route_registry_digest,
                selected_entry.map(|entry| entry.entry_digest.clone()),
                Some(threshold_bundle),
                None,
                policy_digest,
                profile_digest(output_profile(request.profile())),
                foundation,
                report,
                None,
                None,
            ),
            BTreeMap::from([("embeddedAuthorityRoute".to_owned(), predicate)]),
        )
        .with_normalization(policy::analysis_normalization(request, normalized.as_ref())))
    }

    /// Normalizes once, evaluates exact backend bytes, and consumes candidate state through approval.
    pub fn run(&self, image: &Raster, request: &VectorRequest) -> PpResult<VectorOutcome> {
        let profile = profile::analyze_content(image);
        let evidence = profile_evidence(&profile)?;
        let input_resource = input_resource_gate_assessment(
            image,
            self.authority.thresholds().defaults().maximum_input_pixels,
        );
        if !input_resource.passed() {
            return resource_rejection(image, request, evidence, &self.authority, input_resource);
        }
        let private = policy::VectorRequest {
            preset: preset_family(request.preset()),
            profile: Some(output_profile(request.profile())),
            detail: request.detail().map(VectorDetail::get),
            minimum_quality: request.minimum_quality().map(UnitScore::get),
            maximum_quality_loss: request.maximum_quality_loss().map(UnitScore::get),
            maximum_paths: request.maximum_paths().map(|value| value.get()),
            representation: SourceRepresentation::Rgba8Raster,
        };
        let mut normalized = match policy::normalize_request(
            &private,
            &PolicyConstraints::default(),
            route_limits(self.authority.thresholds().defaults()),
            &profile,
        ) {
            Ok(normalized) => normalized,
            Err(NormalizationRejection::InvalidRequest) => {
                return Err(PpError::InvalidRequest(
                    "request constraints are invalid".to_owned(),
                ))
            }
            Err(reason) => {
                return domain_rejection(
                    RejectionContext {
                        image,
                        request,
                        evidence,
                        authority: &self.authority,
                        route: None,
                        normalized: None,
                        limits: None,
                    },
                    normalization_code(&reason),
                    normalization_reason(reason).to_owned(),
                )
            }
        };
        let key = route_key(normalized.family, normalized.profile);
        let route = self.authority.route(&key).ok_or_else(|| {
            PpError::Vectorizer(
                "verified embedded authority lost a required normalized route".to_owned(),
            )
        })?;
        let (limits, fixed_gates) = self
            .authority
            .thresholds()
            .resolve_digest(&route.threshold_bundle_digest)
            .map_err(|error| PpError::Vectorizer(error.to_string()))?;
        normalized = match policy::normalize_request(
            &private,
            &PolicyConstraints::default(),
            route_limits(&limits),
            &profile,
        ) {
            Ok(normalized) => normalized,
            Err(NormalizationRejection::InvalidRequest) => {
                return Err(PpError::InvalidRequest(
                    "request constraints are invalid".to_owned(),
                ))
            }
            Err(reason) => {
                return domain_rejection(
                    RejectionContext {
                        image,
                        request,
                        evidence,
                        authority: &self.authority,
                        route: Some(route),
                        normalized: None,
                        limits: Some(&limits),
                    },
                    normalization_code(&reason),
                    normalization_reason(reason).to_owned(),
                )
            }
        };
        match route.state {
            authority::RouteState::Unsupported => {
                return domain_rejection(
                    RejectionContext {
                        image,
                        request,
                        evidence,
                        authority: &self.authority,
                        route: Some(route),
                        normalized: Some(&normalized),
                        limits: Some(&limits),
                    },
                    VectorRejectionCode::Unsupported,
                    "embedded authority explicitly marks this route unsupported".to_owned(),
                )
            }
            authority::RouteState::CandidateShadow => {
                return domain_rejection(
                    RejectionContext {
                        image,
                        request,
                        evidence,
                        authority: &self.authority,
                        route: Some(route),
                        normalized: Some(&normalized),
                        limits: Some(&limits),
                    },
                    VectorRejectionCode::ProfileNotPromoted,
                    "embedded authority route is candidateShadow and cannot publish".to_owned(),
                )
            }
            authority::RouteState::FoundationActive | authority::RouteState::Promoted => {}
        }

        let mut backend = match backends::dispatch_verified(
            &route.backend,
            &route.backend_version,
            &route.backend_digest,
            image,
        ) {
            Ok(candidate) => candidate,
            Err(PpError::UnsupportedVectorContent(message)) => {
                return domain_rejection(
                    RejectionContext {
                        image,
                        request,
                        evidence,
                        authority: &self.authority,
                        route: Some(route),
                        normalized: Some(&normalized),
                        limits: Some(&limits),
                    },
                    VectorRejectionCode::Unsupported,
                    message,
                )
            }
            Err(error) => return Err(error),
        };
        let backend_source_digest = route.backend_digest.clone();
        if backend.backend_id != route.backend || backend.backend_version != route.backend_version {
            return Err(PpError::Vectorizer(
                "verified backend dispatch returned an unexpected identity".to_owned(),
            ));
        }
        let motion = assess_authorized_motion_structure(
            normalized.profile == OutputProfile::MotionStructureReady,
            route.state == authority::RouteState::Promoted,
            &backend.svg,
        );
        if motion.status() == MotionAssessmentStatus::Passed {
            backend.svg = motion
                .approved_motion_output_bytes()
                .ok_or_else(|| {
                    PpError::Vectorizer(
                        "passed motion assessment did not retain exact assessed scene bytes"
                            .to_owned(),
                    )
                })?
                .to_vec();
        }

        let motion_digest = approved_motion_digest(&motion);
        let svg = std::str::from_utf8(&backend.svg).map_err(|error| {
            PpError::Vectorizer(format!("backend emitted non-UTF-8 SVG: {error}"))
        })?;
        let (svg_report, svg_ir) = match SvgContract::parse(svg) {
            Ok(parsed) => parsed,
            Err(error) => {
                return candidate_parse_rejection(
                    RejectionContext {
                        image,
                        request,
                        evidence,
                        authority: &self.authority,
                        route: Some(route),
                        normalized: Some(&normalized),
                        limits: Some(&limits),
                    },
                    &motion,
                    &backend.svg,
                    format!("backend SVG failed bounded parser contract: {error}"),
                )
            }
        };
        let structural = compact_structural_assessment(&svg_ir);
        let preliminary_fixed = fixed_gates.require_security_gate
            && svg_report.width == image.width()
            && svg_report.height == image.height()
            && !svg_report.contains_raster_payload
            && svg_report.path_count > 0;
        let preliminary_svg_bytes = backend.svg.len() as u64 <= limits.maximum_svg_bytes;
        let preliminary_paths = normalized
            .maximum_paths
            .is_none_or(|maximum| svg_report.path_count <= maximum)
            && svg_report.path_count <= limits.maximum_paths;
        let preliminary_resource = preliminary_svg_bytes && preliminary_paths;
        if !preliminary_fixed || !preliminary_resource {
            return candidate_pre_render_rejection(
                RejectionContext {
                    image,
                    request,
                    evidence,
                    authority: &self.authority,
                    route: Some(route),
                    normalized: Some(&normalized),
                    limits: Some(&limits),
                },
                &backend.svg,
                &svg_report,
                &motion,
                PreRenderGateResults {
                    compact_structure: structural.passed,
                    fixed: preliminary_fixed,
                    svg_bytes: preliminary_svg_bytes,
                    paths: preliminary_paths,
                },
            );
        }
        let render_back = render_svg_to_raster(&svg_ir.source, image.width(), image.height())?;
        let quality = compare_rasters(image, &render_back)?;
        let protected_assessment = protected_raster_gate_assessment(image, &render_back);
        let topology_evidence = quality::topology::protected_topology_gate(image, &render_back);
        let topology = topology_evidence.evidence();
        let exact_rgba = protected_assessment.exact_rgba;
        let adaptive_geometry = backend.evidence.adaptive.as_ref().map(|adaptive| {
            adaptive_protected_geometry_assessment(image, &render_back, adaptive.fragment_pixels)
        });
        let protected_edges = adaptive_geometry
            .map(|assessment| assessment.edges)
            .unwrap_or(exact_rgba);
        let protected_endpoints = adaptive_geometry
            .map(|assessment| assessment.endpoints)
            .unwrap_or(exact_rgba);
        let protected_features = adaptive_geometry
            .map(|assessment| assessment.features)
            .unwrap_or(exact_rgba);
        let protected_junctions = adaptive_geometry
            .map(|assessment| assessment.junctions)
            .unwrap_or(exact_rgba);
        let palette = protected_palette_gate(image, &svg_ir, request.policy());
        let fixed = BTreeMap::from([
            ("security".to_owned(), predicate(preliminary_fixed)),
            (
                "unsupportedContent".to_owned(),
                predicate(
                    fixed_gates.reject_unsupported_content
                        && !matches!(
                            profile.auto_disposition,
                            profile::AutoDisposition::Unsupported
                                | profile::AutoDisposition::Classified(
                                    profile::RasterSignalClass::ContinuousTone
                                )
                        ),
                ),
            ),
        ]);
        let protected = BTreeMap::from([
            ("alpha".to_owned(), predicate(protected_assessment.alpha)),
            ("edges".to_owned(), predicate(protected_edges)),
            ("endpoints".to_owned(), predicate(protected_endpoints)),
            ("features".to_owned(), predicate(protected_features)),
            (
                "interiorTranslucency".to_owned(),
                predicate(!profile.alpha.has_interior_translucency || exact_rgba),
            ),
            ("junctions".to_owned(), predicate(protected_junctions)),
            ("palette".to_owned(), predicate(palette)),
            (
                "topology".to_owned(),
                predicate(protected_assessment.topology),
            ),
        ]);
        let calibrated = BTreeMap::from([
            (
                "localLumaSsim".to_owned(),
                predicate(quality.local_luma_ssim >= limits.minimum_local_luma_ssim),
            ),
            (
                "qualityLoss".to_owned(),
                predicate(1.0 - quality.quality_score <= normalized.maximum_quality_loss),
            ),
            (
                "qualityScore".to_owned(),
                predicate(quality.quality_score >= normalized.minimum_quality),
            ),
            (
                "worstBlockLumaSsim".to_owned(),
                predicate(quality.worst_block_luma_ssim >= limits.minimum_worst_block_luma_ssim),
            ),
        ]);
        let resource = BTreeMap::from([
            (
                "distinctColors".to_owned(),
                predicate(
                    fixed_gates.enforce_resource_limits
                        && input_resource.distinct_colors <= input_resource.maximum_distinct_colors,
                ),
            ),
            (
                "inputPixels".to_owned(),
                predicate(
                    fixed_gates.enforce_resource_limits
                        && input_resource.input_pixels <= input_resource.maximum_input_pixels,
                ),
            ),
            (
                "svgBytes".to_owned(),
                predicate(backend.svg.len() as u64 <= limits.maximum_svg_bytes),
            ),
            (
                "paths".to_owned(),
                predicate(
                    normalized
                        .maximum_paths
                        .is_none_or(|maximum| svg_report.path_count <= maximum)
                        && svg_report.path_count <= limits.maximum_paths,
                ),
            ),
        ]);
        let compact_predicate_state = if normalized.profile == OutputProfile::Compact {
            predicate(structural.passed)
        } else {
            PredicateAvailability::NotEvaluated
        };
        let motion_predicate_state = if normalized.profile == OutputProfile::MotionStructureReady {
            match motion.status() {
                MotionAssessmentStatus::Passed => PredicateAvailability::Passed,
                MotionAssessmentStatus::Failed => PredicateAvailability::Failed,
                MotionAssessmentStatus::NotEvaluated => PredicateAvailability::NotEvaluated,
            }
        } else {
            PredicateAvailability::NotEvaluated
        };
        let profile_predicates = BTreeMap::from([
            ("compact".to_owned(), compact_predicate_state),
            ("motionStructure".to_owned(), motion_predicate_state),
        ]);
        let selected_profile_gate = BTreeMap::from([(
            match normalized.profile {
                OutputProfile::Compact => "compact",
                OutputProfile::MotionStructureReady => "motionStructure",
            }
            .to_owned(),
            match normalized.profile {
                OutputProfile::Compact => compact_predicate_state,
                OutputProfile::MotionStructureReady => motion_predicate_state,
            },
        )]);
        let gates = candidate::GateEvidence::new(
            fixed.clone(),
            protected.clone(),
            calibrated.clone(),
            resource.clone(),
            selected_profile_gate,
        )
        .map_err(|error| PpError::Vectorizer(error.to_string()))?;
        let decision = if gates.all_passed() {
            EvaluationDecision::Approved
        } else {
            EvaluationDecision::Rejected
        };
        let rejection_codes = gate_codes(&gates);
        let rejection_reasons: Vec<String> = rejection_codes
            .iter()
            .map(|code| code.as_str().to_owned())
            .collect();
        let svg_digest = candidate::sha256_identity(&backend.svg);
        let render_back_digest = raster_digest(&render_back);
        let candidate_facts = CandidateFacts::new(
            svg_digest.clone(),
            backend.svg.len(),
            render_back_digest,
            svg_report.width,
            svg_report.height,
            svg_report.path_count,
            svg_report.color_count,
            svg_report.node_count,
            svg_report.curve_segment_count,
            svg_report.closed_path_count,
            backend.evidence.adaptive.clone(),
            TopologyFacts::new(
                topology.source_components,
                topology.candidate_components,
                topology.source_holes,
                topology.candidate_holes,
                topology.mismatched_mask_pixels,
            ),
        );
        let digests = evaluation_digests(
            &self.authority,
            Some(route),
            request,
            Some(&normalized),
            motion_digest.clone(),
            Some(svg_digest.clone()),
        );
        let diagnostics = diagnostic_artifacts(request.diagnostics(), &backend.svg, &render_back)?;
        let measurements = gate_measurements(
            &limits,
            &normalized,
            request,
            MeasuredCandidate {
                quality: &quality,
                svg: &svg_report,
                backend: &backend,
                structural: &structural,
                input: input_resource,
            },
            GateFamilyResults {
                fixed: Some(&fixed),
                protected: Some(&protected),
                calibrated: Some(&calibrated),
                resource: Some(&resource),
                profile: Some(&profile_predicates),
            },
        );
        let report = EvaluationReport::new(
            raster_digest(image),
            request.preset(),
            request.profile(),
            evidence,
            constraint_evidence(request, &normalized, limits.minimum_quality_score)?,
            path_constraint(request, &normalized, limits.maximum_paths),
            digests,
            Some(candidate_facts.clone()),
            fixed,
            protected,
            calibrated,
            resource,
            profile_predicates,
            measurements,
            decision,
            rejection_reasons.clone(),
            rejection_reasons,
            ArtifactIntent::new(request.diagnostics().is_requested(), diagnostics.clone()),
        );
        let evaluated = candidate::EvaluatedCandidate::new(
            backend.svg,
            svg_digest.clone(),
            render_back,
            candidate::SvgCandidateFacts::new(
                candidate::sha256_identity(svg_ir.source.as_bytes()),
                candidate::sha256_identity(
                    &serde_json::to_vec(&candidate_facts)
                        .expect("candidate facts contain only serializable values"),
                ),
                svg_ir.source,
            )
            .map_err(|error| PpError::Vectorizer(error.to_string()))?,
            candidate::NormalizedRequestEvidence::new(
                raster_digest(image),
                &normalized,
                policy_digest(request.policy()),
            )
            .map_err(|error| PpError::Vectorizer(error.to_string()))?,
            candidate::CandidateDigests::new(
                candidate::sha256_identity(route.backend.as_bytes()),
                backend.backend_id.to_owned(),
                backend.backend_version.to_owned(),
                backend_source_digest,
                route.entry_digest.clone(),
                route.threshold_bundle_digest.clone(),
                route.foundation_digest.clone(),
                self.authority.invariants().route_registry_digest,
                route.report_digest.clone(),
                profile_digest(normalized.profile),
                candidate::sha256_identity(
                    &serde_json::to_vec(&normalized).expect("normalized request is serializable"),
                ),
                motion_digest,
            )
            .map_err(|error| PpError::Vectorizer(error.to_string()))?,
            gates,
            report,
            diagnostics,
            Vec::new(),
        )
        .map_err(|error| PpError::Vectorizer(error.to_string()))?;
        approval::approve(evaluated)
    }
}

fn preset_family(selection: VectorPresetSelection) -> Option<PresetFamily> {
    match selection {
        VectorPresetSelection::Auto => None,
        VectorPresetSelection::PixelArt => Some(PresetFamily::PixelArt),
        VectorPresetSelection::ExactGrid => Some(PresetFamily::ExactGrid),
        VectorPresetSelection::FlatIcon => Some(PresetFamily::Icon),
        VectorPresetSelection::LineArt => Some(PresetFamily::LineArt),
        VectorPresetSelection::BoundedIllustration => Some(PresetFamily::Color),
    }
}

fn output_profile(profile: SvgProfile) -> OutputProfile {
    match profile {
        SvgProfile::Compact => OutputProfile::Compact,
        SvgProfile::MotionStructureReady => OutputProfile::MotionStructureReady,
    }
}

fn route_key(family: PresetFamily, profile: OutputProfile) -> authority::RouteKey {
    authority::RouteKey {
        input_family: match family {
            PresetFamily::PixelArt => "pixel-art",
            PresetFamily::ExactGrid => "exact-grid",
            PresetFamily::Icon => "flat-icon",
            PresetFamily::LineArt => "line-art",
            PresetFamily::Color => "bounded-illustration",
        }
        .to_owned(),
        policy_version: VectorPolicy::SCHEMA.to_owned(),
        profile_version: "perfectpixel.profile/1".to_owned(),
        output_profile: match profile {
            OutputProfile::Compact => "perfectpixel.svg-editable/1",
            OutputProfile::MotionStructureReady => "perfectpixel.svg-motion-structure/1",
        }
        .to_owned(),
    }
}

fn route_limits(defaults: &authority::ThresholdDefaults) -> EmbeddedRouteLimits {
    EmbeddedRouteLimits {
        minimum_quality: defaults.minimum_quality_score,
        maximum_quality_loss: (1.0 - defaults.minimum_quality_score).max(0.0),
        maximum_paths: Some(defaults.maximum_paths),
        maximum_detail: 5,
    }
}

fn normalization_code(reason: &NormalizationRejection) -> VectorRejectionCode {
    match reason {
        NormalizationRejection::PresetRequired => VectorRejectionCode::PresetRequired,
        NormalizationRejection::UnsupportedRepresentation
        | NormalizationRejection::UnsupportedContent => VectorRejectionCode::Unsupported,
        NormalizationRejection::PolicyInfeasible | NormalizationRejection::InvalidRequest => {
            VectorRejectionCode::Unsupported
        }
    }
}

fn normalization_reason(reason: NormalizationRejection) -> &'static str {
    match reason {
        NormalizationRejection::PresetRequired => {
            "automatic classification abstained; an explicit preset is required"
        }
        NormalizationRejection::UnsupportedRepresentation => "input representation is unsupported",
        NormalizationRejection::UnsupportedContent => {
            "content is unsupported by the bounded vector contract"
        }
        NormalizationRejection::PolicyInfeasible => {
            "request policy cannot be satisfied by embedded limits"
        }
        NormalizationRejection::InvalidRequest => "request constraints are invalid",
    }
}

fn route_reason(state: authority::RouteState) -> Vec<String> {
    match state {
        authority::RouteState::FoundationActive | authority::RouteState::Promoted => Vec::new(),
        authority::RouteState::CandidateShadow => {
            vec!["route is candidateShadow and cannot publish".to_owned()]
        }
        authority::RouteState::Unsupported => vec!["route is explicitly unsupported".to_owned()],
    }
}

fn route_predicate(state: authority::RouteState) -> PredicateAvailability {
    match state {
        authority::RouteState::FoundationActive | authority::RouteState::Promoted => {
            PredicateAvailability::Passed
        }
        authority::RouteState::CandidateShadow | authority::RouteState::Unsupported => {
            PredicateAvailability::Failed
        }
    }
}

fn predicate(passed: bool) -> PredicateAvailability {
    if passed {
        PredicateAvailability::Passed
    } else {
        PredicateAvailability::Failed
    }
}

fn gate_codes(gates: &candidate::GateEvidence) -> Vec<String> {
    gates
        .failed_families()
        .map(|family| {
            match family {
                candidate::GateFamily::Fixed => VectorRejectionCode::FixedGateFailed,
                candidate::GateFamily::Protected => VectorRejectionCode::ProtectedGateFailed,
                candidate::GateFamily::Calibrated => VectorRejectionCode::CalibratedGateFailed,
                candidate::GateFamily::Resource => VectorRejectionCode::ResourceGateFailed,
                candidate::GateFamily::OutputProfile => {
                    VectorRejectionCode::OutputProfileGateFailed
                }
            }
            .as_str()
            .to_owned()
        })
        .collect()
}
/// The gate-family result maps threaded into a measurement build.
///
/// All five are the same map type, so they travel named: passing them positionally allowed a
/// silent transposition that still compiled and would then mislabel which family failed.
/// `early_*` paths leave the families they have not reached yet empty.
#[derive(Default)]
struct GateFamilyResults<'a> {
    fixed: Option<&'a BTreeMap<String, PredicateAvailability>>,
    protected: Option<&'a BTreeMap<String, PredicateAvailability>>,
    calibrated: Option<&'a BTreeMap<String, PredicateAvailability>>,
    resource: Option<&'a BTreeMap<String, PredicateAvailability>>,
    profile: Option<&'a BTreeMap<String, PredicateAvailability>>,
}

impl<'a> GateFamilyResults<'a> {
    fn family(
        slot: Option<&'a BTreeMap<String, PredicateAvailability>>,
    ) -> &'a BTreeMap<String, PredicateAvailability> {
        const EMPTY: &BTreeMap<String, PredicateAvailability> = &BTreeMap::new();
        slot.unwrap_or(EMPTY)
    }
}

/// Everything measured about a rendered candidate, as one value.
struct MeasuredCandidate<'a> {
    quality: &'a quality::QualityReport,
    svg: &'a crate::core::SvgReport,
    backend: &'a backends::BackendCandidate,
    structural: &'a quality::CompactStructuralAssessment,
    input: quality::ResourceGateAssessment,
}

/// What has been observed about a candidate before render-back, as one value.
#[derive(Default)]
struct ObservedCandidate<'a> {
    svg: Option<&'a [u8]>,
    report: Option<&'a crate::core::SvgReport>,
    expected_dimensions: Option<(u32, u32)>,
}

fn gate_measurements(
    limits: &authority::ThresholdDefaults,
    normalized: &policy::NormalizedRequest,
    request: &VectorRequest,
    candidate: MeasuredCandidate<'_>,
    gates: GateFamilyResults<'_>,
) -> GateMeasurementFamilies {
    let MeasuredCandidate {
        quality,
        svg,
        backend,
        structural,
        input,
    } = candidate;
    let fixed = GateFamilyResults::family(gates.fixed);
    let protected = GateFamilyResults::family(gates.protected);
    let calibrated = GateFamilyResults::family(gates.calibrated);
    let resource = GateFamilyResults::family(gates.resource);
    let profile = GateFamilyResults::family(gates.profile);
    let booleans = |gates: &BTreeMap<String, PredicateAvailability>, reason: String| {
        gates
            .iter()
            .map(|(name, state)| {
                (
                    name.clone(),
                    GateMeasurement::new(
                        (*state != PredicateAvailability::NotEvaluated).then_some(
                            GateActualValue::Boolean(*state == PredicateAvailability::Passed),
                        ),
                        GateComparator::Equal,
                        None,
                        None,
                        None,
                        *state,
                        Some(reason.clone()),
                    ),
                )
            })
            .collect()
    };
    let score = |actual, comparator, approved, requested: Option<f64>, effective, state, reason| {
        GateMeasurement::new(
            GateActualValue::Score(
                UnitScore::new(actual).expect("quality measurements are bounded"),
            ),
            comparator,
            Some(GateThreshold::Score(
                UnitScore::new(approved).expect("verified threshold is bounded"),
            )),
            requested.map(|value| {
                GateThreshold::Score(
                    UnitScore::new(value).expect("validated requested threshold is bounded"),
                )
            }),
            Some(GateThreshold::Score(
                UnitScore::new(effective).expect("effective threshold is bounded"),
            )),
            state,
            Some(reason),
        )
    };
    let approved_loss = 1.0 - limits.minimum_quality_score;
    let calibrated = BTreeMap::from([
        (
            "localLumaSsim".to_owned(),
            score(
                quality.local_luma_ssim,
                GateComparator::GreaterThanOrEqual,
                limits.minimum_local_luma_ssim,
                None,
                limits.minimum_local_luma_ssim,
                calibrated["localLumaSsim"],
                "measured local luma SSIM".to_owned(),
            ),
        ),
        (
            "qualityLoss".to_owned(),
            score(
                1.0 - quality.quality_score,
                GateComparator::LessThanOrEqual,
                approved_loss,
                request.maximum_quality_loss().map(UnitScore::get),
                normalized.maximum_quality_loss,
                calibrated["qualityLoss"],
                "measured render-back quality loss".to_owned(),
            ),
        ),
        (
            "qualityScore".to_owned(),
            score(
                quality.quality_score,
                GateComparator::GreaterThanOrEqual,
                limits.minimum_quality_score,
                request.minimum_quality().map(UnitScore::get),
                normalized.minimum_quality,
                calibrated["qualityScore"],
                "measured render-back quality score".to_owned(),
            ),
        ),
        (
            "worstBlockLumaSsim".to_owned(),
            score(
                quality.worst_block_luma_ssim,
                GateComparator::GreaterThanOrEqual,
                limits.minimum_worst_block_luma_ssim,
                None,
                limits.minimum_worst_block_luma_ssim,
                calibrated["worstBlockLumaSsim"],
                "measured worst-block luma SSIM".to_owned(),
            ),
        ),
    ]);
    let resource = BTreeMap::from([
        (
            "distinctColors".to_owned(),
            GateMeasurement::new(
                GateActualValue::Count(input.distinct_colors as u64),
                GateComparator::LessThanOrEqual,
                Some(GateThreshold::Count(input.maximum_distinct_colors as u64)),
                None,
                Some(GateThreshold::Count(input.maximum_distinct_colors as u64)),
                resource["distinctColors"],
                Some("canonical source RGBA palette budget".to_owned()),
            ),
        ),
        (
            "inputPixels".to_owned(),
            GateMeasurement::new(
                GateActualValue::Count(input.input_pixels),
                GateComparator::LessThanOrEqual,
                Some(GateThreshold::Count(input.maximum_input_pixels)),
                None,
                Some(GateThreshold::Count(input.maximum_input_pixels)),
                resource["inputPixels"],
                Some("source raster pixel budget".to_owned()),
            ),
        ),
        (
            "svgBytes".to_owned(),
            GateMeasurement::new(
                GateActualValue::Count(backend.svg.len() as u64),
                GateComparator::LessThanOrEqual,
                Some(GateThreshold::Count(limits.maximum_svg_bytes)),
                None,
                Some(GateThreshold::Count(limits.maximum_svg_bytes)),
                resource["svgBytes"],
                Some("candidate SVG byte budget".to_owned()),
            ),
        ),
        (
            "paths".to_owned(),
            GateMeasurement::new(
                GateActualValue::Count(svg.path_count as u64),
                GateComparator::LessThanOrEqual,
                Some(GateThreshold::Count(limits.maximum_paths as u64)),
                request
                    .maximum_paths()
                    .map(|value| GateThreshold::Count(value.get() as u64)),
                Some(GateThreshold::Count(
                    normalized.maximum_paths.unwrap_or(limits.maximum_paths) as u64,
                )),
                resource["paths"],
                Some(format!(
                    "candidate-selection detail={:?}; explicit maximumPaths only tightens",
                    normalized.detail
                )),
            ),
        ),
    ]);
    GateMeasurementFamilies::new(
        booleans(fixed, "bounded SVG fixed-gate assessment".to_owned()),
        booleans(
            protected,
            format!("backend method family={}", backend.evidence.family),
        ),
        calibrated,
        resource,
        profile
            .iter()
            .map(|(name, state)| {
                let reason = match name.as_str() {
                    "compact" => format!("compact structural facts={:?}", structural.reasons),
                    "motionStructure" => "authorized motion structure assessment".to_owned(),
                    _ => "output profile assessment".to_owned(),
                };
                (
                    name.clone(),
                    GateMeasurement::new(
                        (*state != PredicateAvailability::NotEvaluated).then_some(
                            GateActualValue::Boolean(*state == PredicateAvailability::Passed),
                        ),
                        GateComparator::Equal,
                        None,
                        None,
                        None,
                        *state,
                        Some(reason),
                    ),
                )
            })
            .collect(),
    )
}

fn constraint_evidence(
    request: &VectorRequest,
    normalized: &policy::NormalizedRequest,
    approved_quality: f64,
) -> PpResult<BTreeMap<String, ConstraintValues<UnitScore>>> {
    let approved_loss = UnitScore::new((1.0 - approved_quality).max(0.0))
        .map_err(|error| PpError::Vectorizer(error.to_string()))?;
    let approved_quality =
        UnitScore::new(approved_quality).map_err(|error| PpError::Vectorizer(error.to_string()))?;
    let quality = UnitScore::new(normalized.minimum_quality)
        .map_err(|error| PpError::Vectorizer(error.to_string()))?;
    let loss = UnitScore::new(normalized.maximum_quality_loss)
        .map_err(|error| PpError::Vectorizer(error.to_string()))?;
    Ok(BTreeMap::from([
        (
            "minimumQuality".to_owned(),
            ConstraintValues::new(
                request.minimum_quality(),
                Some(approved_quality),
                Some(quality),
                Some(quality),
                PredicateAvailability::Passed,
                normalized
                    .relaxation_attempts
                    .iter()
                    .any(|item| item.field == "minimumQuality"),
            ),
        ),
        (
            "maximumQualityLoss".to_owned(),
            ConstraintValues::new(
                request.maximum_quality_loss(),
                Some(approved_loss),
                Some(loss),
                Some(loss),
                PredicateAvailability::Passed,
                normalized
                    .relaxation_attempts
                    .iter()
                    .any(|item| item.field == "maximumQualityLoss"),
            ),
        ),
    ]))
}

fn path_constraint(
    request: &VectorRequest,
    normalized: &policy::NormalizedRequest,
    approved: usize,
) -> ConstraintValues<usize> {
    ConstraintValues::new(
        request.maximum_paths().map(|value| value.get()),
        Some(approved),
        normalized.maximum_paths,
        normalized.maximum_paths,
        PredicateAvailability::Passed,
        normalized
            .relaxation_attempts
            .iter()
            .any(|item| item.field == "maximumPaths"),
    )
}

fn diagnostic_artifacts(
    intent: &DiagnosticsIntent,
    svg: &[u8],
    render_back: &Raster,
) -> PpResult<DiagnosticArtifactSet> {
    if !intent.is_requested() {
        return DiagnosticArtifactSet::new(Vec::new()).map_err(PpError::Vectorizer);
    }
    let all = intent.artifact_kinds().is_empty();
    let mut artifacts = Vec::new();
    if all
        || intent
            .artifact_kinds()
            .iter()
            .any(|kind| kind == DiagnosticsIntent::CANDIDATE_SVG)
    {
        artifacts.push(
            DiagnosticArtifact::new(
                "candidate.svg".to_owned(),
                "image/svg+xml".to_owned(),
                svg.to_vec(),
            )
            .map_err(PpError::Vectorizer)?,
        );
    }
    if all
        || intent
            .artifact_kinds()
            .iter()
            .any(|kind| kind == DiagnosticsIntent::RENDER_BACK)
    {
        artifacts.push(
            DiagnosticArtifact::new(
                "render-back.png".to_owned(),
                "image/png".to_owned(),
                crate::PngEncoder::encode_rgba(render_back)?,
            )
            .map_err(PpError::Vectorizer)?,
        );
    }
    DiagnosticArtifactSet::new(artifacts).map_err(PpError::Vectorizer)
}
fn resource_rejection(
    image: &Raster,
    request: &VectorRequest,
    evidence: ProfileEvidence,
    authority: &authority::VerifiedAuthority,
    input: quality::ResourceGateAssessment,
) -> PpResult<VectorOutcome> {
    let resource_gates = BTreeMap::from([
        (
            "distinctColors".to_owned(),
            predicate(input.distinct_colors <= input.maximum_distinct_colors),
        ),
        (
            "inputPixels".to_owned(),
            predicate(input.input_pixels <= input.maximum_input_pixels),
        ),
    ]);
    let measurements = early_gate_measurements(
        None,
        request,
        input,
        None,
        ObservedCandidate::default(),
        GateFamilyResults {
            resource: Some(&resource_gates),
            ..GateFamilyResults::default()
        },
    );
    candidate_rejection(
        RejectionContext {
            image,
            request,
            evidence,
            authority,
            route: None,
            normalized: None,
            limits: None,
        },
        RejectionGates {
            resource: resource_gates,
            fixed: BTreeMap::new(),
            measurements,
        },
        None,
        None,
        vec![VectorRejectionCode::ResourceGateFailed],
        format!(
            "vector input has {} pixels (maximum {}) and {} canonical colors (maximum {})",
            input.input_pixels,
            input.maximum_input_pixels,
            input.distinct_colors,
            input.maximum_distinct_colors
        ),
        EvaluationDecision::NotApplicable,
    )
}

fn approved_motion_digest(motion: &crate::adapters::motion::MotionAssessment) -> Option<String> {
    motion
        .approved_motion_output_bytes()
        .map(candidate::sha256_identity)
}
fn candidate_parse_rejection(
    context: RejectionContext<'_>,
    motion: &crate::adapters::motion::MotionAssessment,
    svg: &[u8],
    reason: String,
) -> PpResult<VectorOutcome> {
    let limits = context
        .limits
        .expect("parse rejection is only reached once route thresholds resolved");
    let image = context.image;
    let request = context.request;
    let authority = context.authority;
    let normalized = context
        .normalized
        .expect("parse rejection is only reached once the request normalized");
    let within_byte_budget = svg.len() as u64 <= limits.maximum_svg_bytes;

    let mut codes = vec![VectorRejectionCode::FixedGateFailed];
    if !within_byte_budget {
        codes.push(VectorRejectionCode::ResourceGateFailed);
    }
    let measurements = early_gate_measurements(
        Some(limits),
        request,
        input_resource_gate_assessment(
            image,
            authority.thresholds().defaults().maximum_input_pixels,
        ),
        Some(normalized),
        ObservedCandidate {
            svg: Some(svg),
            ..ObservedCandidate::default()
        },
        GateFamilyResults {
            profile: Some(&BTreeMap::from([(
                "motionStructure".to_owned(),
                match motion.status() {
                    MotionAssessmentStatus::Passed => PredicateAvailability::Passed,
                    MotionAssessmentStatus::Failed => PredicateAvailability::Failed,
                    MotionAssessmentStatus::NotEvaluated => PredicateAvailability::NotEvaluated,
                },
            )])),
            fixed: Some(&BTreeMap::from([(
                "security".to_owned(),
                PredicateAvailability::Failed,
            )])),
            resource: Some(&BTreeMap::from([(
                "svgBytes".to_owned(),
                predicate(within_byte_budget),
            )])),
            ..GateFamilyResults::default()
        },
    );
    candidate_rejection(
        context,
        RejectionGates {
            resource: BTreeMap::from([("svgBytes".to_owned(), predicate(within_byte_budget))]),
            fixed: BTreeMap::from([("security".to_owned(), PredicateAvailability::Failed)]),
            measurements,
        },
        approved_motion_digest(motion),
        Some(svg),
        codes,
        reason,
        EvaluationDecision::Rejected,
    )
}

/// Outcome of each bounded gate checked before render-back.
///
/// All four are `bool`, so they travel named rather than as four positional flags that
/// still compile when transposed and would then mislabel which gate failed.
struct PreRenderGateResults {
    compact_structure: bool,
    fixed: bool,
    svg_bytes: bool,
    paths: bool,
}

fn candidate_pre_render_rejection(
    context: RejectionContext<'_>,
    svg: &[u8],
    svg_report: &crate::core::SvgReport,
    motion: &crate::adapters::motion::MotionAssessment,
    gates: PreRenderGateResults,
) -> PpResult<VectorOutcome> {
    let PreRenderGateResults {
        compact_structure,
        fixed,
        svg_bytes,
        paths,
    } = gates;
    let image = context.image;
    let request = context.request;
    let authority = context.authority;
    let limits = context
        .limits
        .expect("pre-render rejection is only reached once route thresholds resolved");
    let normalized = context
        .normalized
        .expect("pre-render rejection is only reached once the request normalized");

    let mut codes = Vec::new();
    if !fixed {
        codes.push(VectorRejectionCode::FixedGateFailed);
    }
    if !svg_bytes || !paths {
        codes.push(VectorRejectionCode::ResourceGateFailed);
    }
    let resource_gates = BTreeMap::from([
        ("svgBytes".to_owned(), predicate(svg_bytes)),
        ("paths".to_owned(), predicate(paths)),
    ]);
    let fixed_gates = BTreeMap::from([("security".to_owned(), predicate(fixed))]);
    let measurements = early_gate_measurements(
        Some(limits),
        request,
        input_resource_gate_assessment(
            image,
            authority.thresholds().defaults().maximum_input_pixels,
        ),
        Some(normalized),
        ObservedCandidate {
            svg: Some(svg),
            report: Some(svg_report),
            expected_dimensions: Some((image.width(), image.height())),
        },
        GateFamilyResults {
            profile: Some(&BTreeMap::from([
                (
                    "compact".to_owned(),
                    if normalized.profile == OutputProfile::Compact {
                        predicate(compact_structure)
                    } else {
                        PredicateAvailability::NotEvaluated
                    },
                ),
                (
                    "motionStructure".to_owned(),
                    if normalized.profile == OutputProfile::MotionStructureReady {
                        match motion.status() {
                            MotionAssessmentStatus::Passed => PredicateAvailability::Passed,
                            MotionAssessmentStatus::Failed => PredicateAvailability::Failed,
                            MotionAssessmentStatus::NotEvaluated => {
                                PredicateAvailability::NotEvaluated
                            }
                        }
                    } else {
                        PredicateAvailability::NotEvaluated
                    },
                ),
            ])),
            fixed: Some(&fixed_gates),
            resource: Some(&resource_gates),
            ..GateFamilyResults::default()
        },
    );
    candidate_rejection(
        context,
        RejectionGates {
            resource: resource_gates,
            fixed: fixed_gates,
            measurements,
        },
        approved_motion_digest(motion),
        Some(svg),
        codes,
        "candidate failed bounded fixed or resource checks before rendering".to_owned(),
        EvaluationDecision::Rejected,
    )
}

fn early_gate_measurements(
    limits: Option<&authority::ThresholdDefaults>,
    request: &VectorRequest,
    input: quality::ResourceGateAssessment,
    normalized: Option<&policy::NormalizedRequest>,
    observed: ObservedCandidate<'_>,
    gates: GateFamilyResults<'_>,
) -> GateMeasurementFamilies {
    let ObservedCandidate {
        svg,
        report: svg_report,
        expected_dimensions,
    } = observed;
    let fixed = GateFamilyResults::family(gates.fixed);
    let resource = GateFamilyResults::family(gates.resource);
    let profile = GateFamilyResults::family(gates.profile);
    let booleans = |names: &[&str],
                    known: &BTreeMap<String, PredicateAvailability>|
     -> BTreeMap<String, GateMeasurement> {
        names
            .iter()
            .map(|name| {
                let state = known
                    .get(*name)
                    .copied()
                    .unwrap_or(PredicateAvailability::NotEvaluated);
                (
                    (*name).to_owned(),
                    GateMeasurement::new(
                        if state == PredicateAvailability::NotEvaluated {
                            None
                        } else {
                            Some(GateActualValue::Boolean(
                                state == PredicateAvailability::Passed,
                            ))
                        },
                        GateComparator::Equal,
                        None,
                        None,
                        None,
                        state,
                        Some("not evaluated before render-back".to_owned()),
                    ),
                )
            })
            .collect()
    };
    let resource_measurement = |name: &str, actual: Option<u64>, threshold: Option<u64>| {
        let state = match name {
            "distinctColors" => predicate(input.distinct_colors <= input.maximum_distinct_colors),
            "inputPixels" => predicate(input.input_pixels <= input.maximum_input_pixels),
            _ => resource
                .get(name)
                .copied()
                .unwrap_or(PredicateAvailability::NotEvaluated),
        };
        GateMeasurement::new(
            if state == PredicateAvailability::NotEvaluated {
                None
            } else {
                actual.map(GateActualValue::Count)
            },
            GateComparator::LessThanOrEqual,
            threshold.map(GateThreshold::Count),
            None,
            threshold.map(GateThreshold::Count),
            state,
            Some(
                if actual.is_some() {
                    "measured before render-back"
                } else {
                    "not evaluated before candidate generation"
                }
                .to_owned(),
            ),
        )
    };
    let resources = BTreeMap::from([
        (
            "distinctColors".to_owned(),
            resource_measurement(
                "distinctColors",
                Some(input.distinct_colors as u64),
                Some(input.maximum_distinct_colors as u64),
            ),
        ),
        (
            "inputPixels".to_owned(),
            resource_measurement(
                "inputPixels",
                Some(input.input_pixels),
                Some(input.maximum_input_pixels),
            ),
        ),
        (
            "svgBytes".to_owned(),
            resource_measurement(
                "svgBytes",
                svg.map(|value| value.len() as u64),
                limits.map(|value| value.maximum_svg_bytes),
            ),
        ),
        ("paths".to_owned(), {
            let state = resource
                .get("paths")
                .copied()
                .unwrap_or(PredicateAvailability::NotEvaluated);
            GateMeasurement::new(
                if state == PredicateAvailability::NotEvaluated {
                    None
                } else {
                    svg_report.map(|value| GateActualValue::Count(value.path_count as u64))
                },
                GateComparator::LessThanOrEqual,
                limits.map(|value| GateThreshold::Count(value.maximum_paths as u64)),
                request
                    .maximum_paths()
                    .map(|value| GateThreshold::Count(value.get() as u64)),
                normalized
                    .and_then(|normalized| {
                        normalized
                            .maximum_paths
                            .or_else(|| limits.map(|value| value.maximum_paths))
                    })
                    .map(|value| GateThreshold::Count(value as u64)),
                state,
                Some("candidate SVG path budget".to_owned()),
            )
        }),
    ]);
    let mut fixed_measurements = booleans(&["security", "unsupportedContent"], fixed);
    if let Some(report) = svg_report {
        fixed_measurements.insert(
            "svgDimensions".to_owned(),
            GateMeasurement::new(
                expected_dimensions.map(|_| GateActualValue::Dimensions {
                    width: report.width,
                    height: report.height,
                }),
                GateComparator::Equal,
                expected_dimensions
                    .map(|(width, height)| GateThreshold::Dimensions { width, height }),
                None,
                expected_dimensions
                    .map(|(width, height)| GateThreshold::Dimensions { width, height }),
                expected_dimensions
                    .map(|(width, height)| {
                        predicate(report.width == width && report.height == height)
                    })
                    .unwrap_or(PredicateAvailability::NotEvaluated),
                Some(format!(
                    "parsed SVG dimensions={}x{}",
                    report.width, report.height
                )),
            ),
        );
        fixed_measurements.insert(
            "svgPathCount".to_owned(),
            GateMeasurement::new(
                GateActualValue::Count(report.path_count as u64),
                GateComparator::GreaterThanOrEqual,
                Some(GateThreshold::Count(1)),
                None,
                Some(GateThreshold::Count(1)),
                predicate(report.path_count > 0),
                Some("parsed SVG path count".to_owned()),
            ),
        );
    }
    GateMeasurementFamilies::new(
        fixed_measurements,
        booleans(
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
            &BTreeMap::new(),
        ),
        booleans(
            &[
                "localLumaSsim",
                "qualityLoss",
                "qualityScore",
                "worstBlockLumaSsim",
            ],
            &BTreeMap::new(),
        ),
        resources,
        booleans(&["compact", "motionStructure"], profile),
    )
}
/// What was being evaluated when a rejection was produced.
///
/// Every rejection path needs this same set to describe the attempt, so it travels as one
/// value rather than as seven positional parameters repeated across four functions.
struct RejectionContext<'a> {
    image: &'a Raster,
    request: &'a VectorRequest,
    evidence: ProfileEvidence,
    authority: &'a authority::VerifiedAuthority,
    route: Option<&'a authority::RouteEntry>,
    normalized: Option<&'a policy::NormalizedRequest>,
    limits: Option<&'a authority::ThresholdDefaults>,
}

/// Gate-family results carried into a rejection report.
///
/// `resource` and `fixed` are the same map type, so passing them positionally allowed a
/// silent transposition that still compiled and produced a wrong report. Naming them here
/// makes that mistake impossible.
struct RejectionGates {
    resource: BTreeMap<String, PredicateAvailability>,
    fixed: BTreeMap<String, PredicateAvailability>,
    measurements: GateMeasurementFamilies,
}

fn candidate_rejection(
    context: RejectionContext<'_>,
    gates: RejectionGates,
    motion: Option<String>,
    svg: Option<&[u8]>,
    codes: Vec<VectorRejectionCode>,
    reason: String,
    decision: EvaluationDecision,
) -> PpResult<VectorOutcome> {
    let RejectionContext {
        image,
        request,
        evidence,
        authority,
        route,
        normalized,
        limits,
    } = context;
    let RejectionGates {
        mut resource,
        fixed,
        measurements,
    } = gates;
    let artifacts = candidate_diagnostic_artifacts(request.diagnostics(), svg)?;
    let input = input_resource_gate_assessment(
        image,
        authority.thresholds().defaults().maximum_input_pixels,
    );
    resource.insert(
        "distinctColors".to_owned(),
        predicate(input.distinct_colors <= input.maximum_distinct_colors),
    );
    resource.insert(
        "inputPixels".to_owned(),
        predicate(input.input_pixels <= input.maximum_input_pixels),
    );
    let profile_predicates = measurements
        .output_profile()
        .iter()
        .map(|(name, measurement)| (name.clone(), measurement.applicability()))
        .collect();
    let report = EvaluationReport::new(
        raster_digest(image),
        request.preset(),
        request.profile(),
        evidence,
        match (normalized, limits) {
            (Some(normalized), Some(limits)) => {
                constraint_evidence(request, normalized, limits.minimum_quality_score)?
            }
            _ => early_constraint_evidence(request),
        },
        match (normalized, limits) {
            (Some(normalized), Some(limits)) => {
                path_constraint(request, normalized, limits.maximum_paths)
            }
            _ => early_path_constraint(request),
        },
        evaluation_digests(
            authority,
            route,
            request,
            normalized,
            motion,
            svg.map(candidate::sha256_identity),
        ),
        None,
        complete_gates(fixed, &["security", "unsupportedContent"]),
        complete_gates(
            BTreeMap::new(),
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
        ),
        complete_gates(
            BTreeMap::new(),
            &[
                "localLumaSsim",
                "qualityLoss",
                "qualityScore",
                "worstBlockLumaSsim",
            ],
        ),
        complete_gates(
            resource,
            &["distinctColors", "inputPixels", "paths", "svgBytes"],
        ),
        complete_gates(profile_predicates, &["compact", "motionStructure"]),
        measurements,
        decision,
        codes.iter().map(|code| code.as_str().to_owned()).collect(),
        codes.iter().map(|_| reason.clone()).collect(),
        ArtifactIntent::new(request.diagnostics().is_requested(), artifacts.clone()),
    );
    Ok(VectorOutcome::Rejected(RejectedVectorOutput::new(
        report,
        artifacts,
        Vec::new(),
        codes,
    )))
}
fn complete_gates(
    mut gates: BTreeMap<String, PredicateAvailability>,
    names: &[&str],
) -> BTreeMap<String, PredicateAvailability> {
    for name in names {
        gates
            .entry((*name).to_owned())
            .or_insert(PredicateAvailability::NotEvaluated);
    }
    gates
}
fn route_constraint_evidence(
    request: &VectorRequest,
    limits: &authority::ThresholdDefaults,
) -> PpResult<BTreeMap<String, ConstraintValues<UnitScore>>> {
    let approved_quality = UnitScore::new(limits.minimum_quality_score)
        .map_err(|error| PpError::Vectorizer(error.to_string()))?;
    let approved_loss = UnitScore::new((1.0 - limits.minimum_quality_score).max(0.0))
        .map_err(|error| PpError::Vectorizer(error.to_string()))?;
    Ok(BTreeMap::from([
        (
            "maximumQualityLoss".to_owned(),
            ConstraintValues::new(
                request.maximum_quality_loss(),
                Some(approved_loss),
                None,
                None,
                PredicateAvailability::NotEvaluated,
                false,
            ),
        ),
        (
            "minimumQuality".to_owned(),
            ConstraintValues::new(
                request.minimum_quality(),
                Some(approved_quality),
                None,
                None,
                PredicateAvailability::NotEvaluated,
                false,
            ),
        ),
    ]))
}

fn route_path_constraint(request: &VectorRequest, approved: usize) -> ConstraintValues<usize> {
    ConstraintValues::new(
        request.maximum_paths().map(|value| value.get()),
        Some(approved),
        None,
        None,
        PredicateAvailability::NotEvaluated,
        false,
    )
}
fn early_constraint_evidence(
    request: &VectorRequest,
) -> BTreeMap<String, ConstraintValues<UnitScore>> {
    BTreeMap::from([
        (
            "maximumQualityLoss".to_owned(),
            ConstraintValues::new(
                request.maximum_quality_loss(),
                None,
                None,
                None,
                PredicateAvailability::NotEvaluated,
                false,
            ),
        ),
        (
            "minimumQuality".to_owned(),
            ConstraintValues::new(
                request.minimum_quality(),
                None,
                None,
                None,
                PredicateAvailability::NotEvaluated,
                false,
            ),
        ),
    ])
}

fn early_path_constraint(request: &VectorRequest) -> ConstraintValues<usize> {
    ConstraintValues::new(
        request.maximum_paths().map(|value| value.get()),
        None,
        None,
        None,
        PredicateAvailability::NotEvaluated,
        false,
    )
}

fn candidate_diagnostic_artifacts(
    intent: &DiagnosticsIntent,
    svg: Option<&[u8]>,
) -> PpResult<DiagnosticArtifactSet> {
    if !intent.is_requested() || svg.is_none() {
        return DiagnosticArtifactSet::new(Vec::new()).map_err(PpError::Vectorizer);
    }
    let all = intent.artifact_kinds().is_empty();
    if !all
        && !intent
            .artifact_kinds()
            .iter()
            .any(|kind| kind == DiagnosticsIntent::CANDIDATE_SVG)
    {
        return DiagnosticArtifactSet::new(Vec::new()).map_err(PpError::Vectorizer);
    }
    DiagnosticArtifactSet::new(vec![DiagnosticArtifact::new(
        "candidate.svg".to_owned(),
        "image/svg+xml".to_owned(),
        svg.expect("checked candidate SVG").to_vec(),
    )
    .map_err(PpError::Vectorizer)?])
    .map_err(PpError::Vectorizer)
}

fn domain_rejection(
    context: RejectionContext<'_>,
    code: VectorRejectionCode,
    reason: String,
) -> PpResult<VectorOutcome> {
    let RejectionContext {
        image,
        request,
        evidence,
        authority,
        route,
        normalized,
        limits,
    } = context;
    let not_evaluated = PredicateAvailability::NotEvaluated;
    let fixed = BTreeMap::from([
        ("security".to_owned(), not_evaluated),
        ("unsupportedContent".to_owned(), not_evaluated),
    ]);
    let protected = [
        "alpha",
        "edges",
        "endpoints",
        "features",
        "interiorTranslucency",
        "junctions",
        "palette",
        "topology",
    ]
    .into_iter()
    .map(|name| (name.to_owned(), not_evaluated))
    .collect();
    let calibrated = [
        "localLumaSsim",
        "qualityLoss",
        "qualityScore",
        "worstBlockLumaSsim",
    ]
    .into_iter()
    .map(|name| (name.to_owned(), not_evaluated))
    .collect();
    let input = input_resource_gate_assessment(
        image,
        authority.thresholds().defaults().maximum_input_pixels,
    );
    let resource = BTreeMap::from([
        (
            "distinctColors".to_owned(),
            predicate(input.distinct_colors <= input.maximum_distinct_colors),
        ),
        (
            "inputPixels".to_owned(),
            predicate(input.input_pixels <= input.maximum_input_pixels),
        ),
        ("paths".to_owned(), not_evaluated),
        ("svgBytes".to_owned(), not_evaluated),
    ]);
    let profile_predicates = BTreeMap::from([
        ("compact".to_owned(), not_evaluated),
        ("motionStructure".to_owned(), not_evaluated),
    ]);
    let artifacts = DiagnosticArtifactSet::new(Vec::new()).map_err(PpError::Vectorizer)?;
    let measurements = early_gate_measurements(
        limits,
        request,
        input,
        normalized,
        ObservedCandidate::default(),
        GateFamilyResults {
            profile: Some(&profile_predicates),
            fixed: Some(&fixed),
            resource: Some(&resource),
            ..GateFamilyResults::default()
        },
    );
    let report = EvaluationReport::new(
        raster_digest(image),
        request.preset(),
        request.profile(),
        evidence,
        match (normalized, limits) {
            (Some(normalized), Some(limits)) => {
                constraint_evidence(request, normalized, limits.minimum_quality_score)?
            }
            (None, Some(limits)) => route_constraint_evidence(request, limits)?,
            _ => early_constraint_evidence(request),
        },
        match (normalized, limits) {
            (Some(normalized), Some(limits)) => {
                path_constraint(request, normalized, limits.maximum_paths)
            }
            (None, Some(limits)) => route_path_constraint(request, limits.maximum_paths),
            _ => early_path_constraint(request),
        },
        evaluation_digests(authority, route, request, normalized, None, None),
        None,
        fixed,
        protected,
        calibrated,
        resource,
        profile_predicates,
        measurements,
        EvaluationDecision::NotApplicable,
        vec![code.as_str().to_owned()],
        vec![reason],
        ArtifactIntent::new(request.diagnostics().is_requested(), artifacts.clone()),
    );
    Ok(VectorOutcome::Rejected(RejectedVectorOutput::new(
        report,
        artifacts,
        Vec::new(),
        vec![code],
    )))
}

fn render_svg_to_raster(svg: &str, width: u32, height: u32) -> PpResult<Raster> {
    let tree = usvg::Tree::from_str(svg, &usvg::Options::default())
        .map_err(|source| PpError::SvgRender(source.to_string()))?;
    let mut pixmap = tiny_skia::Pixmap::new(width, height).ok_or_else(|| {
        PpError::SvgRender(format!(
            "cannot allocate SVG render target {width}x{height}"
        ))
    })?;
    let source_size = tree.size();
    let transform = tiny_skia::Transform::from_scale(
        width as f32 / source_size.width(),
        height as f32 / source_size.height(),
    );
    resvg::render(&tree, transform, &mut pixmap.as_mut());
    Raster::new(width, height, pixmap.take_demultiplied())
}
#[cfg(test)]
mod tests {
    use std::{env, fs, path::Path};

    use super::*;

    fn harness_path(name: &str) -> PpResult<std::path::PathBuf> {
        env::var_os(name)
            .map(Into::into)
            .ok_or_else(|| PpError::InvalidRequest(format!("missing {name}")))
    }

    fn write_json(path: &Path, value: &impl serde::Serialize) -> PpResult<()> {
        let bytes = serde_json::to_vec_pretty(value)
            .map_err(|error| PpError::Vectorizer(error.to_string()))?;
        fs::write(path, bytes).map_err(|error| PpError::FileIo {
            path: path.to_owned(),
            message: error.to_string(),
        })
    }

    #[test]
    fn motion_digest_is_canonical_and_requires_approved_bytes() {
        const SCENE: &[u8] = br#"<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16" viewBox="0 0 16 16"><path id="pp-path-0001" d="M0 0 L16 0 L16 16 Z"/></svg>"#;
        const UNSUPPORTED: &[u8] = br#"<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16"><path id="pp-path-0001" d="M0 0 A4 4 0 0 1 8 8"/></svg>"#;

        let approved = assess_authorized_motion_structure(true, true, SCENE);
        assert_eq!(approved.status(), MotionAssessmentStatus::Passed);
        assert_eq!(
            approved_motion_digest(&approved),
            Some(candidate::sha256_identity(
                approved.approved_motion_output_bytes().unwrap(),
            ))
        );

        let rejected = assess_authorized_motion_structure(true, true, UNSUPPORTED);
        assert_eq!(rejected.status(), MotionAssessmentStatus::Failed);
        assert_eq!(approved_motion_digest(&rejected), None);
    }
    #[test]
    fn family_calibration_harness_from_environment() -> PpResult<()> {
        let Some(input) = env::var_os("PERFECTPIXEL_FAMILY_CALIBRATION_INPUT") else {
            return Ok(());
        };
        let preset = match env::var("PERFECTPIXEL_FAMILY_CALIBRATION_PRESET")
            .map_err(|error| PpError::InvalidRequest(error.to_string()))?
            .as_str()
        {
            "pixel-art" => VectorPresetSelection::PixelArt,
            "flat-icon" => VectorPresetSelection::FlatIcon,
            "line-art" => VectorPresetSelection::LineArt,
            "bounded-illustration" => VectorPresetSelection::BoundedIllustration,
            value => {
                return Err(PpError::InvalidRequest(format!(
                    "unsupported calibration preset {value}"
                )))
            }
        };
        let analysis_path = harness_path("PERFECTPIXEL_FAMILY_CALIBRATION_ANALYSIS")?;
        let analysis_repeat_path = harness_path("PERFECTPIXEL_FAMILY_CALIBRATION_ANALYSIS_REPEAT")?;
        let evaluation_path = harness_path("PERFECTPIXEL_FAMILY_CALIBRATION_EVALUATION")?;
        let evaluation_repeat_path =
            harness_path("PERFECTPIXEL_FAMILY_CALIBRATION_EVALUATION_REPEAT")?;
        let svg_path = harness_path("PERFECTPIXEL_FAMILY_CALIBRATION_SVG")?;
        let svg_repeat_path = harness_path("PERFECTPIXEL_FAMILY_CALIBRATION_SVG_REPEAT")?;
        let image = crate::io::ImageCodec::decode_rgba(input, crate::io::DecodeLimits::PRODUCTION)?;
        let vectorizer = Vectorizer::new()?;
        let policy = VectorPolicy::default();
        let analysis_request =
            VectorAnalysisRequest::new(preset, SvgProfile::Compact, policy.clone())
                .map_err(|error| PpError::InvalidRequest(error.to_string()))?;
        let request = VectorRequest::new(
            preset,
            SvgProfile::Compact,
            None,
            None,
            None,
            None,
            policy,
            DiagnosticsIntent::none(),
        )
        .map_err(|error| PpError::InvalidRequest(error.to_string()))?;

        let analysis = vectorizer.analyze(&image, &analysis_request)?;
        let analysis_repeat = vectorizer.analyze(&image, &analysis_request)?;
        write_json(&analysis_path, &analysis)?;
        write_json(&analysis_repeat_path, &analysis_repeat)?;
        let first = vectorizer.run(&image, &request)?;
        let second = vectorizer.run(&image, &request)?;
        let (first_report, first_svg) = match first {
            VectorOutcome::Approved(output) => (
                output.report().clone(),
                Some(output.exact_svg_bytes().to_vec()),
            ),
            VectorOutcome::Rejected(output) => (output.report().clone(), None),
        };
        let (second_report, second_svg) = match second {
            VectorOutcome::Approved(output) => (
                output.report().clone(),
                Some(output.exact_svg_bytes().to_vec()),
            ),
            VectorOutcome::Rejected(output) => (output.report().clone(), None),
        };
        write_json(&evaluation_path, &first_report)?;
        write_json(&evaluation_repeat_path, &second_report)?;
        if let Some(svg) = first_svg {
            fs::write(&svg_path, svg).map_err(|error| PpError::FileIo {
                path: svg_path.clone(),
                message: error.to_string(),
            })?;
        }
        if let Some(svg) = second_svg {
            fs::write(&svg_repeat_path, svg).map_err(|error| PpError::FileIo {
                path: svg_repeat_path.clone(),
                message: error.to_string(),
            })?;
        }
        Ok(())
    }
}
