use super::*;

use super::pipeline::{pixel_linear_index, rgba_at, set_rgba};
use super::raster::color_distance;

pub(super) fn apply_chroma_if_configured(
    image: &Raster,
    chroma: Option<&NormalizeChroma>,
) -> PpResult<Raster> {
    match chroma {
        Some(chroma) => remove_chroma_background(image, chroma),
        None => Ok(image.clone()),
    }
}

pub(super) fn remove_chroma_background(
    image: &Raster,
    chroma: &NormalizeChroma,
) -> PpResult<Raster> {
    let mut output = image.clone();
    let width = image.width() as usize;
    let height = image.height() as usize;
    let len = width.checked_mul(height).ok_or_else(|| {
        PpError::InvalidRequest(format!(
            "image dimensions overflow: {}x{}",
            image.width(),
            image.height()
        ))
    })?;
    let mut classes = vec![0u8; len];
    let unseen = u8::MAX;
    let mut depths = vec![unseen; len];
    let mut keyed = Vec::new();
    let key = chroma.rgb;

    for y in 0..image.height() {
        for x in 0..image.width() {
            let index = pixel_linear_index(image.width(), x, y);
            let pixel = rgba_at(&output, x, y);
            let color = [pixel[0], pixel[1], pixel[2]];
            if pixel[3] == 0 || color_distance(color, key) <= chroma.threshold {
                set_rgba(&mut output, x, y, [0, 0, 0, 0]);
                classes[index] = 0;
                depths[index] = 0;
                keyed.push(index);
            } else if key_tint_score(color, key) < chroma.fringe_delta {
                classes[index] = 1;
            } else if color_distance(color, key) <= chroma.fringe_threshold {
                classes[index] = 2;
            } else {
                classes[index] = 3;
            }
        }
    }

    let key_tint = key_tint_score(key, key);
    let max_reach = if key_tint > 0.0 {
        chroma.unmix_reach
    } else {
        0
    };
    let has_keyed = !keyed.is_empty();
    let mut frontier = keyed;
    let mut depth = 0u8;
    while !frontier.is_empty() && depth < max_reach {
        depth = depth.saturating_add(1);
        let mut next_frontier = Vec::new();
        for index in frontier {
            let x = (index % width) as i32;
            let y = (index / width) as i32;
            for dy in -1..=1 {
                let ny = y + dy;
                if ny < 0 || ny >= height as i32 {
                    continue;
                }
                for dx in -1..=1 {
                    let nx = x + dx;
                    if nx < 0 || nx >= width as i32 {
                        continue;
                    }
                    let neighbor = ny as usize * width + nx as usize;
                    if depths[neighbor] == unseen {
                        depths[neighbor] = depth;
                        next_frontier.push(neighbor);
                    }
                }
            }
        }
        frontier = next_frontier;
    }

    if key_tint > 0.0 && chroma.unmix_reach > 0 {
        for y in 0..image.height() {
            for x in 0..image.width() {
                let index = pixel_linear_index(image.width(), x, y);
                if depths[index] == 0 || depths[index] > chroma.unmix_reach {
                    continue;
                }
                let class = classes[index];
                if class == 2 && depths[index] > IN_BAND_UNMIX_KEY_DEPTH {
                    continue;
                }
                if class != 2 && class != 3 {
                    continue;
                }
                let pixel = rgba_at(&output, x, y);
                let color = [pixel[0], pixel[1], pixel[2]];
                let unmixed =
                    unmix_key_blend(color, pixel[3], key, key_tint, key_tint_score(color, key));
                set_rgba(&mut output, x, y, unmixed);
            }
        }
    }

    if key_tint > 0.0 && has_keyed && chroma.spill_max_fraction > 0.0 {
        despill_trapped_clusters(&mut output, &classes, key, key_tint, chroma);
    }

    Ok(output)
}

