use super::*;

use super::pipeline::{average_color, median_f64, rgba_at, set_rgba, widest_channel};
use super::resize::{nearest_palette_color, resize_raster};
use crate::core::lossless_content_bbox;

pub(super) fn color_distance(left: [u8; 3], right: [u8; 3]) -> f64 {
    let mut sum = 0u32;
    for channel in 0..3 {
        let delta = i32::from(left[channel]) - i32::from(right[channel]);
        sum = sum.saturating_add((delta * delta) as u32);
    }
    (sum as f64).sqrt()
}

pub(super) fn crop_to_content_or_clone(image: &Raster) -> PpResult<Raster> {
    let bbox = lossless_content_bbox(image);
    if bbox.w == 0 || bbox.h == 0 {
        return Ok(image.clone());
    }
    crop_raster(image, bbox)
}

pub(super) fn conform_state_scale(
    frames: &[Raster],
    max_width: u32,
    max_height: u32,
    detail_bias: bool,
    resample: &str,
    enabled: bool,
) -> PpResult<Vec<Raster>> {
    let mut cropped = Vec::with_capacity(frames.len());
    let mut heights = Vec::new();
    for frame in frames {
        let sprite = crop_to_content_or_clone(frame)?;
        if sprite.width() > 0 && sprite.height() > 0 && alpha_nonzero_count(&sprite) > 0 {
            heights.push(sprite.height() as f64);
        }
        cropped.push(sprite);
    }
    if !enabled || heights.is_empty() {
        return Ok(cropped);
    }
    let target_height = median_f64(&heights).round().max(1.0);
    let mut result = Vec::with_capacity(cropped.len());
    for sprite in cropped {
        if alpha_nonzero_count(&sprite) == 0 {
            result.push(sprite);
            continue;
        }
        let height_delta = (sprite.height() as f64 - target_height).abs();
        let should_conform = height_delta > (target_height * 0.02).max(1.0);
        let mut scale = if should_conform {
            target_height / sprite.height() as f64
        } else {
            1.0
        };
        if sprite.width() as f64 * scale > max_width as f64 {
            scale = max_width as f64 / sprite.width() as f64;
        }
        if sprite.height() as f64 * scale > max_height as f64 {
            scale = scale.min(max_height as f64 / sprite.height() as f64);
        }
        if (scale - 1.0).abs() < f64::EPSILON {
            result.push(sprite);
            continue;
        }
        let new_width = ((sprite.width() as f64 * scale).round() as u32).max(1);
        let new_height = ((sprite.height() as f64 * scale).round() as u32).max(1);
        let resized = resize_raster(&sprite, new_width, new_height, resample, detail_bias)?;
        result.push(crop_to_content_or_clone(&resized)?);
    }
    Ok(result)
}

pub(super) fn kcentroid_downscale(
    image: &Raster,
    width: u32,
    height: u32,
    detail_bias: bool,
) -> PpResult<Raster> {
    let mut output = Raster::blank(width, height)?;
    for oy in 0..height {
        let y0 = oy * image.height() / height;
        let y1 = ((oy + 1) * image.height() / height)
            .max(y0 + 1)
            .min(image.height());
        for ox in 0..width {
            let x0 = ox * image.width() / width;
            let x1 = ((ox + 1) * image.width() / width)
                .max(x0 + 1)
                .min(image.width());
            let mut opaque = Vec::new();
            let mut block_len = 0usize;
            for y in y0..y1 {
                for x in x0..x1 {
                    block_len += 1;
                    let pixel = rgba_at(image, x, y);
                    if pixel[3] >= OPAQUE_ALPHA_THRESHOLD {
                        opaque.push(pixel);
                    }
                }
            }
            if opaque.len() * 2 < block_len || opaque.is_empty() {
                continue;
            }
            let color = dominant_block_color(&opaque, detail_bias);
            let alpha =
                opaque.iter().map(|pixel| u64::from(pixel[3])).sum::<u64>() / opaque.len() as u64;
            set_rgba(
                &mut output,
                ox,
                oy,
                [color[0], color[1], color[2], alpha as u8],
            );
        }
    }
    Ok(output)
}

