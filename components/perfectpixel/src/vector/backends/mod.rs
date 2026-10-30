use serde::Serialize;
use std::collections::BTreeSet;
use std::fmt::Write as _;

use super::candidate;
use crate::core::{PpError, PpResult, Raster};

pub(crate) mod exact_grid;
pub(crate) mod flat_icon;
pub(crate) mod illustration;
pub(crate) mod line_art;
pub(crate) mod pixel_art;

pub(crate) const MAX_PIXELS: usize = 1_048_576;
pub(crate) const MAX_PATHS: usize = 32_768;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AdaptiveMergeEvidence {
    pub policy_identity: String,
    pub model_identity: String,
    pub selector_identity: String,
    pub trace_identity: String,
    pub model_version: String,
    pub source_paint_count: usize,
    pub selected_paint_count: usize,
    pub source_region_count: usize,
    pub selected_region_count: usize,
    pub adjacency_edge_count: usize,
    pub protected_boundary_count: usize,
    pub accepted_merges: usize,
    pub rejected_hard_merges: usize,
    pub rejected_cost_merges: usize,
    pub rejected_resource_merges: usize,
    pub retained_snapshot_count: usize,
    pub selected_snapshot_id: usize,
    pub zero_merges: bool,
    pub stop_reason: String,
    pub trace: Vec<AdaptiveMergeStepEvidence>,
    pub trace_digest: String,
    pub model_eligible: bool,
    pub trace_encoding_version: u8,
    pub fragment_pixels: usize,
    pub protected_thin_pixels: usize,
    pub minimum_plateau_coverage_percent: usize,
    pub maximum_cost_q: u64,
    pub selector_merge_credit_q: u64,
    pub maximum_source_colors: usize,
    pub maximum_fine_regions: usize,
    pub maximum_adjacency_edges: usize,
    pub maximum_merge_evaluations: usize,
    pub maximum_retained_snapshots: usize,
    pub maximum_queue_entries: usize,
    pub maximum_emitted_paths: usize,
    pub rejected_island_merges: usize,
    pub rejected_thin_merges: usize,
    pub rejected_endpoint_merges: usize,
    pub rejected_corner_merges: usize,
    pub rejected_junction_merges: usize,
    pub rejected_plateau_boundary_merges: usize,
    pub merge_evaluations: usize,
    pub queue_entries: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AdaptiveMergeStepEvidence {
    pub sequence: usize,
    pub survivor_id: usize,
    pub removed_id: usize,
    pub shared_boundary_length: usize,
    pub representative_rgba: [u8; 4],
    pub ward_cost_q: u64,
    pub boundary_cost_q: u64,
    pub alpha_cost_q: u64,
    pub structure_cost_q: u64,
    pub fragment_credit_q: u64,
    pub total_cost_q: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MethodEvidence {
    pub family: &'static str,
    pub palette_size: usize,
    pub region_count: usize,
    pub shared_boundary_count: usize,
    pub endpoint_count: usize,
    pub junction_count: usize,
    pub silhouette_fallback: bool,
    pub adaptive: Option<AdaptiveMergeEvidence>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BackendCandidate {
    pub svg: Vec<u8>,
    pub backend_id: &'static str,
    pub backend_version: &'static str,
    pub evidence: MethodEvidence,
}

pub(crate) trait VectorBackend {
    fn id(&self) -> &'static str;
    fn version(&self) -> &'static str;
    fn vectorize(&self, image: &Raster) -> PpResult<BackendCandidate>;
}
pub(crate) fn backend_source_digest(backend_id: &str, backend_version: &str) -> Option<String> {
    let (path, source) = match (backend_id, backend_version) {
        ("logical-grid-pixel-art", "1") => (
            "src/vector/backends/pixel_art.rs",
            include_bytes!("pixel_art.rs").as_slice(),
        ),
        ("perfectpixel/exact-grid/1", "1") => (
            "src/vector/backends/exact_grid.rs",
            include_bytes!("exact_grid.rs").as_slice(),
        ),
        ("bounded-flat-regions", "1") => (
            "src/vector/backends/flat_icon.rs",
            include_bytes!("flat_icon.rs").as_slice(),
        ),
        ("polarity-network-line-art", "1") => (
            "src/vector/backends/line_art.rs",
            include_bytes!("line_art.rs").as_slice(),
        ),
        ("stable-solid-illustration", "2") => (
            "src/vector/backends/illustration.rs",
            include_bytes!("illustration.rs").as_slice(),
        ),
        _ => return None,
    };
    let registry_path = b"src/vector/backends/mod.rs";
    let registry_source = include_bytes!("mod.rs");
    let mut bytes = Vec::with_capacity(
        registry_path.len() + registry_source.len() + path.len() + source.len() + 3,
    );
    bytes.extend_from_slice(registry_path);
    bytes.push(0);
    bytes.extend_from_slice(registry_source);
    bytes.push(0);
    bytes.extend_from_slice(path.as_bytes());
    bytes.push(0);
    bytes.extend_from_slice(source);
    Some(candidate::sha256_identity(&bytes))
}
/// Dispatches only a backend identity already bound by the verified route registry.
pub(crate) fn dispatch_verified(
    backend_id: &str,
    backend_version: &str,
    backend_digest: &str,
    image: &Raster,
) -> PpResult<BackendCandidate> {
    if backend_source_digest(backend_id, backend_version).as_deref() != Some(backend_digest) {
        return Err(PpError::Vectorizer(
            "verified route backend identity has no matching compiled backend".to_owned(),
        ));
    }
    match (backend_id, backend_version) {
        ("logical-grid-pixel-art", "1") => pixel_art::PixelArtBackend.vectorize(image),
        ("perfectpixel/exact-grid/1", "1") => exact_grid::ExactGridBackend.vectorize(image),
        ("bounded-flat-regions", "1") => flat_icon::FlatIconBackend.vectorize(image),
        ("polarity-network-line-art", "1") => line_art::LineArtBackend.vectorize(image),
        ("stable-solid-illustration", "2") => illustration::IllustrationBackend.vectorize(image),
        _ => Err(PpError::Vectorizer(
            "verified route references an unregistered backend".to_owned(),
        )),
    }
}

pub(crate) fn ensure_bounds(image: &Raster) -> PpResult<()> {
    let pixels = (image.width() as usize).saturating_mul(image.height() as usize);
    if image.width() == 0 || image.height() == 0 || pixels > MAX_PIXELS {
        return Err(PpError::UnsupportedVectorContent(format!(
            "backend input exceeds bounded geometry limits: {}x{} (max {} pixels)",
            image.width(),
            image.height(),
            MAX_PIXELS
        )));
    }
    Ok(())
}

pub(crate) fn color_at(image: &Raster, index: usize) -> [u8; 4] {
    let offset = index * 4;
    let pixels = image.pixels();
    let alpha = pixels[offset + 3];
    if alpha == 0 {
        [0, 0, 0, 0]
    } else {
        [
            pixels[offset],
            pixels[offset + 1],
            pixels[offset + 2],
            alpha,
        ]
    }
}

pub(crate) fn colors(
    image: &Raster,
    limit: usize,
    description: &str,
) -> PpResult<BTreeSet<[u8; 4]>> {
    let mut palette = BTreeSet::new();
    for index in 0..(image.width() as usize * image.height() as usize) {
        palette.insert(color_at(image, index));
        if palette.len() > limit {
            return Err(PpError::UnsupportedVectorContent(format!(
                "{description} palette exceeds {limit} canonical RGBA entries"
            )));
        }
    }
    Ok(palette)
}

pub(crate) fn has_partial_alpha(image: &Raster) -> bool {
    image
        .pixels()
        .as_chunks::<4>()
        .0
        .iter()
        .any(|pixel| pixel[3] != 0 && pixel[3] != 255)
}

pub(crate) fn svg_start(image: &Raster, crisp: bool) -> String {
    let rendering = if crisp {
        " shape-rendering=\"crispEdges\""
    } else {
        ""
    };
    format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{}\" height=\"{}\" viewBox=\"0 0 {} {}\"{}>",
        image.width(), image.height(), image.width(), image.height(), rendering
    )
}

pub(crate) fn append_run_path(svg: &mut String, runs: &[(u32, u32, u32)], color: [u8; 4]) {
    if color[3] == 0 || runs.is_empty() {
        return;
    }
    let _ = write!(
        svg,
        "<path fill=\"#{:02x}{:02x}{:02x}\"",
        color[0], color[1], color[2]
    );
    if color[3] != 255 {
        let _ = write!(svg, " fill-opacity=\"{}\"", alpha_decimal(color[3]));
    }
    svg.push_str(" d=\"");
    for &(x, y, width) in runs {
        let _ = write!(svg, "M{x} {y}h{width}v1H{x}Z");
    }
    svg.push_str("\"/>");
}

pub(crate) fn alpha_decimal(alpha: u8) -> String {
    format!("{:.6}", f64::from(alpha) / 255.0)
}

pub(crate) fn finish_svg(mut svg: String) -> Vec<u8> {
    svg.push_str("</svg>");
    svg.into_bytes()
}

#[cfg(test)]
mod tests {
    use super::backend_source_digest;

    #[test]
    fn removed_exact_grid_backend_identity_is_not_registered() {
        let removed_backend = concat!("perfectpixel/", "legacy", "-", "lossless", "/1");
        assert!(backend_source_digest(removed_backend, "1").is_none());
    }
}
