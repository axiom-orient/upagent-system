use super::*;

use super::chroma::chroma_adjacent_count;
use super::pipeline::{
    edge_histograms, pixel_delta, pixel_logical_geometry, range_f64, rgba_at, set_rgba,
};
use super::raster::{
    alpha_centroid_x, alpha_nonzero_count, conform_state_scale, crop_to_content_or_clone,
    dominant_block_color, non_empty_bbox,
};
use super::registration::register_row_frames;

pub(super) fn prepare_pixel_state(
    request: &NormalizeRequest,
    source: NormalizeStateImages,
    images: Vec<Raster>,
    mut warnings: Vec<String>,
) -> PpResult<PreparedState> {
    let per_frame_pitch = crate::io::parallel_map(&images, |image| {
        Ok::<_, std::convert::Infallible>(detect_pixel_pitch(image, 48))
    })
    .unwrap();
    let pitch = consensus_pitch(&per_frame_pitch, request.fit.pitch_hint);
    if pitch.is_none() && request.fit.pitch_hint.is_none() {
        warnings.push(format!(
            "{}: pixel pitch detection inconclusive; snap disabled",
            source.name
        ));
    }
    let snapped_and_phases = crate::io::parallel_map(&images, |image| {
        if let Some(pitch) = pitch {
            let phase = grid_phase(image, pitch);
            let snapped_image = grid_snap_downscale(image, pitch, request.fit.detail_bias, phase)?;
            Ok((crop_to_content_or_clone(&snapped_image)?, Some(phase)))
        } else {
            Ok((crop_to_content_or_clone(image)?, None))
        }
    })?;
    let (snapped, phases): (Vec<_>, Vec<_>) = snapped_and_phases.into_iter().unzip();
    let (logical_width, logical_height, scale) = pixel_logical_geometry(request);
    let conformed = conform_state_scale(
        &snapped,
        logical_width,
        logical_height,
        true,
        &request.fit.resample,
        request.fit.scale_conform,
    )?;
    let registered = register_row_frames(&conformed, 8, 3)?;
    Ok(PreparedState {
        name: source.name,
        method: "components/pixel-perfect".to_string(),
        frames: registered,
        pixel_perfect: true,
        pitch,
        scale,
        phases,
        warnings,
    })
}

pub(super) fn inspect_normalized_state(
    request: &NormalizeRequest,
    prepared: &PreparedState,
    output: &NormalizedStateOutput,
) -> NormalizeStateReport {
    let indexed_frames = output.frames.iter().enumerate().collect::<Vec<_>>();
    let records = crate::io::parallel_map(&indexed_frames, |&(index, frame)| {
        let bbox = non_empty_bbox(frame);
        let nontransparent_pixels = alpha_nonzero_count(frame);
        let chroma_adjacent_pixels = request
            .chroma
            .as_ref()
            .map_or(0, |chroma| chroma_adjacent_count(frame, chroma));
        let ground_y = bbox.map_or(0, |rect| rect.y + rect.h);
        let center_x = alpha_centroid_x(frame, 1.0);
        let phase = prepared.phases.get(index).and_then(|value| *value);
        Ok::<_, std::convert::Infallible>(NormalizeFrameReport {
            index: index as u32,
            width: frame.width(),
            height: frame.height(),
            content_box: bbox,
            nontransparent_pixels,
            chroma_adjacent_pixels,
            ground_y,
            center_x,
            pitch: prepared.pitch,
            phase_x: phase.map(|item| item.0),
            phase_y: phase.map(|item| item.1),
        })
    })
    .unwrap();

    let content_heights = records
        .iter()
        .filter_map(|record| record.content_box.map(|rect| f64::from(rect.h)))
        .collect::<Vec<_>>();
    let ground_ys = records
        .iter()
        .filter(|record| record.content_box.is_some())
        .map(|record| f64::from(record.ground_y))
        .collect::<Vec<_>>();
    let centers = records
        .iter()
        .filter(|record| record.content_box.is_some())
        .map(|record| record.center_x)
        .collect::<Vec<_>>();

    let mut errors = Vec::new();
    for record in &records {
        if record.nontransparent_pixels < request.quality.min_used_pixels {
            errors.push(format!(
                "frame {:02} is empty or too sparse ({} pixels)",
                record.index, record.nontransparent_pixels
            ));
        }
        if record.width != request.cell_width || record.height != request.cell_height {
            errors.push(format!(
                "frame {:02} is {}x{}, expected {}x{}",
                record.index, record.width, record.height, request.cell_width, request.cell_height
            ));
        }
    }

    NormalizeStateReport {
        name: output.name.clone(),
        method: prepared.method.clone(),
        frames: output.frames.len(),
        pixel_perfect: prepared.pixel_perfect,
        pitch: prepared.pitch,
        scale: prepared.scale,
        content_height_range: range_f64(&content_heights),
        ground_y_range: range_f64(&ground_ys),
        center_x_range: range_f64(&centers),
        frame_records: records,
        ok: errors.is_empty(),
        errors,
        warnings: prepared.warnings.clone(),
    }
}

