use perfectpixel::{
    DiagnosticsIntent, Raster, SvgProfile, UnitScore, VectorDetail, VectorOutcome, VectorPolicy,
    VectorPresetSelection, VectorRejectionCode, VectorRequest, Vectorizer,
};
use std::num::NonZeroUsize;

fn solid() -> Raster {
    Raster::new(4, 4, [16, 32, 48, 255].repeat(16)).unwrap()
}

fn sixteen_color_grid() -> Raster {
    let mut pixels = Vec::with_capacity(32 * 32 * 4);
    for y in 0..32u8 {
        for x in 0..32u8 {
            pixels.extend_from_slice(&[(x / 8) * 50 + 30, (y / 8) * 35 + 70, 180, 255]);
        }
    }
    Raster::new(32, 32, pixels).unwrap()
}

#[test]
fn requested_relaxations_are_recorded_without_weakening_embedded_gates() {
    let request = VectorRequest::new(
        VectorPresetSelection::ExactGrid,
        SvgProfile::Compact,
        None,
        Some(UnitScore::new(0.0).unwrap()),
        Some(UnitScore::new(1.0).unwrap()),
        NonZeroUsize::new(usize::MAX),
        VectorPolicy::new("consumer-policy", vec![], vec![], false, true).unwrap(),
        DiagnosticsIntent::none(),
    )
    .unwrap();
    let outcome = Vectorizer::new().unwrap().run(&solid(), &request).unwrap();
    let report = match &outcome {
        VectorOutcome::Approved(output) => output.report(),
        VectorOutcome::Rejected(output) => output.report(),
    };
    let json = serde_json::to_value(report).unwrap();
    for key in ["minimumQuality", "maximumQualityLoss"] {
        let value = &json["constraints"][key];
        assert!(value.get("requested").is_some());
        assert!(value.get("approved").is_some());
        assert!(value.get("effective").is_some());
        assert!(value.get("attemptedRelaxation").is_some());
    }
    let paths = &json["pathLimit"];
    assert!(paths.get("requested").is_some());
    assert!(paths.get("approved").is_some());
    assert!(paths.get("effective").is_some());
    assert!(paths.get("attemptedRelaxation").is_some());
    for gate_family in [
        "fixedGates",
        "protectedGates",
        "resourceGates",
        "profilePredicates",
    ] {
        assert!(
            json[gate_family].is_object(),
            "{gate_family} must remain an embedded universal gate record"
        );
    }
}
#[test]
fn authority_digests_are_canonical_prefixed_identities() {
    let outcome = Vectorizer::new()
        .unwrap()
        .run(
            &solid(),
            &VectorRequest::new(
                VectorPresetSelection::ExactGrid,
                SvgProfile::Compact,
                None,
                None,
                None,
                None,
                VectorPolicy::default(),
                DiagnosticsIntent::none(),
            )
            .unwrap(),
        )
        .unwrap();
    let report = match outcome {
        VectorOutcome::Approved(value) => serde_json::to_value(value.report()).unwrap(),
        VectorOutcome::Rejected(value) => serde_json::to_value(value.report()).unwrap(),
    };
    for key in [
        "backend",
        "route",
        "registry",
        "registryEntry",
        "thresholdBundle",
        "policy",
        "profile",
        "foundation",
        "report",
    ] {
        let value = report["digests"][key].as_str().unwrap();
        assert!(value.starts_with("sha256-"), "{key} must be canonical");
        assert_eq!(value.len(), "sha256-".len() + 64);
    }
}

#[test]
fn detail_selects_candidates_without_tightening_explicit_path_limits() {
    let vectorizer = Vectorizer::new().unwrap();
    let request = |detail| {
        VectorRequest::new(
            VectorPresetSelection::ExactGrid,
            SvgProfile::Compact,
            Some(VectorDetail::new(detail).unwrap()),
            None,
            None,
            None,
            VectorPolicy::default(),
            DiagnosticsIntent::none(),
        )
        .unwrap()
    };

    let low = vectorizer.run(&sixteen_color_grid(), &request(1)).unwrap();
    let high = vectorizer.run(&sixteen_color_grid(), &request(5)).unwrap();
    let low_json = match &low {
        VectorOutcome::Approved(value) => serde_json::to_value(value.report()).unwrap(),
        VectorOutcome::Rejected(value) => serde_json::to_value(value.report()).unwrap(),
    };
    let high_json = match &high {
        VectorOutcome::Approved(value) => serde_json::to_value(value.report()).unwrap(),
        VectorOutcome::Rejected(value) => serde_json::to_value(value.report()).unwrap(),
    };
    assert_eq!(
        low_json["pathLimit"]["effective"],
        high_json["pathLimit"]["effective"]
    );
    assert_eq!(low_json["pathLimit"]["effective"], 64);
    assert!(low_json["gateMeasurements"]["resource"]["paths"]["reason"]
        .as_str()
        .unwrap()
        .contains("candidate-selection detail=Some(1)"));
}

