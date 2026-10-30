use image::{codecs::png::PngEncoder as ImagePngEncoder, ExtendedColorType, ImageEncoder};

use crate::core::{PpError, PpResult, Raster};

pub struct PngEncoder;

impl PngEncoder {
    pub fn encode_rgba(image: &Raster) -> PpResult<Vec<u8>> {
        let mut bytes = Vec::new();
        ImagePngEncoder::new(&mut bytes)
            .write_image(
                image.pixels(),
                image.width(),
                image.height(),
                ExtendedColorType::Rgba8,
            )
            .map_err(|source| PpError::ImageEncode {
                message: source.to_string(),
            })?;
        Ok(bytes)
    }
}
