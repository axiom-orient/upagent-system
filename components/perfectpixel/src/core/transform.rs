use fast_image_resize as fir;
use serde::{Deserialize, Serialize};

mod background;

#[cfg(test)]
use background::feathered_alpha;
pub use background::{plan_remove_background_auto, AutoBackgroundPlan};
use background::{remove_background, validate_auto_background_request, validate_background_keys};

use super::{PpError, PpResult, Raster};

const MAX_EDIT_DIMENSION: u32 = 8_192;
const MAX_EDIT_PIXELS: u64 = (MAX_EDIT_DIMENSION as u64) * (MAX_EDIT_DIMENSION as u64);
/// Cumulative pixel work allowed for one sequential edit request. This bounds
/// repeated full-raster transforms even when every intermediate image stays
/// within the per-raster dimension limit.
const MAX_EDIT_PIXEL_WORK: u64 = 256 * 1024 * 1024;

/// Resampling algorithms with stable pixel semantics for the public raster core.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResampleFilter {
    /// Copies the nearest source pixel. This is appropriate for pixel-art assets.
    Nearest,
    /// High-quality Lanczos3 resampling with alpha-aware interpolation.
    Lanczos3,
}

/// Resizes an RGBA raster without coupling the image-processing core to a file format.
///
/// The caller owns output-dimension policy. Zero-sized outputs are rejected because no
/// image encoder has a useful portable representation for them.
pub fn resize_raster(
    image: &Raster,
    width: u32,
    height: u32,
    filter: ResampleFilter,
) -> PpResult<Raster> {
    if width == 0 || height == 0 {
        return Err(PpError::InvalidRequest(
            "resize dimensions must be positive".to_string(),
        ));
    }
    if image.width() == width && image.height() == height {
        return Ok(image.clone());
    }

    match filter {
        ResampleFilter::Nearest => resize_nearest(image, width, height),
        ResampleFilter::Lanczos3 => resize_lanczos3(image, width, height),
    }
}

/// One deterministic, bounded edit in a raster pipeline.
///
/// Edits are applied in the order supplied by the caller.  The enum deliberately
/// contains only geometric operations; semantic or generative image editing is
/// outside PerfectPixel's local authority.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "op",
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum RasterEdit {
    /// Keep the requested source rectangle.
    Crop {
        x: u32,
        y: u32,
        width: u32,
        height: u32,
    },
    /// Rotate clockwise by 90, 180, or 270 degrees.
    RotateQuarterTurns { quarter_turns: u8 },
    /// Mirror around the vertical axis.
    FlipHorizontal,
    /// Mirror around the horizontal axis.
    FlipVertical,
    /// Resize with one of the bounded core filters.
    Resize {
        width: u32,
        height: u32,
        filter: ResampleFilter,
    },
    /// Clear only key-colored pixels connected to the canvas edge.
    RemoveBackground {
        keys: Vec<[u8; 3]>,
        tolerance: u8,
        feather: u8,
    },
    /// Select a bounded edge palette by exact coverage, then apply the same
    /// four-connected keyed removal as [`RasterEdit::RemoveBackground`].
    RemoveBackgroundAuto {
        max_keys: u8,
        min_edge_coverage_basis_points: u16,
        tolerance: u8,
        feather: u8,
    },
}

/// Apply a bounded sequence of deterministic raster edits.
pub fn apply_raster_edits(image: &Raster, edits: &[RasterEdit]) -> PpResult<Raster> {
    apply_raster_edits_with_evidence(image, edits).map(|(raster, _)| raster)
}

