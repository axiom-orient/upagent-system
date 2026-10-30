use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::core::Raster;

use super::canonical_rgba;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RasterSignalClass {
    PixelArt,
    FlatIcon,
    TransparentIllustration,
    ColorIllustration,
    ContinuousTone,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AutoDisposition {
    Classified(RasterSignalClass),
    PresetRequired,
    Unsupported,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GeometryEvidence {
    pub width: u32,
    pub height: u32,
    pub horizontal_run_ratio: f64,
    pub strong_edge_ratio: f64,
    pub hard_grid_likelihood: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PaintEvidence {
    pub unique_color_count: usize,
    pub unique_color_ratio: f64,
    pub palette_likelihood: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AlphaEvidence {
    pub transparent_pixel_ratio: f64,
    pub partial_alpha_ratio: f64,
    pub has_interior_translucency: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ComplexityEvidence {
    pub pixel_count: usize,
    pub horizontal_run_count: usize,
    pub estimated_region_complexity: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceNoiseEvidence {
    pub isolated_pixel_ratio: f64,
    pub source_noise_likelihood: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ProfileConflict {
    GridAndContinuousTone,
    PaletteAndContinuousTone,
    OpaqueAndPartialAlpha,
    HighSourceNoise,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RasterEvidenceProfile {
    pub class: RasterSignalClass,
    pub auto_disposition: AutoDisposition,
    pub confidence: f64,
    pub conflicts: Vec<ProfileConflict>,
    pub abstains: bool,
    pub geometry: GeometryEvidence,
    pub paint: PaintEvidence,
    pub alpha: AlphaEvidence,
    pub complexity: ComplexityEvidence,
    pub source_noise: SourceNoiseEvidence,
    pub reasons: Vec<String>,
}

impl RasterEvidenceProfile {
    pub(crate) fn is_supported_for_auto(&self) -> bool {
        matches!(self.auto_disposition, AutoDisposition::Classified(_))
    }
}

pub(super) fn analyze_content(image: &Raster) -> RasterEvidenceProfile {
    let width = image.width() as usize;
    let height = image.height() as usize;
    let pixel_count = width.saturating_mul(height);
    let pixels = image.pixels();
    let mut colors = BTreeSet::new();
    let mut horizontal_runs = 0usize;
    let mut transparent_pixels = 0usize;
    let mut partial_alpha_pixels = 0usize;
    let mut isolated_pixels = 0usize;

    for y in 0..height {
        for x in 0..width {
            let offset = (y * width + x) * 4;
            let color = canonical_rgba(&pixels[offset..offset + 4]);
            colors.insert(color);
            if x == 0 || canonical_rgba(&pixels[offset - 4..offset]) != color {
                horizontal_runs += 1;
            }
            if color[3] == 0 {
                transparent_pixels += 1;
            } else if color[3] < 255 {
                partial_alpha_pixels += 1;
            }
            if is_isolated_pixel(pixels, width, height, x, y, color) {
                isolated_pixels += 1;
            }
        }
    }

    let neighbor_count = height.saturating_mul(width.saturating_sub(1))
        + width.saturating_mul(height.saturating_sub(1));
    let mut strong_edges = 0usize;
    for y in 0..height {
        for x in 0..width {
            let current = premultiplied_luma(pixels, width, x, y);
            if x + 1 < width
                && (current - premultiplied_luma(pixels, width, x + 1, y)).abs() >= 24.0
            {
                strong_edges += 1;
            }
            if y + 1 < height
                && (current - premultiplied_luma(pixels, width, x, y + 1)).abs() >= 24.0
            {
                strong_edges += 1;
            }
        }
    }

    let denominator = pixel_count.max(1) as f64;
    let unique_color_ratio = colors.len() as f64 / denominator;
    let horizontal_run_ratio = horizontal_runs as f64 / denominator;
    let transparent_pixel_ratio = transparent_pixels as f64 / denominator;
    let partial_alpha_ratio = partial_alpha_pixels as f64 / denominator;
    let strong_edge_ratio = strong_edges as f64 / neighbor_count.max(1) as f64;
    let isolated_pixel_ratio = isolated_pixels as f64 / denominator;
    let hard_grid_likelihood = (1.0 - horizontal_run_ratio / 0.45).clamp(0.0, 1.0)
        * (1.0 - unique_color_ratio / 0.05).clamp(0.0, 1.0)
        * (1.0 - partial_alpha_ratio * 20.0).clamp(0.0, 1.0);
    let palette_likelihood = (1.0 - unique_color_ratio / 0.30).clamp(0.0, 1.0);
    let source_noise_likelihood = (isolated_pixel_ratio * 8.0).clamp(0.0, 1.0);
    let estimated_region_complexity =
        (horizontal_run_ratio * (1.0 + unique_color_ratio)).clamp(0.0, 1.0);

    let geometry = GeometryEvidence {
        width: image.width(),
        height: image.height(),
        horizontal_run_ratio,
        strong_edge_ratio,
        hard_grid_likelihood,
    };
    let paint = PaintEvidence {
        unique_color_count: colors.len(),
        unique_color_ratio,
        palette_likelihood,
    };
    let alpha = AlphaEvidence {
        transparent_pixel_ratio,
        partial_alpha_ratio,
        has_interior_translucency: has_interior_translucency(pixels, width, height),
    };
    let complexity = ComplexityEvidence {
        pixel_count,
        horizontal_run_count: horizontal_runs,
        estimated_region_complexity,
    };
    let source_noise = SourceNoiseEvidence {
        isolated_pixel_ratio,
        source_noise_likelihood,
    };

    let class = if width <= 512
        && height <= 512
        && colors.len() <= 1_024
        && hard_grid_likelihood >= 0.55
        && transparent_pixel_ratio < 0.01
    {
        RasterSignalClass::PixelArt
    } else if transparent_pixel_ratio >= 0.05 || partial_alpha_ratio >= 0.005 {
        RasterSignalClass::TransparentIllustration
    } else if colors.len() > 1 && (horizontal_run_ratio >= 0.95 || unique_color_ratio >= 0.30) {
        RasterSignalClass::ContinuousTone
    } else if colors.len() <= 50_000 && horizontal_run_ratio <= 0.55 {
        RasterSignalClass::FlatIcon
    } else {
        RasterSignalClass::ColorIllustration
    };

    let mut conflicts = Vec::new();
    if hard_grid_likelihood >= 0.55 && (horizontal_run_ratio >= 0.95 || unique_color_ratio >= 0.30)
    {
        conflicts.push(ProfileConflict::GridAndContinuousTone);
    }
    if palette_likelihood >= 0.60 && unique_color_ratio >= 0.30 {
        conflicts.push(ProfileConflict::PaletteAndContinuousTone);
    }
    if partial_alpha_ratio > 0.0 && transparent_pixel_ratio == 0.0 {
        conflicts.push(ProfileConflict::OpaqueAndPartialAlpha);
    }
    if source_noise_likelihood >= 0.25 {
        conflicts.push(ProfileConflict::HighSourceNoise);
    }
    let confidence = match class {
        RasterSignalClass::PixelArt => hard_grid_likelihood,
        RasterSignalClass::ContinuousTone => {
            (horizontal_run_ratio.max(unique_color_ratio)).clamp(0.0, 1.0)
        }
        RasterSignalClass::FlatIcon => palette_likelihood * (1.0 - estimated_region_complexity),
        RasterSignalClass::TransparentIllustration => {
            (transparent_pixel_ratio + partial_alpha_ratio * 10.0).clamp(0.0, 1.0)
        }
        RasterSignalClass::ColorIllustration => (1.0 - unique_color_ratio)
            .min(1.0 - horizontal_run_ratio)
            .clamp(0.0, 1.0),
    };
    let auto_disposition =
        if class == RasterSignalClass::ContinuousTone || alpha.has_interior_translucency {
            AutoDisposition::Unsupported
        } else if confidence < 0.60 || !conflicts.is_empty() {
            AutoDisposition::PresetRequired
        } else {
            AutoDisposition::Classified(class)
        };
    let abstains = !matches!(auto_disposition, AutoDisposition::Classified(_));

    RasterEvidenceProfile {
        class,
        auto_disposition,
        confidence,
        conflicts,
        abstains,
        geometry,
        paint,
        alpha,
        complexity,
        source_noise,
        reasons: vec![
            format!("dimensions={}x{}", image.width(), image.height()),
            format!("uniqueColorRatio={unique_color_ratio:.6}"),
            format!("horizontalRunRatio={horizontal_run_ratio:.6}"),
            format!("transparentPixelRatio={transparent_pixel_ratio:.6}"),
            format!("partialAlphaRatio={partial_alpha_ratio:.6}"),
            format!("strongEdgeRatio={strong_edge_ratio:.6}"),
            format!("confidence={confidence:.6}"),
        ],
    }
}

fn is_isolated_pixel(
    pixels: &[u8],
    width: usize,
    height: usize,
    x: usize,
    y: usize,
    color: [u8; 4],
) -> bool {
    let mut neighbors = 0usize;
    let mut different = 0usize;
    for (nx, ny) in [
        (x.wrapping_sub(1), y),
        (x + 1, y),
        (x, y.wrapping_sub(1)),
        (x, y + 1),
    ] {
        if nx < width && ny < height {
            neighbors += 1;
            let offset = (ny * width + nx) * 4;
            if canonical_rgba(&pixels[offset..offset + 4]) != color {
                different += 1;
            }
        }
    }
    neighbors > 0 && different == neighbors
}

fn has_interior_translucency(pixels: &[u8], width: usize, height: usize) -> bool {
    for y in 0..height {
        for x in 0..width {
            let offset = (y * width + x) * 4;
            let alpha = pixels[offset + 3];
            if alpha == 0 || alpha == 255 {
                continue;
            }
            let mut neighbors = 0;
            let mut all_covered = true;
            for (nx, ny) in [
                (x.wrapping_sub(1), y),
                (x + 1, y),
                (x, y.wrapping_sub(1)),
                (x, y + 1),
            ] {
                if nx < width && ny < height {
                    neighbors += 1;
                    all_covered &= pixels[(ny * width + nx) * 4 + 3] > 0;
                }
            }
            if neighbors >= 2 && all_covered {
                return true;
            }
        }
    }
    false
}

fn premultiplied_luma(pixels: &[u8], width: usize, x: usize, y: usize) -> f64 {
    let offset = (y * width + x) * 4;
    let alpha = f64::from(pixels[offset + 3]) / 255.0;
    alpha
        * (0.2126 * f64::from(pixels[offset])
            + 0.7152 * f64::from(pixels[offset + 1])
            + 0.0722 * f64::from(pixels[offset + 2]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hidden_transparent_rgb_does_not_change_profile_evidence() {
        let canonical = Raster::new(
            3,
            2,
            vec![
                255, 0, 0, 255, 0, 0, 0, 0, 0, 0, 255, 255, 0, 0, 0, 0, 255, 0, 0, 255, 0, 0, 255,
                255,
            ],
        )
        .unwrap();
        let hidden_rgb = Raster::new(
            3,
            2,
            vec![
                255, 0, 0, 255, 255, 0, 255, 0, 0, 0, 255, 255, 12, 34, 56, 0, 255, 0, 0, 255, 0,
                0, 255, 255,
            ],
        )
        .unwrap();

        assert_eq!(analyze_content(&canonical), analyze_content(&hidden_rgb));
    }
}