pub(super) fn despill_trapped_clusters(
    image: &mut Raster,
    classes: &[u8],
    key: [u8; 3],
    key_tint: f64,
    chroma: &NormalizeChroma,
) {
    let width = image.width() as usize;
    let height = image.height() as usize;
    let subject_count = classes.iter().filter(|class| **class != 0).count();
    let spill_limit =
        32usize.max((subject_count as f64 * chroma.spill_max_fraction).round() as usize);
    let mut tints_left = vec![-1.0f64; width * height];
    for y in 0..image.height() {
        for x in 0..image.width() {
            let pixel = rgba_at(image, x, y);
            if pixel[3] == 0 {
                continue;
            }
            let color = [pixel[0], pixel[1], pixel[2]];
            let tint = key_tint_score(color, key);
            if tint >= chroma.fringe_delta {
                tints_left[pixel_linear_index(image.width(), x, y)] = tint;
            }
        }
    }

    let mut visited = vec![false; width * height];
    for start in 0..tints_left.len() {
        if tints_left[start] < 0.0 || visited[start] {
            continue;
        }
        let mut stack = vec![start];
        let mut cluster = Vec::new();
        visited[start] = true;
        while let Some(index) = stack.pop() {
            cluster.push(index);
            let x = index % width;
            let y = index / width;
            let x0 = x.saturating_sub(1);
            let y0 = y.saturating_sub(1);
            let x1 = (x + 1).min(width - 1);
            let y1 = (y + 1).min(height - 1);
            for ny in y0..=y1 {
                for nx in x0..=x1 {
                    let neighbor = ny * width + nx;
                    if tints_left[neighbor] >= 0.0 && !visited[neighbor] {
                        visited[neighbor] = true;
                        stack.push(neighbor);
                    }
                }
            }
        }
        if cluster.len() > spill_limit {
            continue;
        }
        if cluster
            .iter()
            .map(|index| tints_left[*index])
            .fold(0.0f64, f64::max)
            <= SPILL_MIN_TINT
        {
            continue;
        }
        for index in cluster {
            let x = (index % width) as u32;
            let y = (index / width) as u32;
            let pixel = rgba_at(image, x, y);
            let color = [pixel[0], pixel[1], pixel[2]];
            let (coverage, despilled) =
                despill_color(color, key, key_tint, key_tint_score(color, key));
            if coverage > 0.0 {
                set_rgba(
                    image,
                    x,
                    y,
                    [despilled[0], despilled[1], despilled[2], pixel[3]],
                );
            }
        }
    }
}

pub(super) fn key_tint_score(color: [u8; 3], chroma_key: [u8; 3]) -> f64 {
    let mut keyed_sum = 0u32;
    let mut keyed_count = 0u32;
    let mut unkeyed_sum = 0u32;
    let mut unkeyed_count = 0u32;
    for index in 0..3 {
        if chroma_key[index] >= 192 {
            keyed_sum += u32::from(color[index]);
            keyed_count += 1;
        } else if chroma_key[index] < 64 {
            unkeyed_sum += u32::from(color[index]);
            unkeyed_count += 1;
        }
    }
    if keyed_count == 0 || unkeyed_count == 0 {
        return 0.0;
    }
    keyed_sum as f64 / keyed_count as f64 - unkeyed_sum as f64 / unkeyed_count as f64
}

pub(super) fn despill_color(
    color: [u8; 3],
    chroma_key: [u8; 3],
    key_tint: f64,
    tint: f64,
) -> (f64, [u8; 3]) {
    let k = (tint / key_tint).min(1.0);
    let coverage = 1.0 - k;
    if coverage <= 0.0 {
        return (0.0, [0, 0, 0]);
    }
    let mut despilled = [0u8; 3];
    for index in 0..3 {
        let value = (f64::from(color[index]) - k * f64::from(chroma_key[index])) / coverage;
        despilled[index] = value.round().clamp(0.0, 255.0) as u8;
    }
    (coverage, despilled)
}

pub(super) fn unmix_key_blend(
    color: [u8; 3],
    alpha: u8,
    chroma_key: [u8; 3],
    key_tint: f64,
    tint: f64,
) -> [u8; 4] {
    let (coverage, despilled) = despill_color(color, chroma_key, key_tint, tint);
    let out_alpha = (f64::from(alpha) * coverage).round().clamp(0.0, 255.0) as u8;
    if out_alpha == 0 {
        return [0, 0, 0, 0];
    }
    [despilled[0], despilled[1], despilled[2], out_alpha]
}

pub(super) fn chroma_adjacent_count(image: &Raster, chroma: &NormalizeChroma) -> u32 {
    let mut count = 0u32;
    for pixel in image.pixels().as_chunks::<4>().0 {
        if pixel[3] > HARD_ALPHA_THRESHOLD
            && color_distance([pixel[0], pixel[1], pixel[2]], chroma.rgb)
                <= chroma.adjacent_threshold
        {
            count += 1;
        }
    }
    count
}

pub(super) fn default_key_threshold() -> f64 {
    96.0
}

pub(super) fn default_unmix_reach() -> u8 {
    4
}

pub(super) fn default_spill_max_fraction() -> f64 {
    0.005
}

pub(super) fn default_chroma_adjacent_threshold() -> f64 {
    150.0
}

pub(super) fn default_chroma_adjacent_pixel_threshold() -> u32 {
    120
}
