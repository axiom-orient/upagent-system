use perfectpixel::{
    DiagnosticsIntent, Raster, SvgProfile, VectorAnalysisRequest, VectorPolicy,
    VectorPresetSelection, Vectorizer,
};

fn corpus_raster() -> Raster {
    Raster::new(4, 4, [20, 40, 60, 255].repeat(16)).unwrap()
}
#[test]
fn diagnostics_intent_is_validated_at_construction_and_deserialization() {
    assert!(DiagnosticsIntent::requested(vec!["unknown".to_owned()]).is_err());
    assert!(DiagnosticsIntent::requested(vec![
        DiagnosticsIntent::CANDIDATE_SVG.to_owned(),
        DiagnosticsIntent::CANDIDATE_SVG.to_owned(),
    ])
    .is_err());
    assert!(
        DiagnosticsIntent::requested(vec![DiagnosticsIntent::CANDIDATE_SVG.to_owned(); 17])
            .is_err()
    );
    assert!(serde_json::from_str::<DiagnosticsIntent>(
        r#"{"requested":true,"artifactKinds":["candidate-svg","candidate-svg"]}"#
    )
    .is_err());
    assert!(
        serde_json::from_value::<DiagnosticsIntent>(serde_json::json!({
            "requested": true,
            "artifactKinds": vec![DiagnosticsIntent::CANDIDATE_SVG; 17],
        }))
        .is_err()
    );
    assert!(serde_json::from_str::<DiagnosticsIntent>(
        r#"{"requested":true,"artifactKinds":["unknown"]}"#
    )
    .is_err());
    let intent =
        DiagnosticsIntent::requested(vec![DiagnosticsIntent::RENDER_BACK.to_owned()]).unwrap();
    assert_eq!(
        serde_json::to_string(&intent).unwrap(),
        r#"{"requested":true,"artifactKinds":["render-back"]}"#
    );
}

#[test]
fn analysis_is_factorized_candidate_free_and_deterministic() {
    let raster = corpus_raster();
    let request = VectorAnalysisRequest::new(
        VectorPresetSelection::Auto,
        SvgProfile::Compact,
        VectorPolicy::default(),
    )
    .unwrap();
    let vectorizer = Vectorizer::new().unwrap();
    let first = vectorizer.analyze(&raster, &request).unwrap();
    let second = vectorizer.analyze(&raster, &request).unwrap();
    assert_eq!(first, second);
    assert_eq!(first.schema(), "perfectpixel.vector-analysis/1");
    assert_eq!(first.input_digest(), second.input_digest());
    assert_eq!(first.route(), second.route());
    assert_eq!(first.route_reasons(), second.route_reasons());
    assert!(first
        .evidence()
        .geometry()
        .contains_key("horizontalRunRatio"));
    assert!(first.evidence().paint().contains_key("uniqueColorCount"));
    assert!(first
        .evidence()
        .alpha()
        .contains_key("transparentPixelRatio"));
    assert!(first.evidence().complexity().contains_key("pixelCount"));
    assert!(first
        .evidence()
        .source_noise()
        .contains_key("sourceNoiseLikelihood"));
    assert!((0.0..=1.0).contains(&first.evidence().confidence().get()));
    assert!(first.evidence().abstained());
    assert!(!first.evidence().conflicts().is_empty() || first.route().is_none());
    assert!(first.digests().candidate_bytes().is_none());

    let json = serde_json::to_value(&first).unwrap();
    assert_eq!(
        serde_json::to_string(&first).unwrap(),
        serde_json::to_string(&second).unwrap()
    );
    assert_eq!(
        json["environmentMeasurements"]["renderTimeMs"],
        serde_json::Value::Null
    );
    assert_eq!(
        json["environmentMeasurements"]["peakMemoryBytes"],
        serde_json::Value::Null
    );
    assert_eq!(
        json["environmentMeasurements"]["throughputPixelsPerSecond"],
        serde_json::Value::Null
    );
    assert_eq!(
        json["environmentMeasurements"]["machineProtocolId"],
        serde_json::Value::Null
    );
    assert!(json.get("artifacts").is_none());
    assert!(
        json.to_string().contains("candidateBytes"),
        "candidate digest field must be explicit"
    );
    assert_eq!(json["digests"]["candidateBytes"], serde_json::Value::Null);
    assert_eq!(
        json["digests"]["normalizedRequest"],
        serde_json::Value::Null
    );
}

#[test]
fn analysis_route_facts_do_not_become_an_approval_capability() {
    let request = VectorAnalysisRequest::new(
        VectorPresetSelection::PixelArt,
        SvgProfile::Compact,
        VectorPolicy::default(),
    )
    .unwrap();
    let analysis = Vectorizer::new()
        .unwrap()
        .analyze(&corpus_raster(), &request)
        .unwrap();
    assert_eq!(analysis.route(), Some("logical-grid-pixel-art"));
    assert_eq!(
        analysis.predicates()["embeddedAuthorityRoute"],
        perfectpixel::PredicateAvailability::Passed
    );
    assert!(analysis.digests().candidate_bytes().is_none());
    assert!(serde_json::to_value(analysis)
        .unwrap()
        .get("actualDecision")
        .is_none());
}