pub(super) fn detect_pixel_pitch(image: &Raster, max_pitch: u32) -> u32 {
    let (col_edges, row_edges) = edge_histograms(image);
    let total_col = col_edges.iter().sum::<u32>().max(1);
    let total_row = row_edges.iter().sum::<u32>().max(1);
    let max_pitch = max_pitch.min(image.width().max(image.height()).max(2));
    let mut best_pitch = 1u32;
    let mut best_score = 0.2f64;
    for pitch in 2..=max_pitch {
        let score = axis_pitch_score(&col_edges, total_col, pitch)
            + axis_pitch_score(&row_edges, total_row, pitch);
        if score > best_score {
            best_pitch = pitch;
            best_score = score;
        }
    }
    best_pitch
}

pub(super) fn axis_pitch_score(edges: &[u32], total: u32, pitch: u32) -> f64 {
    let window = if pitch >= 8 { 1i32 } else { 0i32 };
    let mut best = 0.0_f64;
    for phase in 0..pitch {
        let mut hit = 0u32;
        for offset in -window..=window {
            let mut start = phase as i32 + offset;
            while start < 0 {
                start += pitch as i32;
            }
            let mut index = start as usize % pitch as usize;
            while index < edges.len() {
                hit += edges[index];
                index += pitch as usize;
            }
        }
        let frac = hit as f64 / total as f64;
        let chance = ((2 * window + 1) as f64 / pitch as f64).min(1.0);
        best = best.max(frac - chance);
    }
    best
}

pub(super) fn consensus_pitch(per_frame: &[u32], hint: Option<u32>) -> Option<u32> {
    let mut confident = per_frame
        .iter()
        .copied()
        .filter(|pitch| *pitch >= 2)
        .collect::<Vec<_>>();
    confident.sort_unstable();
    if !confident.is_empty() {
        return Some(confident[confident.len() / 2]);
    }
    hint.filter(|pitch| *pitch >= 2)
}

pub(super) fn grid_phase(image: &Raster, pitch: u32) -> (u32, u32) {
    let mut col_hits = vec![0u32; pitch as usize];
    let mut row_hits = vec![0u32; pitch as usize];
    for y in (0..image.height()).step_by(2) {
        for x in 1..image.width() {
            let a = rgba_at(image, x, y);
            let b = rgba_at(image, x - 1, y);
            if pixel_delta(a, b) > EDGE_DELTA_THRESHOLD {
                col_hits[(x % pitch) as usize] += 1;
            }
        }
    }
    for x in (0..image.width()).step_by(2) {
        for y in 1..image.height() {
            let a = rgba_at(image, x, y);
            let b = rgba_at(image, x, y - 1);
            if pixel_delta(a, b) > EDGE_DELTA_THRESHOLD {
                row_hits[(y % pitch) as usize] += 1;
            }
        }
    }
    (
        col_hits
            .iter()
            .enumerate()
            .max_by_key(|(_, value)| **value)
            .map(|(index, _)| index as u32)
            .unwrap_or(0),
        row_hits
            .iter()
            .enumerate()
            .max_by_key(|(_, value)| **value)
            .map(|(index, _)| index as u32)
            .unwrap_or(0),
    )
}

pub(super) fn grid_snap_downscale(
    image: &Raster,
    pitch: u32,
    detail_bias: bool,
    phase: (u32, u32),
) -> PpResult<Raster> {
    let mut x_edges = vec![0u32];
    let mut cursor_x = if phase.0 > 0 { phase.0 } else { pitch };
    while cursor_x < image.width() {
        x_edges.push(cursor_x);
        cursor_x += pitch;
    }
    x_edges.push(image.width());
    x_edges.sort_unstable();
    x_edges.dedup();

    let mut y_edges = vec![0u32];
    let mut cursor_y = if phase.1 > 0 { phase.1 } else { pitch };
    while cursor_y < image.height() {
        y_edges.push(cursor_y);
        cursor_y += pitch;
    }
    y_edges.push(image.height());
    y_edges.sort_unstable();
    y_edges.dedup();

    let mut output = Raster::blank((x_edges.len() - 1) as u32, (y_edges.len() - 1) as u32)?;
    for oy in 0..(y_edges.len() - 1) {
        for ox in 0..(x_edges.len() - 1) {
            let mut opaque = Vec::new();
            let mut block_len = 0usize;
            for y in y_edges[oy]..y_edges[oy + 1] {
                for x in x_edges[ox]..x_edges[ox + 1] {
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
            set_rgba(
                &mut output,
                ox as u32,
                oy as u32,
                [color[0], color[1], color[2], 255],
            );
        }
    }
    Ok(output)
}
