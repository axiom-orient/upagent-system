use std::collections::BTreeMap;

use crate::core::{PpError, PpResult, Raster};

use super::{
    color_at, colors, ensure_bounds, finish_svg, has_partial_alpha, svg_start, BackendCandidate,
    MethodEvidence, VectorBackend, MAX_PATHS,
};

pub(crate) struct LineArtBackend;

impl VectorBackend for LineArtBackend {
    fn id(&self) -> &'static str {
        "polarity-network-line-art"
    }
    fn version(&self) -> &'static str {
        "1"
    }

    fn vectorize(&self, image: &Raster) -> PpResult<BackendCandidate> {
        ensure_bounds(image)?;
        if has_partial_alpha(image) {
            return Err(PpError::UnsupportedVectorContent(
                "line-art backend rejects translucent strokes".into(),
            ));
        }
        let palette = colors(image, 8, "line-art")?;
        if palette.len() != 2 {
            return Err(PpError::UnsupportedVectorContent(
                "line-art requires exactly two canonical paint values; multi-paint inputs are not silently discarded"
                    .into(),
            ));
        }
        let width = image.width() as usize;
        let height = image.height() as usize;
        let mut frequency = BTreeMap::<[u8; 4], usize>::new();
        for index in 0..width * height {
            *frequency.entry(color_at(image, index)).or_default() += 1;
        }
        let background = frequency
            .iter()
            .max_by_key(|(_, count)| *count)
            .map(|(color, _)| *color)
            .unwrap();
        let foreground = frequency
            .iter()
            .filter(|(color, _)| **color != background && color[3] != 0)
            .min_by_key(|(color, count)| (*count, luma(**color)))
            .map(|(color, _)| *color)
            .ok_or_else(|| {
                PpError::UnsupportedVectorContent(
                    "line-art has no opaque foreground polarity".into(),
                )
            })?;
        let selected: Vec<bool> = (0..width * height)
            .map(|i| color_at(image, i) == foreground)
            .collect();
        let mut shared_boundaries = 0usize;
        if background[3] != 0 {
            for y in 0..height {
                for x in 0..width {
                    let index = y * width + x;
                    if x + 1 < width && selected[index] != selected[index + 1] {
                        shared_boundaries += 1;
                    }
                    if y + 1 < height && selected[index] != selected[index + width] {
                        shared_boundaries += 1;
                    }
                }
            }
        }
        let background_run_count = if background[3] == 0 { 0 } else { height };
        if background_run_count > MAX_PATHS {
            return Err(PpError::UnsupportedVectorContent(format!(
                "line-art background has {background_run_count} emitted runs; limit is {MAX_PATHS}"
            )));
        }
        let mut svg = svg_start(image, true);
        if background_run_count != 0 {
            let background_runs = (0..height)
                .map(|y| (0, y as u32, width as u32))
                .collect::<Vec<_>>();
            super::append_run_path(&mut svg, &background_runs, background);
        }
        let mut foreground_runs = Vec::new();
        for y in 0..height {
            let mut x = 0;
            while x < width {
                if !selected[y * width + x] {
                    x += 1;
                    continue;
                }
                let start = x;
                while x < width && selected[y * width + x] {
                    x += 1;
                }
                if background_run_count + foreground_runs.len() >= MAX_PATHS {
                    return Err(PpError::UnsupportedVectorContent(format!(
                        "line-art has more than {MAX_PATHS} emitted runs"
                    )));
                }
                foreground_runs.push((start as u32, y as u32, (x - start) as u32));
            }
        }
        super::append_run_path(&mut svg, &foreground_runs, foreground);
        Ok(BackendCandidate {
            svg: finish_svg(svg),
            backend_id: self.id(),
            backend_version: self.version(),
            evidence: MethodEvidence {
                family: "line-art",
                palette_size: palette.len(),
                region_count: background_run_count + foreground_runs.len(),
                shared_boundary_count: shared_boundaries,
                endpoint_count: 0,
                junction_count: 0,
                silhouette_fallback: false,
                adaptive: None,
            },
        })
    }
}

fn luma(color: [u8; 4]) -> u32 {
    2126 * color[0] as u32 + 7152 * color[1] as u32 + 722 * color[2] as u32
}
