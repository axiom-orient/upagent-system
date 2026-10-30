use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::{content_bbox, FrameRect, Point, Raster, ALPHA_THRESHOLD};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RasterInspection {
    pub width: u32,
    pub height: u32,
    pub foreground_pixels: usize,
    pub alpha_ratio: f64,
    pub content_box: FrameRect,
    pub center: Point,
    pub ground_y: u32,
    pub touches_edge: bool,
    /// True when at least one decoded RGBA8 pixel is not fully opaque.
    pub has_alpha: bool,
    /// Stable decoded pixel representation used by all inspection math.
    pub pixel_format: &'static str,
    /// Number of unique canvas-edge coordinates sampled exactly once.
    pub edge_pixel_count: u64,
    /// At most sixteen edge RGB colors, ordered by count descending then RGB.
    pub edge_palette: Vec<EdgePaletteEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EdgePaletteEntry {
    pub rgb: [u8; 3],
    pub count: u64,
}

/// Geometry/alpha inspection over decoded RGBA pixels only.
/// Color provenance is intentionally not inferred here because `Raster` does not own it.
pub fn inspect_raster(image: &Raster) -> RasterInspection {
    let foreground_pixels = image
        .pixels()
        .as_chunks::<4>()
        .0
        .iter()
        .filter(|rgba| rgba[3] > ALPHA_THRESHOLD)
        .count();
    let content_box = content_bbox(image);
    let pixel_count = u64::from(image.width()) * u64::from(image.height());
    let (edge_pixel_count, edge_palette) = edge_palette(image);

    RasterInspection {
        width: image.width(),
        height: image.height(),
        foreground_pixels,
        alpha_ratio: if pixel_count == 0 {
            0.0
        } else {
            foreground_pixels as f64 / pixel_count as f64
        },
        center: content_center(content_box),
        ground_y: content_box.y + content_box.h,
        touches_edge: touches_edge(content_box, image.width(), image.height()),
        has_alpha: image
            .pixels()
            .as_chunks::<4>()
            .0
            .iter()
            .any(|rgba| rgba[3] != u8::MAX),
        pixel_format: "rgba8",
        edge_pixel_count,
        edge_palette,
        content_box,
    }
}

fn edge_palette(image: &Raster) -> (u64, Vec<EdgePaletteEntry>) {
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

    let mut palette = colors
        .into_iter()
        .map(|(rgb, count)| EdgePaletteEntry { rgb, count })
        .collect::<Vec<_>>();
    palette.sort_by(|left, right| {
        right
            .count
            .cmp(&left.count)
            .then_with(|| left.rgb.cmp(&right.rgb))
    });
    palette.truncate(16);
    (count, palette)
}

fn content_center(rect: FrameRect) -> Point {
    if rect.w == 0 || rect.h == 0 {
        return Point::default();
    }
    Point {
        x: rect.x + rect.w / 2,
        y: rect.y + rect.h / 2,
    }
}

fn touches_edge(rect: FrameRect, width: u32, height: u32) -> bool {
    if rect.w == 0 || rect.h == 0 {
        return false;
    }
    rect.x == 0 || rect.y == 0 || rect.x + rect.w >= width || rect.y + rect.h >= height
}
