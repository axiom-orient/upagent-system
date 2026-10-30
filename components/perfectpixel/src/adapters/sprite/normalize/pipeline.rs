use super::*;

use super::component::source_images;
use super::pixel_art::prepare_pixel_state;
use super::resize::prepare_regular_state;

pub(super) fn prepare_state(
    request: &NormalizeRequest,
    source: NormalizeStateImages,
) -> PpResult<PreparedState> {
    let mut warnings = Vec::new();
    let source_images = source_images(request, &source, &mut warnings)?;
    if source_images.is_empty() {
        return Err(PpError::InvalidRequest(format!(
            "state '{}' produced no source frames",
            source.name
        )));
    }
    if request.fit.pixel_perfect {
        prepare_pixel_state(request, source, source_images, warnings)
    } else {
        prepare_regular_state(request, source, source_images, warnings)
    }
}

pub(super) fn bundle_request_for(
    request: &NormalizeRequest,
    states: &[NormalizedStateOutput],
) -> SpriteBundleRequest {
    let state_requests = request
        .states
        .iter()
        .zip(states.iter())
        .map(|(state_request, output)| StateRequest {
            name: state_request.name.clone(),
            fps: state_request.fps,
            looped: state_request.looped,
            frames: (0..output.frames.len())
                .map(|index| format!("frames/{}/frame-{index:02}.png", state_request.name))
                .collect(),
        })
        .collect();
    SpriteBundleRequest {
        character: request.character.clone(),
        sheet_image: request.sheet_image.clone(),
        cell_width: request.cell_width,
        cell_height: request.cell_height,
        packing: request.packing.clone(),
        states: state_requests,
    }
}

pub(super) fn rects_intersect_with_padding(
    left: FrameRect,
    right: FrameRect,
    pad_x: u32,
    pad_y: u32,
) -> bool {
    let left_x0 = left.x;
    let left_y0 = left.y;
    let left_x1 = left.x + left.w;
    let left_y1 = left.y + left.h;
    let right_x0 = right.x.saturating_sub(pad_x);
    let right_y0 = right.y.saturating_sub(pad_y);
    let right_x1 = right.x + right.w + pad_x;
    let right_y1 = right.y + right.h + pad_y;
    left_x0 < right_x1 && left_x1 > right_x0 && left_y0 < right_y1 && left_y1 > right_y0
}

pub(super) fn edge_histograms(image: &Raster) -> (Vec<u32>, Vec<u32>) {
    let mut col_edges = vec![0u32; image.width() as usize];
    let mut row_edges = vec![0u32; image.height() as usize];
    for y in (0..image.height()).step_by(2) {
        for x in 1..image.width() {
            let a = rgba_at(image, x, y);
            let b = rgba_at(image, x - 1, y);
            if pixel_delta(a, b) > EDGE_DELTA_THRESHOLD {
                col_edges[x as usize] += 1;
            }
        }
    }
    for x in (0..image.width()).step_by(2) {
        for y in 1..image.height() {
            let a = rgba_at(image, x, y);
            let b = rgba_at(image, x, y - 1);
            if pixel_delta(a, b) > EDGE_DELTA_THRESHOLD {
                row_edges[y as usize] += 1;
            }
        }
    }
    (col_edges, row_edges)
}

pub(super) fn base_position(
    frame: &Raster,
    canvas_width: u32,
    canvas_height: u32,
    slack_y: u32,
) -> (u32, u32) {
    (
        (canvas_width - frame.width()) / 2,
        canvas_height
            .saturating_sub(slack_y)
            .saturating_sub(frame.height()),
    )
}

pub(super) fn pixel_logical_geometry(request: &NormalizeRequest) -> (u32, u32, u32) {
    let usable_height = request
        .cell_height
        .saturating_sub(request.safe_margin_y.saturating_mul(2))
        .max(1);
    let logical_height = request
        .fit
        .logical_height
        .unwrap_or(request.cell_height)
        .max(1);
    let mut scale = (request.cell_height / logical_height).max(1);
    if logical_height.saturating_mul(scale) > request.cell_height {
        scale = (usable_height / logical_height).max(1);
    }
    let logical_width = (request.cell_width / scale).max(1);
    (logical_width, logical_height, scale)
}

pub(super) fn overlay_raster(
    target: &mut Raster,
    source: &Raster,
    dst_x: u32,
    dst_y: u32,
) -> PpResult<()> {
    for y in 0..source.height() {
        for x in 0..source.width() {
            let pixel = rgba_at(source, x, y);
            match (dst_x.checked_add(x), dst_y.checked_add(y)) {
                (Some(target_x), Some(target_y))
                    if target_x < target.width() && target_y < target.height() =>
                {
                    if pixel[3] > 0 {
                        set_rgba(target, target_x, target_y, pixel);
                    }
                }
                _ if pixel[3] > 0 => {
                    return Err(PpError::InvalidRequest(format!(
                        "nontransparent source pixel ({x}, {y}) falls outside target {}x{} at ({dst_x}, {dst_y})",
                        target.width(),
                        target.height()
                    )));
                }
                _ => {}
            }
        }
    }
    Ok(())
}

