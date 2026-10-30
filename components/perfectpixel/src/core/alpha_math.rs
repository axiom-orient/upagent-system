use super::{PpResult, Raster};

/// Converts straight RGBA8 into canonical premultiplied RGBA8 using integer round-to-nearest.
/// Fully transparent output has RGB=0 so invisible source color cannot become hidden state.
pub fn premultiply_rgba8(source: &Raster) -> PpResult<Raster> {
    let mut pixels = source.pixels().to_vec();
    for pixel in pixels.as_chunks_mut::<4>().0 {
        let alpha = u32::from(pixel[3]);
        if alpha == 0 {
            pixel[..3].fill(0);
            continue;
        }
        for channel in &mut pixel[..3] {
            *channel = ((u32::from(*channel) * alpha + 127) / 255) as u8;
        }
    }
    Raster::new(source.width(), source.height(), pixels)
}

/// Converts premultiplied RGBA8 into canonical straight RGBA8. Partial-alpha conversion can lose
/// information because RGBA8 premultiplication is quantized; callers must not require arbitrary
/// partial-alpha premultiply/unpremultiply byte round-trips to be lossless.
pub fn unpremultiply_rgba8(source: &Raster) -> PpResult<Raster> {
    let mut pixels = source.pixels().to_vec();
    for pixel in pixels.as_chunks_mut::<4>().0 {
        let alpha = u32::from(pixel[3]);
        if alpha == 0 {
            pixel[..3].fill(0);
            continue;
        }
        if alpha == 255 {
            continue;
        }
        for channel in &mut pixel[..3] {
            *channel = ((u32::from(*channel) * 255 + alpha / 2) / alpha).min(255) as u8;
        }
    }
    Raster::new(source.width(), source.height(), pixels)
}

/// Structural validity check for premultiplied RGBA8: every color channel must be <= alpha.
pub fn is_valid_premultiplied_rgba8(source: &Raster) -> bool {
    source
        .pixels()
        .as_chunks::<4>()
        .0
        .iter()
        .all(|pixel| pixel[0] <= pixel[3] && pixel[1] <= pixel[3] && pixel[2] <= pixel[3])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transparent_rgb_is_canonicalized() -> PpResult<()> {
        let source = Raster::new(1, 1, vec![255, 100, 42, 0])?;
        assert_eq!(premultiply_rgba8(&source)?.pixels(), &[0, 0, 0, 0]);
        assert_eq!(unpremultiply_rgba8(&source)?.pixels(), &[0, 0, 0, 0]);
        Ok(())
    }

    #[test]
    fn opaque_pixels_are_exact_identity() -> PpResult<()> {
        let source = Raster::new(1, 1, vec![201, 17, 99, 255])?;
        assert_eq!(premultiply_rgba8(&source)?.pixels(), source.pixels());
        assert_eq!(unpremultiply_rgba8(&source)?.pixels(), source.pixels());
        Ok(())
    }

    #[test]
    fn premultiplication_enforces_channel_bounds() -> PpResult<()> {
        let source = Raster::new(2, 1, vec![255, 128, 64, 128, 20, 30, 40, 5])?;
        assert!(is_valid_premultiplied_rgba8(&premultiply_rgba8(&source)?));
        Ok(())
    }
}