pub(super) fn dominant_block_color(opaque: &[[u8; 4]], detail_bias: bool) -> [u8; 3] {
    if opaque.len() == 1 {
        return [opaque[0][0], opaque[0][1], opaque[0][2]];
    }
    let lo = opaque
        .iter()
        .min_by_key(|pixel| luma(**pixel))
        .copied()
        .unwrap_or([0, 0, 0, 0]);
    let hi = opaque
        .iter()
        .max_by_key(|pixel| luma(**pixel))
        .copied()
        .unwrap_or([255, 255, 255, 255]);
    let mut centroids = [[lo[0], lo[1], lo[2]], [hi[0], hi[1], hi[2]]];
    let mut assignments = vec![0usize; opaque.len()];
    for _ in 0..3 {
        for (index, pixel) in opaque.iter().enumerate() {
            let d0 = color_distance_sq([pixel[0], pixel[1], pixel[2]], centroids[0]);
            let d1 = color_distance_sq([pixel[0], pixel[1], pixel[2]], centroids[1]);
            assignments[index] = if d0 <= d1 { 0 } else { 1 };
        }
        for (cluster, centroid) in centroids.iter_mut().enumerate() {
            let mut sum = [0u64; 3];
            let mut count = 0u64;
            for (index, pixel) in opaque.iter().enumerate() {
                if assignments[index] == cluster {
                    sum[0] += u64::from(pixel[0]);
                    sum[1] += u64::from(pixel[1]);
                    sum[2] += u64::from(pixel[2]);
                    count += 1;
                }
            }
            if count > 0 {
                *centroid = average_rgb_sum(sum, count);
            }
        }
    }
    let count0 = assignments
        .iter()
        .filter(|assignment| **assignment == 0)
        .count();
    let count1 = assignments.len() - count0;
    let mut dominant = if count0 >= count1 { 0 } else { 1 };
    if detail_bias {
        let darker = if luma3(centroids[0]) <= luma3(centroids[1]) {
            0
        } else {
            1
        };
        let share = assignments
            .iter()
            .filter(|assignment| **assignment == darker)
            .count() as f64
            / assignments.len() as f64;
        if darker != dominant
            && share >= 0.40
            && luma3(centroids[darker]) < 70_000
            && luma3(centroids[1 - darker]).saturating_sub(luma3(centroids[darker])) > 50_000
        {
            dominant = darker;
        }
    }
    let mut sum = [0u64; 3];
    let mut count = 0u64;
    for (index, pixel) in opaque.iter().enumerate() {
        if assignments[index] == dominant {
            sum[0] += u64::from(pixel[0]);
            sum[1] += u64::from(pixel[1]);
            sum[2] += u64::from(pixel[2]);
            count += 1;
        }
    }
    if count == 0 {
        return centroids[dominant];
    }
    average_rgb_sum(sum, count)
}

fn average_rgb_sum(sum: [u64; 3], count: u64) -> [u8; 3] {
    [
        (sum[0] / count) as u8,
        (sum[1] / count) as u8,
        (sum[2] / count) as u8,
    ]
}

pub(super) fn alpha_points_in_upper(
    frame: &Raster,
    offset_x: u32,
    offset_y: u32,
    upper_limit: u32,
) -> BTreeSet<(u32, u32)> {
    let mut points = BTreeSet::new();
    for y in 0..frame.height() {
        if offset_y + y >= upper_limit {
            break;
        }
        for x in 0..frame.width() {
            if rgba_at(frame, x, y)[3] >= OPAQUE_ALPHA_THRESHOLD {
                points.insert((offset_x + x, offset_y + y));
            }
        }
    }
    points
}

pub(super) fn alpha_points_in_frame_upper(
    frame: &Raster,
    base_y: u32,
    upper_limit: u32,
) -> Vec<(u32, u32)> {
    let mut points = Vec::new();
    for y in 0..frame.height() {
        if base_y + y >= upper_limit {
            break;
        }
        for x in 0..frame.width() {
            if rgba_at(frame, x, y)[3] >= OPAQUE_ALPHA_THRESHOLD {
                points.push((x, y));
            }
        }
    }
    points
}

pub(super) fn build_shared_palette(frames: &[&Raster], size: usize) -> Vec<[u8; 3]> {
    let mut colors = Vec::new();
    for frame in frames {
        for y in 0..frame.height() {
            for x in 0..frame.width() {
                let pixel = rgba_at(frame, x, y);
                if pixel[3] >= OPAQUE_ALPHA_THRESHOLD {
                    colors.push([pixel[0], pixel[1], pixel[2]]);
                }
            }
        }
    }
    if colors.is_empty() || size == 0 {
        return Vec::new();
    }
    let mut boxes = vec![colors];
    while boxes.len() < size {
        let mut best: Option<(u8, usize, usize)> = None;
        for (index, color_box) in boxes.iter().enumerate() {
            if color_box.len() < 2 {
                continue;
            }
            let (spread, channel) = widest_channel(color_box);
            if spread > 0 && best.is_none_or(|current| spread > current.0) {
                best = Some((spread, channel, index));
            }
        }
        let Some((_, channel, index)) = best else {
            break;
        };
        let mut color_box = boxes.remove(index);
        color_box.sort_by_key(|color| color[channel]);
        let mid = color_box.len() / 2;
        boxes.push(color_box[..mid].to_vec());
        boxes.push(color_box[mid..].to_vec());
    }
    boxes
        .iter()
        .filter(|color_box| !color_box.is_empty())
        .map(|color_box| average_color(color_box))
        .collect()
}

