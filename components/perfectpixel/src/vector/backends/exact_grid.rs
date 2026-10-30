use std::collections::BTreeMap;
use std::fmt::Write as _;

use crate::core::{PpError, PpResult, Raster};

use super::{BackendCandidate, MethodEvidence, VectorBackend};

pub(crate) struct ExactGridBackend;

impl VectorBackend for ExactGridBackend {
    fn id(&self) -> &'static str {
        "perfectpixel/exact-grid/1"
    }

    fn version(&self) -> &'static str {
        "1"
    }

    fn vectorize(&self, image: &Raster) -> PpResult<BackendCandidate> {
        const MAX_COLORS: usize = 65_536;
        const MAX_RUNS: usize = 250_000;
        let mut layers = BTreeMap::<[u8; 4], String>::new();
        let mut run_count = 0usize;
        let width = image.width() as usize;
        for y in 0..image.height() as usize {
            let mut x = 0;
            while x < width {
                let offset = (y * width + x) * 4;
                let color = [
                    image.pixels()[offset],
                    image.pixels()[offset + 1],
                    image.pixels()[offset + 2],
                    image.pixels()[offset + 3],
                ];
                let mut end = x + 1;
                while end < width
                    && image.pixels()[(y * width + end) * 4..(y * width + end) * 4 + 4] == color
                {
                    end += 1;
                }
                if color[3] != 0 {
                    if layers.len() >= MAX_COLORS && !layers.contains_key(&color) {
                        return Err(PpError::UnsupportedVectorContent(
                            "exact grid exceeds bounded color geometry".to_owned(),
                        ));
                    }
                    run_count += 1;
                    if run_count > MAX_RUNS {
                        return Err(PpError::UnsupportedVectorContent(
                            "exact grid exceeds bounded run geometry".to_owned(),
                        ));
                    }
                    let path = layers.entry(color).or_default();
                    write!(path, "M{x} {y}h{}v1h-{}z", end - x, end - x)
                        .map_err(|_| PpError::Vectorizer("cannot build exact SVG".to_owned()))?;
                }
                x = end;
            }
        }
        let palette_size = layers.len();
        let mut svg = format!(
            "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{}\" height=\"{}\" viewBox=\"0 0 {} {}\">",
            image.width(),
            image.height(),
            image.width(),
            image.height()
        );
        for (color, path) in layers {
            write!(
                svg,
                "<path fill=\"#{:02x}{:02x}{:02x}\"",
                color[0], color[1], color[2]
            )
            .map_err(|_| PpError::Vectorizer("cannot build exact SVG".to_owned()))?;
            if color[3] != 255 {
                write!(svg, " fill-opacity=\"{:.8}\"", f64::from(color[3]) / 255.0)
                    .map_err(|_| PpError::Vectorizer("cannot build exact SVG".to_owned()))?;
            }
            write!(svg, " d=\"{path}\"/>")
                .map_err(|_| PpError::Vectorizer("cannot build exact SVG".to_owned()))?;
        }
        svg.push_str("</svg>");
        Ok(BackendCandidate {
            svg: svg.into_bytes(),
            backend_id: self.id(),
            backend_version: self.version(),
            evidence: MethodEvidence {
                family: "exact-grid",
                palette_size,
                region_count: run_count,
                shared_boundary_count: 0,
                endpoint_count: 0,
                junction_count: 0,
                silhouette_fallback: false,
                adaptive: None,
            },
        })
    }
}
