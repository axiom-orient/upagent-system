//! Reusable vector quality and protected-predicate gates.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use crate::core::{PpError, PpResult, Raster, SvgIr};

use super::{
    canonical_rgba,
    request::{canonical_palette_color, VectorPolicy},
};

pub(crate) mod alpha;
pub(crate) mod topology;

const SSIM_BLOCK_SIZE: u32 = 8;
/// Deterministic compact-profile assessment over the bounded SVG IR.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CompactStructuralAssessment {
    pub(crate) passed: bool,
    pub(crate) reasons: Vec<String>,
}

/// Compact output forbids executable, animated, foreign, and externally linked structure.
pub(crate) fn compact_structural_assessment(candidate: &SvgIr) -> CompactStructuralAssessment {
    let mut reasons = Vec::new();
    for element in &candidate.elements {
        if matches!(
            element.local_name.as_str(),
            "animate" | "animateMotion" | "animateTransform" | "script" | "set" | "foreignObject"
        ) {
            reasons.push(format!("forbiddenElement:{}", element.local_name));
        }
        for (name, value) in &element.attributes {
            if name.starts_with("on") {
                reasons.push(format!("eventAttribute:{name}"));
            }
            if matches!(name.as_str(), "href" | "xlink:href")
                && (value.starts_with("http:")
                    || value.starts_with("https:")
                    || value.starts_with("data:"))
            {
                reasons.push(format!("externalReference:{name}"));
            }
        }
    }
    CompactStructuralAssessment {
        passed: reasons.is_empty(),
        reasons,
    }
}

/// Shared protected raster predicates used by generation and controlled defect evidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ProtectedRasterGateAssessment {
    pub(crate) exact_rgba: bool,
    pub(crate) alpha: bool,
    pub(crate) topology: bool,
}

/// Shared resource predicates parameterized by verified embedded limits.
pub(crate) const MAXIMUM_INPUT_COLORS: usize = 65_536;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ResourceGateAssessment {
    pub(crate) input_pixels: u64,
    pub(crate) maximum_input_pixels: u64,
    pub(crate) distinct_colors: usize,
    pub(crate) maximum_distinct_colors: usize,
}

impl ResourceGateAssessment {
    pub(crate) const fn passed(self) -> bool {
        self.input_pixels <= self.maximum_input_pixels
            && self.distinct_colors <= self.maximum_distinct_colors
    }
}

pub(crate) fn protected_raster_gate_assessment(
    source: &Raster,
    candidate: &Raster,
) -> ProtectedRasterGateAssessment {
    let exact_rgba = exact_rgba_preservation_gate(source, candidate);
    if exact_rgba {
        return ProtectedRasterGateAssessment {
            exact_rgba,
            alpha: true,
            topology: true,
        };
    }
    ProtectedRasterGateAssessment {
        exact_rgba,
        alpha: alpha::protected_alpha_gate(source, candidate).passed(),
        topology: topology::protected_topology_gate(source, candidate).passed(),
    }
}
/// Independent geometry evidence for adaptive recolors. Backend adaptive evidence supplies only
/// the maximum fragment size; this assessment derives every pass/fail fact from the render-back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct AdaptiveProtectedGeometryAssessment {
    pub(crate) dimensions_match: bool,
    pub(crate) alpha_bytes_match: bool,
    pub(crate) opacity_mask_match: bool,
    pub(crate) components_match: bool,
    pub(crate) holes_match: bool,
    pub(crate) output_paints_existing: bool,
    pub(crate) changed_components: usize,
    pub(crate) largest_changed_component: usize,
    pub(crate) oversized_components: usize,
    pub(crate) island_components: usize,
    pub(crate) narrow_components: usize,
    pub(crate) edge_components: usize,
    pub(crate) corner_components: usize,
    pub(crate) endpoint_components: usize,
    pub(crate) junction_components: usize,
    pub(crate) mixed_target_components: usize,
    pub(crate) invalid_boundary_components: usize,
    pub(crate) incomplete_source_components: usize,
    pub(crate) edges: bool,
    pub(crate) endpoints: bool,
    pub(crate) features: bool,
    pub(crate) junctions: bool,
}