pub(super) fn apply_palette(image: &Raster, palette: &[[u8; 3]]) -> PpResult<Raster> {
    if palette.is_empty() {
        return Ok(image.clone());
    }
    let mut output = image.clone();
    for y in 0..output.height() {
        for x in 0..output.width() {
            let pixel = rgba_at(&output, x, y);
            if pixel[3] < OPAQUE_ALPHA_THRESHOLD {
                set_rgba(&mut output, x, y, [0, 0, 0, 0]);
                continue;
            }
            let color = nearest_palette_color([pixel[0], pixel[1], pixel[2]], palette);
            set_rgba(&mut output, x, y, [color[0], color[1], color[2], 255]);
        }
    }
    Ok(output)
}

pub(super) fn enforce_outline(image: &Raster, strength: f64) -> PpResult<Raster> {
    let mut output = image.clone();
    let mut boundary = Vec::new();
    for y in 0..image.height() {
        for x in 0..image.width() {
            if rgba_at(image, x, y)[3] < OPAQUE_ALPHA_THRESHOLD {
                continue;
            }
            let touches_transparent = x == 0
                || y == 0
                || x + 1 >= image.width()
                || y + 1 >= image.height()
                || rgba_at(image, x - 1, y)[3] < OPAQUE_ALPHA_THRESHOLD
                || rgba_at(image, x + 1, y)[3] < OPAQUE_ALPHA_THRESHOLD
                || rgba_at(image, x, y - 1)[3] < OPAQUE_ALPHA_THRESHOLD
                || rgba_at(image, x, y + 1)[3] < OPAQUE_ALPHA_THRESHOLD;
            if touches_transparent {
                boundary.push((x, y));
            }
        }
    }
    let keep = 1.0 - strength.clamp(0.0, 1.0);
    for (x, y) in boundary {
        let pixel = rgba_at(&output, x, y);
        set_rgba(
            &mut output,
            x,
            y,
            [
                (f64::from(pixel[0]) * keep).round() as u8,
                (f64::from(pixel[1]) * keep).round() as u8,
                (f64::from(pixel[2]) * keep).round() as u8,
                255,
            ],
        );
    }
    Ok(output)
}

pub(super) fn crop_raster(image: &Raster, rect: FrameRect) -> PpResult<Raster> {
    if rect.w == 0 || rect.h == 0 {
        return Raster::blank(1, 1);
    }
    if rect.x + rect.w > image.width() || rect.y + rect.h > image.height() {
        return Err(PpError::InvalidRequest(format!(
            "crop {}x{} at {},{} exceeds image {}x{}",
            rect.w,
            rect.h,
            rect.x,
            rect.y,
            image.width(),
            image.height()
        )));
    }
    let mut output = Raster::blank(rect.w, rect.h)?;
    for y in 0..rect.h {
        for x in 0..rect.w {
            set_rgba(&mut output, x, y, rgba_at(image, rect.x + x, rect.y + y));
        }
    }
    Ok(output)
}

pub(super) fn non_empty_bbox(image: &Raster) -> Option<FrameRect> {
    let bbox = content_bbox(image);
    if bbox.w == 0 || bbox.h == 0 {
        None
    } else {
        Some(bbox)
    }
}

pub(super) fn alpha_nonzero_count(image: &Raster) -> u32 {
    image
        .pixels()
        .as_chunks::<4>()
        .0
        .iter()
        .filter(|pixel| pixel[3] > 0)
        .count() as u32
}

pub(super) fn alpha_centroid_x(image: &Raster, bottom_fraction: f64) -> f64 {
    let height = image.height();
    let rows = ((height as f64 * bottom_fraction).round() as u32)
        .max(2)
        .min(height);
    let y_start = height.saturating_sub(rows);
    let mut total = 0u64;
    let mut weighted = 0.0;
    for y in y_start..height {
        for x in 0..image.width() {
            let alpha = u64::from(rgba_at(image, x, y)[3]);
            if alpha > 0 {
                total += alpha;
                weighted += alpha as f64 * (x as f64 + 0.5);
            }
        }
    }
    if total == 0 && bottom_fraction < 1.0 {
        return alpha_centroid_x(image, 1.0);
    }
    if total == 0 {
        image.width() as f64 / 2.0
    } else {
        weighted / total as f64
    }
}

pub(super) fn color_distance_sq(left: [u8; 3], right: [u8; 3]) -> u32 {
    let mut sum = 0u32;
    for channel in 0..3 {
        let delta = i32::from(left[channel]) - i32::from(right[channel]);
        sum += (delta * delta) as u32;
    }
    sum
}

pub(super) fn luma(pixel: [u8; 4]) -> u32 {
    luma3([pixel[0], pixel[1], pixel[2]])
}

pub(super) fn luma3(color: [u8; 3]) -> u32 {
    u32::from(color[0]) * 299 + u32::from(color[1]) * 587 + u32::from(color[2]) * 114
}

#[cfg(test)]
mod tests {
    use super::average_rgb_sum;

    #[test]
    fn dominant_color_average_handles_sum_larger_than_u32() {
        let count = u64::from(u32::MAX) / 255 + 1;
        let sum = count * 255;

        assert!(sum > u64::from(u32::MAX));
        assert_eq!(average_rgb_sum([sum; 3], count), [255; 3]);
    }
}