/// Apply edits and return evidence for each auto background step in execution order.
pub fn apply_raster_edits_with_evidence(
    image: &Raster,
    edits: &[RasterEdit],
) -> PpResult<(Raster, Vec<AutoBackgroundPlan>)> {
    if edits.len() > 64 {
        return Err(PpError::InvalidRequest(
            "raster edit pipeline may contain at most 64 steps".to_string(),
        ));
    }
    validate_edit_dimensions(image.width(), image.height())?;
    preflight_pixel_work(image.width(), image.height(), edits)?;
    let mut current = image.clone();
    let mut auto_evidence = Vec::new();
    for edit in edits {
        current = match edit {
            RasterEdit::Crop {
                x,
                y,
                width,
                height,
            } => crop_raster(&current, *x, *y, *width, *height)?,
            RasterEdit::RotateQuarterTurns { quarter_turns } => {
                rotate_raster(&current, *quarter_turns)?
            }
            RasterEdit::FlipHorizontal => flip_raster(&current, true)?,
            RasterEdit::FlipVertical => flip_raster(&current, false)?,
            RasterEdit::Resize {
                width,
                height,
                filter,
            } => resize_raster(&current, *width, *height, *filter)?,
            RasterEdit::RemoveBackground {
                keys,
                tolerance,
                feather,
            } => remove_background(&current, keys, *tolerance, *feather)?,
            RasterEdit::RemoveBackgroundAuto {
                max_keys,
                min_edge_coverage_basis_points,
                tolerance,
                feather,
            } => {
                let plan = plan_remove_background_auto(
                    &current,
                    *max_keys,
                    *min_edge_coverage_basis_points,
                )?;
                let next = remove_background(&current, &plan.selected_keys, *tolerance, *feather)?;
                auto_evidence.push(plan);
                next
            }
        };
        validate_edit_dimensions(current.width(), current.height())?;
    }
    Ok((current, auto_evidence))
}

fn preflight_pixel_work(width: u32, height: u32, edits: &[RasterEdit]) -> PpResult<()> {
    let mut current_width = width;
    let mut current_height = height;
    let mut work = pixel_count(width, height)?;
    for edit in edits {
        let (next_width, next_height, comparison_keys) = match edit {
            RasterEdit::Crop {
                x,
                y,
                width,
                height,
            } => {
                validate_crop_geometry(current_width, current_height, *x, *y, *width, *height)?;
                (*width, *height, 1_u64)
            }
            RasterEdit::RotateQuarterTurns { quarter_turns } => {
                if !(1..=3).contains(quarter_turns) {
                    return Err(PpError::InvalidRequest(
                        "rotate quarterTurns must be 1, 2, or 3".to_string(),
                    ));
                }
                if *quarter_turns == 2 {
                    (current_width, current_height, 1)
                } else {
                    (current_height, current_width, 1)
                }
            }
            RasterEdit::FlipHorizontal | RasterEdit::FlipVertical => {
                (current_width, current_height, 1)
            }
            RasterEdit::Resize { width, height, .. } => (*width, *height, 1),
            RasterEdit::RemoveBackground { keys, .. } => {
                validate_background_keys(keys)?;
                (
                    current_width,
                    current_height,
                    u64::try_from(keys.len()).map_err(|_| {
                        PpError::InvalidRequest("background key count overflowed".to_string())
                    })?,
                )
            }
            RasterEdit::RemoveBackgroundAuto { max_keys, .. } => {
                validate_auto_background_request(*max_keys, 1)?;
                (current_width, current_height, u64::from(*max_keys))
            }
        };
        validate_edit_dimensions(next_width, next_height)?;
        let touched = pixel_count(current_width, current_height)?;
        let output = pixel_count(next_width, next_height)?;
        let comparisons = touched
            .checked_mul(comparison_keys)
            .ok_or_else(|| PpError::InvalidRequest("edit pixel work overflowed".to_string()))?;
        work = work
            .checked_add(comparisons)
            .and_then(|value| value.checked_add(output))
            .ok_or_else(|| PpError::InvalidRequest("edit pixel work overflowed".to_string()))?;
        if work > MAX_EDIT_PIXEL_WORK {
            return Err(PpError::InvalidRequest(format!(
                "edit pixel work exceeds the bounded limit of {MAX_EDIT_PIXEL_WORK}"
            )));
        }
        current_width = next_width;
        current_height = next_height;
    }
    Ok(())
}