/// Assesses adaptive recolors without trusting backend success claims.
///
/// A source paint is stable only when it occurs outside the permitted fragment budget. This
/// prevents a changed fragment from laundering another small feature through its boundary.
pub(crate) fn adaptive_protected_geometry_assessment(
    source: &Raster,
    candidate: &Raster,
    fragment_pixels: usize,
) -> AdaptiveProtectedGeometryAssessment {
    let dimensions_match =
        source.width() == candidate.width() && source.height() == candidate.height();
    let topology = topology::protected_topology_gate(source, candidate);
    let topology_evidence = topology.evidence();
    let mut assessment = AdaptiveProtectedGeometryAssessment {
        dimensions_match,
        alpha_bytes_match: dimensions_match
            && source
                .pixels()
                .as_chunks::<4>()
                .0
                .iter()
                .zip(candidate.pixels().as_chunks::<4>().0)
                .all(|(left, right)| left[3] == right[3]),
        opacity_mask_match: dimensions_match && topology_evidence.mismatched_mask_pixels == 0,
        components_match: dimensions_match
            && topology_evidence.source_components == topology_evidence.candidate_components,
        holes_match: dimensions_match
            && topology_evidence.source_holes == topology_evidence.candidate_holes,
        output_paints_existing: false,
        changed_components: 0,
        largest_changed_component: 0,
        oversized_components: 0,
        island_components: 0,
        narrow_components: 0,
        edge_components: 0,
        corner_components: 0,
        endpoint_components: 0,
        junction_components: 0,
        mixed_target_components: 0,
        invalid_boundary_components: 0,
        incomplete_source_components: 0,
        edges: false,
        endpoints: false,
        features: false,
        junctions: false,
    };
    if !dimensions_match || fragment_pixels == 0 {
        return assessment;
    }

    let source_paints = source
        .pixels()
        .as_chunks::<4>()
        .0
        .iter()
        .map(|pixel| canonical_rgba(pixel))
        .map(|pixel| [pixel[0], pixel[1], pixel[2]])
        .collect::<BTreeSet<_>>();
    assessment.output_paints_existing = candidate
        .pixels()
        .as_chunks::<4>()
        .0
        .iter()
        .map(|pixel| canonical_rgba(pixel))
        .all(|pixel| source_paints.contains(&[pixel[0], pixel[1], pixel[2]]));

    let width = source.width() as usize;
    let height = source.height() as usize;
    let mut source_paint_counts = BTreeMap::new();
    for pixel in source.pixels().as_chunks::<4>().0 {
        if pixel[3] != 0 {
            *source_paint_counts
                .entry([pixel[0], pixel[1], pixel[2]])
                .or_insert(0usize) += 1;
        }
    }
    let changed = source
        .pixels()
        .as_chunks::<4>()
        .0
        .iter()
        .zip(candidate.pixels().as_chunks::<4>().0)
        .map(|(left, right)| left != right)
        .collect::<Vec<_>>();
    let mut visited = vec![false; changed.len()];

    for start in 0..changed.len() {
        if !changed[start] || visited[start] {
            continue;
        }
        let mut component = Vec::new();
        let mut queue = VecDeque::from([start]);
        visited[start] = true;
        while let Some(index) = queue.pop_front() {
            component.push(index);
            let x = index % width;
            let y = index / width;
            for neighbor in raster_neighbors(x, y, width, height) {
                if changed[neighbor] && !visited[neighbor] {
                    visited[neighbor] = true;
                    queue.push_back(neighbor);
                }
            }
        }

        assessment.changed_components += 1;
        assessment.largest_changed_component =
            assessment.largest_changed_component.max(component.len());
        if component.len() > fragment_pixels {
            assessment.oversized_components += 1;
        }
        if component.len() == 1 {
            assessment.island_components += 1;
        }

        let mut min_x = width;
        let mut max_x = 0;
        let mut min_y = height;
        let mut max_y = 0;
        let mut targets = BTreeSet::new();
        let mut source_colors = BTreeSet::new();
        let component_members = component.iter().copied().collect::<BTreeSet<_>>();
        let mut boundary_paints = BTreeSet::new();
        let mut touches_alpha_or_image_edge = false;
        for &index in &component {
            let x = index % width;
            let y = index / width;
            min_x = min_x.min(x);
            max_x = max_x.max(x);
            min_y = min_y.min(y);
            max_y = max_y.max(y);
            let source_pixel = &source.pixels()[index * 4..index * 4 + 4];
            let candidate_pixel = &candidate.pixels()[index * 4..index * 4 + 4];
            source_colors.insert([source_pixel[0], source_pixel[1], source_pixel[2]]);
            targets.insert([candidate_pixel[0], candidate_pixel[1], candidate_pixel[2]]);
            if x == 0 || y == 0 || x + 1 == width || y + 1 == height {
                touches_alpha_or_image_edge = true;
            }
            for neighbor in raster_neighbors(x, y, width, height) {
                if component_members.contains(&neighbor) {
                    continue;
                }
                let neighbor_pixel = &source.pixels()[neighbor * 4..neighbor * 4 + 4];
                if neighbor_pixel[3] == 0 {
                    touches_alpha_or_image_edge = true;
                } else {
                    boundary_paints.insert([
                        neighbor_pixel[0],
                        neighbor_pixel[1],
                        neighbor_pixel[2],
                    ]);
                }
            }
        }
        if max_x - min_x + 1 < 2 || max_y - min_y + 1 < 2 {
            assessment.narrow_components += 1;
        }
        if component.len() != (max_x - min_x + 1) * (max_y - min_y + 1) {
            assessment.corner_components += 1;
        }
        if touches_alpha_or_image_edge {
            assessment.edge_components += 1;
        }
        if targets.len() != 1 {
            assessment.mixed_target_components += 1;
        }

        let source_component_complete = source_colors.len() == 1
            && complete_source_component(&component_members, source, component[0], width, height);
        if !source_component_complete {
            assessment.incomplete_source_components += 1;
        }
        let target = targets.iter().next().copied();
        let stable_boundary = boundary_paints.len() == 2
            && boundary_paints.iter().all(|paint| {
                source_paint_counts.get(paint).copied().unwrap_or(0) > fragment_pixels
            })
            && target.is_some_and(|paint| boundary_paints.contains(&paint));
        if !stable_boundary || !source_component_complete {
            assessment.invalid_boundary_components += 1;
            match boundary_paints.len() {
                0 | 1 => assessment.endpoint_components += 1,
                2 => {}
                _ => assessment.junction_components += 1,
            }
        }
    }

    let foundation = assessment.dimensions_match
        && assessment.alpha_bytes_match
        && assessment.opacity_mask_match
        && assessment.components_match
        && assessment.holes_match
        && assessment.output_paints_existing;
    assessment.edges = foundation && assessment.edge_components == 0;
    assessment.endpoints =
        foundation && assessment.edge_components == 0 && assessment.endpoint_components == 0;
    assessment.features = foundation
        && assessment.island_components == 0
        && assessment.narrow_components == 0
        && assessment.corner_components == 0
        && assessment.oversized_components == 0
        && assessment.incomplete_source_components == 0;
    assessment.junctions = foundation
        && assessment.junction_components == 0
        && assessment.invalid_boundary_components == 0
        && assessment.mixed_target_components == 0
        && assessment.incomplete_source_components == 0;
    assessment
}

