use perfectpixel::{
    DiagnosticsIntent, EvaluationDecision, Raster, SvgProfile, VectorOutcome, VectorPolicy,
    VectorPresetSelection, VectorRejectionCode, VectorRequest, Vectorizer,
};

fn raster() -> Raster {
    Raster::new(
        2,
        2,
        vec![0, 0, 0, 255, 0, 0, 0, 255, 0, 0, 0, 255, 0, 0, 0, 255],
    )
    .unwrap()
}
fn continuous_raster() -> Raster {
    Raster::new(
        2,
        2,
        vec![0, 0, 0, 255, 255, 0, 0, 255, 255, 0, 0, 255, 0, 0, 0, 255],
    )
    .unwrap()
}
fn request(preset: VectorPresetSelection, profile: SvgProfile) -> VectorRequest {
    VectorRequest::new(
        preset,
        profile,
        None,
        None,
        None,
        None,
        VectorPolicy::default(),
        DiagnosticsIntent::requested(vec![DiagnosticsIntent::CANDIDATE_SVG.into()]).unwrap(),
    )
    .unwrap()
}
fn rejected(outcome: VectorOutcome) -> perfectpixel::RejectedVectorOutput {
    match outcome {
        VectorOutcome::Rejected(value) => value,
        VectorOutcome::Approved(_) => panic!("test route must not publish"),
    }
}

#[test]
fn domain_rejections_are_structured_and_never_expose_svg() {
    let vectorizer = Vectorizer::new().unwrap();
    let preset_required = rejected(
        vectorizer
            .run(
                &raster(),
                &request(VectorPresetSelection::Auto, SvgProfile::Compact),
            )
            .unwrap(),
    );
    assert!(preset_required
        .codes()
        .contains(&VectorRejectionCode::PresetRequired));
    assert_eq!(
        preset_required.report().actual_decision(),
        EvaluationDecision::NotApplicable
    );
    assert!(preset_required.report().candidate_facts().is_none());
    assert!(!serde_json::to_string(preset_required.report())
        .unwrap()
        .contains("<svg"));
    let unsupported_content = rejected(
        vectorizer
            .run(
                &continuous_raster(),
                &request(VectorPresetSelection::Auto, SvgProfile::Compact),
            )
            .unwrap(),
    );
    assert!(unsupported_content
        .codes()
        .contains(&VectorRejectionCode::Unsupported));

    let unsupported = rejected(
        vectorizer
            .run(
                &raster(),
                &request(
                    VectorPresetSelection::ExactGrid,
                    SvgProfile::MotionStructureReady,
                ),
            )
            .unwrap(),
    );
    assert!(unsupported
        .codes()
        .contains(&VectorRejectionCode::ProfileNotPromoted));
    assert_eq!(
        unsupported.report().actual_decision(),
        EvaluationDecision::NotApplicable
    );

    let shadow = rejected(
        vectorizer
            .run(
                &raster(),
                &request(
                    VectorPresetSelection::PixelArt,
                    SvgProfile::MotionStructureReady,
                ),
            )
            .unwrap(),
    );
    assert!(shadow
        .codes()
        .contains(&VectorRejectionCode::ProfileNotPromoted));
    let shadow_json = serde_json::to_value(shadow.report()).unwrap();
    assert!(shadow_json["digests"]["normalizedRequest"].is_string());
    assert!(shadow_json["digests"]["thresholdBundle"].is_string());
    assert!(shadow_json["gateMeasurements"]["resource"]["inputPixels"]["actual"].is_object());
    assert_eq!(
        shadow_json["gateMeasurements"]["fixed"]["security"]["actual"],
        serde_json::Value::Null
    );
    assert_eq!(
        shadow_json["gateMeasurements"]["outputProfile"]["compact"]["actual"],
        serde_json::Value::Null
    );
    assert!(shadow
        .artifacts()
        .artifacts()
        .iter()
        .all(|artifact| !artifact.digest().contains("<svg")));
}
#[test]
fn continuous_tone_rejects_every_explicit_preset_before_backend_routing() {
    let vectorizer = Vectorizer::new().unwrap();
    for preset in [
        VectorPresetSelection::PixelArt,
        VectorPresetSelection::ExactGrid,
        VectorPresetSelection::FlatIcon,
        VectorPresetSelection::LineArt,
        VectorPresetSelection::BoundedIllustration,
    ] {
        let rejected = rejected(
            vectorizer
                .run(&continuous_raster(), &request(preset, SvgProfile::Compact))
                .unwrap(),
        );
        assert!(rejected.codes().contains(&VectorRejectionCode::Unsupported));
        assert_eq!(
            rejected.report().actual_decision(),
            EvaluationDecision::NotApplicable
        );
    }
}

#[test]
fn early_rejections_emit_complete_canonical_gate_families() {
    let rejected = rejected(
        Vectorizer::new()
            .unwrap()
            .run(
                &continuous_raster(),
                &request(VectorPresetSelection::FlatIcon, SvgProfile::Compact),
            )
            .unwrap(),
    );
    let report = serde_json::to_value(rejected.report()).unwrap();
    for (family, keys) in [
        ("fixedGates", &["security", "unsupportedContent"][..]),
        (
            "protectedGates",
            &[
                "alpha",
                "edges",
                "endpoints",
                "features",
                "interiorTranslucency",
                "junctions",
                "palette",
                "topology",
            ][..],
        ),
        (
            "calibratedGates",
            &[
                "localLumaSsim",
                "qualityLoss",
                "qualityScore",
                "worstBlockLumaSsim",
            ][..],
        ),
        (
            "resourceGates",
            &["distinctColors", "inputPixels", "paths", "svgBytes"][..],
        ),
        ("profilePredicates", &["compact", "motionStructure"][..]),
    ] {
        let gates = report[family].as_object().unwrap();
        assert_eq!(gates.len(), keys.len());
        for &key in keys {
            assert!(gates.contains_key(key));
        }
    }
    for key in ["distinctColors", "inputPixels"] {
        assert_eq!(
            report["resourceGates"][key],
            report["gateMeasurements"]["resource"][key]["applicability"]
        );
    }
}

