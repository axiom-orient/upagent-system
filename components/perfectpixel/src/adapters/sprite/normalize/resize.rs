use super::*;

use super::raster::{
    color_distance_sq, conform_state_scale, crop_raster, crop_to_content_or_clone,
    kcentroid_downscale,
};
use super::registration::register_row_frames;
use crate::core::lossless_content_bbox;
use crate::core::{resize_raster as resize_core_raster, ResampleFilter};

pub(super) fn prepare_regular_state(
    request: &NormalizeRequest,
    source: NormalizeStateImages,
    images: Vec<Raster>,
    warnings: Vec<String>,
) -> PpResult<PreparedState> {
    let max_width = request
        .cell_width
        .saturating_sub(request.safe_margin_x.saturating_mul(2))
        .max(1);
    let max_height = request
        .cell_height
        .saturating_sub(request.safe_margin_y.saturating_mul(2))
        .max(1);
    let fitted = crate::io::parallel_map(&images, |image| {
        resize_to_fit(
            image,
            max_width,
            max_height,
            &request.fit.resample,
            request.fit.detail_bias,
        )
    })?;
    let conformed = conform_state_scale(
        &fitted,
        max_width,
        max_height,
        request.fit.detail_bias,
        &request.fit.resample,
        request.fit.scale_conform,
    )?;
    let registered = register_row_frames(&conformed, 8, 3)?;
    Ok(PreparedState {
        name: source.name,
        method: "frames/registered".to_string(),
        frames: registered,
        pixel_perfect: false,
        pitch: None,
        scale: 1,
        phases: vec![None; images.len()],
        warnings,
    })
}

pub(super) fn resize_to_fit(
    image: &Raster,
    max_width: u32,
    max_height: u32,
    resample: &str,
    detail_bias: bool,
) -> PpResult<Raster> {
    let bbox = lossless_content_bbox(image);
    let mut sprite = if bbox.w == 0 || bbox.h == 0 {
        image.clone()
    } else {
        crop_raster(image, bbox)?
    };
    if sprite.width() <= max_width && sprite.height() <= max_height {
        return Ok(sprite);
    }
    let scale = (max_width as f64 / sprite.width() as f64)
        .min(max_height as f64 / sprite.height() as f64)
        .min(1.0);
    let new_width = ((sprite.width() as f64 * scale).round() as u32).max(1);
    let new_height = ((sprite.height() as f64 * scale).round() as u32).max(1);
    sprite = resize_raster(&sprite, new_width, new_height, resample, detail_bias)?;
    crop_to_content_or_clone(&sprite)
}

pub(super) fn resize_raster(
    image: &Raster,
    width: u32,
    height: u32,
    resample: &str,
    detail_bias: bool,
) -> PpResult<Raster> {
    if image.width() == width && image.height() == height {
        return Ok(image.clone());
    }
    match resample {
        "nearest" => resize_nearest(image, width, height),
        "kcentroid" => {
            if width <= image.width() && height <= image.height() {
                kcentroid_downscale(image, width, height, detail_bias)
            } else {
                resize_nearest(image, width, height)
            }
        }
        "lanczos" => resize_lanczos(image, width, height),
        _ => Err(PpError::InvalidRequest(format!(
            "unsupported resample mode '{resample}'"
        ))),
    }
}

pub(super) fn resize_lanczos(image: &Raster, width: u32, height: u32) -> PpResult<Raster> {
    resize_core_raster(image, width, height, ResampleFilter::Lanczos3)
}

pub(super) fn resize_nearest(image: &Raster, width: u32, height: u32) -> PpResult<Raster> {
    resize_core_raster(image, width, height, ResampleFilter::Nearest)
}

pub(super) fn nearest_palette_color(color: [u8; 3], palette: &[[u8; 3]]) -> [u8; 3] {
    palette
        .iter()
        .min_by_key(|candidate| color_distance_sq(color, **candidate))
        .copied()
        .unwrap_or(color)
}

pub(super) fn default_resample() -> String {
    "kcentroid".to_string()
}
