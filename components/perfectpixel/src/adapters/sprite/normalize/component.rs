use super::*;

use super::chroma::apply_chroma_if_configured;
use super::pipeline::{rects_intersect_with_padding, rgba_at, set_rgba};
use super::raster::{crop_raster, crop_to_content_or_clone};

pub(super) fn source_images(
    request: &NormalizeRequest,
    source: &NormalizeStateImages,
    warnings: &mut Vec<String>,
) -> PpResult<Vec<Raster>> {
    match &source.source {
        NormalizeStateSource::Frames(frames) => crate::io::parallel_map(frames, |frame| {
            let cleaned = apply_chroma_if_configured(frame, request.chroma.as_ref())?;
            crop_to_content_or_clone(&cleaned)
        }),
        NormalizeStateSource::Strip { image, frame_count } => {
            let cleaned = apply_chroma_if_configured(image, request.chroma.as_ref())?;
            if let Some(images) = extract_component_images(&cleaned, *frame_count as usize)? {
                return Ok(images);
            }
            if !request.fit.allow_slot_fallback {
                return Err(PpError::InvalidRequest(format!(
                    "state '{}' could not extract {} sprite components",
                    source.name, frame_count
                )));
            }
            warnings.push(format!(
                "{}: component extraction failed; using explicit slot fallback",
                source.name
            ));
            extract_slot_images(&cleaned, *frame_count)
        }
    }
}

pub(super) fn connected_components(image: &Raster) -> Vec<Component> {
    let width = image.width() as usize;
    let height = image.height() as usize;
    let len = width.saturating_mul(height);
    let mut visited = vec![false; len];
    let mut components = Vec::new();

    for start in 0..len {
        if visited[start] || image.pixels()[start * 4 + 3] <= HARD_ALPHA_THRESHOLD {
            continue;
        }
        let mut stack = vec![start];
        visited[start] = true;
        let mut pixels = Vec::new();
        let mut min_x = width;
        let mut min_y = height;
        let mut max_x = 0usize;
        let mut max_y = 0usize;

        while let Some(current) = stack.pop() {
            pixels.push(current);
            let x = current % width;
            let y = current / width;
            min_x = min_x.min(x);
            min_y = min_y.min(y);
            max_x = max_x.max(x);
            max_y = max_y.max(y);

            if x > 0 {
                push_component_neighbor(image, current - 1, &mut visited, &mut stack);
            }
            if x + 1 < width {
                push_component_neighbor(image, current + 1, &mut visited, &mut stack);
            }
            if y > 0 {
                push_component_neighbor(image, current - width, &mut visited, &mut stack);
            }
            if y + 1 < height {
                push_component_neighbor(image, current + width, &mut visited, &mut stack);
            }
        }

        let area = pixels.len();
        components.push(Component {
            pixels,
            area,
            bbox: FrameRect {
                x: min_x as u32,
                y: min_y as u32,
                w: (max_x - min_x + 1) as u32,
                h: (max_y - min_y + 1) as u32,
            },
            center_x: (min_x + max_x + 1) as f64 / 2.0,
        });
    }
    components
}

pub(super) fn push_component_neighbor(
    image: &Raster,
    index: usize,
    visited: &mut [bool],
    stack: &mut Vec<usize>,
) {
    if visited[index] || image.pixels()[index * 4 + 3] <= HARD_ALPHA_THRESHOLD {
        return;
    }
    visited[index] = true;
    stack.push(index);
}

