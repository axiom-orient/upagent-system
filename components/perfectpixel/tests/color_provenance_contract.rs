use perfectpixel::{
    AlphaMode, ColorSpec, DecodeLimits, ImageCodec, PixelFormat, PngEncoder, Raster,
};

#[test]
fn decode_without_embedded_profile_does_not_invent_srgb() -> Result<(), Box<dyn std::error::Error>>
{
    let source = Raster::new(2, 1, vec![255, 0, 0, 255, 0, 255, 0, 64])?;
    let png = PngEncoder::encode_rgba(&source)?;
    let decoded =
        ImageCodec::decode_rgba_bytes_with_metadata("fixture.png", &png, DecodeLimits::default())?;

    assert_eq!(decoded.raster(), &source);
    assert_eq!(decoded.pixel_spec().pixel_format, PixelFormat::Rgba8);
    assert_eq!(decoded.pixel_spec().alpha, AlphaMode::Straight);
    assert_eq!(decoded.pixel_spec().color, ColorSpec::Unknown);
    assert!(decoded.icc_profile().is_none());
    Ok(())
}