fn pixel_count(width: u32, height: u32) -> PpResult<u64> {
    let pixels = u64::from(width)
        .checked_mul(u64::from(height))
        .ok_or_else(|| PpError::InvalidRequest("edit pixel count overflowed".to_string()))?;
    if pixels > MAX_EDIT_PIXELS {
        return Err(PpError::InvalidRequest(
            "edit pixel count exceeds the bounded limit".to_string(),
        ));
    }
    Ok(pixels)
}

fn validate_crop_geometry(
    source_width: u32,
    source_height: u32,
    x: u32,
    y: u32,
    width: u32,
    height: u32,
) -> PpResult<()> {
    if width == 0 || height == 0 {
        return Err(PpError::InvalidRequest(
            "crop dimensions must be positive".to_string(),
        ));
    }
    let right = x.checked_add(width).ok_or_else(|| {
        PpError::InvalidRequest("crop rectangle exceeds source bounds".to_string())
    })?;
    let bottom = y.checked_add(height).ok_or_else(|| {
        PpError::InvalidRequest("crop rectangle exceeds source bounds".to_string())
    })?;
    if right > source_width || bottom > source_height {
        return Err(PpError::InvalidRequest(
            "crop rectangle exceeds source bounds".to_string(),
        ));
    }
    Ok(())
}

fn validate_edit_dimensions(width: u32, height: u32) -> PpResult<()> {
    if width == 0 || height == 0 {
        return Err(PpError::InvalidRequest(
            "edit dimensions must be positive".to_string(),
        ));
    }
    if width > MAX_EDIT_DIMENSION || height > MAX_EDIT_DIMENSION {
        return Err(PpError::InvalidRequest(format!(
            "edit dimensions exceed the bounded limit of {MAX_EDIT_DIMENSION} per side"
        )));
    }
    if u64::from(width) * u64::from(height) > MAX_EDIT_PIXELS {
        return Err(PpError::InvalidRequest(
            "edit pixel count exceeds the bounded limit".to_string(),
        ));
    }
    Ok(())
}

fn crop_raster(image: &Raster, x: u32, y: u32, width: u32, height: u32) -> PpResult<Raster> {
    validate_crop_geometry(image.width(), image.height(), x, y, width, height)?;
    let mut output = Raster::blank(width, height)?;
    output.copy_region(
        image,
        super::FrameRect {
            x,
            y,
            w: width,
            h: height,
        },
        0,
        0,
    )?;
    Ok(output)
}

fn rotate_raster(image: &Raster, quarter_turns: u8) -> PpResult<Raster> {
    if !(1..=3).contains(&quarter_turns) {
        return Err(PpError::InvalidRequest(
            "rotate quarterTurns must be 1, 2, or 3".to_string(),
        ));
    }
    let (width, height) = if quarter_turns == 2 {
        (image.width(), image.height())
    } else {
        (image.height(), image.width())
    };
    let mut output = Raster::blank(width, height)?;
    for y in 0..image.height() {
        for x in 0..image.width() {
            let (dst_x, dst_y) = match quarter_turns {
                1 => (image.height() - 1 - y, x),
                2 => (image.width() - 1 - x, image.height() - 1 - y),
                3 => (y, image.width() - 1 - x),
                _ => unreachable!("validated quarter turns"),
            };
            let source = ((y as usize) * (image.width() as usize) + x as usize) * 4;
            let destination = ((dst_y as usize) * (output.width() as usize) + dst_x as usize) * 4;
            output.pixels_mut()[destination..destination + 4]
                .copy_from_slice(&image.pixels()[source..source + 4]);
        }
    }
    Ok(output)
}

