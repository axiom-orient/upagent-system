use perfectpixel::{
    DiagnosticsIntent, PpError, Raster, RequestValidationError, SvgProfile, UnitScore,
    VectorAnalysisRequest, VectorDetail, VectorPolicy, VectorPresetSelection, VectorRequest,
    Vectorizer,
};
use std::num::NonZeroUsize;

#[test]
fn request_values_are_validated_and_serde_is_closed() {
    assert_eq!(VectorDetail::new(1).unwrap().get(), 1);
    assert_eq!(VectorDetail::new(5).unwrap().get(), 5);
    assert_eq!(
        VectorDetail::new(0),
        Err(RequestValidationError::DetailOutOfRange(0))
    );
    assert_eq!(
        VectorDetail::new(6),
        Err(RequestValidationError::DetailOutOfRange(6))
    );
    assert_eq!(UnitScore::new(0.0).unwrap().get(), 0.0);
    assert_eq!(UnitScore::new(1.0).unwrap().get(), 1.0);
    assert!(matches!(
        UnitScore::new(f64::NAN),
        Err(RequestValidationError::ScoreNotFinite)
    ));
    assert!(matches!(
        UnitScore::new(-0.01),
        Err(RequestValidationError::ScoreOutOfRange(_))
    ));
    assert!(matches!(
        UnitScore::new(1.01),
        Err(RequestValidationError::ScoreOutOfRange(_))
    ));

    let policy: VectorPolicy = serde_json::from_str(r##"{"schema":"perfectpixel.vector-policy/1","version":"consumer/1","allowedPalette":["#112233"],"requiredPalette":["#112233"],"rejectUnmapped":true,"allowDropNoise":false}"##).unwrap();
    let request = VectorRequest::new(
        VectorPresetSelection::ExactGrid,
        SvgProfile::Compact,
        Some(VectorDetail::new(3).unwrap()),
        Some(UnitScore::new(0.99).unwrap()),
        Some(UnitScore::new(0.01).unwrap()),
        NonZeroUsize::new(32),
        policy.clone(),
        DiagnosticsIntent::none(),
    )
    .unwrap();
    assert_eq!(
        serde_json::to_value(&request).unwrap()["preset"],
        "exact-grid"
    );
    assert_eq!(
        serde_json::to_value(&request).unwrap()["profile"],
        "compact"
    );
    assert_eq!(request.preset, VectorPresetSelection::ExactGrid);
    assert_eq!(request.profile, SvgProfile::Compact);
    assert_eq!(request.detail.unwrap().get(), 3);
    assert_eq!(request.minimum_quality.unwrap().get(), 0.99);
    assert_eq!(request.maximum_quality_loss.unwrap().get(), 0.01);
    assert_eq!(request.maximum_paths.unwrap().get(), 32);
    assert_eq!(request.policy, policy);
    assert_eq!(request.diagnostics, DiagnosticsIntent::none());
    assert!(serde_json::from_str::<VectorRequest>(r#"{"preset":"exact-grid","profile":"compact","detail":3,"minimumQuality":0.9,"maximumQualityLoss":0.1,"maximumPaths":32,"policy":{"schema":"perfectpixel.vector-policy/1","version":"v","allowedPalette":[],"requiredPalette":[],"rejectUnmapped":true,"allowDropNoise":false},"diagnostics":{"requested":false,"artifactKinds":[]},"authority":"caller-controlled"}"#).is_err());
    assert!(serde_json::from_str::<VectorPolicy>(r##"{"schema":"wrong","version":"v","allowedPalette":[],"requiredPalette":[],"rejectUnmapped":false,"allowDropNoise":false}"##).is_err());
    assert!(serde_json::from_str::<VectorPolicy>(r##"{"schema":"perfectpixel.vector-policy/1","version":"v","allowedPalette":["#fff"],"requiredPalette":[],"rejectUnmapped":false,"allowDropNoise":false}"##).is_err());
    assert!(matches!(
        VectorPolicy::new("", vec![], vec![], true, false),
        Err(RequestValidationError::EmptyPolicyVersion)
    ));
    assert!(matches!(
        VectorPolicy::new(
            "v",
            vec!["#FFFFFF".into(), "#ffffff".into()],
            vec![],
            true,
            false
        ),
        Err(RequestValidationError::DuplicatePaletteColor(_))
    ));
    assert!(matches!(
        VectorPolicy::new(
            "v",
            vec!["#FFFFFF".into()],
            vec!["#000000".into()],
            true,
            false
        ),
        Err(RequestValidationError::RequiredPaletteNotAllowed(_))
    ));

    let analysis = VectorAnalysisRequest::new(
        VectorPresetSelection::Auto,
        SvgProfile::MotionStructureReady,
        policy,
    )
    .unwrap();
    assert_eq!(serde_json::to_value(&analysis).unwrap()["preset"], "auto");
    for preset in [
        VectorPresetSelection::Auto,
        VectorPresetSelection::PixelArt,
        VectorPresetSelection::ExactGrid,
        VectorPresetSelection::FlatIcon,
        VectorPresetSelection::LineArt,
        VectorPresetSelection::BoundedIllustration,
    ] {
        assert_eq!(
            serde_json::from_value::<VectorPresetSelection>(serde_json::to_value(preset).unwrap())
                .unwrap(),
            preset
        );
    }
    for profile in [SvgProfile::Compact, SvgProfile::MotionStructureReady] {
        assert_eq!(
            serde_json::from_value::<SvgProfile>(serde_json::to_value(profile).unwrap()).unwrap(),
            profile
        );
    }
}

#[test]
fn only_validated_requests_reach_the_public_vectorizer_boundary() {
    let invalid_raster = Raster::new(2, 2, vec![0; 3]);
    assert!(
        matches!(invalid_raster, Err(PpError::InvalidRequest(_))),
        "infrastructure/input failures remain PpError"
    );
    let raster = Raster::new(1, 1, vec![0, 0, 0, 255]).unwrap();
    let request = VectorAnalysisRequest::new(
        VectorPresetSelection::Auto,
        SvgProfile::Compact,
        VectorPolicy::default(),
    )
    .unwrap();
    let analysis = Vectorizer::new()
        .unwrap()
        .analyze(&raster, &request)
        .unwrap();
    assert_eq!(analysis.schema(), "perfectpixel.vector-analysis/1");
}

#[test]
fn documented_request_examples_match_the_public_contract() {
    let policy: VectorPolicy =
        serde_json::from_str(include_str!("../docs/vectorize/examples/policy.json")).unwrap();
    assert_eq!(policy.schema(), VectorPolicy::SCHEMA);

    let request: VectorRequest = serde_json::from_str(include_str!(
        "../docs/vectorize/examples/generation-request.json"
    ))
    .unwrap();
    assert_eq!(request.preset(), VectorPresetSelection::PixelArt);
    assert_eq!(request.profile(), SvgProfile::Compact);

    let analysis_request: VectorAnalysisRequest = serde_json::from_str(include_str!(
        "../docs/vectorize/examples/analysis-request.json"
    ))
    .unwrap();
    assert_eq!(analysis_request.preset(), VectorPresetSelection::Auto);
    assert_eq!(analysis_request.profile(), SvgProfile::MotionStructureReady);
}