pub(super) fn extract_component_images(
    image: &Raster,
    frame_count: usize,
) -> PpResult<Option<Vec<Raster>>> {
    let components = connected_components(image);
    if components.is_empty() || frame_count == 0 {
        return Ok(None);
    }
    let largest_area = components
        .iter()
        .map(|component| component.area)
        .max()
        .unwrap_or(0);
    let seed_threshold = 120usize.max((largest_area as f64 * 0.20).round() as usize);
    let mut seeds = components
        .iter()
        .filter(|component| component.area >= seed_threshold)
        .cloned()
        .collect::<Vec<_>>();
    if seeds.len() < frame_count {
        seeds = components.clone();
        seeds.sort_by_key(|component| std::cmp::Reverse(component.area));
        seeds.truncate(frame_count);
    }
    if seeds.len() < frame_count {
        return Ok(None);
    }
    seeds.sort_by(|left, right| left.center_x.total_cmp(&right.center_x));

    let mut groups = seeds
        .iter()
        .cloned()
        .map(|seed| vec![seed])
        .collect::<Vec<_>>();
    let seed_keys = seeds
        .iter()
        .map(|seed| {
            (
                seed.bbox.x,
                seed.bbox.y,
                seed.bbox.w,
                seed.bbox.h,
                seed.area,
            )
        })
        .collect::<BTreeSet<_>>();
    let noise_threshold = 12usize.max((largest_area as f64 * 0.002).round() as usize);

    for component in components {
        let key = (
            component.bbox.x,
            component.bbox.y,
            component.bbox.w,
            component.bbox.h,
            component.area,
        );
        if seed_keys.contains(&key) || component.area < noise_threshold {
            continue;
        }
        let nearest_index = seeds
            .iter()
            .enumerate()
            .min_by(|(_, left), (_, right)| {
                (left.center_x - component.center_x)
                    .abs()
                    .total_cmp(&(right.center_x - component.center_x).abs())
            })
            .map(|(index, _)| index)
            .unwrap_or(0);
        let seed = &seeds[nearest_index];
        let pad_x = 6u32.max((f64::from(seed.bbox.w) * 0.15).round() as u32);
        let pad_y = 6u32.max((f64::from(seed.bbox.h) * 0.15).round() as u32);
        if rects_intersect_with_padding(component.bbox, seed.bbox, pad_x, pad_y) {
            groups[nearest_index].push(component);
        }
    }

    groups
        .iter()
        .map(|group| component_group_image(image, group, 4))
        .collect::<PpResult<Vec<_>>>()
        .map(Some)
}

pub(super) fn component_group_image(
    image: &Raster,
    components: &[Component],
    padding: u32,
) -> PpResult<Raster> {
    let mut min_x = image.width();
    let mut min_y = image.height();
    let mut max_x = 0u32;
    let mut max_y = 0u32;
    for component in components {
        min_x = min_x.min(component.bbox.x);
        min_y = min_y.min(component.bbox.y);
        max_x = max_x.max(component.bbox.x + component.bbox.w);
        max_y = max_y.max(component.bbox.y + component.bbox.h);
    }
    min_x = min_x.saturating_sub(padding);
    min_y = min_y.saturating_sub(padding);
    max_x = image.width().min(max_x + padding);
    max_y = image.height().min(max_y + padding);
    let mut output = Raster::blank(max_x - min_x, max_y - min_y)?;
    let width = image.width() as usize;
    for component in components {
        for pixel_index in &component.pixels {
            let x = (*pixel_index % width) as u32;
            let y = (*pixel_index / width) as u32;
            let pixel = rgba_at(image, x, y);
            set_rgba(&mut output, x - min_x, y - min_y, pixel);
        }
    }
    Ok(output)
}

pub(super) fn extract_slot_images(image: &Raster, frame_count: u32) -> PpResult<Vec<Raster>> {
    let mut frames = Vec::new();
    for index in 0..frame_count {
        let left = ((u64::from(index) * u64::from(image.width()) + u64::from(frame_count) / 2)
            / u64::from(frame_count)) as u32;
        let right = ((u64::from(index + 1) * u64::from(image.width()) + u64::from(frame_count) / 2)
            / u64::from(frame_count)) as u32;
        let rect = FrameRect {
            x: left,
            y: 0,
            w: right.saturating_sub(left).max(1),
            h: image.height(),
        };
        frames.push(crop_raster(image, rect)?);
    }
    Ok(frames)
}
