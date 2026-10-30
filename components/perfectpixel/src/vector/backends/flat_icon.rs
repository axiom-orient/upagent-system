use std::collections::{BTreeMap, VecDeque};

use crate::core::{PpError, PpResult, Raster};

use super::{
    append_run_path, color_at, colors, ensure_bounds, finish_svg, has_partial_alpha, svg_start,
    BackendCandidate, MethodEvidence, VectorBackend, MAX_PATHS,
};

type RegionRuns = BTreeMap<([u8; 4], usize), Vec<(u32, u32, u32)>>;

pub(crate) struct FlatIconBackend;

impl VectorBackend for FlatIconBackend {
    fn id(&self) -> &'static str {
        "bounded-flat-regions"
    }
    fn version(&self) -> &'static str {
        "1"
    }

    fn vectorize(&self, image: &Raster) -> PpResult<BackendCandidate> {
        ensure_bounds(image)?;
        if has_partial_alpha(image) {
            return Err(PpError::UnsupportedVectorContent(
                "flat-icon backend rejects interior translucency".into(),
            ));
        }
        let palette = colors(image, 256, "flat-icon")?;
        let width = image.width() as usize;
        let height = image.height() as usize;
        let mut seen = vec![false; width * height];
        let mut regions: RegionRuns = BTreeMap::new();
        let mut region_id = 0usize;
        let mut shared_boundaries = 0usize;
        let mut emitted_runs = 0usize;
        for start in 0..seen.len() {
            if seen[start] || color_at(image, start)[3] == 0 {
                continue;
            }
            let color = color_at(image, start);
            let mut queue = VecDeque::from([start]);
            let mut pixels = Vec::new();
            seen[start] = true;
            while let Some(index) = queue.pop_front() {
                pixels.push(index);
                let x = index % width;
                let y = index / width;
                for neighbor in neighbors(x, y, width, height) {
                    let other = color_at(image, neighbor);
                    if neighbor > index && other[3] != 0 && other != color {
                        shared_boundaries += 1;
                    }
                    if !seen[neighbor] && other == color {
                        seen[neighbor] = true;
                        queue.push_back(neighbor);
                    }
                }
            }
            pixels.sort_unstable();
            let mut runs = Vec::new();
            let mut cursor = 0;
            while cursor < pixels.len() {
                let index = pixels[cursor];
                let y = index / width;
                let x = index % width;
                let mut end = x + 1;
                cursor += 1;
                while end < width && cursor < pixels.len() && pixels[cursor] == y * width + end {
                    end += 1;
                    cursor += 1;
                }
                if emitted_runs >= MAX_PATHS {
                    return Err(PpError::UnsupportedVectorContent(format!(
                        "flat-icon geometry has more than {MAX_PATHS} emitted runs"
                    )));
                }
                runs.push((x as u32, y as u32, (end - x) as u32));
                emitted_runs += 1;
            }
            regions.insert((color, region_id), runs);
            region_id += 1;
        }
        let mut svg = svg_start(image, true);
        for ((color, _), runs) in &regions {
            append_run_path(&mut svg, runs, *color);
        }
        Ok(BackendCandidate {
            svg: finish_svg(svg),
            backend_id: self.id(),
            backend_version: self.version(),
            evidence: MethodEvidence {
                family: "flat-icon",
                palette_size: palette.len(),
                region_count: region_id,
                shared_boundary_count: shared_boundaries,
                endpoint_count: 0,
                junction_count: 0,
                silhouette_fallback: false,
                adaptive: None,
            },
        })
    }
}

fn neighbors(x: usize, y: usize, width: usize, height: usize) -> impl Iterator<Item = usize> {
    let mut values = [None; 4];
    if x > 0 {
        values[0] = Some(y * width + x - 1);
    }
    if x + 1 < width {
        values[1] = Some(y * width + x + 1);
    }
    if y > 0 {
        values[2] = Some((y - 1) * width + x);
    }
    if y + 1 < height {
        values[3] = Some((y + 1) * width + x);
    }
    values.into_iter().flatten()
}
