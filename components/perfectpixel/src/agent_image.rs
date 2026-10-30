use std::collections::VecDeque;

use serde::{Deserialize, Serialize};

use crate::agent::{validate_image_assertion_id, IMAGE_ASSERTION_ID_MAX_LENGTH};
use crate::{
    apply_raster_edits, resize_raster, PpError, PpResult, Raster, RasterEdit, ResampleFilter,
};

pub const MAX_AGENT_IMAGE_DIMENSION: u32 = 8_192;
pub const MAX_AGENT_IMAGE_PIXELS: u64 = 8_192 * 8_192;
pub const MAX_EXTRACT_FEATHER_RADIUS: u8 = 32;
pub const MAX_RENDER_NODES: usize = 64;
pub const MAX_RENDER_PIXEL_WORK: u64 = 256 * 1024 * 1024;
pub const MAX_COMPARE_ASSERTIONS: usize = 256;
/// Aggregate decoded mask budget.  Individual rasters are bounded, but a
/// compare request must also bound the sum before retaining many masks.
pub const MAX_COMPARE_MASK_PIXELS: u64 = MAX_AGENT_IMAGE_PIXELS;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum ExtractSelector {
    ExistingAlpha {
        #[serde(default = "default_minimum_alpha")]
        minimum_alpha: u8,
    },
    ChromaKey {
        keys: Vec<[u8; 3]>,
        tolerance: u8,
    },
    ColorRange {
        minimum: [u8; 3],
        maximum: [u8; 3],
    },
    AlphaComponent {
        point: NormalizedPoint,
        #[serde(default = "default_minimum_alpha")]
        minimum_alpha: u8,
    },
    ProvidedMask,
}