pub(super) fn union_raster(frames: &[Raster]) -> PpResult<Raster> {
    let width = frames.iter().map(Raster::width).max().unwrap_or(1);
    let height = frames.iter().map(Raster::height).max().unwrap_or(1);
    let mut union = Raster::blank(width, height)?;
    for frame in frames {
        overlay_raster(&mut union, frame, 0, 0)?;
    }
    Ok(union)
}

pub(super) fn range_f64(values: &[f64]) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    let min = values.iter().copied().fold(f64::INFINITY, f64::min);
    let max = values.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    max - min
}

pub(super) fn median_f64(values: &[f64]) -> f64 {
    let mut sorted = values.to_vec();
    sorted.sort_by(|left, right| left.total_cmp(right));
    sorted[sorted.len() / 2]
}

pub(super) fn widest_channel(colors: &[[u8; 3]]) -> (u8, usize) {
    let mut best_range = 0u8;
    let mut best_channel = 0usize;
    for channel in 0..3 {
        let min = colors.iter().map(|color| color[channel]).min().unwrap_or(0);
        let max = colors.iter().map(|color| color[channel]).max().unwrap_or(0);
        let range = max.saturating_sub(min);
        if range > best_range {
            best_range = range;
            best_channel = channel;
        }
    }
    (best_range, best_channel)
}

pub(super) fn average_color(colors: &[[u8; 3]]) -> [u8; 3] {
    let mut sum = [0u64; 3];
    for color in colors {
        sum[0] += u64::from(color[0]);
        sum[1] += u64::from(color[1]);
        sum[2] += u64::from(color[2]);
    }
    [
        (sum[0] / colors.len() as u64) as u8,
        (sum[1] / colors.len() as u64) as u8,
        (sum[2] / colors.len() as u64) as u8,
    ]
}

pub(super) fn pixel_delta(left: [u8; 4], right: [u8; 4]) -> u32 {
    (i32::from(left[0]) - i32::from(right[0])).unsigned_abs()
        + (i32::from(left[1]) - i32::from(right[1])).unsigned_abs()
        + (i32::from(left[2]) - i32::from(right[2])).unsigned_abs()
        + (i32::from(left[3]) - i32::from(right[3])).unsigned_abs()
}

pub(super) fn rgba_at(image: &Raster, x: u32, y: u32) -> [u8; 4] {
    let index = pixel_byte_index(image.width(), x, y);
    let pixels = image.pixels();
    [
        pixels[index],
        pixels[index + 1],
        pixels[index + 2],
        pixels[index + 3],
    ]
}

pub(super) fn set_rgba(image: &mut Raster, x: u32, y: u32, pixel: [u8; 4]) {
    let index = pixel_byte_index(image.width(), x, y);
    image.pixels_mut()[index..index + 4].copy_from_slice(&pixel);
}

pub(super) fn pixel_linear_index(width: u32, x: u32, y: u32) -> usize {
    y as usize * width as usize + x as usize
}

pub(super) fn pixel_byte_index(width: u32, x: u32, y: u32) -> usize {
    pixel_linear_index(width, x, y) * 4
}

pub(super) fn clamp_i32(value: i32, min: i32, max: i32) -> i32 {
    value.max(min).min(max.max(min))
}

pub(super) fn is_safe_state_name(value: &str) -> bool {
    !value.is_empty()
        && value == value.trim()
        && !value.contains('/')
        && !value.contains('\\')
        && !value.contains("..")
        && !value.contains('\0')
}

pub(super) fn is_safe_file_name(value: &str) -> bool {
    !value.is_empty()
        && value == value.trim()
        && value != "."
        && value != ".."
        && !value.contains('/')
        && !value.contains('\\')
        && !value.contains('\0')
}

pub(super) fn default_sheet_image() -> String {
    "sprite-sheet.png".to_string()
}

pub(super) fn default_fps() -> u32 {
    8
}

pub(super) fn default_loop() -> bool {
    true
}

pub(super) fn default_true() -> bool {
    true
}

pub(super) fn default_fringe_threshold() -> f64 {
    180.0
}

pub(super) fn default_fringe_delta() -> f64 {
    18.0
}

pub(super) fn default_palette_size() -> usize {
    24
}

pub(super) fn default_outline_strength() -> f64 {
    0.62
}

pub(super) fn default_min_used_pixels() -> u32 {
    1
}

pub(super) fn default_registration_drift_px() -> f64 {
    2.0
}