fn complete_source_component(
    changed_component: &BTreeSet<usize>,
    source: &Raster,
    start: usize,
    width: usize,
    height: usize,
) -> bool {
    let source_pixel = &source.pixels()[start * 4..start * 4 + 4];
    let mut source_component = BTreeSet::from([start]);
    let mut queue = VecDeque::from([start]);
    while let Some(index) = queue.pop_front() {
        let x = index % width;
        let y = index / width;
        for neighbor in raster_neighbors(x, y, width, height) {
            if source_component.contains(&neighbor) {
                continue;
            }
            let neighbor_pixel = &source.pixels()[neighbor * 4..neighbor * 4 + 4];
            if neighbor_pixel == source_pixel {
                source_component.insert(neighbor);
                queue.push_back(neighbor);
            }
        }
    }
    source_component == *changed_component
}
fn raster_neighbors(
    x: usize,
    y: usize,
    width: usize,
    height: usize,
) -> impl Iterator<Item = usize> {
    [
        (x.wrapping_sub(1), y),
        (x + 1, y),
        (x, y.wrapping_sub(1)),
        (x, y + 1),
    ]
    .into_iter()
    .filter(move |(next_x, next_y)| *next_x < width && *next_y < height)
    .map(move |(next_x, next_y)| next_y * width + next_x)
}

