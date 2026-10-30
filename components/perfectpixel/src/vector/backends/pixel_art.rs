use std::collections::BTreeMap;

use crate::core::{PpError, PpResult, Raster};

use super::{
    append_run_path, color_at, colors, ensure_bounds, finish_svg, svg_start, BackendCandidate,
    MethodEvidence, VectorBackend, MAX_PATHS,
};

pub(crate) struct PixelArtBackend;

impl VectorBackend for PixelArtBackend {
    fn id(&self) -> &'static str {
        "logical-grid-pixel-art"
    }
    fn version(&self) -> &'static str {
        "1"
    }

    fn vectorize(&self, image: &Raster) -> PpResult<BackendCandidate> {
        ensure_bounds(image)?;
        let palette = colors(image, 65_536, "pixel-art")?;
        let width = image.width() as usize;
        let height = image.height() as usize;
        let mut by_color: BTreeMap<[u8; 4], Vec<(u32, u32, u32)>> = BTreeMap::new();
        let mut paths = 0usize;
        for y in 0..height {
            let mut x = 0;
            while x < width {
                let color = color_at(image, y * width + x);
                let start = x;
                x += 1;
                while x < width && color_at(image, y * width + x) == color {
                    x += 1;
                }
                if color[3] != 0 {
                    if paths >= MAX_PATHS {
                        return Err(PpError::UnsupportedVectorContent(format!(
                            "pixel art has more than {MAX_PATHS} emitted logical grid runs"
                        )));
                    }
                    by_color.entry(color).or_default().push((
                        start as u32,
                        y as u32,
                        (x - start) as u32,
                    ));
                    paths += 1;
                }
            }
        }
        let mut svg = svg_start(image, true);
        for (color, runs) in &by_color {
            append_run_path(&mut svg, runs, *color);
        }
        Ok(BackendCandidate {
            svg: finish_svg(svg),
            backend_id: self.id(),
            backend_version: self.version(),
            evidence: MethodEvidence {
                family: "pixel-art",
                palette_size: palette.len(),
                region_count: paths,
                shared_boundary_count: 0,
                endpoint_count: 0,
                junction_count: 0,
                silhouette_fallback: false,
                adaptive: None,
            },
        })
    }
}