#[test]
fn explicit_presets_and_policy_cannot_publish_a_shadow_or_unpromoted_profile() {
    let vectorizer = Vectorizer::new().unwrap();
    for (preset, profile) in [
        (VectorPresetSelection::PixelArt, SvgProfile::Compact),
        (
            VectorPresetSelection::ExactGrid,
            SvgProfile::MotionStructureReady,
        ),
    ] {
        let request = VectorRequest::new(
            preset,
            profile,
            None,
            None,
            None,
            None,
            VectorPolicy::new("relax", vec![], vec![], false, true).unwrap(),
            DiagnosticsIntent::none(),
        )
        .unwrap();
        assert!(
            matches!(
                vectorizer.run(&solid(), &request).unwrap(),
                VectorOutcome::Rejected(_)
            ),
            "preset and consumer policy cannot waive authority route/profile gates"
        );
    }
}
#[test]
fn palette_policy_validates_canonical_hex_and_infeasible_requirements() {
    assert!(VectorPolicy::new(
        "palette",
        vec!["#102030".to_owned(), "#ABCDEF".to_owned()],
        vec!["#102030".to_owned()],
        true,
        false,
    )
    .is_ok());
    assert!(VectorPolicy::new("palette", vec!["102030".to_owned()], vec![], true, false).is_err());
    assert!(VectorPolicy::new("palette", vec!["#12345G".to_owned()], vec![], true, false).is_err());
    assert!(VectorPolicy::new(
        "palette",
        vec!["#102030".to_owned()],
        vec!["#ABCDEF".to_owned()],
        true,
        false,
    )
    .is_err());
}

#[test]
fn required_and_allowed_palette_failures_are_protected_and_never_publish_svg() {
    let vectorizer = Vectorizer::new().unwrap();
    for policy in [
        VectorPolicy::new("palette", vec!["#ABCDEF".to_owned()], vec![], false, false).unwrap(),
        VectorPolicy::new("palette", vec![], vec!["#ABCDEF".to_owned()], false, false).unwrap(),
    ] {
        let request = VectorRequest::new(
            VectorPresetSelection::ExactGrid,
            SvgProfile::Compact,
            None,
            None,
            None,
            None,
            policy,
            DiagnosticsIntent::none(),
        )
        .unwrap();
        let outcome = vectorizer.run(&solid(), &request).unwrap();
        let VectorOutcome::Rejected(rejected) = outcome else {
            panic!("infeasible palette policy must not publish SVG");
        };
        assert!(rejected
            .codes()
            .contains(&VectorRejectionCode::ProtectedGateFailed));
        assert_eq!(
            serde_json::to_value(rejected.report()).unwrap()["protectedGates"]["palette"],
            "failed"
        );
    }
}

#[test]
fn reject_unmapped_is_a_zero_budget_palette_gate_and_noise_flag_cannot_waive_it() {
    let plateau_left = [200, 20, 20, 255];
    let plateau_right = [20, 20, 200, 255];
    let boundary_fragment = [195, 25, 25, 255];
    let mut pixels = Vec::with_capacity(8 * 8 * 4);
    for y in 0..8 {
        for x in 0..8 {
            let color = if (3..5).contains(&x) && (3..5).contains(&y) {
                boundary_fragment
            } else if x < 4 {
                plateau_left
            } else {
                plateau_right
            };
            pixels.extend_from_slice(&color);
        }
    }
    let source = Raster::new(8, 8, pixels).unwrap();
    let vectorizer = Vectorizer::new().unwrap();
    let request = |reject_unmapped, allow_drop_noise| {
        VectorRequest::new(
            VectorPresetSelection::BoundedIllustration,
            SvgProfile::Compact,
            None,
            None,
            None,
            None,
            VectorPolicy::new(
                "palette",
                vec!["#c81414".to_owned(), "#1414c8".to_owned()],
                vec![],
                reject_unmapped,
                allow_drop_noise,
            )
            .unwrap(),
            DiagnosticsIntent::none(),
        )
        .unwrap()
    };

    let mapped = vectorizer.run(&source, &request(false, false)).unwrap();
    let mapped_report = match &mapped {
        VectorOutcome::Approved(output) => output.report(),
        VectorOutcome::Rejected(output) => output.report(),
    };
    assert_eq!(
        serde_json::to_value(mapped_report).unwrap()["protectedGates"]["palette"],
        "passed"
    );

    let outcome = vectorizer.run(&source, &request(true, true)).unwrap();
    let VectorOutcome::Rejected(rejected) = outcome else {
        panic!("unmapped source colors must not publish SVG");
    };
    assert!(rejected
        .codes()
        .contains(&VectorRejectionCode::ProtectedGateFailed));
    assert_eq!(
        serde_json::to_value(rejected.report()).unwrap()["protectedGates"]["palette"],
        "failed"
    );
}