fn flip_raster(image: &Raster, horizontal: bool) -> PpResult<Raster> {
    let mut output = Raster::blank(image.width(), image.height())?;
    for y in 0..image.height() {
        for x in 0..image.width() {
            let (dst_x, dst_y) = if horizontal {
                (image.width() - 1 - x, y)
            } else {
                (x, image.height() - 1 - y)
            };
            let source = ((y as usize) * (image.width() as usize) + x as usize) * 4;
            let destination = ((dst_y as usize) * (output.width() as usize) + dst_x as usize) * 4;
            output.pixels_mut()[destination..destination + 4]
                .copy_from_slice(&image.pixels()[source..source + 4]);
        }
    }
    Ok(output)
}

fn resize_nearest(image: &Raster, width: u32, height: u32) -> PpResult<Raster> {
    let mut pixels = Vec::with_capacity(
        usize::try_from(width)
            .ok()
            .and_then(|value| value.checked_mul(usize::try_from(height).ok()?))
            .and_then(|value| value.checked_mul(4))
            .ok_or_else(|| {
                PpError::InvalidRequest(format!("image dimensions overflow: {width}x{height}"))
            })?,
    );
    for y in 0..height {
        let source_y = ((u64::from(y) * u64::from(image.height())) / u64::from(height)) as u32;
        for x in 0..width {
            let source_x = ((u64::from(x) * u64::from(image.width())) / u64::from(width)) as u32;
            let source_index =
                ((source_y as usize) * (image.width() as usize) + source_x as usize) * 4;
            pixels.extend_from_slice(&image.pixels()[source_index..source_index + 4]);
        }
    }
    Raster::new(width, height, pixels)
}

fn resize_lanczos3(image: &Raster, width: u32, height: u32) -> PpResult<Raster> {
    let source = fir::images::Image::from_vec_u8(
        image.width(),
        image.height(),
        image.pixels().to_vec(),
        fir::PixelType::U8x4,
    )
    .map_err(resize_error)?;
    let mut output = fir::images::Image::new(width, height, fir::PixelType::U8x4);
    let options = fir::ResizeOptions::new()
        .resize_alg(fir::ResizeAlg::Convolution(fir::FilterType::Lanczos3))
        .use_alpha(true);
    fir::Resizer::new()
        .resize(&source, &mut output, &options)
        .map_err(resize_error)?;
    Raster::new(width, height, output.into_vec())
}

