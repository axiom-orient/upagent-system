use std::collections::{BTreeMap, BTreeSet, VecDeque};

use super::{PpError, PpResult, Raster};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutoBackgroundPlan {
    pub selected_keys: Vec<[u8; 3]>,
    pub edge_coverage_basis_points: u16,
}

/// Select edge colors for controlled background removal without guessing at semantics.
pub fn plan_remove_background_auto(
    image: &Raster,
    max_keys: u8,
    min_edge_coverage_basis_points: u16,
) -> PpResult<AutoBackgroundPlan> {
    validate_auto_background_request(max_keys, min_edge_coverage_basis_points)?;
    let (edge_pixel_count, colors) = edge_rgb_counts(image);
    let selected_keys = colors
        .iter()
        .take(usize::from(max_keys))
        .map(|(rgb, _)| *rgb)
        .collect::<Vec<_>>();
    let selected_count = colors
        .iter()
        .take(usize::from(max_keys))
        .map(|(_, count)| *count)
        .sum::<u64>();
    let edge_coverage_basis_points = selected_count
        .checked_mul(10_000)
        .and_then(|value| value.checked_div(edge_pixel_count))
        .and_then(|value| u16::try_from(value).ok())
        .ok_or_else(|| {
            PpError::InvalidRequest("edge coverage calculation overflowed".to_string())
        })?;
    if edge_coverage_basis_points < min_edge_coverage_basis_points {
        return Err(PpError::InvalidRequest(format!(
            "remove_background_auto edge coverage {edge_coverage_basis_points} basis points is below required {min_edge_coverage_basis_points}"
        )));
    }
    Ok(AutoBackgroundPlan {
        selected_keys,
        edge_coverage_basis_points,
    })
}

pub(super) fn validate_auto_background_request(
    max_keys: u8,
    min_edge_coverage_basis_points: u16,
) -> PpResult<()> {
    if !(1..=16).contains(&max_keys) {
        return Err(PpError::InvalidRequest(
            "remove_background_auto maxKeys must be from 1 through 16".to_string(),
        ));
    }
    if !(1..=10_000).contains(&min_edge_coverage_basis_points) {
        return Err(PpError::InvalidRequest(
            "remove_background_auto minEdgeCoverageBasisPoints must be from 1 through 10000"
                .to_string(),
        ));
    }
    Ok(())
}

fn edge_rgb_counts(image: &Raster) -> (u64, Vec<([u8; 3], u64)>) {
    let mut colors = BTreeMap::<[u8; 3], u64>::new();
    let mut count = 0_u64;
    let mut record = |x: u32, y: u32| {
        let index = ((y as usize) * (image.width() as usize) + x as usize) * 4;
        let pixels = image.pixels();
        *colors
            .entry([pixels[index], pixels[index + 1], pixels[index + 2]])
            .or_default() += 1;
        count += 1;
    };

    for x in 0..image.width() {
        record(x, 0);
        if image.height() > 1 {
            record(x, image.height() - 1);
        }
    }
    if image.height() > 2 {
        for y in 1..image.height() - 1 {
            record(0, y);
            if image.width() > 1 {
                record(image.width() - 1, y);
            }
        }
    }

    let mut sorted = colors.into_iter().collect::<Vec<_>>();
    sorted.sort_by(|left, right| right.1.cmp(&left.1).then_with(|| left.0.cmp(&right.0)));
    (count, sorted)
}

pub(super) fn validate_background_keys(keys: &[[u8; 3]]) -> PpResult<()> {
    if keys.is_empty() || keys.len() > 16 {
        return Err(PpError::InvalidRequest(
            "remove_background keys must contain 1..=16 unique RGB colors".to_string(),
        ));
    }
    let unique = keys.iter().copied().collect::<BTreeSet<_>>();
    if unique.len() != keys.len() {
        return Err(PpError::InvalidRequest(
            "remove_background keys must contain 1..=16 unique RGB colors".to_string(),
        ));
    }
    Ok(())
}

pub(super) fn remove_background(
    image: &Raster,
    keys: &[[u8; 3]],
    tolerance: u8,
    feather: u8,
) -> PpResult<Raster> {
    validate_background_keys(keys)?;
    let eligibility = u16::from(tolerance) + u16::from(feather);
    let pixel_count = usize::try_from(u64::from(image.width()) * u64::from(image.height()))
        .map_err(|_| PpError::InvalidRequest("background mask size overflowed".to_string()))?;
    let mut visited = vec![false; pixel_count];
    let mut queue = VecDeque::<(u32, u32)>::new();

    for x in 0..image.width() {
        enqueue_background(image, x, 0, keys, eligibility, &mut visited, &mut queue);
        if image.height() > 1 {
            enqueue_background(
                image,
                x,
                image.height() - 1,
                keys,
                eligibility,
                &mut visited,
                &mut queue,
            );
        }
    }
    for y in 1..image.height().saturating_sub(1) {
        enqueue_background(image, 0, y, keys, eligibility, &mut visited, &mut queue);
        if image.width() > 1 {
            enqueue_background(
                image,
                image.width() - 1,
                y,
                keys,
                eligibility,
                &mut visited,
                &mut queue,
            );
        }
    }

    let mut output = image.clone();
    while let Some((x, y)) = queue.pop_front() {
        let byte_index = ((y as usize) * (image.width() as usize) + x as usize) * 4;
        let distance = key_distance(&image.pixels()[byte_index..byte_index + 3], keys);
        let alpha = image.pixels()[byte_index + 3];
        output.pixels_mut()[byte_index + 3] = feathered_alpha(alpha, distance, tolerance, feather);

        if x > 0 {
            enqueue_background(image, x - 1, y, keys, eligibility, &mut visited, &mut queue);
        }
        if x + 1 < image.width() {
            enqueue_background(image, x + 1, y, keys, eligibility, &mut visited, &mut queue);
        }
        if y > 0 {
            enqueue_background(image, x, y - 1, keys, eligibility, &mut visited, &mut queue);
        }
        if y + 1 < image.height() {
            enqueue_background(image, x, y + 1, keys, eligibility, &mut visited, &mut queue);
        }
    }
    Ok(output)
}

fn enqueue_background(
    image: &Raster,
    x: u32,
    y: u32,
    keys: &[[u8; 3]],
    eligibility: u16,
    visited: &mut [bool],
    queue: &mut VecDeque<(u32, u32)>,
) {
    let pixel_index = (y as usize) * (image.width() as usize) + x as usize;
    if visited[pixel_index] {
        return;
    }
    visited[pixel_index] = true;
    let byte_index = pixel_index * 4;
    if u16::from(key_distance(
        &image.pixels()[byte_index..byte_index + 3],
        keys,
    )) <= eligibility
    {
        queue.push_back((x, y));
    }
}

fn key_distance(rgb: &[u8], keys: &[[u8; 3]]) -> u8 {
    keys.iter()
        .map(|key| {
            rgb[0]
                .abs_diff(key[0])
                .max(rgb[1].abs_diff(key[1]))
                .max(rgb[2].abs_diff(key[2]))
        })
        .min()
        .unwrap_or(u8::MAX)
}

pub(super) fn feathered_alpha(alpha: u8, distance: u8, tolerance: u8, feather: u8) -> u8 {
    if distance <= tolerance {
        return 0;
    }
    if feather == 0 || u16::from(distance) >= u16::from(tolerance) + u16::from(feather) {
        return alpha;
    }
    let delta = u16::from(distance - tolerance);
    let feather = u16::from(feather);
    let scaled = u16::from(alpha) * delta;
    u8::try_from((scaled + feather / 2) / feather).unwrap_or(alpha)
}