pub(crate) fn input_resource_gate_assessment(
    source: &Raster,
    maximum_input_pixels: u64,
) -> ResourceGateAssessment {
    let input_pixels = u64::from(source.width()).saturating_mul(u64::from(source.height()));
    if input_pixels > maximum_input_pixels {
        return ResourceGateAssessment {
            input_pixels,
            maximum_input_pixels,
            distinct_colors: 0,
            maximum_distinct_colors: MAXIMUM_INPUT_COLORS,
        };
    }
    let mut colors = BTreeSet::new();
    for pixel in source.pixels().as_chunks::<4>().0 {
        colors.insert(if pixel[3] == 0 {
            [0, 0, 0, 0]
        } else {
            [pixel[0], pixel[1], pixel[2], pixel[3]]
        });
        if colors.len() > MAXIMUM_INPUT_COLORS {
            break;
        }
    }
    ResourceGateAssessment {
        input_pixels,
        maximum_input_pixels,
        distinct_colors: colors.len(),
        maximum_distinct_colors: MAXIMUM_INPUT_COLORS,
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct QualityReport {
    pub(crate) quality_score: f64,
    pub(crate) pixel_score: f64,
    pub(crate) alpha_score: f64,
    pub(crate) luma_ssim: f64,
    pub(crate) local_luma_ssim: f64,
    pub(crate) worst_block_luma_ssim: f64,
    pub(crate) normalized_rmse: f64,
    pub(crate) normalized_mae: f64,
    pub(crate) changed_pixel_ratio: f64,
    pub(crate) max_channel_error: u8,
}

pub(super) fn compare_rasters(reference: &Raster, rendered: &Raster) -> PpResult<QualityReport> {
    if reference.width() != rendered.width() || reference.height() != rendered.height() {
        return Err(PpError::SvgRender(format!(
            "rendered SVG size {}x{} does not match input {}x{}",
            rendered.width(),
            rendered.height(),
            reference.width(),
            reference.height()
        )));
    }
    let mut sum_abs = 0u64;
    let mut sum_sq = 0u64;
    let mut alpha_abs = 0u64;
    let mut changed_pixels = 0usize;
    let mut max_channel_error = 0u8;
    let mut reference_luma = Vec::new();
    let mut rendered_luma = Vec::new();

    for y in 0..reference.height() {
        for x in 0..reference.width() {
            let left = reference.premultiplied_pixel(x, y);
            let right = rendered.premultiplied_pixel(x, y);
            let mut pixel_changed = false;
            for channel in 0..4 {
                let diff = left[channel].abs_diff(right[channel]);
                if diff > 0 {
                    pixel_changed = true;
                    max_channel_error = max_channel_error.max(diff);
                }
                sum_abs += u64::from(diff);
                sum_sq += u64::from(diff) * u64::from(diff);
            }
            alpha_abs += u64::from(left[3].abs_diff(right[3]));
            reference_luma.push(luma(&left));
            rendered_luma.push(luma(&right));
            if pixel_changed {
                changed_pixels += 1;
            }
        }
    }

    let pixel_count = u64::from(reference.width()) * u64::from(reference.height());
    if pixel_count == 0 {
        return Err(PpError::SvgRender(
            "cannot evaluate empty image quality".to_string(),
        ));
    }
    let channel_count = pixel_count as f64 * 4.0;
    let normalized_mae = sum_abs as f64 / (channel_count * 255.0);
    let normalized_rmse = (sum_sq as f64 / (channel_count * 255.0 * 255.0)).sqrt();
    let changed_pixel_ratio = changed_pixels as f64 / pixel_count as f64;
    let pixel_score = (1.0 - normalized_rmse).clamp(0.0, 1.0);
    let alpha_score = (1.0 - alpha_abs as f64 / (pixel_count as f64 * 255.0)).clamp(0.0, 1.0);
    let luma_ssim = ssim_for_samples(&reference_luma, &rendered_luma).clamp(0.0, 1.0);
    let local_ssim = local_luma_ssim(reference, rendered);
    let quality_score = pixel_score
        .min(alpha_score)
        .min(luma_ssim)
        .min(local_ssim.mean);

    Ok(QualityReport {
        quality_score,
        pixel_score,
        alpha_score,
        luma_ssim,
        local_luma_ssim: local_ssim.mean,
        worst_block_luma_ssim: local_ssim.worst,
        normalized_rmse,
        normalized_mae,
        changed_pixel_ratio,
        max_channel_error,
    })
}

fn luma(pixel: &[u8]) -> f64 {
    0.2126 * f64::from(pixel[0]) + 0.7152 * f64::from(pixel[1]) + 0.0722 * f64::from(pixel[2])
}

#[derive(Debug, Clone, Copy)]
struct LocalSsim {
    mean: f64,
    worst: f64,
}

fn local_luma_ssim(reference: &Raster, rendered: &Raster) -> LocalSsim {
    let block_size = SSIM_BLOCK_SIZE
        .min(reference.width())
        .min(reference.height())
        .max(1);
    let mut block_scores = Vec::new();

    let mut y = 0;
    while y < reference.height() {
        let block_height = block_size.min(reference.height() - y);
        let mut x = 0;
        while x < reference.width() {
            let block_width = block_size.min(reference.width() - x);
            block_scores.push(block_luma_quality(
                reference,
                rendered,
                x,
                y,
                block_width,
                block_height,
            ));
            x += block_size;
        }
        y += block_size;
    }

    if block_scores.is_empty() {
        return LocalSsim {
            mean: 0.0,
            worst: 0.0,
        };
    }

    let sum = block_scores.iter().sum::<f64>();
    let worst = block_scores
        .iter()
        .copied()
        .fold(1.0_f64, |current, score| current.min(score));
    LocalSsim {
        mean: (sum / block_scores.len() as f64).clamp(0.0, 1.0),
        worst: worst.clamp(0.0, 1.0),
    }
}

fn block_luma_quality(
    reference: &Raster,
    rendered: &Raster,
    x0: u32,
    y0: u32,
    width: u32,
    height: u32,
) -> f64 {
    let mut reference_luma = Vec::with_capacity((width * height) as usize);
    let mut rendered_luma = Vec::with_capacity((width * height) as usize);
    for y in y0..y0 + height {
        for x in x0..x0 + width {
            reference_luma.push(luma(&reference.premultiplied_pixel(x, y)));
            rendered_luma.push(luma(&rendered.premultiplied_pixel(x, y)));
        }
    }
    let ssim = ssim_for_samples(&reference_luma, &rendered_luma).clamp(0.0, 1.0);
    let mean_square_error = reference_luma
        .iter()
        .zip(&rendered_luma)
        .map(|(left, right)| (left - right).powi(2))
        .sum::<f64>()
        / reference_luma.len() as f64;
    let pixel_score = (1.0 - mean_square_error.sqrt() / 255.0).clamp(0.0, 1.0);
    let low_energy_floor = (0.03_f64 * 255.0).powi(2);
    let maximum_energy = [reference_luma.as_slice(), rendered_luma.as_slice()]
        .into_iter()
        .map(|samples| {
            samples.iter().map(|value| value * value).sum::<f64>() / samples.len() as f64
        })
        .fold(0.0_f64, f64::max);

    // SSIM's contrast term is ill-conditioned below its standard C2 stabilizer.
    // There, use normalized RMS error; otherwise retain SSIM without a masking max().
    if maximum_energy <= low_energy_floor {
        pixel_score
    } else {
        ssim
    }
}

fn ssim_for_samples(reference: &[f64], rendered: &[f64]) -> f64 {
    if reference.len() != rendered.len() || reference.is_empty() {
        return 0.0;
    }
    let n = reference.len() as f64;
    let mean_ref = reference.iter().sum::<f64>() / n;
    let mean_rendered = rendered.iter().sum::<f64>() / n;
    let mut var_ref = 0.0;
    let mut var_rendered = 0.0;
    let mut covariance = 0.0;
    for (&left, &right) in reference.iter().zip(rendered.iter()) {
        let d_ref = left - mean_ref;
        let d_rendered = right - mean_rendered;
        var_ref += d_ref * d_ref;
        var_rendered += d_rendered * d_rendered;
        covariance += d_ref * d_rendered;
    }
    var_ref /= n;
    var_rendered /= n;
    covariance /= n;

    let c1 = (0.01_f64 * 255.0).powi(2);
    let c2 = (0.03_f64 * 255.0).powi(2);
    ((2.0 * mean_ref * mean_rendered + c1) * (2.0 * covariance + c2))
        / ((mean_ref.powi(2) + mean_rendered.powi(2) + c1) * (var_ref + var_rendered + c2))
}

/// Verifies declared palette restrictions against the bounded SVG IR. It intentionally does not
/// classify or discard source pixels: `allow_drop_noise` cannot weaken this zero-budget gate.
pub(crate) fn protected_palette_gate(
    source: &Raster,
    candidate: &SvgIr,
    policy: &VectorPolicy,
) -> bool {
    if policy.allow_drop_noise() {
        return false;
    }
    let Some(output_colors) = candidate_palette(candidate) else {
        return false;
    };
    let Some(allowed) = policy
        .allowed_palette()
        .iter()
        .map(|color| canonical_palette_color(color))
        .collect::<Option<BTreeSet<_>>>()
    else {
        return false;
    };
    let Some(required) = policy
        .required_palette()
        .iter()
        .map(|color| canonical_palette_color(color))
        .collect::<Option<BTreeSet<_>>>()
    else {
        return false;
    };

    (allowed.is_empty() || output_colors.is_subset(&allowed))
        && required.is_subset(&output_colors)
        && (!policy.reject_unmapped() || source_palette(source).is_subset(&output_colors))
}

/// Conservatively proves every zero-budget protected geometry predicate at once.
pub(crate) fn exact_rgba_preservation_gate(source: &Raster, candidate: &Raster) -> bool {
    source.width() == candidate.width()
        && source.height() == candidate.height()
        && source.pixels() == candidate.pixels()
}

fn candidate_palette(candidate: &SvgIr) -> Option<BTreeSet<String>> {
    let mut colors = BTreeSet::new();
    for paint in candidate
        .elements
        .iter()
        .filter_map(|element| element.paint.as_ref())
    {
        match paint.fill.as_deref() {
            Some(value) if value.eq_ignore_ascii_case("none") => {}
            Some(value) => {
                colors.insert(canonical_palette_color(value)?);
            }
            None => {
                colors.insert("#000000".to_owned());
            }
        };
        if let Some(value) = paint.stroke.as_deref() {
            if !value.eq_ignore_ascii_case("none") {
                colors.insert(canonical_palette_color(value)?);
            }
        }
    }
    Some(colors)
}

fn source_palette(source: &Raster) -> BTreeSet<String> {
    source
        .pixels()
        .as_chunks::<4>()
        .0
        .iter()
        .filter(|pixel| pixel[3] != 0)
        .map(|pixel| format!("#{:02x}{:02x}{:02x}", pixel[0], pixel[1], pixel[2]))
        .collect()
}

#[cfg(test)]
fn controlled_defect_probe() -> PpResult<()> {
    use crate::io::{DecodeLimits, ImageCodec};
    use std::{env, fs, path::Path};

    let parent = env::var("PERFECTPIXEL_CONTROLLED_DEFECT_PARENT").map_err(|_| {
        PpError::InvalidRequest("controlled defect parent path is required".to_owned())
    })?;
    let defect = env::var("PERFECTPIXEL_CONTROLLED_DEFECT_DEFECT").map_err(|_| {
        PpError::InvalidRequest("controlled defect raster path is required".to_owned())
    })?;
    let report = env::var("PERFECTPIXEL_CONTROLLED_DEFECT_REPORT").map_err(|_| {
        PpError::InvalidRequest("controlled defect report path is required".to_owned())
    })?;
    let parent = ImageCodec::decode_rgba(Path::new(&parent), DecodeLimits::PRODUCTION)?;
    let defect = ImageCodec::decode_rgba(Path::new(&defect), DecodeLimits::PRODUCTION)?;
    let authority = super::authority::VerifiedAuthority::embedded()
        .map_err(|error| PpError::Vectorizer(error.to_string()))?;
    let resource = input_resource_gate_assessment(
        &defect,
        authority.thresholds().defaults().maximum_input_pixels,
    );
    let protected = protected_raster_gate_assessment(&parent, &defect);
    let actual = if !resource.passed() {
        "RESOURCE_GATE_FAILED"
    } else if !(protected.exact_rgba && protected.alpha && protected.topology) {
        "PROTECTED_GATE_FAILED"
    } else {
        "APPROVED"
    };
    let rejected = actual != "APPROVED";
    let bytes = format!(
        "{{\"schemaVersion\":1,\"kind\":\"perfectpixel.controlled-defect-probe/1\",\"parentWidth\":{},\"parentHeight\":{},\"defectWidth\":{},\"defectHeight\":{},\"actualRejectionCode\":\"{}\",\"rejected\":{},\"passed\":true,\"resourceGate\":{{\"passed\":{},\"inputPixels\":{},\"maximumInputPixels\":{},\"distinctColors\":{},\"maximumDistinctColors\":{}}},\"protectedGate\":{{\"exactRgba\":{},\"alpha\":{},\"topology\":{}}}}}\n",
        parent.width(),
        parent.height(),
        defect.width(),
        defect.height(),
        actual,
        rejected,
        resource.passed(),
        resource.input_pixels,
        resource.maximum_input_pixels,
        resource.distinct_colors,
        resource.maximum_distinct_colors,
        protected.exact_rgba,
        protected.alpha,
        protected.topology,
    );
    fs::write(report, bytes).map_err(|error| PpError::FileIo {
        path: env::var("PERFECTPIXEL_CONTROLLED_DEFECT_REPORT")
            .unwrap()
            .into(),
        message: error.to_string(),
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIN_WORST_BLOCK_LUMA_SSIM: f64 = 0.950;

    #[test]
    fn controlled_defect_probe_from_environment() -> PpResult<()> {
        if std::env::var_os("PERFECTPIXEL_CONTROLLED_DEFECT_PARENT").is_none() {
            return Ok(());
        }
        controlled_defect_probe()
    }
    #[test]
    fn identical_rasters_score_one() -> PpResult<()> {
        let image = Raster::new(1, 1, vec![10, 20, 30, 255])?;
        let quality = compare_rasters(&image, &image)?;
        assert_eq!(quality.quality_score, 1.0);
        assert_eq!(quality.normalized_rmse, 0.0);
        assert_eq!(quality.normalized_mae, 0.0);
        assert_eq!(quality.changed_pixel_ratio, 0.0);
        assert_eq!(quality.max_channel_error, 0);
        assert_eq!(quality.local_luma_ssim, 1.0);
        assert_eq!(quality.worst_block_luma_ssim, 1.0);
        Ok(())
    }

    #[test]
    fn localized_difference_is_visible_in_local_ssim() -> PpResult<()> {
        let reference = Raster::new(16, 16, vec![0; 16 * 16 * 4])?;
        let mut rendered_pixels = vec![0; 16 * 16 * 4];
        for y in 0..8 {
            for x in 0..8 {
                let index = ((y * 16 + x) * 4) as usize;
                rendered_pixels[index..index + 3].fill(255);
                rendered_pixels[index + 3] = 255;
            }
        }
        let rendered = Raster::new(16, 16, rendered_pixels)?;
        let quality = compare_rasters(&reference, &rendered)?;
        assert!(quality.local_luma_ssim < 1.0);
        assert!(quality.worst_block_luma_ssim < quality.local_luma_ssim);
        assert!(quality.worst_block_luma_ssim < MIN_WORST_BLOCK_LUMA_SSIM);
        Ok(())
    }

    #[test]
    fn low_energy_dark_noise_uses_bounded_rms_treatment() -> PpResult<()> {
        let mut reference = vec![0; 8 * 8 * 4];
        let mut rendered = vec![0; 8 * 8 * 4];
        for index in 0..64usize {
            let offset = index * 4;
            let reference_value = (index % 10) as u8;
            let rendered_value = (index % 5) as u8;
            reference[offset..offset + 3].fill(reference_value);
            rendered[offset..offset + 3].fill(rendered_value);
            reference[offset + 3] = 255;
            rendered[offset + 3] = 255;
        }
        let reference = Raster::new(8, 8, reference)?;
        let rendered = Raster::new(8, 8, rendered)?;
        let quality = compare_rasters(&reference, &rendered)?;
        assert!(quality.worst_block_luma_ssim >= MIN_WORST_BLOCK_LUMA_SSIM);
        Ok(())
    }
    #[test]
    fn resource_gate_rejects_before_color_counting_and_stops_at_color_limit() -> PpResult<()> {
        let oversized = Raster::new(2, 1, vec![1, 2, 3, 255, 4, 5, 6, 255])?;
        let oversized_assessment = input_resource_gate_assessment(&oversized, 1);
        assert!(!oversized_assessment.passed());
        assert_eq!(oversized_assessment.distinct_colors, 0);

        let mut pixels = Vec::with_capacity((MAXIMUM_INPUT_COLORS + 1) * 4);
        for color in 0..=MAXIMUM_INPUT_COLORS {
            pixels.extend_from_slice(&[
                (color & 0xff) as u8,
                ((color >> 8) & 0xff) as u8,
                ((color >> 16) & 0xff) as u8,
                255,
            ]);
        }
        let colorful = Raster::new((MAXIMUM_INPUT_COLORS + 1) as u32, 1, pixels)?;
        let color_assessment =
            input_resource_gate_assessment(&colorful, (MAXIMUM_INPUT_COLORS + 1) as u64);
        assert_eq!(color_assessment.distinct_colors, MAXIMUM_INPUT_COLORS + 1);
        assert!(!color_assessment.passed());
        Ok(())
    }
    const RED: [u8; 4] = [255, 0, 0, 255];
    const BLUE: [u8; 4] = [0, 0, 255, 255];
    const GREEN: [u8; 4] = [0, 255, 0, 255];

    fn raster_with_blue_square() -> PpResult<(Raster, Vec<u8>)> {
        let mut pixels = vec![0; 7 * 7 * 4];
        for pixel in pixels.as_chunks_mut::<4>().0 {
            pixel.copy_from_slice(&RED);
        }
        for y in 2..=4 {
            for x in 2..=4 {
                pixels[(y * 7 + x) * 4..(y * 7 + x + 1) * 4].copy_from_slice(&BLUE);
            }
        }
        Ok((Raster::new(7, 7, pixels.clone())?, pixels))
    }

    fn recolor(pixels: &mut [u8], coordinates: &[(usize, usize)], color: [u8; 4]) {
        for &(x, y) in coordinates {
            pixels[(y * 7 + x) * 4..(y * 7 + x + 1) * 4].copy_from_slice(&color);
        }
    }

    fn safe_fragment() -> PpResult<(Raster, Raster)> {
        let mut pixels = vec![0; 7 * 7 * 4];
        for pixel in pixels.as_chunks_mut::<4>().0 {
            pixel.copy_from_slice(&RED);
        }
        recolor(&mut pixels, &[(2, 2), (3, 2), (2, 3), (3, 3)], BLUE);
        recolor(
            &mut pixels,
            &[(4, 1), (4, 2), (4, 3), (4, 4), (4, 5)],
            GREEN,
        );
        let source = Raster::new(7, 7, pixels.clone())?;
        recolor(&mut pixels, &[(2, 2), (3, 2), (2, 3), (3, 3)], RED);
        Ok((source, Raster::new(7, 7, pixels)?))
    }

    #[test]
    fn adaptive_geometry_accepts_internal_two_by_two_boundary_fragment() -> PpResult<()> {
        let (source, candidate) = safe_fragment()?;
        let assessment = adaptive_protected_geometry_assessment(&source, &candidate, 4);
        assert!(assessment.edges);
        assert!(assessment.endpoints);
        assert!(assessment.features);
        assert!(assessment.junctions);
        assert_eq!(assessment.changed_components, 1);
        assert_eq!(assessment.largest_changed_component, 4);
        Ok(())
    }

    #[test]
    fn adaptive_geometry_rejects_partial_source_component_recolor() -> PpResult<()> {
        let (source, mut pixels) = raster_with_blue_square()?;
        recolor(&mut pixels, &[(2, 2), (3, 2), (2, 3), (3, 3)], RED);
        let candidate = Raster::new(7, 7, pixels)?;
        let assessment = adaptive_protected_geometry_assessment(&source, &candidate, 4);
        assert!(!assessment.features);
        assert!(!assessment.junctions);
        assert_eq!(assessment.incomplete_source_components, 1);
        Ok(())
    }

    #[test]
    fn adaptive_geometry_rejects_island_and_narrow_line() -> PpResult<()> {
        let (source, mut island_pixels) = raster_with_blue_square()?;
        recolor(&mut island_pixels, &[(2, 2)], RED);
        let island = Raster::new(7, 7, island_pixels)?;
        let island_assessment = adaptive_protected_geometry_assessment(&source, &island, 4);
        assert!(!island_assessment.features);
        assert_eq!(island_assessment.island_components, 1);

        let (_, mut line_pixels) = raster_with_blue_square()?;
        recolor(&mut line_pixels, &[(2, 2), (3, 2)], RED);
        let line = Raster::new(7, 7, line_pixels)?;
        let line_assessment = adaptive_protected_geometry_assessment(&source, &line, 4);
        assert!(!line_assessment.features);
        assert_eq!(line_assessment.narrow_components, 1);
        Ok(())
    }

    #[test]
    fn adaptive_geometry_rejects_image_and_alpha_edges() -> PpResult<()> {
        let (source, mut image_edge_pixels) = raster_with_blue_square()?;
        recolor(
            &mut image_edge_pixels,
            &[(0, 2), (1, 2), (0, 3), (1, 3)],
            BLUE,
        );
        let image_edge = Raster::new(7, 7, image_edge_pixels)?;
        let image_edge_assessment = adaptive_protected_geometry_assessment(&source, &image_edge, 4);
        assert!(!image_edge_assessment.edges);
        assert_eq!(image_edge_assessment.edge_components, 1);

        let (mut alpha_source, mut alpha_candidate_pixels) = raster_with_blue_square()?;
        let mut alpha_source_pixels = alpha_source.pixels().to_vec();
        alpha_source_pixels[(2 * 7 + 1) * 4 + 3] = 0;
        alpha_source = Raster::new(7, 7, alpha_source_pixels.clone())?;
        recolor(
            &mut alpha_candidate_pixels,
            &[(2, 2), (3, 2), (2, 3), (3, 3)],
            RED,
        );
        alpha_candidate_pixels[(2 * 7 + 1) * 4 + 3] = 0;
        let alpha_edge = Raster::new(7, 7, alpha_candidate_pixels)?;
        let alpha_edge_assessment =
            adaptive_protected_geometry_assessment(&alpha_source, &alpha_edge, 4);
        assert!(!alpha_edge_assessment.edges);
        assert_eq!(alpha_edge_assessment.edge_components, 1);
        Ok(())
    }

    #[test]
    fn adaptive_geometry_rejects_three_neighbor_junction() -> PpResult<()> {
        let (_, mut source_pixels) = raster_with_blue_square()?;
        source_pixels[9 * 4..10 * 4].copy_from_slice(&GREEN);
        let source = Raster::new(7, 7, source_pixels.clone())?;
        recolor(&mut source_pixels, &[(2, 2), (3, 2), (2, 3), (3, 3)], RED);
        let candidate = Raster::new(7, 7, source_pixels)?;
        let assessment = adaptive_protected_geometry_assessment(&source, &candidate, 4);
        assert!(!assessment.junctions);
        assert_eq!(assessment.junction_components, 1);
        Ok(())
    }

    #[test]
    fn adaptive_geometry_rejects_new_paint_and_oversized_component() -> PpResult<()> {
        let (source, mut new_paint_pixels) = raster_with_blue_square()?;
        recolor(
            &mut new_paint_pixels,
            &[(2, 2), (3, 2), (2, 3), (3, 3)],
            GREEN,
        );
        let new_paint = Raster::new(7, 7, new_paint_pixels)?;
        let new_paint_assessment = adaptive_protected_geometry_assessment(&source, &new_paint, 4);
        assert!(!new_paint_assessment.features);
        assert!(!new_paint_assessment.output_paints_existing);
        let (_, mut mixed_source_pixels) = raster_with_blue_square()?;
        mixed_source_pixels[(6 * 7 + 6) * 4..(6 * 7 + 7) * 4].copy_from_slice(&GREEN);
        let mixed_source = Raster::new(7, 7, mixed_source_pixels.clone())?;
        recolor(&mut mixed_source_pixels, &[(2, 2), (3, 2)], GREEN);
        recolor(&mut mixed_source_pixels, &[(2, 3), (3, 3)], RED);
        let mixed_target = Raster::new(7, 7, mixed_source_pixels)?;
        let mixed_target_assessment =
            adaptive_protected_geometry_assessment(&mixed_source, &mixed_target, 4);
        assert!(!mixed_target_assessment.junctions);
        assert_eq!(mixed_target_assessment.mixed_target_components, 1);

        let (_, mut oversized_pixels) = raster_with_blue_square()?;
        recolor(
            &mut oversized_pixels,
            &[(2, 2), (3, 2), (4, 2), (2, 3), (3, 3), (4, 3)],
            RED,
        );
        let oversized = Raster::new(7, 7, oversized_pixels)?;
        let oversized_assessment = adaptive_protected_geometry_assessment(&source, &oversized, 4);
        assert!(!oversized_assessment.features);
        assert_eq!(oversized_assessment.oversized_components, 1);
        Ok(())
    }

    #[test]
    fn adaptive_paint_validation_ignores_hidden_transparent_rgb() -> PpResult<()> {
        let hidden = Raster::new(2, 1, vec![255, 0, 255, 0, 255, 0, 0, 255])?;
        let canonical = Raster::new(2, 1, vec![0, 0, 0, 0, 255, 0, 0, 255])?;

        assert!(
            adaptive_protected_geometry_assessment(&hidden, &canonical, 1).output_paints_existing
        );
        assert!(
            adaptive_protected_geometry_assessment(&canonical, &hidden, 1).output_paints_existing
        );
        Ok(())
    }

    #[test]
    fn non_adaptive_protection_remains_exact_rgba() -> PpResult<()> {
        let (source, candidate) = safe_fragment()?;
        let protected = protected_raster_gate_assessment(&source, &candidate);
        assert!(!protected.exact_rgba);
        assert!(protected.alpha);
        assert!(protected.topology);
        Ok(())
    }
}