fn resize_error(error: impl std::fmt::Display) -> PpError {
    PpError::InvalidRequest(format!("raster resize failed: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nearest_upscale_repeats_source_pixels_exactly() {
        let image = Raster::new(2, 1, vec![1, 2, 3, 255, 4, 5, 6, 255]).unwrap();
        let resized = resize_raster(&image, 4, 1, ResampleFilter::Nearest).unwrap();
        assert_eq!(
            resized.pixels(),
            &[1, 2, 3, 255, 1, 2, 3, 255, 4, 5, 6, 255, 4, 5, 6, 255]
        );
    }

    #[test]
    fn lanczos_resize_preserves_transparent_edge_without_hidden_color_halo() {
        let image = Raster::new(2, 1, vec![255, 0, 0, 255, 0, 255, 0, 0]).unwrap();
        let resized = resize_raster(&image, 1, 1, ResampleFilter::Lanczos3).unwrap();
        assert!(resized.pixels()[0] > resized.pixels()[1]);
        assert!(resized.pixels()[3] > 0 && resized.pixels()[3] < 255);
    }

    #[test]
    fn resize_rejects_zero_dimension() {
        let image = Raster::blank(1, 1).unwrap();
        assert!(matches!(
            resize_raster(&image, 0, 1, ResampleFilter::Nearest),
            Err(PpError::InvalidRequest(_))
        ));
    }

    #[test]
    fn edits_are_applied_sequentially_with_expected_geometry() {
        let image = Raster::new(
            2,
            3,
            vec![
                1, 0, 0, 255, 2, 0, 0, 255, 3, 0, 0, 255, 4, 0, 0, 255, 5, 0, 0, 255, 6, 0, 0, 255,
            ],
        )
        .unwrap();
        let edited = apply_raster_edits(
            &image,
            &[
                RasterEdit::Crop {
                    x: 0,
                    y: 1,
                    width: 2,
                    height: 2,
                },
                RasterEdit::RotateQuarterTurns { quarter_turns: 1 },
                RasterEdit::FlipHorizontal,
            ],
        )
        .unwrap();
        assert_eq!((edited.width(), edited.height()), (2, 2));
        assert_eq!(edited.pixels()[0], 3);
        assert_eq!(edited.pixels()[4], 5);
    }

    #[test]
    fn edits_reject_invalid_crop_and_rotation() {
        let image = Raster::blank(2, 2).unwrap();
        assert!(apply_raster_edits(
            &image,
            &[RasterEdit::Crop {
                x: 1,
                y: 1,
                width: 2,
                height: 2,
            }]
        )
        .is_err());
        assert!(apply_raster_edits(
            &image,
            &[RasterEdit::RotateQuarterTurns { quarter_turns: 4 }]
        )
        .is_err());
    }

    #[test]
    fn rotations_two_and_three_and_vertical_flip_have_exact_orientation() {
        let image = Raster::new(
            2,
            3,
            vec![
                1, 0, 0, 255, 2, 0, 0, 255, 3, 0, 0, 255, 4, 0, 0, 255, 5, 0, 0, 255, 6, 0, 0, 255,
            ],
        )
        .unwrap();
        let rotate_two = apply_raster_edits(
            &image,
            &[RasterEdit::RotateQuarterTurns { quarter_turns: 2 }],
        )
        .unwrap();
        assert_eq!((rotate_two.width(), rotate_two.height()), (2, 3));
        assert_eq!(
            rotate_two
                .pixels()
                .as_chunks::<4>()
                .0
                .iter()
                .map(|pixel| pixel[0])
                .collect::<Vec<_>>(),
            vec![6, 5, 4, 3, 2, 1]
        );

        let rotate_three = apply_raster_edits(
            &image,
            &[RasterEdit::RotateQuarterTurns { quarter_turns: 3 }],
        )
        .unwrap();
        assert_eq!((rotate_three.width(), rotate_three.height()), (3, 2));
        assert_eq!(
            rotate_three
                .pixels()
                .as_chunks::<4>()
                .0
                .iter()
                .map(|pixel| pixel[0])
                .collect::<Vec<_>>(),
            vec![2, 4, 6, 1, 3, 5]
        );

        let vertical = apply_raster_edits(&image, &[RasterEdit::FlipVertical]).unwrap();
        assert_eq!(
            vertical
                .pixels()
                .as_chunks::<4>()
                .0
                .iter()
                .map(|pixel| pixel[0])
                .collect::<Vec<_>>(),
            vec![5, 6, 3, 4, 1, 2]
        );
    }

    #[test]
    fn resize_dimensions_propagate_through_a_pipeline() {
        let image = Raster::new(
            2,
            3,
            vec![
                1, 0, 0, 255, 2, 0, 0, 255, 3, 0, 0, 255, 4, 0, 0, 255, 5, 0, 0, 255, 6, 0, 0, 255,
            ],
        )
        .unwrap();
        let edited = apply_raster_edits(
            &image,
            &[
                RasterEdit::Crop {
                    x: 0,
                    y: 0,
                    width: 2,
                    height: 2,
                },
                RasterEdit::Resize {
                    width: 3,
                    height: 1,
                    filter: ResampleFilter::Nearest,
                },
            ],
        )
        .unwrap();
        assert_eq!((edited.width(), edited.height()), (3, 1));
        assert_eq!(
            edited
                .pixels()
                .as_chunks::<4>()
                .0
                .iter()
                .map(|pixel| pixel[0])
                .collect::<Vec<_>>(),
            vec![1, 1, 2]
        );
    }

    #[test]
    fn cumulative_pixel_work_rejects_repeated_full_raster_edits_before_allocating() {
        let image = Raster::blank(2_048, 2_048).unwrap();
        let edits = vec![RasterEdit::FlipHorizontal; 32];
        assert!(matches!(
            apply_raster_edits(&image, &edits),
            Err(PpError::InvalidRequest(message)) if message.contains("pixel work")
        ));
    }

    #[test]
    fn edge_connected_background_removal_preserves_isolated_matching_subject_pixels() {
        let mut pixels = Vec::new();
        for y in 0..5 {
            for x in 0..5 {
                let rgb = if (x + y) % 2 == 0 {
                    [10, 20, 30]
                } else {
                    [40, 50, 60]
                };
                pixels.extend_from_slice(&[rgb[0], rgb[1], rgb[2], 255]);
            }
        }
        // A non-key ring disconnects the center key-colored subject pixel from
        // the checkerboard edge background.
        for (x, y) in [(1, 2), (2, 1), (3, 2), (2, 3)] {
            let index = (y * 5 + x) * 4;
            pixels[index..index + 4].copy_from_slice(&[255, 255, 255, 255]);
        }
        let image = Raster::new(5, 5, pixels).unwrap();
        let edited = apply_raster_edits(
            &image,
            &[RasterEdit::RemoveBackground {
                keys: vec![[10, 20, 30], [40, 50, 60]],
                tolerance: 0,
                feather: 0,
            }],
        )
        .unwrap();
        assert_eq!(edited.alpha_at(0, 0), 0);
        assert_eq!(edited.alpha_at(4, 4), 0);
        assert_eq!(edited.alpha_at(2, 2), 255);
        assert_eq!(edited.alpha_at(2, 1), 255);
    }

    #[test]
    fn background_feather_uses_exact_linear_alpha_math() {
        let image =
            Raster::new(3, 1, vec![0, 0, 0, 200, 15, 15, 15, 200, 30, 30, 30, 200]).unwrap();
        let edited = apply_raster_edits(
            &image,
            &[RasterEdit::RemoveBackground {
                keys: vec![[0, 0, 0]],
                tolerance: 10,
                feather: 10,
            }],
        )
        .unwrap();
        assert_eq!(edited.alpha_at(0, 0), 0);
        assert_eq!(edited.alpha_at(1, 0), 100);
        assert_eq!(edited.alpha_at(2, 0), 200);
        assert_eq!(feathered_alpha(200, 255, 250, 10), 100);
    }

    #[test]
    fn background_removal_rejects_empty_duplicate_and_excessive_keys() {
        let image = Raster::blank(1, 1).unwrap();
        for keys in [Vec::new(), vec![[0, 0, 0], [0, 0, 0]], vec![[0, 0, 0]; 17]] {
            assert!(apply_raster_edits(
                &image,
                &[RasterEdit::RemoveBackground {
                    keys,
                    tolerance: 0,
                    feather: 0,
                }]
            )
            .is_err());
        }
    }

    #[test]
    fn automatic_background_removal_uses_edge_coverage_and_preserves_subject() {
        let mut pixels = Vec::new();
        for y in 0..5 {
            for x in 0..5 {
                let rgb = if (1..=3).contains(&x) && (1..=3).contains(&y) {
                    [255, 255, 255]
                } else if (x + y) % 2 == 0 {
                    [220, 10, 20]
                } else {
                    [10, 30, 220]
                };
                pixels.extend_from_slice(&[rgb[0], rgb[1], rgb[2], 255]);
            }
        }
        let image = Raster::new(5, 5, pixels).unwrap();
        let plan = plan_remove_background_auto(&image, 2, 10_000).unwrap();
        assert_eq!(plan.edge_coverage_basis_points, 10_000);
        assert_eq!(plan.selected_keys, vec![[10, 30, 220], [220, 10, 20]]);
        let (edited, evidence) = apply_raster_edits_with_evidence(
            &image,
            &[RasterEdit::RemoveBackgroundAuto {
                max_keys: 2,
                min_edge_coverage_basis_points: 10_000,
                tolerance: 0,
                feather: 0,
            }],
        )
        .unwrap();
        assert_eq!(evidence, vec![plan]);
        assert_eq!(edited.alpha_at(0, 0), 0);
        assert_eq!(edited.alpha_at(4, 4), 0);
        assert_eq!(edited.alpha_at(2, 2), 255);
    }

    #[test]
    fn automatic_background_removal_fails_closed_when_coverage_is_low() {
        let mut pixels = Vec::new();
        for y in 0..5 {
            for x in 0..5 {
                let rgb = if (x + y) % 2 == 0 {
                    [10, 20, 30]
                } else {
                    [40, 50, 60]
                };
                pixels.extend_from_slice(&[rgb[0], rgb[1], rgb[2], 255]);
            }
        }
        let image = Raster::new(5, 5, pixels).unwrap();
        assert!(matches!(
            apply_raster_edits(
                &image,
                &[RasterEdit::RemoveBackgroundAuto {
                    max_keys: 1,
                    min_edge_coverage_basis_points: 10_000,
                    tolerance: 0,
                    feather: 0,
                }]
            ),
            Err(PpError::InvalidRequest(message)) if message.contains("edge coverage")
        ));
    }

    #[test]
    fn automatic_background_removal_rejects_heterogeneous_edges_and_work_overflow() {
        let mut pixels = vec![255_u8; 17 * 17 * 4];
        let mut next = 0_u8;
        for y in 0..17_u32 {
            for x in 0..17_u32 {
                if x == 0 || y == 0 || x == 16 || y == 16 {
                    let index = ((y * 17 + x) * 4) as usize;
                    pixels[index..index + 4].copy_from_slice(&[next, next / 2, 255 - next, 255]);
                    next = next.wrapping_add(1);
                }
            }
        }
        let image = Raster::new(17, 17, pixels).unwrap();
        assert!(matches!(
            plan_remove_background_auto(&image, 16, 9_000),
            Err(PpError::InvalidRequest(message)) if message.contains("edge coverage")
        ));

        let large = Raster::blank(2_048, 2_048).unwrap();
        let edits = vec![
            RasterEdit::RemoveBackgroundAuto {
                max_keys: 1,
                min_edge_coverage_basis_points: 10_000,
                tolerance: 0,
                feather: 0,
            };
            32
        ];
        assert!(matches!(
            apply_raster_edits(&large, &edits),
            Err(PpError::InvalidRequest(message)) if message.contains("pixel work")
        ));
    }

    #[test]
    fn automatic_background_removal_allows_minor_edge_noise_when_coverage_is_met() {
        let width = 33_u32;
        let height = 33_u32;
        let mut pixels = vec![255_u8; (width * height * 4) as usize];
        for y in 0..height {
            for x in 0..width {
                if x == 0 || y == 0 || x + 1 == width || y + 1 == height {
                    let index = ((y * width + x) * 4) as usize;
                    pixels[index..index + 4].copy_from_slice(&[10, 20, 30, 255]);
                }
            }
        }
        for x in 0..16_u32 {
            let index = x as usize * 4;
            pixels[index..index + 4].copy_from_slice(&[
                (x + 40) as u8,
                (x + 80) as u8,
                (x + 120) as u8,
                255,
            ]);
        }
        let image = Raster::new(width, height, pixels).unwrap();
        let plan = plan_remove_background_auto(&image, 1, 8_000).unwrap();
        assert_eq!(plan.selected_keys, vec![[10, 20, 30]]);
        assert_eq!(plan.edge_coverage_basis_points, 8_750);
        let edited = apply_raster_edits(
            &image,
            &[RasterEdit::RemoveBackgroundAuto {
                max_keys: 1,
                min_edge_coverage_basis_points: 8_000,
                tolerance: 0,
                feather: 0,
            }],
        )
        .unwrap();
        assert_eq!(edited.alpha_at(32, 16), 0);
    }
}
