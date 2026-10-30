use super::*;

use super::pipeline::{base_position, clamp_i32, overlay_raster, union_raster};
use super::raster::{
    alpha_centroid_x, alpha_points_in_frame_upper, alpha_points_in_upper, apply_palette,
    build_shared_palette, crop_raster, crop_to_content_or_clone, enforce_outline,
};
use super::resize::resize_nearest;
use crate::core::lossless_content_bbox;

pub(super) fn finalize_prepared_states(
    request: &NormalizeRequest,
    prepared_states: &[PreparedState],
) -> PpResult<Vec<NormalizedStateOutput>> {
    let palette = if prepared_states.iter().any(|state| state.pixel_perfect) {
        let palette_frames = prepared_states
            .iter()
            .filter(|state| state.pixel_perfect)
            .flat_map(|state| state.frames.iter())
            .collect::<Vec<_>>();
        build_shared_palette(&palette_frames, request.fit.palette_size)
    } else {
        Vec::new()
    };

    let mut outputs = Vec::with_capacity(prepared_states.len());
    for prepared in prepared_states {
        let mut frames = prepared.frames.clone();
        if prepared.pixel_perfect {
            frames = frames
                .iter()
                .map(|frame| apply_palette(frame, &palette))
                .collect::<PpResult<Vec<_>>>()?;
            if request.fit.outline.enabled {
                frames = frames
                    .iter()
                    .map(|frame| enforce_outline(frame, request.fit.outline.strength))
                    .collect::<PpResult<Vec<_>>>()?;
            }
        }
        let (left, top) = row_placement(
            &frames,
            &prepared.name,
            request.cell_width,
            request.cell_height,
            request.safe_margin_y,
            prepared.scale,
            &request.fit.align_x,
        )?;
        let placed = frames
            .iter()
            .enumerate()
            .map(|(frame_index, frame)| {
                place_row_frame(
                    frame,
                    &prepared.name,
                    frame_index,
                    (request.cell_width, request.cell_height),
                    prepared.scale,
                    (left, top),
                    request.fit.ground_frames.then_some(request.safe_margin_y),
                )
            })
            .collect::<PpResult<Vec<_>>>()?;
        outputs.push(NormalizedStateOutput {
            name: prepared.name.clone(),
            frames: placed,
        });
    }
    Ok(outputs)
}

pub(super) fn register_row_frames(
    frames: &[Raster],
    slack_x: u32,
    slack_y: u32,
) -> PpResult<Vec<Raster>> {
    let mut cropped = Vec::with_capacity(frames.len());
    for frame in frames {
        cropped.push(crop_to_content_or_clone(frame)?);
    }
    let max_width = cropped.iter().map(Raster::width).max().unwrap_or(1);
    let max_height = cropped.iter().map(Raster::height).max().unwrap_or(1);
    let canvas_width = max_width + slack_x * 2;
    let canvas_height = max_height + slack_y * 2;
    let reference = if let Some(first) = cropped.first() {
        first.clone()
    } else {
        Raster::blank(1, 1)?
    };
    let (ref_x, ref_y) = base_position(&reference, canvas_width, canvas_height, slack_y);
    let upper_limit = ref_y + (reference.height() as f64 * 0.65).round() as u32;
    let ref_mask = alpha_points_in_upper(&reference, ref_x, ref_y, upper_limit);
    let mut registered = Vec::with_capacity(cropped.len());

    for (index, frame) in cropped.iter().enumerate() {
        let (base_x, base_y) = base_position(frame, canvas_width, canvas_height, slack_y);
        let mut best_dx = 0i32;
        let mut best_dy = 0i32;
        if index > 0 && !ref_mask.is_empty() {
            let points = alpha_points_in_frame_upper(frame, base_y, upper_limit);
            let mut best_score = -1i32;
            for dy in -(slack_y as i32)..=(slack_y as i32) {
                for dx in -(slack_x as i32)..=(slack_x as i32) {
                    let mut score = 0i32;
                    for (x, y) in &points {
                        let px = base_x as i32 + *x as i32 + dx;
                        let py = base_y as i32 + *y as i32 + dy;
                        if px >= 0 && py >= 0 && ref_mask.contains(&(px as u32, py as u32)) {
                            score += 1;
                        }
                    }
                    if score > best_score {
                        best_score = score;
                        best_dx = dx;
                        best_dy = dy;
                    }
                }
            }
        }
        let mut canvas = Raster::blank(canvas_width, canvas_height)?;
        let placed_x = clamp_i32(
            base_x as i32 + best_dx,
            0,
            (canvas_width - frame.width()) as i32,
        ) as u32;
        let placed_y = clamp_i32(
            base_y as i32 + best_dy,
            0,
            (canvas_height - frame.height()) as i32,
        ) as u32;
        overlay_raster(&mut canvas, frame, placed_x, placed_y)?;
        registered.push(canvas);
    }

    let union = union_raster(&registered)?;
    let union_box = lossless_content_bbox(&union);
    if union_box.w == 0 || union_box.h == 0 {
        return Ok(registered);
    }
    registered
        .iter()
        .map(|frame| crop_raster(frame, union_box))
        .collect()
}