#[test]
fn reports_are_evidence_not_publication_authority() {
    let outcome = Vectorizer::new()
        .unwrap()
        .run(
            &raster(),
            &request(VectorPresetSelection::ExactGrid, SvgProfile::Compact),
        )
        .unwrap();
    match outcome {
        VectorOutcome::Approved(approved) => {
            assert_eq!(
                approved.report().actual_decision(),
                EvaluationDecision::Approved
            );
            assert_eq!(
                approved.svg_sha256(),
                approved
                    .report()
                    .digests()
                    .candidate_bytes()
                    .expect("approved report binds exact SVG digest")
            );
            assert_eq!(approved.svg_sha256().len(), "sha256-".len() + 64);
            let facts = approved
                .report()
                .candidate_facts()
                .expect("evaluated report carries candidate facts");
            assert_eq!(facts.candidate_sha256(), approved.svg_sha256());
            assert_eq!(
                facts.candidate_byte_count(),
                approved.exact_svg_bytes().len()
            );
            assert_eq!(facts.dimensions(), (2, 2));
            assert!(facts.render_back_sha256().starts_with("sha256-"));
            assert!(!serde_json::to_string(approved.report())
                .unwrap()
                .contains("<svg"));
            assert!(!approved.exact_svg_bytes().is_empty());
        }
        VectorOutcome::Rejected(rejected) => {
            assert_eq!(
                rejected.report().actual_decision(),
                EvaluationDecision::Rejected
            );
            assert!(!serde_json::to_string(rejected.report())
                .unwrap()
                .contains("svgBytes"));
        }
    }
}
#[test]
fn raster_identity_binds_dimensions_and_route_bound_measurements() {
    let vectorizer = Vectorizer::new().unwrap();
    let first = vectorizer
        .run(
            &raster(),
            &request(VectorPresetSelection::ExactGrid, SvgProfile::Compact),
        )
        .unwrap();
    let same_pixels_different_shape = Raster::new(
        1,
        4,
        vec![0, 0, 0, 255, 0, 0, 0, 255, 0, 0, 0, 255, 0, 0, 0, 255],
    )
    .unwrap();
    let second = vectorizer
        .run(
            &same_pixels_different_shape,
            &request(VectorPresetSelection::ExactGrid, SvgProfile::Compact),
        )
        .unwrap();
    let input_digest = |outcome: &VectorOutcome| match outcome {
        VectorOutcome::Approved(value) => value.report().input_digest().to_owned(),
        VectorOutcome::Rejected(value) => value.report().input_digest().to_owned(),
    };
    assert_ne!(input_digest(&first), input_digest(&second));
    let json = match &first {
        VectorOutcome::Approved(value) => serde_json::to_value(value.report()).unwrap(),
        VectorOutcome::Rejected(value) => serde_json::to_value(value.report()).unwrap(),
    };
    assert!(json["digests"]["route"]
        .as_str()
        .unwrap()
        .starts_with("sha256-"));
    assert!(json["digests"]["normalizedRequest"]
        .as_str()
        .is_some_and(|digest| digest.starts_with("sha256-")));
    assert!(json["digests"]["thresholdBundle"]
        .as_str()
        .unwrap()
        .starts_with("sha256-"));
    assert!(json["gateMeasurements"]["calibrated"]["qualityScore"]["actual"].is_object());
    assert!(
        json["gateMeasurements"]["calibrated"]["qualityScore"]["effectiveThreshold"].is_object()
    );
}

#[test]
fn diagnostic_intent_emits_exactly_selected_artifacts() {
    let vectorizer = Vectorizer::new().unwrap();
    for (kind, expected_path) in [
        (DiagnosticsIntent::CANDIDATE_SVG, "candidate.svg"),
        (DiagnosticsIntent::RENDER_BACK, "render-back.png"),
    ] {
        let request = VectorRequest::new(
            VectorPresetSelection::ExactGrid,
            SvgProfile::Compact,
            None,
            None,
            None,
            None,
            VectorPolicy::default(),
            DiagnosticsIntent::requested(vec![kind.to_owned()]).unwrap(),
        )
        .unwrap();
        let outcome = vectorizer.run(&raster(), &request).unwrap();
        let VectorOutcome::Approved(output) = outcome else {
            panic!("exact-grid candidate must approve");
        };
        let artifacts = output.artifacts().artifacts();
        assert_eq!(artifacts.len(), 1);
        assert_eq!(artifacts[0].relative_path(), expected_path);
        assert!(!artifacts[0].exact_bytes().is_empty());
        let intent = serde_json::to_value(output.report()).unwrap();
        assert_eq!(
            intent["artifactIntent"]["artifacts"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        let metadata = &intent["artifactIntent"]["artifacts"][0];
        assert_eq!(metadata["relativePath"], expected_path);
        assert_eq!(metadata["digest"], artifacts[0].digest());
        assert_eq!(metadata["mediaType"], artifacts[0].media_type());
        assert_eq!(metadata["byteCount"], artifacts[0].exact_bytes().len());
    }
}
