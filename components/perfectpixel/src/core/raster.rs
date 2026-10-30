use super::{PpError, PpResult};
use serde::{Deserialize, Serialize};

pub const ALPHA_THRESHOLD: u8 = 10;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct FrameRect {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Size {
    pub w: u32,
    pub h: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Point {
    pub x: u32,
    pub y: u32,
}

// Fields are private, not `pub(crate)`: `new`/`blank` are the only way to construct a
// `Raster` anywhere in the crate, so the non-empty-dimension invariant validated in both is
// structural rather than a convention a struct-literal could bypass.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Raster {
    width: u32,
    height: u32,
    pixels: Vec<u8>,
}

impl Raster {
    pub fn new(width: u32, height: u32, pixels: Vec<u8>) -> PpResult<Self> {
        validate_dimensions(width, height)?;
        let expected = pixel_len(width, height).ok_or_else(|| {
            PpError::InvalidRequest(format!("image dimensions overflow: {width}x{height}"))
        })?;
        if pixels.len() != expected {
            return Err(PpError::InvalidRequest(format!(
                "RGBA buffer length mismatch: got {}, expected {} for {}x{}",
                pixels.len(),
                expected,
                width,
                height
            )));
        }
        Ok(Self {
            width,
            height,
            pixels,
        })
    }

    pub fn blank(width: u32, height: u32) -> PpResult<Self> {
        validate_dimensions(width, height)?;
        let len = pixel_len(width, height).ok_or_else(|| {
            PpError::InvalidRequest(format!("image dimensions overflow: {width}x{height}"))
        })?;
        Ok(Self {
            width,
            height,
            pixels: vec![0; len],
        })
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    pub fn pixels(&self) -> &[u8] {
        &self.pixels
    }

    pub fn pixels_mut(&mut self) -> &mut [u8] {
        &mut self.pixels
    }

    fn index(&self, x: u32, y: u32) -> usize {
        ((y as usize) * (self.width as usize) + (x as usize)) * 4
    }

    pub(crate) fn alpha_at(&self, x: u32, y: u32) -> u8 {
        self.pixels[self.index(x, y) + 3]
    }

    pub(crate) fn premultiplied_pixel(&self, x: u32, y: u32) -> [u8; 4] {
        let index = self.index(x, y);
        premultiply_pixel(&self.pixels[index..index + 4])
    }

    pub fn copy_from(&mut self, src: &Raster, dst_x: u32, dst_y: u32) -> PpResult<()> {
        self.copy_region(
            src,
            FrameRect {
                x: 0,
                y: 0,
                w: src.width,
                h: src.height,
            },
            dst_x,
            dst_y,
        )
    }

    pub fn copy_region(
        &mut self,
        src: &Raster,
        src_rect: FrameRect,
        dst_x: u32,
        dst_y: u32,
    ) -> PpResult<()> {
        validate_region(src, src_rect)?;
        ensure_fits(
            self.width,
            self.height,
            src_rect.w,
            src_rect.h,
            dst_x,
            dst_y,
        )?;
        for y in 0..src_rect.h {
            let src_start = src.index(src_rect.x, src_rect.y + y);
            let src_end = src_start + (src_rect.w as usize) * 4;
            let dst_start = self.index(dst_x, dst_y + y);
            let dst_end = dst_start + (src_rect.w as usize) * 4;
            self.pixels[dst_start..dst_end].copy_from_slice(&src.pixels[src_start..src_end]);
        }
        Ok(())
    }

    /// Sibling of `copy_region`, sharing its validation and index math. Currently only
    /// the sprite atlas packer's rotation feature calls this, but it stays beside
    /// `copy_region`/`copy_from` rather than moving to that adapter: all three are the
    /// same "blit a source rect into this raster" family over the same private pixel
    /// buffer, and splitting one variant out while keeping the other two would trade a
    /// single-caller cosmetic asymmetry for a real one (one sibling elsewhere, needing
    /// either new public byte-index API on `Raster` or a duplicated bounds-check helper).
    pub fn copy_region_rotated_cw(
        &mut self,
        src: &Raster,
        src_rect: FrameRect,
        dst_x: u32,
        dst_y: u32,
    ) -> PpResult<()> {
        validate_region(src, src_rect)?;
        ensure_fits(
            self.width,
            self.height,
            src_rect.h,
            src_rect.w,
            dst_x,
            dst_y,
        )?;
        for y in 0..src_rect.h {
            for x in 0..src_rect.w {
                let src_index = src.index(src_rect.x + x, src_rect.y + y);
                let rotated_x = src_rect.h - 1 - y;
                let rotated_y = x;
                let dst_index = self.index(dst_x + rotated_x, dst_y + rotated_y);
                self.pixels[dst_index..dst_index + 4]
                    .copy_from_slice(&src.pixels[src_index..src_index + 4]);
            }
        }
        Ok(())
    }
}

fn validate_dimensions(width: u32, height: u32) -> PpResult<()> {
    if width == 0 || height == 0 {
        return Err(PpError::InvalidRequest(format!(
            "image dimensions must be positive: {width}x{height}"
        )));
    }
    Ok(())
}

pub fn pixel_len(width: u32, height: u32) -> Option<usize> {
    (width as usize)
        .checked_mul(height as usize)?
        .checked_mul(4)
}

fn validate_region(src: &Raster, rect: FrameRect) -> PpResult<()> {
    let right = rect.x.checked_add(rect.w).ok_or_else(|| {
        PpError::InvalidRequest(format!("source region overflow at {},{}", rect.x, rect.y))
    })?;
    let bottom = rect.y.checked_add(rect.h).ok_or_else(|| {
        PpError::InvalidRequest(format!("source region overflow at {},{}", rect.x, rect.y))
    })?;
    if right > src.width || bottom > src.height {
        return Err(PpError::InvalidRequest(format!(
            "source region {}x{} at {},{} exceeds source {}x{}",
            rect.w, rect.h, rect.x, rect.y, src.width, src.height
        )));
    }
    Ok(())
}

fn ensure_fits(
    dst_width: u32,
    dst_height: u32,
    src_width: u32,
    src_height: u32,
    dst_x: u32,
    dst_y: u32,
) -> PpResult<()> {
    let dst_right = dst_x.checked_add(src_width).ok_or_else(|| {
        PpError::InvalidRequest(format!(
            "source {}x{} does not fit destination {}x{} at {},{}",
            src_width, src_height, dst_width, dst_height, dst_x, dst_y
        ))
    })?;
    let dst_bottom = dst_y.checked_add(src_height).ok_or_else(|| {
        PpError::InvalidRequest(format!(
            "source {}x{} does not fit destination {}x{} at {},{}",
            src_width, src_height, dst_width, dst_height, dst_x, dst_y
        ))
    })?;
    if dst_right > dst_width || dst_bottom > dst_height {
        return Err(PpError::InvalidRequest(format!(
            "source {}x{} does not fit destination {}x{} at {},{}",
            src_width, src_height, dst_width, dst_height, dst_x, dst_y
        )));
    }
    Ok(())
}

fn premultiply_pixel(pixel: &[u8]) -> [u8; 4] {
    let alpha = u32::from(pixel[3]);
    [
        premultiply_channel(pixel[0], alpha),
        premultiply_channel(pixel[1], alpha),
        premultiply_channel(pixel[2], alpha),
        pixel[3],
    ]
}

fn premultiply_channel(value: u8, alpha: u32) -> u8 {
    ((u32::from(value) * alpha + 127) / 255) as u8
}

fn content_bbox_above_alpha(frame: &Raster, alpha: u8) -> FrameRect {
    let mut min_x = frame.width();
    let mut min_y = frame.height();
    let mut max_x: Option<u32> = None;
    let mut max_y: Option<u32> = None;
    for y in 0..frame.height() {
        for x in 0..frame.width() {
            if frame.alpha_at(x, y) <= alpha {
                continue;
            }
            min_x = min_x.min(x);
            min_y = min_y.min(y);
            max_x = Some(max_x.map_or(x, |current| current.max(x)));
            max_y = Some(max_y.map_or(y, |current| current.max(y)));
        }
    }
    let (Some(max_x), Some(max_y)) = (max_x, max_y) else {
        return FrameRect::default();
    };
    FrameRect {
        x: min_x,
        y: min_y,
        w: max_x - min_x + 1,
        h: max_y - min_y + 1,
    }
}

/// Bounding box using the product's default visibility threshold.
pub fn content_bbox(frame: &Raster) -> FrameRect {
    content_bbox_above_alpha(frame, ALPHA_THRESHOLD)
}

/// Bounding box that counts any non-zero alpha as content, for lossless trimming.
pub fn lossless_content_bbox(frame: &Raster) -> FrameRect {
    content_bbox_above_alpha(frame, 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raster_rejects_zero_width() {
        let error = Raster::new(0, 1, Vec::new()).expect_err("zero width must be rejected");
        assert!(error.to_string().contains("must be positive"));
    }

    #[test]
    fn raster_rejects_zero_height() {
        let error = Raster::blank(1, 0).expect_err("zero height must be rejected");
        assert!(error.to_string().contains("must be positive"));
    }
}
