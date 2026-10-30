use serde::{Deserialize, Serialize};

use super::{linear16_to_srgb8, srgb8_to_linear16, PpError, PpResult, Raster};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BlendMode {
    Normal,
    Multiply,
    Screen,
    Overlay,
    Darken,
    Lighten,
}

/// Composites two straight-alpha RGBA8 rasters in linear sRGB using fixed-point arithmetic.
/// Dimensions must match. The returned raster is straight-alpha RGBA8.
pub fn composite_source_over_linear_srgb(
    backdrop: &Raster,
    source: &Raster,
    blend: BlendMode,
) -> PpResult<Raster> {
    if backdrop.width() != source.width() || backdrop.height() != source.height() {
        return Err(PpError::InvalidRequest(
            "composite source and backdrop dimensions must match".to_string(),
        ));
    }
    let mut output = vec![0u8; backdrop.pixels().len()];
    for ((backdrop_pixel, source_pixel), output_pixel) in backdrop
        .pixels()
        .as_chunks::<4>()
        .0
        .iter()
        .zip(source.pixels().as_chunks::<4>().0)
        .zip(output.as_chunks_mut::<4>().0)
    {
        composite_pixel(backdrop_pixel, source_pixel, output_pixel, blend);
    }
    Raster::new(backdrop.width(), backdrop.height(), output)
}

fn composite_pixel(backdrop: &[u8], source: &[u8], output: &mut [u8], blend: BlendMode) {
    if source[3] == 0 {
        output.copy_from_slice(backdrop);
        return;
    }
    if source[3] == 255 && blend == BlendMode::Normal {
        output.copy_from_slice(source);
        return;
    }

    let alpha_backdrop = u32::from(backdrop[3]) * 257;
    let alpha_source = u32::from(source[3]) * 257;
    let one_minus_source = 65_535 - alpha_source;
    let alpha_output = alpha_source + mul_u16(alpha_backdrop, one_minus_source);
    output[3] = u16_to_u8(alpha_output);

    if alpha_output == 0 {
        output[..3].fill(0);
        return;
    }

    for channel in 0..3 {
        let cb = u32::from(srgb8_to_linear16(backdrop[channel]));
        let cs = u32::from(srgb8_to_linear16(source[channel]));
        let blended = blend_channel(cb, cs, blend);
        let source_uncovered = mul_u16(alpha_source, 65_535 - alpha_backdrop);
        let source_overlap = mul_u16(alpha_source, alpha_backdrop);
        let backdrop_remaining = mul_u16(one_minus_source, alpha_backdrop);
        let premultiplied = mul_u16(source_uncovered, cs)
            .saturating_add(mul_u16(source_overlap, blended))
            .saturating_add(mul_u16(backdrop_remaining, cb))
            .min(65_535);
        let straight = div_u16(premultiplied, alpha_output);
        output[channel] = linear16_to_srgb8(straight as u16);
    }
}

fn blend_channel(backdrop: u32, source: u32, mode: BlendMode) -> u32 {
    match mode {
        BlendMode::Normal => source,
        BlendMode::Multiply => mul_u16(backdrop, source),
        BlendMode::Screen => 65_535 - mul_u16(65_535 - backdrop, 65_535 - source),
        BlendMode::Overlay => {
            if backdrop <= 32_767 {
                mul_u16(backdrop.saturating_mul(2).min(65_535), source)
            } else {
                65_535
                    - mul_u16(
                        (65_535 - backdrop).saturating_mul(2).min(65_535),
                        65_535 - source,
                    )
            }
        }
        BlendMode::Darken => backdrop.min(source),
        BlendMode::Lighten => backdrop.max(source),
    }
}

fn mul_u16(left: u32, right: u32) -> u32 {
    ((u64::from(left) * u64::from(right) + 32_767) / 65_535) as u32
}

fn div_u16(premultiplied: u32, alpha: u32) -> u32 {
    if alpha == 0 {
        0
    } else {
        ((u64::from(premultiplied) * 65_535 + u64::from(alpha) / 2) / u64::from(alpha)).min(65_535)
            as u32
    }
}

fn u16_to_u8(value: u32) -> u8 {
    ((value.min(65_535) + 128) / 257) as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transparent_source_is_identity() -> PpResult<()> {
        let backdrop = Raster::new(1, 1, vec![12, 34, 56, 200])?;
        let source = Raster::new(1, 1, vec![255, 0, 0, 0])?;
        let output = composite_source_over_linear_srgb(&backdrop, &source, BlendMode::Normal)?;
        assert_eq!(output, backdrop);
        Ok(())
    }

    #[test]
    fn opaque_source_replaces_backdrop_in_normal_mode() -> PpResult<()> {
        let backdrop = Raster::new(1, 1, vec![12, 34, 56, 200])?;
        let source = Raster::new(1, 1, vec![240, 100, 20, 255])?;
        let output = composite_source_over_linear_srgb(&backdrop, &source, BlendMode::Normal)?;
        assert_eq!(output, source);
        Ok(())
    }
}
