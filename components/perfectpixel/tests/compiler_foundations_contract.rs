use perfectpixel::{
    verify_raster_exact, AlphaMode, ArtifactRef, ColorSpec, ExactAssertion, PixelFormat, PixelSpec,
    Raster, Sha256Digest, VerificationSpec,
};

#[test]
fn artifact_ref_deserialization_is_strict_and_validated() -> Result<(), Box<dyn std::error::Error>>
{
    let digest = Sha256Digest::from_bytes(b"artifact");
    let value = serde_json::json!({
        "sha256": digest.as_str(),
        "mediaType": "image/png",
        "bytes": 8
    });
    let artifact: ArtifactRef = serde_json::from_value(value)?;
    assert_eq!(artifact.sha256(), &digest);

    let unknown = serde_json::json!({
        "sha256": digest.as_str(),
        "mediaType": "image/png",
        "bytes": 8,
        "path": "not-identity.png"
    });
    assert!(serde_json::from_value::<ArtifactRef>(unknown).is_err());
    assert!(Sha256Digest::parse("0".repeat(63)).is_err());
    Ok(())
}

#[test]
fn pixel_spec_requires_explicit_alpha_and_color_semantics() -> Result<(), Box<dyn std::error::Error>>
{
    let digest = Sha256Digest::from_bytes(b"icc-profile");
    let spec = PixelSpec::new(
        PixelFormat::Rgba8,
        AlphaMode::Premultiplied,
        ColorSpec::Icc {
            digest: digest.clone(),
        },
    );
    let encoded = serde_json::to_value(&spec)?;
    let decoded: PixelSpec = serde_json::from_value(encoded)?;
    assert_eq!(decoded, spec);

    let missing_color = serde_json::json!({
        "pixelFormat": "rgba8",
        "alpha": "straight"
    });
    assert!(serde_json::from_value::<PixelSpec>(missing_color).is_err());
    Ok(())
}

#[test]
fn exact_verification_is_machine_readable_and_fails_closed(
) -> Result<(), Box<dyn std::error::Error>> {
    let raster = Raster::new(2, 1, vec![255, 0, 0, 255, 0, 0, 0, 0])?;
    let pixel_spec = PixelSpec::new(PixelFormat::Rgba8, AlphaMode::Straight, ColorSpec::Unknown);
    let expected_digest = Sha256Digest::from_bytes(b"expected-output");
    let spec = VerificationSpec {
        exact: vec![
            ExactAssertion::Dimensions {
                width: 2,
                height: 1,
            },
            ExactAssertion::AlphaBounds {
                minimum: 0,
                maximum: 255,
            },
            ExactAssertion::ArtifactSha256 {
                expected: expected_digest,
            },
        ],
        perceptual: Vec::new(),
        regions: Vec::new(),
    };

    let report = verify_raster_exact(&spec, &raster, &pixel_spec, None)?;
    assert!(!report.ok);
    assert_eq!(report.exact.len(), 3);
    assert!(report.exact[0].passed);
    assert!(report.exact[1].passed);
    assert!(!report.exact[2].passed);
    assert!(report.perceptual.is_empty());
    assert!(report.regions.is_empty());
    assert!(serde_json::to_value(report)?.is_object());

    let empty = VerificationSpec::default();
    assert!(verify_raster_exact(&empty, &raster, &pixel_spec, None).is_err());
    Ok(())
}
