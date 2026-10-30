use std::path::Path;

use crate::{
    linear16_to_srgb8, parse_srgb8_hex, srgb8_to_linear16, PngEncoder, PpError, PpResult, Raster,
};
use image::{
    codecs::{jpeg::JpegEncoder as ImageJpegEncoder, webp::WebPEncoder as ImageWebPEncoder},
    ExtendedColorType, ImageEncoder,
};

pub(super) const DEFAULT_JPEG_QUALITY: u8 = 85;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum AssetOutputFormat {
    Png,
    Jpeg,
    Webp,
}

#[derive(Debug, Clone, Copy)]
pub(super) struct AssetEncodeOptions {
    pub format: AssetOutputFormat,
    pub jpeg_quality: u8,
    pub background: Option<[u8; 3]>,
}

pub(super) fn output_format(path: &Path) -> PpResult<AssetOutputFormat> {
    match path
        .extension()
        .and_then(|value| value.to_str())
        .map(|value| value.to_ascii_lowercase())
        .as_deref()
    {
        Some("png") => Ok(AssetOutputFormat::Png),
        Some("jpg") | Some("jpeg") => Ok(AssetOutputFormat::Jpeg),
        Some("webp") => Ok(AssetOutputFormat::Webp),
        _ => Err(PpError::InvalidRequest(
            "asset output must use one of these extensions: png, jpg, jpeg, webp".to_string(),
        )),
    }
}

pub(super) fn encode_raster(image: &Raster, options: AssetEncodeOptions) -> PpResult<Vec<u8>> {
    match options.format {
        AssetOutputFormat::Png => PngEncoder::encode_rgba(image),
        AssetOutputFormat::Jpeg => encode_jpeg(image, options.jpeg_quality, options.background),
        AssetOutputFormat::Webp => encode_webp(image),
    }
}

pub(super) fn parse_background(raw: &str) -> PpResult<[u8; 3]> {
    parse_srgb8_hex(raw)
        .ok_or_else(|| PpError::InvalidOption("--background must be a #RRGGBB color".to_string()))
}

fn encode_jpeg(image: &Raster, quality: u8, background: Option<[u8; 3]>) -> PpResult<Vec<u8>> {
    let rgb = flatten_for_jpeg(image, background)?;
    let mut bytes = Vec::new();
    ImageJpegEncoder::new_with_quality(&mut bytes, quality)
        .write_image(&rgb, image.width(), image.height(), ExtendedColorType::Rgb8)
        .map_err(encode_error)?;
    Ok(bytes)
}

fn encode_webp(image: &Raster) -> PpResult<Vec<u8>> {
    let mut bytes = Vec::new();
    ImageWebPEncoder::new_lossless(&mut bytes)
        .write_image(
            image.pixels(),
            image.width(),
            image.height(),
            ExtendedColorType::Rgba8,
        )
        .map_err(encode_error)?;
    Ok(bytes)
}

fn flatten_for_jpeg(image: &Raster, background: Option<[u8; 3]>) -> PpResult<Vec<u8>> {
    let has_transparency = image
        .pixels()
        .as_chunks::<4>()
        .0
        .iter()
        .any(|pixel| pixel[3] != 255);
    if has_transparency && background.is_none() {
        return Err(PpError::InvalidRequest(
            "JPEG output requires --background #RRGGBB when the input has transparency".to_string(),
        ));
    }

    let background = background.unwrap_or([0, 0, 0]);
    let background_linear = background.map(srgb8_to_linear16);
    let mut rgb = Vec::with_capacity(image.pixels().len() / 4 * 3);
    for pixel in image.pixels().as_chunks::<4>().0 {
        let alpha = u32::from(pixel[3]) * 257;
        let inverse_alpha = 65_535 - alpha;
        for channel in 0..3 {
            let foreground = u32::from(srgb8_to_linear16(pixel[channel]));
            let back = u32::from(background_linear[channel]);
            let mixed = ((u64::from(foreground) * u64::from(alpha)
                + u64::from(back) * u64::from(inverse_alpha)
                + 32_767)
                / 65_535) as u16;
            rgb.push(linear16_to_srgb8(mixed));
        }
    }
    Ok(rgb)
}

fn encode_error(error: impl std::fmt::Display) -> PpError {
    PpError::ImageEncode {
        message: error.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transparent_jpeg_requires_an_explicit_background() {
        let image = Raster::new(1, 1, vec![255, 0, 0, 127]).unwrap();
        assert!(matches!(
            encode_raster(
                &image,
                AssetEncodeOptions {
                    format: AssetOutputFormat::Jpeg,
                    jpeg_quality: DEFAULT_JPEG_QUALITY,
                    background: None,
                },
            ),
            Err(PpError::InvalidRequest(_))
        ));
    }

    #[test]
    fn transparent_matte_is_integer_deterministic() {
        let image = Raster::new(1, 1, vec![255, 0, 0, 128]).unwrap();
        let first = flatten_for_jpeg(&image, Some([255, 255, 255])).unwrap();
        let second = flatten_for_jpeg(&image, Some([255, 255, 255])).unwrap();
        assert_eq!(first, second);
        assert_eq!(first[0], 255);
    }

    #[test]
    fn webp_lossless_round_trips_rgba() {
        let image = Raster::new(1, 1, vec![10, 20, 30, 40]).unwrap();
        let bytes = encode_raster(
            &image,
            AssetEncodeOptions {
                format: AssetOutputFormat::Webp,
                jpeg_quality: DEFAULT_JPEG_QUALITY,
                background: None,
            },
        )
        .unwrap();
        let decoded = image::load_from_memory(&bytes).unwrap().into_rgba8();
        assert_eq!(decoded.as_raw(), image.pixels());
    }
}