const fn default_minimum_alpha() -> u8 {
    1
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NormalizedPoint {
    pub x_millionths: u32,
    pub y_millionths: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MatteRefinement {
    #[serde(default)]
    pub feather_radius: u8,
    #[serde(default)]
    pub threshold: Option<u8>,
    #[serde(default)]
    pub require_soft_alpha: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ObjectBounds {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtractedObject {
    pub object: Raster,
    pub mask: Raster,
    pub remainder: Raster,
    pub source_bounds: ObjectBounds,
    pub source_width: u32,
    pub source_height: u32,
    pub soft_alpha_pixels: u64,
}

pub fn extract_object(
    source: &Raster,
    selector: &ExtractSelector,
    provided_mask: Option<&Raster>,
    refinement: MatteRefinement,
) -> PpResult<ExtractedObject> {
    validate_raster_bound(source)?;
    if refinement.feather_radius > MAX_EXTRACT_FEATHER_RADIUS {
        return Err(PpError::InvalidRequest(format!(
            "featherRadius exceeds {MAX_EXTRACT_FEATHER_RADIUS}"
        )));
    }
    let mut matte = match selector {
        ExtractSelector::ExistingAlpha { minimum_alpha } => {
            matte_from_existing_alpha(source, *minimum_alpha)
        }
        ExtractSelector::ChromaKey { keys, tolerance } => {
            matte_from_chroma(source, keys, *tolerance)?
        }
        ExtractSelector::ColorRange { minimum, maximum } => {
            matte_from_color_range(source, *minimum, *maximum)?
        }
        ExtractSelector::AlphaComponent {
            point,
            minimum_alpha,
        } => matte_from_alpha_component(source, *point, *minimum_alpha)?,
        ExtractSelector::ProvidedMask => {
            let mask = provided_mask.ok_or_else(|| {
                PpError::InvalidRequest(
                    "provided_mask selector requires a mask artifact".to_owned(),
                )
            })?;
            matte_from_provided_mask(source, mask)?
        }
    };

    if refinement.feather_radius > 0 {
        matte = box_blur_plane(
            &matte,
            source.width(),
            source.height(),
            refinement.feather_radius,
        );
    }
    if let Some(threshold) = refinement.threshold {
        for value in &mut matte {
            *value = if *value >= threshold { 255 } else { 0 };
        }
    }
    clamp_matte_to_source_alpha(source, &mut matte);

    let soft_alpha_pixels = matte
        .iter()
        .filter(|value| **value > 0 && **value < 255)
        .count() as u64;
    if refinement.require_soft_alpha && soft_alpha_pixels == 0 {
        return Err(PpError::InvalidRequest(
            "MATTE_QUALITY_BELOW_THRESHOLD: final matte contains no soft-alpha pixels".to_owned(),
        ));
    }

    let bounds =
        nonzero_alpha_bounds(source.width(), source.height(), &matte).ok_or_else(|| {
            PpError::InvalidRequest(
                "SELECTOR_TARGET_NOT_FOUND: selector produced an empty matte".to_owned(),
            )
        })?;
    let object = crop_with_final_alpha(source, &matte, bounds)?;
    let mask = crop_mask(&matte, source.width(), bounds)?;
    let remainder = remainder_from_matte(source, &matte)?;
    Ok(ExtractedObject {
        object,
        mask,
        remainder,
        source_bounds: bounds,
        source_width: source.width(),
        source_height: source.height(),
        soft_alpha_pixels,
    })
}

fn validate_raster_bound(raster: &Raster) -> PpResult<()> {
    validate_raster_dimensions_bound(raster.width(), raster.height())
}

fn validate_raster_dimensions_bound(width: u32, height: u32) -> PpResult<()> {
    let pixels = u64::from(width) * u64::from(height);
    if width > MAX_AGENT_IMAGE_DIMENSION
        || height > MAX_AGENT_IMAGE_DIMENSION
        || pixels > MAX_AGENT_IMAGE_PIXELS
    {
        return Err(PpError::InvalidRequest(
            "image exceeds agent image processing bounds".to_owned(),
        ));
    }
    Ok(())
}

fn matte_from_existing_alpha(source: &Raster, minimum_alpha: u8) -> Vec<u8> {
    source
        .pixels()
        .as_chunks::<4>()
        .0
        .iter()
        .map(|pixel| {
            if pixel[3] >= minimum_alpha {
                pixel[3]
            } else {
                0
            }
        })
        .collect()
}

fn matte_from_chroma(source: &Raster, keys: &[[u8; 3]], tolerance: u8) -> PpResult<Vec<u8>> {
    if keys.is_empty() || keys.len() > 16 {
        return Err(PpError::InvalidRequest(
            "chroma keys must contain 1..=16 colors".to_owned(),
        ));
    }
    let edited = apply_raster_edits(
        source,
        &[RasterEdit::RemoveBackground {
            keys: keys.to_vec(),
            tolerance,
            feather: 0,
        }],
    )?;
    Ok(edited
        .pixels()
        .as_chunks::<4>()
        .0
        .iter()
        .map(|pixel| pixel[3])
        .collect())
}

fn matte_from_color_range(
    source: &Raster,
    minimum: [u8; 3],
    maximum: [u8; 3],
) -> PpResult<Vec<u8>> {
    if (0..3).any(|index| minimum[index] > maximum[index]) {
        return Err(PpError::InvalidRequest(
            "color range minimum exceeds maximum".to_owned(),
        ));
    }
    Ok(source
        .pixels()
        .as_chunks::<4>()
        .0
        .iter()
        .map(|pixel| {
            let selected = (0..3)
                .all(|index| pixel[index] >= minimum[index] && pixel[index] <= maximum[index]);
            if selected {
                pixel[3]
            } else {
                0
            }
        })
        .collect())
}

fn matte_from_alpha_component(
    source: &Raster,
    point: NormalizedPoint,
    minimum_alpha: u8,
) -> PpResult<Vec<u8>> {
    if point.x_millionths > 1_000_000 || point.y_millionths > 1_000_000 {
        return Err(PpError::InvalidRequest(
            "normalized component point must be within 0..=1000000".to_owned(),
        ));
    }
    let x = if source.width() == 1 {
        0
    } else {
        ((u64::from(source.width() - 1) * u64::from(point.x_millionths)) / 1_000_000) as u32
    };
    let y = if source.height() == 1 {
        0
    } else {
        ((u64::from(source.height() - 1) * u64::from(point.y_millionths)) / 1_000_000) as u32
    };
    let pixel_count = (source.width() as usize) * (source.height() as usize);
    let seed = (y as usize) * (source.width() as usize) + x as usize;
    let alpha = source.pixels()[seed * 4 + 3];
    if alpha < minimum_alpha {
        return Err(PpError::InvalidRequest(
            "SELECTOR_TARGET_NOT_FOUND: component seed is transparent".to_owned(),
        ));
    }
    let mut visited = vec![false; pixel_count];
    let mut matte = vec![0_u8; pixel_count];
    let mut queue = VecDeque::from([(x, y)]);
    while let Some((cx, cy)) = queue.pop_front() {
        let index = (cy as usize) * (source.width() as usize) + cx as usize;
        if visited[index] {
            continue;
        }
        visited[index] = true;
        let alpha = source.pixels()[index * 4 + 3];
        if alpha < minimum_alpha {
            continue;
        }
        matte[index] = alpha;
        if cx > 0 {
            queue.push_back((cx - 1, cy));
        }
        if cy > 0 {
            queue.push_back((cx, cy - 1));
        }
        if cx + 1 < source.width() {
            queue.push_back((cx + 1, cy));
        }
        if cy + 1 < source.height() {
            queue.push_back((cx, cy + 1));
        }
    }
    Ok(matte)
}

fn matte_from_provided_mask(source: &Raster, mask: &Raster) -> PpResult<Vec<u8>> {
    if source.width() != mask.width() || source.height() != mask.height() {
        return Err(PpError::InvalidRequest(
            "provided mask dimensions must equal source dimensions".to_owned(),
        ));
    }
    Ok(mask
        .pixels()
        .as_chunks::<4>()
        .0
        .iter()
        .zip(source.pixels().as_chunks::<4>().0)
        .map(|(mask_pixel, source_pixel)| mask_pixel[3].min(source_pixel[3]))
        .collect())
}

fn clamp_matte_to_source_alpha(source: &Raster, matte: &mut [u8]) {
    for (value, pixel) in matte.iter_mut().zip(source.pixels().as_chunks::<4>().0) {
        *value = (*value).min(pixel[3]);
    }
}

fn box_blur_plane(alpha: &[u8], width: u32, height: u32, radius: u8) -> Vec<u8> {
    if radius == 0 {
        return alpha.to_vec();
    }
    let width_usize = width as usize;
    let height_usize = height as usize;
    let r = radius as usize;
    let mut horizontal = vec![0_u8; alpha.len()];
    for y in 0..height_usize {
        let row = &alpha[y * width_usize..(y + 1) * width_usize];
        let mut prefix = vec![0_u32; width_usize + 1];
        for x in 0..width_usize {
            prefix[x + 1] = prefix[x] + u32::from(row[x]);
        }
        for x in 0..width_usize {
            let left = x.saturating_sub(r);
            let right = (x + r + 1).min(width_usize);
            horizontal[y * width_usize + x] =
                ((prefix[right] - prefix[left]) / (right - left) as u32) as u8;
        }
    }
    let mut output = vec![0_u8; alpha.len()];
    for x in 0..width_usize {
        let mut prefix = vec![0_u32; height_usize + 1];
        for y in 0..height_usize {
            prefix[y + 1] = prefix[y] + u32::from(horizontal[y * width_usize + x]);
        }
        for y in 0..height_usize {
            let top = y.saturating_sub(r);
            let bottom = (y + r + 1).min(height_usize);
            output[y * width_usize + x] =
                ((prefix[bottom] - prefix[top]) / (bottom - top) as u32) as u8;
        }
    }
    output
}

fn nonzero_alpha_bounds(width: u32, height: u32, matte: &[u8]) -> Option<ObjectBounds> {
    let mut min_x = width;
    let mut min_y = height;
    let mut max_x = 0;
    let mut max_y = 0;
    let mut any = false;
    for y in 0..height {
        for x in 0..width {
            let value = matte[(y as usize) * (width as usize) + x as usize];
            if value == 0 {
                continue;
            }
            any = true;
            min_x = min_x.min(x);
            min_y = min_y.min(y);
            max_x = max_x.max(x);
            max_y = max_y.max(y);
        }
    }
    any.then(|| ObjectBounds {
        x: min_x,
        y: min_y,
        width: max_x - min_x + 1,
        height: max_y - min_y + 1,
    })
}

fn crop_with_final_alpha(source: &Raster, matte: &[u8], bounds: ObjectBounds) -> PpResult<Raster> {
    let mut pixels = Vec::with_capacity(bounds.width as usize * bounds.height as usize * 4);
    for y in bounds.y..bounds.y + bounds.height {
        for x in bounds.x..bounds.x + bounds.width {
            let index = (y as usize) * (source.width() as usize) + x as usize;
            let source_index = index * 4;
            pixels.extend_from_slice(&source.pixels()[source_index..source_index + 3]);
            pixels.push(matte[index]);
        }
    }
    Raster::new(bounds.width, bounds.height, pixels)
}

fn crop_mask(matte: &[u8], source_width: u32, bounds: ObjectBounds) -> PpResult<Raster> {
    let mut pixels = Vec::with_capacity(bounds.width as usize * bounds.height as usize * 4);
    for y in bounds.y..bounds.y + bounds.height {
        for x in bounds.x..bounds.x + bounds.width {
            let value = matte[(y as usize) * (source_width as usize) + x as usize];
            pixels.extend_from_slice(&[255, 255, 255, value]);
        }
    }
    Raster::new(bounds.width, bounds.height, pixels)
}

fn remainder_from_matte(source: &Raster, matte: &[u8]) -> PpResult<Raster> {
    let mut pixels = source.pixels().to_vec();
    for (index, value) in matte.iter().enumerate() {
        pixels[index * 4 + 3] = pixels[index * 4 + 3].saturating_sub(*value);
    }
    Raster::new(source.width(), source.height(), pixels)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompareSeverity {
    Required,
    Advisory,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum CompareAssertion {
    ExactEqual {
        id: String,
        severity: CompareSeverity,
    },
    ChangedRatio {
        id: String,
        severity: CompareSeverity,
        #[serde(default)]
        minimum: Option<f64>,
        #[serde(default)]
        maximum: Option<f64>,
    },
    OutsideMaskChangedRatio {
        id: String,
        severity: CompareSeverity,
        maximum: f64,
        mask_index: usize,
    },
    InsideMaskChangedRatio {
        id: String,
        severity: CompareSeverity,
        minimum: f64,
        #[serde(default)]
        maximum: Option<f64>,
        mask_index: usize,
    },
    UnchangedRegionExact {
        id: String,
        severity: CompareSeverity,
        region: ObjectBounds,
    },
    AlphaIou {
        id: String,
        severity: CompareSeverity,
        minimum: f64,
        mask_index: usize,
    },
    MaskLeakageRatio {
        id: String,
        severity: CompareSeverity,
        maximum: f64,
        mask_index: usize,
    },
    ObjectBounds {
        id: String,
        severity: CompareSeverity,
        expected: ObjectBounds,
        tolerance: u32,
    },
    ObjectCentroid {
        id: String,
        severity: CompareSeverity,
        expected: [f64; 2],
        tolerance: f64,
    },
    ObjectArea {
        id: String,
        severity: CompareSeverity,
        expected: u64,
        tolerance: u64,
    },
    MaximumChannelError {
        id: String,
        severity: CompareSeverity,
        maximum: u8,
    },
    MeanAbsoluteError {
        id: String,
        severity: CompareSeverity,
        maximum: f64,
    },
}

impl CompareAssertion {
    #[must_use]
    pub fn id(&self) -> &str {
        match self {
            Self::ExactEqual { id, .. }
            | Self::ChangedRatio { id, .. }
            | Self::OutsideMaskChangedRatio { id, .. }
            | Self::InsideMaskChangedRatio { id, .. }
            | Self::UnchangedRegionExact { id, .. }
            | Self::AlphaIou { id, .. }
            | Self::MaskLeakageRatio { id, .. }
            | Self::ObjectBounds { id, .. }
            | Self::ObjectCentroid { id, .. }
            | Self::ObjectArea { id, .. }
            | Self::MaximumChannelError { id, .. }
            | Self::MeanAbsoluteError { id, .. } => id,
        }
    }

    #[must_use]
    pub const fn severity(&self) -> CompareSeverity {
        match self {
            Self::ExactEqual { severity, .. }
            | Self::ChangedRatio { severity, .. }
            | Self::OutsideMaskChangedRatio { severity, .. }
            | Self::InsideMaskChangedRatio { severity, .. }
            | Self::UnchangedRegionExact { severity, .. }
            | Self::AlphaIou { severity, .. }
            | Self::MaskLeakageRatio { severity, .. }
            | Self::ObjectBounds { severity, .. }
            | Self::ObjectCentroid { severity, .. }
            | Self::ObjectArea { severity, .. }
            | Self::MaximumChannelError { severity, .. }
            | Self::MeanAbsoluteError { severity, .. } => *severity,
        }
    }

    fn kind(&self) -> &'static str {
        match self {
            Self::ExactEqual { .. } => "exact_equal",
            Self::ChangedRatio { .. } => "changed_ratio",
            Self::OutsideMaskChangedRatio { .. } => "outside_mask_changed_ratio",
            Self::InsideMaskChangedRatio { .. } => "inside_mask_changed_ratio",
            Self::UnchangedRegionExact { .. } => "unchanged_region_exact",
            Self::AlphaIou { .. } => "alpha_iou",
            Self::MaskLeakageRatio { .. } => "mask_leakage_ratio",
            Self::ObjectBounds { .. } => "object_bounds",
            Self::ObjectCentroid { .. } => "object_centroid",
            Self::ObjectArea { .. } => "object_area",
            Self::MaximumChannelError { .. } => "maximum_channel_error",
            Self::MeanAbsoluteError { .. } => "mean_absolute_error",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CompareMetrics {
    pub changed_pixel_ratio: f64,
    pub mean_absolute_error: f64,
    pub maximum_channel_error: u8,
    pub alpha_iou: f64,
    pub after_foreground_pixels: u64,
    pub after_bounds: Option<ObjectBounds>,
    pub after_centroid: Option<[f64; 2]>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CompareAssertionResult {
    pub id: String,
    pub assertion_type: String,
    pub severity: CompareSeverity,
    pub passed: bool,
    pub observed: serde_json::Value,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CompareOutcome {
    pub all_required_passed: bool,
    pub metrics: CompareMetrics,
    pub assertions: Vec<CompareAssertionResult>,
}

pub fn compare_images(
    before: &Raster,
    after: &Raster,
    masks: &[Raster],
    assertions: &[CompareAssertion],
) -> PpResult<CompareOutcome> {
    validate_raster_bound(before)?;
    validate_raster_bound(after)?;
    if before.width() != after.width() || before.height() != after.height() {
        return Err(PpError::InvalidRequest(
            "compare before/after dimensions must match".to_owned(),
        ));
    }
    if assertions.is_empty() || assertions.len() > MAX_COMPARE_ASSERTIONS {
        return Err(PpError::InvalidRequest(format!(
            "compare assertions must contain 1..={MAX_COMPARE_ASSERTIONS} entries"
        )));
    }
    let mut mask_pixels = 0_u64;
    for mask in masks {
        validate_raster_bound(mask)?;
        if mask.width() != before.width() || mask.height() != before.height() {
            return Err(PpError::InvalidRequest(
                "compare mask dimensions must match before/after".to_owned(),
            ));
        }
        mask_pixels = mask_pixels
            .checked_add(u64::from(mask.width()) * u64::from(mask.height()))
            .ok_or_else(|| {
                PpError::InvalidRequest("compare mask pixel budget overflowed".to_owned())
            })?;
        if mask_pixels > MAX_COMPARE_MASK_PIXELS {
            return Err(PpError::InvalidRequest(format!(
                "compare mask aggregate pixels exceed {MAX_COMPARE_MASK_PIXELS}"
            )));
        }
    }
    let mut ids = std::collections::BTreeSet::new();
    for assertion in assertions {
        validate_image_assertion_id(assertion.id())?;
        if assertion.id().len() > IMAGE_ASSERTION_ID_MAX_LENGTH
            || !ids.insert(assertion.id().to_owned())
        {
            return Err(PpError::InvalidRequest(
                "compare assertion ids must be unique bounded values".to_owned(),
            ));
        }
    }

    let pixel_count = u64::from(before.width()) * u64::from(before.height());
    let mut changed_pixels = 0_u64;
    let mut absolute_error_sum = 0_u64;
    let mut maximum_channel_error = 0_u8;
    let mut alpha_intersection = 0_u64;
    let mut alpha_union = 0_u64;
    for (left, right) in before
        .pixels()
        .as_chunks::<4>()
        .0
        .iter()
        .zip(after.pixels().as_chunks::<4>().0)
    {
        if left != right {
            changed_pixels = changed_pixels.saturating_add(1);
        }
        for channel in 0..4 {
            let error = left[channel].abs_diff(right[channel]);
            maximum_channel_error = maximum_channel_error.max(error);
            absolute_error_sum = absolute_error_sum.saturating_add(u64::from(error));
        }
        let left_alpha = left[3] > 0;
        let right_alpha = right[3] > 0;
        if left_alpha && right_alpha {
            alpha_intersection = alpha_intersection.saturating_add(1);
        }
        if left_alpha || right_alpha {
            alpha_union = alpha_union.saturating_add(1);
        }
    }
    let changed_pixel_ratio = ratio(changed_pixels, pixel_count);
    let denominator = pixel_count.saturating_mul(4).saturating_mul(255);
    let mean_absolute_error = ratio(absolute_error_sum, denominator);
    let alpha_iou = if alpha_union == 0 {
        1.0
    } else {
        ratio(alpha_intersection, alpha_union)
    };
    let (after_bounds, after_foreground_pixels, after_centroid) = alpha_geometry(after);
    let metrics = CompareMetrics {
        changed_pixel_ratio,
        mean_absolute_error,
        maximum_channel_error,
        alpha_iou,
        after_foreground_pixels,
        after_bounds,
        after_centroid,
    };
    let mut results = Vec::with_capacity(assertions.len());
    for assertion in assertions {
        results.push(evaluate_assertion(
            before, after, masks, assertion, &metrics,
        )?);
    }
    let all_required_passed = results
        .iter()
        .all(|result| result.severity != CompareSeverity::Required || result.passed);
    Ok(CompareOutcome {
        all_required_passed,
        metrics,
        assertions: results,
    })
}

fn evaluate_assertion(
    before: &Raster,
    after: &Raster,
    masks: &[Raster],
    assertion: &CompareAssertion,
    metrics: &CompareMetrics,
) -> PpResult<CompareAssertionResult> {
    let (passed, observed, message) = match assertion {
        CompareAssertion::ExactEqual { .. } => (
            metrics.changed_pixel_ratio == 0.0,
            serde_json::json!(metrics.changed_pixel_ratio == 0.0),
            if metrics.changed_pixel_ratio == 0.0 {
                "exactly equal"
            } else {
                "pixels differ"
            }
            .to_owned(),
        ),
        CompareAssertion::ChangedRatio {
            minimum, maximum, ..
        } => {
            validate_ratio_bounds(*minimum, *maximum)?;
            let passed = minimum.is_none_or(|minimum| metrics.changed_pixel_ratio >= minimum)
                && maximum.is_none_or(|maximum| metrics.changed_pixel_ratio <= maximum);
            (
                passed,
                serde_json::json!(metrics.changed_pixel_ratio),
                "changed pixel ratio evaluated".to_owned(),
            )
        }
        CompareAssertion::OutsideMaskChangedRatio {
            maximum,
            mask_index,
            ..
        } => {
            validate_unit_interval(*maximum, "outside-mask maximum")?;
            let mask = mask_at(masks, *mask_index)?;
            let observed = changed_ratio_by_mask(before, after, mask, false);
            (
                observed <= *maximum,
                serde_json::json!(observed),
                "outside-mask locality evaluated".to_owned(),
            )
        }
        CompareAssertion::InsideMaskChangedRatio {
            minimum,
            maximum,
            mask_index,
            ..
        } => {
            validate_ratio_bounds(Some(*minimum), *maximum)?;
            let mask = mask_at(masks, *mask_index)?;
            let observed = changed_ratio_by_mask(before, after, mask, true);
            let passed = observed >= *minimum && maximum.is_none_or(|maximum| observed <= maximum);
            (
                passed,
                serde_json::json!(observed),
                "inside-mask change evaluated".to_owned(),
            )
        }
        CompareAssertion::UnchangedRegionExact { region, .. } => {
            validate_region(before, *region)?;
            let equal = region_equal(before, after, *region);
            (
                equal,
                serde_json::json!(equal),
                if equal {
                    "region unchanged"
                } else {
                    "region changed"
                }
                .to_owned(),
            )
        }
        CompareAssertion::AlphaIou {
            minimum,
            mask_index,
            ..
        } => {
            validate_unit_interval(*minimum, "alpha IoU minimum")?;
            let observed = alpha_iou_against_mask(after, mask_at(masks, *mask_index)?);
            (
                observed >= *minimum,
                serde_json::json!(observed),
                "alpha IoU evaluated".to_owned(),
            )
        }
        CompareAssertion::MaskLeakageRatio {
            maximum,
            mask_index,
            ..
        } => {
            validate_unit_interval(*maximum, "mask leakage maximum")?;
            let observed = mask_leakage_ratio(after, mask_at(masks, *mask_index)?);
            (
                observed <= *maximum,
                serde_json::json!(observed),
                "mask leakage evaluated".to_owned(),
            )
        }
        CompareAssertion::ObjectBounds {
            expected,
            tolerance,
            ..
        } => {
            validate_region(after, *expected)?;
            let observed = metrics.after_bounds;
            let passed = observed
                .is_some_and(|observed| bounds_within_tolerance(observed, *expected, *tolerance));
            (
                passed,
                serde_json::to_value(observed).unwrap_or(serde_json::Value::Null),
                "object bounds evaluated".to_owned(),
            )
        }
        CompareAssertion::ObjectCentroid {
            expected,
            tolerance,
            ..
        } => {
            if expected.iter().any(|value| !value.is_finite())
                || !tolerance.is_finite()
                || *tolerance < 0.0
            {
                return Err(PpError::InvalidRequest(
                    "object centroid assertion is invalid".to_owned(),
                ));
            }
            let observed = metrics.after_centroid;
            let passed = observed.is_some_and(|observed| {
                (observed[0] - expected[0]).abs() <= *tolerance
                    && (observed[1] - expected[1]).abs() <= *tolerance
            });
            (
                passed,
                serde_json::to_value(observed).unwrap_or(serde_json::Value::Null),
                "object centroid evaluated".to_owned(),
            )
        }
        CompareAssertion::ObjectArea {
            expected,
            tolerance,
            ..
        } => {
            let lower = expected.saturating_sub(*tolerance);
            let upper = expected.saturating_add(*tolerance);
            let observed = metrics.after_foreground_pixels;
            (
                observed >= lower && observed <= upper,
                serde_json::json!(observed),
                "object area evaluated".to_owned(),
            )
        }
        CompareAssertion::MaximumChannelError { maximum, .. } => (
            metrics.maximum_channel_error <= *maximum,
            serde_json::json!(metrics.maximum_channel_error),
            "maximum channel error evaluated".to_owned(),
        ),
        CompareAssertion::MeanAbsoluteError { maximum, .. } => {
            validate_unit_interval(*maximum, "mean absolute error maximum")?;
            (
                metrics.mean_absolute_error <= *maximum,
                serde_json::json!(metrics.mean_absolute_error),
                "mean absolute error evaluated".to_owned(),
            )
        }
    };
    Ok(CompareAssertionResult {
        id: assertion.id().to_owned(),
        assertion_type: assertion.kind().to_owned(),
        severity: assertion.severity(),
        passed,
        observed,
        message,
    })
}

fn ratio(numerator: u64, denominator: u64) -> f64 {
    if denominator == 0 {
        0.0
    } else {
        numerator as f64 / denominator as f64
    }
}

fn validate_unit_interval(value: f64, label: &str) -> PpResult<()> {
    if !value.is_finite() || !(0.0..=1.0).contains(&value) {
        return Err(PpError::InvalidRequest(format!(
            "{label} must be within 0..=1"
        )));
    }
    Ok(())
}

fn validate_ratio_bounds(minimum: Option<f64>, maximum: Option<f64>) -> PpResult<()> {
    if minimum.is_none() && maximum.is_none() {
        return Err(PpError::InvalidRequest(
            "ratio assertion requires minimum or maximum".to_owned(),
        ));
    }
    if let Some(minimum) = minimum {
        validate_unit_interval(minimum, "ratio minimum")?;
    }
    if let Some(maximum) = maximum {
        validate_unit_interval(maximum, "ratio maximum")?;
    }
    if minimum
        .zip(maximum)
        .is_some_and(|(minimum, maximum)| minimum > maximum)
    {
        return Err(PpError::InvalidRequest(
            "ratio minimum exceeds maximum".to_owned(),
        ));
    }
    Ok(())
}

fn mask_at(masks: &[Raster], index: usize) -> PpResult<&Raster> {
    masks
        .get(index)
        .ok_or_else(|| PpError::InvalidRequest("assertion mask index is out of range".to_owned()))
}

fn changed_ratio_by_mask(before: &Raster, after: &Raster, mask: &Raster, inside: bool) -> f64 {
    let mut population = 0_u64;
    let mut changed = 0_u64;
    for ((left, right), mask_pixel) in before
        .pixels()
        .as_chunks::<4>()
        .0
        .iter()
        .zip(after.pixels().as_chunks::<4>().0)
        .zip(mask.pixels().as_chunks::<4>().0)
    {
        let selected = mask_pixel[3] > 0;
        if selected == inside {
            population = population.saturating_add(1);
            if left != right {
                changed = changed.saturating_add(1);
            }
        }
    }
    ratio(changed, population)
}

fn validate_region(raster: &Raster, region: ObjectBounds) -> PpResult<()> {
    if region.width == 0
        || region.height == 0
        || region
            .x
            .checked_add(region.width)
            .is_none_or(|right| right > raster.width())
        || region
            .y
            .checked_add(region.height)
            .is_none_or(|bottom| bottom > raster.height())
    {
        return Err(PpError::InvalidRequest(
            "compare region is outside the raster".to_owned(),
        ));
    }
    Ok(())
}

fn region_equal(before: &Raster, after: &Raster, region: ObjectBounds) -> bool {
    for y in region.y..region.y + region.height {
        for x in region.x..region.x + region.width {
            let index = ((y as usize) * before.width() as usize + x as usize) * 4;
            if before.pixels()[index..index + 4] != after.pixels()[index..index + 4] {
                return false;
            }
        }
    }
    true
}

fn alpha_iou_against_mask(after: &Raster, mask: &Raster) -> f64 {
    let mut intersection = 0_u64;
    let mut union = 0_u64;
    for (pixel, mask_pixel) in after
        .pixels()
        .as_chunks::<4>()
        .0
        .iter()
        .zip(mask.pixels().as_chunks::<4>().0)
    {
        let foreground = pixel[3] > 0;
        let selected = mask_pixel[3] > 0;
        if foreground && selected {
            intersection = intersection.saturating_add(1);
        }
        if foreground || selected {
            union = union.saturating_add(1);
        }
    }
    if union == 0 {
        1.0
    } else {
        ratio(intersection, union)
    }
}

fn mask_leakage_ratio(after: &Raster, mask: &Raster) -> f64 {
    let mut foreground = 0_u64;
    let mut leakage = 0_u64;
    for (pixel, mask_pixel) in after
        .pixels()
        .as_chunks::<4>()
        .0
        .iter()
        .zip(mask.pixels().as_chunks::<4>().0)
    {
        if pixel[3] == 0 {
            continue;
        }
        foreground = foreground.saturating_add(1);
        if mask_pixel[3] == 0 {
            leakage = leakage.saturating_add(1);
        }
    }
    ratio(leakage, foreground)
}

fn alpha_geometry(raster: &Raster) -> (Option<ObjectBounds>, u64, Option<[f64; 2]>) {
    let mut min_x = raster.width();
    let mut min_y = raster.height();
    let mut max_x = 0_u32;
    let mut max_y = 0_u32;
    let mut count = 0_u64;
    let mut weighted_x = 0_f64;
    let mut weighted_y = 0_f64;
    let mut weight = 0_f64;
    for y in 0..raster.height() {
        for x in 0..raster.width() {
            let alpha =
                raster.pixels()[((y as usize) * raster.width() as usize + x as usize) * 4 + 3];
            if alpha == 0 {
                continue;
            }
            count = count.saturating_add(1);
            min_x = min_x.min(x);
            min_y = min_y.min(y);
            max_x = max_x.max(x);
            max_y = max_y.max(y);
            let alpha_weight = f64::from(alpha);
            weighted_x += f64::from(x) * alpha_weight;
            weighted_y += f64::from(y) * alpha_weight;
            weight += alpha_weight;
        }
    }
    if count == 0 {
        return (None, 0, None);
    }
    (
        Some(ObjectBounds {
            x: min_x,
            y: min_y,
            width: max_x - min_x + 1,
            height: max_y - min_y + 1,
        }),
        count,
        Some([weighted_x / weight, weighted_y / weight]),
    )
}

fn bounds_within_tolerance(observed: ObjectBounds, expected: ObjectBounds, tolerance: u32) -> bool {
    observed.x.abs_diff(expected.x) <= tolerance
        && observed.y.abs_diff(expected.y) <= tolerance
        && observed.width.abs_diff(expected.width) <= tolerance
        && observed.height.abs_diff(expected.height) <= tolerance
}

pub fn difference_preview(before: &Raster, after: &Raster) -> PpResult<Raster> {
    if before.width() != after.width() || before.height() != after.height() {
        return Err(PpError::InvalidRequest(
            "difference preview dimensions must match".to_owned(),
        ));
    }
    let mut pixels = Vec::with_capacity(before.pixels().len());
    for (left, right) in before
        .pixels()
        .as_chunks::<4>()
        .0
        .iter()
        .zip(after.pixels().as_chunks::<4>().0)
    {
        let difference = [
            left[0].abs_diff(right[0]),
            left[1].abs_diff(right[1]),
            left[2].abs_diff(right[2]),
            left[3].abs_diff(right[3]),
        ];
        let alpha = if difference.iter().any(|value| *value != 0) {
            255
        } else {
            0
        };
        pixels.extend_from_slice(&[difference[0], difference[1], difference[2], alpha]);
    }
    Raster::new(before.width(), before.height(), pixels)
}

pub fn mask_overlay_preview(after: &Raster, mask: &Raster) -> PpResult<Raster> {
    if after.width() != mask.width() || after.height() != mask.height() {
        return Err(PpError::InvalidRequest(
            "mask overlay dimensions must match".to_owned(),
        ));
    }
    let mut pixels = after.pixels().to_vec();
    for (pixel, mask_pixel) in pixels
        .as_chunks_mut::<4>()
        .0
        .iter_mut()
        .zip(mask.pixels().as_chunks::<4>().0)
    {
        let coverage = u32::from(mask_pixel[3]);
        if coverage == 0 {
            continue;
        }
        pixel[0] = ((u32::from(pixel[0]) * (255 - coverage) + 255 * coverage) / 255) as u8;
        pixel[1] = ((u32::from(pixel[1]) * (255 - coverage)) / 255) as u8;
        pixel[2] = ((u32::from(pixel[2]) * (255 - coverage)) / 255) as u8;
    }
    Raster::new(after.width(), after.height(), pixels)
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AffineTransform {
    pub matrix: [f64; 9],
}

impl AffineTransform {
    #[must_use]
    pub const fn identity() -> Self {
        Self {
            matrix: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0],
        }
    }

    fn validate(self) -> PpResult<()> {
        if self.matrix.iter().any(|value| !value.is_finite())
            || self.matrix[6] != 0.0
            || self.matrix[7] != 0.0
            || self.matrix[8] != 1.0
        {
            return Err(PpError::InvalidRequest(
                "render transform must be a finite 2D affine 3x3 matrix".to_owned(),
            ));
        }
        let determinant = self.matrix[0] * self.matrix[4] - self.matrix[1] * self.matrix[3];
        if determinant.abs() < 1e-12 {
            return Err(PpError::InvalidRequest(
                "NON_INVERTIBLE_TRANSFORM".to_owned(),
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RenderFilter {
    Nearest,
    Lanczos3,
}

#[derive(Debug, Clone)]
pub struct RenderNode {
    pub id: String,
    pub z: i32,
    pub source: Raster,
    pub transform: AffineTransform,
    pub opacity: u8,
    pub filter: RenderFilter,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RenderCanvas {
    pub width: u32,
    pub height: u32,
    pub background: [u8; 4],
}

pub fn render_composition(canvas: RenderCanvas, nodes: &[RenderNode]) -> PpResult<Raster> {
    if nodes.len() > MAX_RENDER_NODES {
        return Err(PpError::InvalidRequest(format!(
            "render nodes exceed {MAX_RENDER_NODES}"
        )));
    }
    // Keep Raster's non-empty-dimension error before applying agent bounds.
    if canvas.width != 0 && canvas.height != 0 {
        validate_raster_dimensions_bound(canvas.width, canvas.height)?;
    }
    let mut output = Raster::blank(canvas.width, canvas.height)?;
    for pixel in output.pixels_mut().as_chunks_mut::<4>().0 {
        pixel.copy_from_slice(&canvas.background);
    }
    let mut ordered = nodes.iter().collect::<Vec<_>>();
    ordered.sort_by(|left, right| left.z.cmp(&right.z).then_with(|| left.id.cmp(&right.id)));
    let mut pixel_work = u64::from(canvas.width) * u64::from(canvas.height);
    for node in ordered {
        if node.id.trim().is_empty() || node.id.len() > 128 {
            return Err(PpError::InvalidRequest(
                "render node id is invalid".to_owned(),
            ));
        }
        node.transform.validate()?;
        if node.filter != RenderFilter::Nearest {
            return Err(PpError::InvalidRequest(
                "M4 affine renderer currently exposes nearest sampling only".to_owned(),
            ));
        }
        validate_raster_bound(&node.source)?;
        pixel_work = pixel_work
            .checked_add(u64::from(node.source.width()) * u64::from(node.source.height()))
            .ok_or_else(|| PpError::InvalidRequest("render pixel work overflowed".to_owned()))?;
        if pixel_work > MAX_RENDER_PIXEL_WORK {
            return Err(PpError::InvalidRequest("WORK_BUDGET_EXCEEDED".to_owned()));
        }
        composite_affine_nearest(&mut output, node)?;
    }
    Ok(output)
}

fn composite_affine_nearest(output: &mut Raster, node: &RenderNode) -> PpResult<()> {
    let m = node.transform.matrix;
    let determinant = m[0] * m[4] - m[1] * m[3];
    if determinant.abs() < 1e-12 {
        return Err(PpError::InvalidRequest(
            "NON_INVERTIBLE_TRANSFORM".to_owned(),
        ));
    }
    let inv00 = m[4] / determinant;
    let inv01 = -m[1] / determinant;
    let inv10 = -m[3] / determinant;
    let inv11 = m[0] / determinant;
    let tx = m[2];
    let ty = m[5];
    for dy in 0..output.height() {
        for dx in 0..output.width() {
            let px = f64::from(dx) + 0.5 - tx;
            let py = f64::from(dy) + 0.5 - ty;
            let sx = inv00 * px + inv01 * py - 0.5;
            let sy = inv10 * px + inv11 * py - 0.5;
            if sx < -0.5
                || sy < -0.5
                || sx >= f64::from(node.source.width()) - 0.5
                || sy >= f64::from(node.source.height()) - 0.5
            {
                continue;
            }
            let source_x = sx.round().clamp(0.0, f64::from(node.source.width() - 1)) as u32;
            let source_y = sy.round().clamp(0.0, f64::from(node.source.height() - 1)) as u32;
            let source_index =
                ((source_y as usize) * (node.source.width() as usize) + source_x as usize) * 4;
            let destination_index = ((dy as usize) * (output.width() as usize) + dx as usize) * 4;
            let source_pixel = &node.source.pixels()[source_index..source_index + 4];
            let destination = &mut output.pixels_mut()[destination_index..destination_index + 4];
            source_over(destination, source_pixel, node.opacity);
        }
    }
    Ok(())
}

fn source_over(destination: &mut [u8], source: &[u8], opacity: u8) {
    let source_alpha = (u32::from(source[3]) * u32::from(opacity) + 127) / 255;
    let destination_alpha = u32::from(destination[3]);
    let inverse = 255 - source_alpha;
    let output_alpha = source_alpha + (destination_alpha * inverse + 127) / 255;
    if output_alpha == 0 {
        destination.copy_from_slice(&[0, 0, 0, 0]);
        return;
    }
    for channel in 0..3 {
        let source_premultiplied = u32::from(source[channel]) * source_alpha;
        let destination_premultiplied =
            (u32::from(destination[channel]) * destination_alpha * inverse + 127) / 255;
        destination[channel] =
            ((source_premultiplied + destination_premultiplied + output_alpha / 2) / output_alpha)
                .min(255) as u8;
    }
    destination[3] = output_alpha.min(255) as u8;
}

pub fn preprocess_render_source(
    source: &Raster,
    width: Option<u32>,
    height: Option<u32>,
    quarter_turns: u8,
    filter: RenderFilter,
) -> PpResult<Raster> {
    let mut current = source.clone();
    if let (Some(width), Some(height)) = (width, height) {
        current = resize_raster(
            &current,
            width,
            height,
            match filter {
                RenderFilter::Nearest => ResampleFilter::Nearest,
                RenderFilter::Lanczos3 => ResampleFilter::Lanczos3,
            },
        )?;
    } else if width.is_some() || height.is_some() {
        return Err(PpError::InvalidRequest(
            "render resize requires both width and height".to_owned(),
        ));
    }
    let turns = quarter_turns % 4;
    if turns != 0 {
        current = apply_raster_edits(
            &current,
            &[RasterEdit::RotateQuarterTurns {
                quarter_turns: turns,
            }],
        )?;
    }
    Ok(current)
}

pub fn feather_matte_for_source(source: &Raster, matte: &[u8], radius: u8) -> PpResult<Vec<u8>> {
    if matte.len() != source.width() as usize * source.height() as usize {
        return Err(PpError::InvalidRequest(
            "matte length does not match source".to_owned(),
        ));
    }
    Ok(box_blur_plane(
        matte,
        source.width(),
        source.height(),
        radius,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raster(width: u32, height: u32, pixels: Vec<[u8; 4]>) -> Raster {
        Raster::new(width, height, pixels.into_iter().flatten().collect()).unwrap()
    }

    #[test]
    fn existing_alpha_does_not_square_transparency() {
        let source = raster(1, 1, vec![[10, 20, 30, 128]]);
        let result = extract_object(
            &source,
            &ExtractSelector::ExistingAlpha { minimum_alpha: 1 },
            None,
            MatteRefinement::default(),
        )
        .unwrap();
        assert_eq!(result.object.pixels()[3], 128);
        assert_eq!(result.remainder.pixels()[3], 0);
    }

    #[test]
    fn extraction_bounds_include_low_alpha_fringe() {
        let source = raster(2, 1, vec![[1, 2, 3, 1], [4, 5, 6, 255]]);
        let result = extract_object(
            &source,
            &ExtractSelector::ExistingAlpha { minimum_alpha: 1 },
            None,
            MatteRefinement::default(),
        )
        .unwrap();
        assert_eq!(result.source_bounds.width, 2);
    }

    #[test]
    fn empty_alpha_extraction_returns_target_not_found() {
        let source = raster(1, 1, vec![[10, 20, 30, 0]]);
        let error = extract_object(
            &source,
            &ExtractSelector::ExistingAlpha { minimum_alpha: 1 },
            None,
            MatteRefinement::default(),
        )
        .expect_err("a transparent source has no extraction target");
        assert!(error.to_string().contains("SELECTOR_TARGET_NOT_FOUND"));
    }

    #[test]
    fn render_rejects_invalid_canvas_dimensions_before_allocating() {
        for (width, height) in [
            (u32::MAX, u32::MAX),
            (MAX_AGENT_IMAGE_DIMENSION + 1, 1),
            (1, MAX_AGENT_IMAGE_DIMENSION + 1),
            (0, 1),
            (1, 0),
            (0, u32::MAX),
            (u32::MAX, 0),
        ] {
            let error = render_composition(
                RenderCanvas {
                    width,
                    height,
                    background: [0, 0, 0, 0],
                },
                &[],
            )
            .expect_err("invalid canvas dimensions must be rejected");
            let message = error.to_string();
            if width == 0 || height == 0 {
                assert!(message.contains("image dimensions must be positive"));
            } else {
                assert!(message.contains("image exceeds agent image processing bounds"));
            }
        }
    }

    #[test]
    fn render_orders_nodes_by_z() {
        let red = raster(1, 1, vec![[255, 0, 0, 255]]);
        let blue = raster(1, 1, vec![[0, 0, 255, 255]]);
        let output = render_composition(
            RenderCanvas {
                width: 1,
                height: 1,
                background: [0, 0, 0, 0],
            },
            &[
                RenderNode {
                    id: "top".to_owned(),
                    z: 2,
                    source: blue,
                    transform: AffineTransform::identity(),
                    opacity: 255,
                    filter: RenderFilter::Nearest,
                },
                RenderNode {
                    id: "bottom".to_owned(),
                    z: 1,
                    source: red,
                    transform: AffineTransform::identity(),
                    opacity: 255,
                    filter: RenderFilter::Nearest,
                },
            ],
        )
        .unwrap();
        assert_eq!(&output.pixels()[..4], &[0, 0, 255, 255]);
    }

    #[test]
    fn compare_detects_non_target_corruption() {
        let before = raster(
            3,
            1,
            vec![[10, 10, 10, 255], [20, 20, 20, 255], [30, 30, 30, 255]],
        );
        let after = raster(
            3,
            1,
            vec![[11, 10, 10, 255], [99, 99, 99, 255], [30, 30, 30, 255]],
        );
        let mask = raster(
            3,
            1,
            vec![[255, 255, 255, 0], [255, 255, 255, 255], [255, 255, 255, 0]],
        );
        let outcome = compare_images(
            &before,
            &after,
            &[mask],
            &[
                CompareAssertion::OutsideMaskChangedRatio {
                    id: "preserve".to_owned(),
                    severity: CompareSeverity::Required,
                    maximum: 0.0,
                    mask_index: 0,
                },
                CompareAssertion::InsideMaskChangedRatio {
                    id: "target".to_owned(),
                    severity: CompareSeverity::Required,
                    minimum: 1.0,
                    maximum: Some(1.0),
                    mask_index: 0,
                },
            ],
        )
        .unwrap();
        assert!(!outcome.all_required_passed);
        assert_eq!(outcome.assertions.len(), 2);
        assert!(!outcome.assertions[0].passed);
        assert!(outcome.assertions[1].passed);
    }

    #[test]
    fn geometry_assertion_honors_one_pixel_tolerance() {
        let transparent = [0, 0, 0, 0];
        let opaque = [10, 20, 30, 255];
        let before = raster(4, 2, vec![transparent; 8]);
        let after = raster(
            4,
            2,
            vec![
                transparent,
                opaque,
                opaque,
                transparent,
                transparent,
                opaque,
                opaque,
                transparent,
            ],
        );
        let outcome = compare_images(
            &before,
            &after,
            &[],
            &[CompareAssertion::ObjectBounds {
                id: "bounds".to_owned(),
                severity: CompareSeverity::Required,
                expected: ObjectBounds {
                    x: 0,
                    y: 0,
                    width: 2,
                    height: 2,
                },
                tolerance: 1,
            }],
        )
        .unwrap();
        assert!(outcome.all_required_passed);
    }

    #[test]
    fn required_and_advisory_assertions_are_distinct() {
        let before = raster(1, 1, vec![[0, 0, 0, 255]]);
        let after = raster(1, 1, vec![[1, 0, 0, 255]]);
        let outcome = compare_images(
            &before,
            &after,
            &[],
            &[
                CompareAssertion::ChangedRatio {
                    id: "required".to_owned(),
                    severity: CompareSeverity::Required,
                    minimum: Some(1.0),
                    maximum: None,
                },
                CompareAssertion::MaximumChannelError {
                    id: "advisory".to_owned(),
                    severity: CompareSeverity::Advisory,
                    maximum: 0,
                },
            ],
        )
        .unwrap();
        assert!(outcome.assertions[0].passed);
        assert!(!outcome.assertions[1].passed);
        assert!(outcome.all_required_passed);
    }

    #[test]
    fn compare_uses_canonical_assertion_ids_and_rejects_reversed_ratios() {
        let before = raster(1, 1, vec![[0, 0, 0, 255]]);
        let after = before.clone();
        let invalid_id = CompareAssertion::ExactEqual {
            id: "contains.dot".to_owned(),
            severity: CompareSeverity::Required,
        };
        assert!(compare_images(&before, &after, &[], &[invalid_id]).is_err());
        let reversed = CompareAssertion::ChangedRatio {
            id: "changed".to_owned(),
            severity: CompareSeverity::Required,
            minimum: Some(0.8),
            maximum: Some(0.2),
        };
        assert!(compare_images(&before, &after, &[], &[reversed]).is_err());
    }

    #[test]
    fn object_centroid_uses_pixel_index_coordinates_on_non_unit_raster() {
        let transparent = [0, 0, 0, 0];
        let opaque = [10, 20, 30, 255];
        let before = raster(4, 2, vec![transparent; 8]);
        let after = raster(
            4,
            2,
            vec![
                transparent,
                opaque,
                opaque,
                transparent,
                transparent,
                opaque,
                opaque,
                transparent,
            ],
        );
        let pass = compare_images(
            &before,
            &after,
            &[],
            &[CompareAssertion::ObjectCentroid {
                id: "centroid".to_owned(),
                severity: CompareSeverity::Required,
                expected: [1.5, 0.5],
                tolerance: 0.0,
            }],
        )
        .unwrap();
        assert_eq!(pass.metrics.after_centroid, Some([1.5, 0.5]));
        assert!(pass.all_required_passed);

        let fail = compare_images(
            &before,
            &after,
            &[],
            &[CompareAssertion::ObjectCentroid {
                id: "centroid".to_owned(),
                severity: CompareSeverity::Required,
                expected: [1.75, 0.5],
                tolerance: 0.0,
            }],
        )
        .unwrap();
        assert!(!fail.all_required_passed);
        assert!(!fail.assertions[0].passed);
    }

    #[test]
    fn diff_and_mask_overlay_are_deterministic() {
        let before = raster(1, 1, vec![[10, 20, 30, 255]]);
        let after = raster(1, 1, vec![[20, 10, 40, 255]]);
        let mask = raster(1, 1, vec![[255, 255, 255, 128]]);
        let diff = difference_preview(&before, &after).unwrap();
        assert_eq!(&diff.pixels()[..4], &[10, 10, 10, 255]);
        let overlay = mask_overlay_preview(&after, &mask).unwrap();
        assert_eq!(overlay.width(), 1);
        assert_eq!(overlay.height(), 1);
        assert_eq!(overlay.pixels()[3], 255);
    }
}