pub(super) fn row_placement(
    frames: &[Raster],
    state_name: &str,
    cell_width: u32,
    cell_height: u32,
    safe_margin_y: u32,
    scale: u32,
    align_x: &str,
) -> PpResult<(u32, u32)> {
    let union = union_raster(frames)?;
    if lossless_content_bbox(&union).w == 0 {
        return Ok((0, 0));
    }
    let sprite = if scale > 1 {
        let scaled_width =
            checked_scale_dimension(union.width(), scale, state_name, "union width")?;
        let scaled_height =
            checked_scale_dimension(union.height(), scale, state_name, "union height")?;
        resize_nearest(&union, scaled_width, scaled_height)?
    } else {
        union
    };
    if sprite.width() > cell_width || sprite.height() > cell_height {
        return Err(PpError::InvalidRequest(format!(
            "frame 0 for state '{state_name}' is {}x{}, larger than cell {}x{}",
            sprite.width(),
            sprite.height(),
            cell_width,
            cell_height
        )));
    }
    let raw_left = match align_x {
        "foot-centroid" => {
            (cell_width as f64 / 2.0 - alpha_centroid_x(&sprite, 0.2)).round() as i32
        }
        "centroid" => (cell_width as f64 / 2.0 - alpha_centroid_x(&sprite, 1.0)).round() as i32,
        _ => (cell_width as i32 - sprite.width() as i32) / 2,
    };
    let max_left = cell_width.saturating_sub(sprite.width()) as i32;
    let mut left = clamp_i32(raw_left, 0, max_left) as u32;
    if scale > 1 {
        left -= left % scale;
    }
    let bbox = content_bbox(&sprite);
    let content_bottom = if bbox.h == 0 {
        sprite.height()
    } else {
        bbox.y + bbox.h
    };
    let top = cell_height
        .saturating_sub(safe_margin_y)
        .saturating_sub(content_bottom);
    Ok((left, top))
}

pub(super) fn place_row_frame(
    frame: &Raster,
    state_name: &str,
    frame_index: usize,
    cell_size: (u32, u32),
    scale: u32,
    origin: (u32, u32),
    ground_margin_y: Option<u32>,
) -> PpResult<Raster> {
    let (cell_width, cell_height) = cell_size;
    let (left, top) = origin;
    let mut target = Raster::blank(cell_width, cell_height)?;
    if lossless_content_bbox(frame).w == 0 {
        return Ok(target);
    }
    let sprite = if scale > 1 {
        let scaled_width =
            checked_scale_dimension(frame.width(), scale, state_name, "frame width")?;
        let scaled_height =
            checked_scale_dimension(frame.height(), scale, state_name, "frame height")?;
        resize_nearest(frame, scaled_width, scaled_height)?
    } else {
        frame.clone()
    };
    if sprite.width() > cell_width || sprite.height() > cell_height {
        return Err(PpError::InvalidRequest(format!(
            "frame {frame_index} for state '{state_name}' is {}x{}, larger than cell {}x{}",
            sprite.width(),
            sprite.height(),
            cell_width,
            cell_height
        )));
    }
    let mut frame_top = top;
    if let Some(safe_margin_y) = ground_margin_y {
        let bbox = content_bbox(&sprite);
        let content_bottom = if bbox.h == 0 {
            sprite.height()
        } else {
            bbox.y + bbox.h
        };
        frame_top = cell_height
            .saturating_sub(safe_margin_y)
            .saturating_sub(content_bottom);
    }
    let max_left = cell_width.saturating_sub(sprite.width());
    let max_top = cell_height.saturating_sub(sprite.height());
    overlay_raster(
        &mut target,
        &sprite,
        left.min(max_left),
        frame_top.min(max_top),
    )?;
    Ok(target)
}

fn checked_scale_dimension(
    dimension: u32,
    scale: u32,
    state_name: &str,
    label: &str,
) -> PpResult<u32> {
    dimension.checked_mul(scale).ok_or_else(|| {
        PpError::InvalidRequest(format!(
            "state '{state_name}' {label} {dimension} overflows at scale {scale}"
        ))
    })
}

pub(super) fn default_align_x() -> String {
    "foot-centroid".to_string()
}

pub(super) fn default_align_y() -> String {
    "bottom".to_string()
}

pub(super) fn default_ground_y_variance_px() -> f64 {
    1.0
}
