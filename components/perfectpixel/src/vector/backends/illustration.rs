use std::collections::{BTreeMap, BTreeSet, VecDeque};

use crate::core::{PpError, PpResult, Raster};

use super::{
    append_run_path, color_at, colors, ensure_bounds, finish_svg, has_partial_alpha, svg_start,
    AdaptiveMergeEvidence, AdaptiveMergeStepEvidence, BackendCandidate, MethodEvidence,
    VectorBackend, MAX_PATHS,
};
use crate::vector::candidate;

const MAX_SOURCE_COLORS: usize = 1_024;
const MAX_FINE_REGIONS: usize = 4_096;
const MAX_ADJACENCY_EDGES: usize = 16_384;
const MAX_MERGE_EVALUATIONS: usize = 262_144;
const MAX_RETAINED_SNAPSHOTS: usize = 2;
const MAX_QUEUE_ENTRIES: usize = 65_536;
const TRACE_ENCODING_VERSION: u8 = 1;
const MINIMUM_RAMP_PLATEAUS: usize = 4;
const MINIMUM_TEXTURE_BANDS: usize = 5;
const MAXIMUM_RAMP_STEP_DISTANCE_Q: u64 = 4_096;
type RegionRuns = BTreeMap<([u8; 4], usize), Vec<(u32, u32, u32)>>;

#[derive(Clone, Copy)]
struct AdaptivePolicy {
    identity: &'static str,
    model_identity: &'static str,
    selector_identity: &'static str,
    trace_identity: &'static str,
    fragment_pixels: usize,
    protected_thin_pixels: usize,
    minimum_plateau_coverage_percent: usize,
    maximum_cost_q: u64,
    selector_merge_credit_q: u64,
}

const POLICY: AdaptivePolicy = AdaptivePolicy {
    identity: "adaptive-fragment-merge/2",
    model_identity: "stable-solid-rgba/2",
    selector_identity: "minimum-score-with-merge-credit/1",
    trace_identity: "perfectpixel.adaptive-trace/1",
    fragment_pixels: 4,
    protected_thin_pixels: 5,
    minimum_plateau_coverage_percent: 60,
    maximum_cost_q: 100_000,
    selector_merge_credit_q: 1_000,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ProtectionReason {
    Island,
    Thin,
    Endpoint,
    Corner,
    Junction,
    PlateauBoundary,
}

impl ProtectionReason {
    const COUNT: usize = 6;
    fn index(self) -> usize {
        match self {
            Self::Island => 0,
            Self::Thin => 1,
            Self::Endpoint => 2,
            Self::Corner => 3,
            Self::Junction => 4,
            Self::PlateauBoundary => 5,
        }
    }
}

#[derive(Clone)]
struct FineRegion {
    id: usize,
    color: [u8; 4],
    pixels: Vec<usize>,
    min_x: usize,
    max_x: usize,
    min_y: usize,
    max_y: usize,
    paints: BTreeMap<[u8; 4], usize>,
    protections: BTreeSet<usize>,
}

impl FineRegion {
    fn area(&self) -> usize {
        self.pixels.len()
    }
    fn fragment(&self) -> bool {
        self.area() <= POLICY.fragment_pixels && self.protections.is_empty()
    }
    fn plateau(&self) -> bool {
        self.area() >= POLICY.protected_thin_pixels
    }
}

#[derive(Default, Clone, Copy)]
struct BoundaryStats {
    shared_length: usize,
}

#[derive(Clone)]
struct MergeCandidate {
    left: usize,
    right: usize,
    boundary: BoundaryStats,
    representative_rgba: [u8; 4],
    ward_cost_q: u64,
    boundary_cost_q: u64,
    alpha_cost_q: u64,
    structure_cost_q: u64,
    fragment_credit_q: u64,
    total_cost_q: u64,
}

#[derive(Clone)]
struct Snapshot {
    id: usize,
    labels: Vec<usize>,
    live: BTreeMap<usize, FineRegion>,
    score_q: i64,
}

struct SnapshotSelector {
    baseline: Snapshot,
    best: Option<Snapshot>,
}

impl SnapshotSelector {
    fn new(labels: &[usize], live: &BTreeMap<usize, FineRegion>) -> Self {
        Self {
            baseline: Snapshot {
                id: 0,
                labels: labels.to_vec(),
                live: live.clone(),
                score_q: 0,
            },
            best: None,
        }
    }

    fn consider(
        &mut self,
        id: usize,
        score_q: i64,
        labels: &[usize],
        live: &BTreeMap<usize, FineRegion>,
    ) {
        if (score_q, id) < (self.selected().score_q, self.selected().id) {
            self.best = Some(Snapshot {
                id,
                labels: labels.to_vec(),
                live: live.clone(),
                score_q,
            });
        }
    }

    fn selected(&self) -> &Snapshot {
        self.best.as_ref().unwrap_or(&self.baseline)
    }

    fn retained_count(&self) -> usize {
        1 + usize::from(self.best.is_some())
    }
}

struct AdaptiveMergeResult {
    trace: Vec<MergeCandidate>,
    selector: SnapshotSelector,
    protected_boundaries: usize,
    rejected_hard: usize,
    rejected_cost: usize,
    rejected_resource: usize,
    evaluations: usize,
    queue_entries: usize,
    reason_counts: [usize; ProtectionReason::COUNT],
    stop_reason: &'static str,
}

pub(crate) struct IllustrationBackend;

impl VectorBackend for IllustrationBackend {
    fn id(&self) -> &'static str {
        "stable-solid-illustration"
    }
    fn version(&self) -> &'static str {
        "2"
    }

    fn vectorize(&self, image: &Raster) -> PpResult<BackendCandidate> {
        ensure_bounds(image)?;
        if has_partial_alpha(image) {
            return Err(PpError::UnsupportedVectorContent(
                "illustration backend rejects interior translucency".into(),
            ));
        }
        let source_palette = colors(image, MAX_SOURCE_COLORS, "illustration")?;
        let (mut regions, mut labels) = build_fine_regions(image)?;
        let source_regions = regions.len();
        let source_edges = build_adjacency(image, &labels)?;
        classify_stable_solid_model(image, &regions, &source_edges)?;
        materialize_protections(image, &mut regions, &labels, &source_edges);
        let mut live: BTreeMap<usize, FineRegion> =
            regions.into_iter().map(|r| (r.id, r)).collect();
        let merge = adaptive_merge(image, &mut live, &mut labels)?;
        let selected = merge.selector.selected();
        labels = selected.labels.clone();
        live = selected.live.clone();
        let trace_evidence: Vec<_> = merge
            .trace
            .iter()
            .enumerate()
            .map(|(sequence, step)| AdaptiveMergeStepEvidence {
                sequence: sequence + 1,
                survivor_id: step.left,
                removed_id: step.right,
                shared_boundary_length: step.boundary.shared_length,
                representative_rgba: step.representative_rgba,
                ward_cost_q: step.ward_cost_q,
                boundary_cost_q: step.boundary_cost_q,
                alpha_cost_q: step.alpha_cost_q,
                structure_cost_q: step.structure_cost_q,
                fragment_credit_q: step.fragment_credit_q,
                total_cost_q: step.total_cost_q,
            })
            .collect();
        let trace_digest = candidate::sha256_identity(&canonical_trace_bytes(&trace_evidence));
        let runs = runs_for_snapshot(image, &labels, &live)?;
        let selected_paints = live
            .values()
            .map(|r| r.color)
            .collect::<BTreeSet<_>>()
            .len();
        let evidence = AdaptiveMergeEvidence {
            policy_identity: POLICY.identity.to_owned(),
            model_identity: POLICY.model_identity.to_owned(),
            selector_identity: POLICY.selector_identity.to_owned(),
            trace_identity: POLICY.trace_identity.to_owned(),
            model_version: POLICY.model_identity.to_owned(),
            model_eligible: true,
            trace_encoding_version: TRACE_ENCODING_VERSION,
            fragment_pixels: POLICY.fragment_pixels,
            protected_thin_pixels: POLICY.protected_thin_pixels,
            minimum_plateau_coverage_percent: POLICY.minimum_plateau_coverage_percent,
            maximum_cost_q: POLICY.maximum_cost_q,
            selector_merge_credit_q: POLICY.selector_merge_credit_q,
            maximum_source_colors: MAX_SOURCE_COLORS,
            maximum_fine_regions: MAX_FINE_REGIONS,
            maximum_adjacency_edges: MAX_ADJACENCY_EDGES,
            maximum_merge_evaluations: MAX_MERGE_EVALUATIONS,
            maximum_retained_snapshots: MAX_RETAINED_SNAPSHOTS,
            maximum_queue_entries: MAX_QUEUE_ENTRIES,
            maximum_emitted_paths: MAX_PATHS,
            source_paint_count: source_palette
                .len()
                .saturating_sub(usize::from(source_palette.contains(&[0, 0, 0, 0]))),
            selected_paint_count: selected_paints,
            source_region_count: source_regions,
            selected_region_count: live.len(),
            adjacency_edge_count: source_edges.len(),
            protected_boundary_count: merge.protected_boundaries,
            rejected_island_merges: merge.reason_counts[0],
            rejected_thin_merges: merge.reason_counts[1],
            rejected_endpoint_merges: merge.reason_counts[2],
            rejected_corner_merges: merge.reason_counts[3],
            rejected_junction_merges: merge.reason_counts[4],
            rejected_plateau_boundary_merges: merge.reason_counts[5],
            accepted_merges: merge.trace.len(),
            rejected_hard_merges: merge.rejected_hard,
            rejected_cost_merges: merge.rejected_cost,
            rejected_resource_merges: merge.rejected_resource,
            merge_evaluations: merge.evaluations,
            queue_entries: merge.queue_entries,
            retained_snapshot_count: merge.selector.retained_count(),
            selected_snapshot_id: selected.id,
            zero_merges: selected.id == 0,
            stop_reason: merge.stop_reason.to_owned(),
            trace: trace_evidence,
            trace_digest,
        };
        let mut svg = svg_start(image, true);
        for ((color, _), region_runs) in runs {
            append_run_path(&mut svg, &region_runs, color);
        }
        Ok(BackendCandidate {
            svg: finish_svg(svg),
            backend_id: self.id(),
            backend_version: self.version(),
            evidence: MethodEvidence {
                family: "illustration",
                palette_size: selected_paints,
                region_count: live.len(),
                shared_boundary_count: source_edges.len(),
                endpoint_count: 0,
                junction_count: 0,
                silhouette_fallback: false,
                adaptive: Some(evidence),
            },
        })
    }
}

fn build_fine_regions(image: &Raster) -> PpResult<(Vec<FineRegion>, Vec<usize>)> {
    let (width, height) = (image.width() as usize, image.height() as usize);
    let mut labels = vec![
        usize::MAX;
        width.checked_mul(height).ok_or_else(|| {
            PpError::UnsupportedVectorContent("illustration geometry overflow".into())
        })?
    ];
    let mut regions = Vec::new();
    for start in 0..labels.len() {
        let color = color_at(image, start);
        if color[3] == 0 || labels[start] != usize::MAX {
            continue;
        }
        if regions.len() == MAX_FINE_REGIONS {
            return Err(PpError::UnsupportedVectorContent(format!(
                "illustration has more than {MAX_FINE_REGIONS} fine regions"
            )));
        }
        let mut queue = VecDeque::from([start]);
        let mut pixels = Vec::new();
        labels[start] = start;
        let (mut min_x, mut max_x, mut min_y, mut max_y) = (width, 0, height, 0);
        while let Some(index) = queue.pop_front() {
            let (x, y) = (index % width, index / width);
            min_x = min_x.min(x);
            max_x = max_x.max(x);
            min_y = min_y.min(y);
            max_y = max_y.max(y);
            pixels.push(index);
            for next in neighbors(x, y, width, height) {
                if labels[next] == usize::MAX && color_at(image, next) == color {
                    labels[next] = start;
                    queue.push_back(next);
                }
            }
        }
        regions.push(FineRegion {
            id: start,
            color,
            paints: BTreeMap::from([(color, pixels.len())]),
            pixels,
            min_x,
            max_x,
            min_y,
            max_y,
            protections: BTreeSet::new(),
        });
    }
    Ok((regions, labels))
}

fn build_adjacency(
    image: &Raster,
    labels: &[usize],
) -> PpResult<BTreeMap<(usize, usize), BoundaryStats>> {
    let (width, height) = (image.width() as usize, image.height() as usize);
    let mut edges: BTreeMap<(usize, usize), BoundaryStats> = BTreeMap::new();
    for y in 0..height {
        for x in 0..width {
            let index = y * width + x;
            if labels[index] == usize::MAX {
                continue;
            }
            for next in [
                (x + 1 < width).then_some(index + 1),
                (y + 1 < height).then_some(index + width),
            ]
            .into_iter()
            .flatten()
            {
                if labels[next] == usize::MAX || labels[next] == labels[index] {
                    continue;
                }
                let pair = (
                    labels[index].min(labels[next]),
                    labels[index].max(labels[next]),
                );
                if !edges.contains_key(&pair) && edges.len() == MAX_ADJACENCY_EDGES {
                    return Err(PpError::UnsupportedVectorContent(format!(
                        "illustration has more than {MAX_ADJACENCY_EDGES} adjacency edges"
                    )));
                }
                edges.entry(pair).or_default().shared_length += 1;
            }
        }
    }
    Ok(edges)
}

fn classify_stable_solid_model(
    image: &Raster,
    regions: &[FineRegion],
    edges: &BTreeMap<(usize, usize), BoundaryStats>,
) -> PpResult<()> {
    let opaque = image
        .pixels()
        .as_chunks::<4>()
        .0
        .iter()
        .filter(|pixel| pixel[3] != 0)
        .count();
    let plateau_pixels: usize = regions
        .iter()
        .filter(|r| r.plateau())
        .map(FineRegion::area)
        .sum();
    let transition_pixels: usize = regions
        .iter()
        .filter(|r| !r.plateau())
        .map(FineRegion::area)
        .sum();
    let coverage = plateau_pixels
        .checked_mul(100)
        .and_then(|v| v.checked_div(opaque.max(1)))
        .ok_or_else(|| {
            PpError::UnsupportedVectorContent(
                "illustration stable-solid arithmetic overflow".into(),
            )
        })?;
    let shared = edges.values().map(|e| e.shared_length).sum::<usize>();
    if coverage < POLICY.minimum_plateau_coverage_percent
        || transition_pixels > opaque / 3
        || shared > opaque.saturating_mul(2)
        || contains_ordered_plateau_ramp(regions, edges)
        || contains_parallel_plateau_band_chain(regions, edges)
    {
        return Err(PpError::UnsupportedVectorContent(
            "illustration stable-solid model rejects gradients, stripes, or texture".into(),
        ));
    }
    Ok(())
}

fn contains_ordered_plateau_ramp(
    regions: &[FineRegion],
    edges: &BTreeMap<(usize, usize), BoundaryStats>,
) -> bool {
    let plateau_ids = regions
        .iter()
        .filter(|region| region.plateau())
        .map(|region| region.id)
        .collect::<BTreeSet<_>>();
    let colors = regions
        .iter()
        .map(|region| (region.id, region.color))
        .collect::<BTreeMap<_, _>>();
    let mut neighbors = BTreeMap::<usize, Vec<usize>>::new();
    let mut source_degrees = BTreeMap::<usize, usize>::new();
    for &(left, right) in edges.keys() {
        *source_degrees.entry(left).or_default() += 1;
        *source_degrees.entry(right).or_default() += 1;
        if plateau_ids.contains(&left) && plateau_ids.contains(&right) {
            neighbors.entry(left).or_default().push(right);
            neighbors.entry(right).or_default().push(left);
        }
    }
    let mut visited = BTreeSet::new();
    for &start in &plateau_ids {
        if !visited.insert(start) {
            continue;
        }
        let mut component = Vec::new();
        let mut pending = VecDeque::from([start]);
        let mut bounded_chain = true;
        while let Some(id) = pending.pop_front() {
            component.push(id);
            let adjacent = neighbors.get(&id).map(Vec::as_slice).unwrap_or(&[]);
            if source_degrees.get(&id).copied().unwrap_or(0) > 2 {
                bounded_chain = false;
            }
            for &next in adjacent {
                if visited.insert(next) {
                    pending.push_back(next);
                }
            }
        }
        if bounded_chain
            && component.len() >= MINIMUM_RAMP_PLATEAUS
            && ordered_small_step_ramp(&component, &neighbors, &colors)
        {
            return true;
        }
    }
    false
}

fn ordered_small_step_ramp(
    component: &[usize],
    neighbors: &BTreeMap<usize, Vec<usize>>,
    colors: &BTreeMap<usize, [u8; 4]>,
) -> bool {
    let start = component
        .iter()
        .copied()
        .find(|id| neighbors.get(id).map_or(0, Vec::len) <= 1);
    let Some(mut current) = start else {
        return false;
    };
    let mut previous = None;
    let mut direction = [None; 3];
    for _ in 1..component.len() {
        let Some(next) = neighbors
            .get(&current)
            .and_then(|adjacent| adjacent.iter().copied().find(|id| Some(*id) != previous))
        else {
            return false;
        };
        let left = colors[&current];
        let right = colors[&next];
        if left[3] != right[3]
            || perceptual_distance_q(left, right)
                .is_none_or(|distance| distance == 0 || distance > MAXIMUM_RAMP_STEP_DISTANCE_Q)
        {
            return false;
        }
        for channel in 0..3 {
            let step = i16::from(right[channel]) - i16::from(left[channel]);
            if step != 0 {
                let sign = step.signum();
                if direction[channel].is_some_and(|existing| existing != sign) {
                    return false;
                }
                direction[channel] = Some(sign);
            }
        }
        previous = Some(current);
        current = next;
    }
    true
}
fn contains_parallel_plateau_band_chain(
    regions: &[FineRegion],
    edges: &BTreeMap<(usize, usize), BoundaryStats>,
) -> bool {
    let plateau_ids = regions
        .iter()
        .filter(|region| region.plateau())
        .map(|region| region.id)
        .collect::<BTreeSet<_>>();
    let region_by_id = regions
        .iter()
        .map(|region| (region.id, region))
        .collect::<BTreeMap<_, _>>();
    let mut neighbors = BTreeMap::<usize, Vec<usize>>::new();
    let mut source_degrees = BTreeMap::<usize, usize>::new();
    for &(left, right) in edges.keys() {
        *source_degrees.entry(left).or_default() += 1;
        *source_degrees.entry(right).or_default() += 1;
        if plateau_ids.contains(&left) && plateau_ids.contains(&right) {
            neighbors.entry(left).or_default().push(right);
            neighbors.entry(right).or_default().push(left);
        }
    }

    let mut visited = BTreeSet::new();
    for &start in &plateau_ids {
        if !visited.insert(start) {
            continue;
        }
        let mut component = Vec::new();
        let mut pending = VecDeque::from([start]);
        let mut bounded_chain = true;
        while let Some(id) = pending.pop_front() {
            component.push(id);
            let adjacent = neighbors.get(&id).map(Vec::as_slice).unwrap_or(&[]);
            if source_degrees.get(&id).copied().unwrap_or(0) > 2
                || !(1..=2).contains(&adjacent.len())
            {
                bounded_chain = false;
            }
            for &next in adjacent {
                if visited.insert(next) {
                    pending.push_back(next);
                }
            }
        }

        let endpoints = component
            .iter()
            .filter(|id| neighbors.get(id).map_or(0, Vec::len) == 1)
            .count();
        if bounded_chain
            && component.len() >= MINIMUM_TEXTURE_BANDS
            && endpoints == 2
            && component_has_parallel_full_span_boundaries(
                &component,
                &neighbors,
                &region_by_id,
                edges,
            )
        {
            return true;
        }
    }
    false
}

fn component_has_parallel_full_span_boundaries(
    component: &[usize],
    neighbors: &BTreeMap<usize, Vec<usize>>,
    regions: &BTreeMap<usize, &FineRegion>,
    edges: &BTreeMap<(usize, usize), BoundaryStats>,
) -> bool {
    [true, false].into_iter().any(|vertical| {
        component_has_full_span_boundaries(component, neighbors, regions, edges, vertical)
    })
}

fn component_has_full_span_boundaries(
    component: &[usize],
    neighbors: &BTreeMap<usize, Vec<usize>>,
    regions: &BTreeMap<usize, &FineRegion>,
    edges: &BTreeMap<(usize, usize), BoundaryStats>,
    vertical: bool,
) -> bool {
    let first = regions[&component[0]];
    let (minimum, maximum) = if vertical {
        (first.min_y, first.max_y)
    } else {
        (first.min_x, first.max_x)
    };
    let span = maximum - minimum + 1;
    component.iter().all(|&id| {
        let region = regions[&id];
        let (region_minimum, region_maximum) = if vertical {
            (region.min_y, region.max_y)
        } else {
            (region.min_x, region.max_x)
        };
        region_minimum == minimum
            && region_maximum == maximum
            && neighbors.get(&id).is_some_and(|adjacent| {
                adjacent.iter().all(|&next| {
                    edges
                        .get(&(id.min(next), id.max(next)))
                        .is_some_and(|boundary| boundary.shared_length == span)
                })
            })
    })
}

fn materialize_protections(
    image: &Raster,
    regions: &mut [FineRegion],
    labels: &[usize],
    edges: &BTreeMap<(usize, usize), BoundaryStats>,
) {
    let width = image.width() as usize;
    let height = image.height() as usize;
    let mut degrees = BTreeMap::new();
    for &(left, right) in edges.keys() {
        *degrees.entry(left).or_insert(0usize) += 1;
        *degrees.entry(right).or_insert(0usize) += 1;
    }
    for region in regions {
        let degree = degrees.get(&region.id).copied().unwrap_or(0);
        if region.area() == 1 {
            region.protections.insert(ProtectionReason::Island.index());
        }
        let narrow = region.min_x == region.max_x || region.min_y == region.max_y;
        if narrow && region.area() <= POLICY.protected_thin_pixels {
            region.protections.insert(ProtectionReason::Thin.index());
        }
        if region.area() <= POLICY.protected_thin_pixels && degree <= 1 {
            region
                .protections
                .insert(ProtectionReason::Endpoint.index());
        }
        if region.area() <= POLICY.protected_thin_pixels
            && degree == 2
            && touches_corner(region, width, height)
        {
            region.protections.insert(ProtectionReason::Corner.index());
        }
        if region.area() <= POLICY.protected_thin_pixels && degree >= 3 {
            region
                .protections
                .insert(ProtectionReason::Junction.index());
        }
    }
    let _ = labels;
}

fn touches_corner(region: &FineRegion, width: usize, height: usize) -> bool {
    (region.min_x == 0 || region.max_x + 1 == width)
        && (region.min_y == 0 || region.max_y + 1 == height)
}

fn adaptive_merge(
    image: &Raster,
    live: &mut BTreeMap<usize, FineRegion>,
    labels: &mut [usize],
) -> PpResult<AdaptiveMergeResult> {
    let mut trace = Vec::new();
    let mut selector = SnapshotSelector::new(labels, live);
    let mut protected = 0;
    let mut rejected_hard = 0;
    let mut rejected_cost = 0;
    let mut evaluations: usize = 0;
    let mut queue_entries: usize = 0;
    let mut reason_counts = [0; ProtectionReason::COUNT];
    let stop_reason;
    loop {
        let edges = build_adjacency(image, labels)?;
        let mut queue = BTreeMap::new();
        for ((left, right), boundary) in edges {
            evaluations = evaluations.checked_add(1).ok_or_else(|| {
                PpError::UnsupportedVectorContent("illustration merge evaluation overflow".into())
            })?;
            if evaluations > MAX_MERGE_EVALUATIONS {
                return Err(PpError::UnsupportedVectorContent(
                    "illustration merge evaluations exceed bounded work".into(),
                ));
            }
            let left_region = live.get(&left).expect("live adjacency left");
            let right_region = live.get(&right).expect("live adjacency right");
            if let Some(reason) = hard_reason(left_region, right_region) {
                protected += 1;
                rejected_hard += 1;
                reason_counts[reason.index()] += 1;
                continue;
            }
            let candidate = merge_cost(left_region, right_region, boundary).ok_or_else(|| {
                PpError::UnsupportedVectorContent("illustration fixed-point cost overflow".into())
            })?;
            if candidate.total_cost_q > POLICY.maximum_cost_q {
                rejected_cost += 1;
                continue;
            }
            record_queue_entry(&mut queue_entries, MAX_QUEUE_ENTRIES)?;
            queue.insert(
                (candidate.total_cost_q, candidate.left, candidate.right),
                candidate,
            );
        }
        let Some(((_, survivor, removed), candidate)) = queue.pop_first() else {
            stop_reason = if trace.is_empty() {
                "zero-merge-baseline"
            } else {
                "no-eligible-adjacent-fragment"
            };
            break;
        };
        let mut right = live.remove(&removed).expect("candidate right live");
        let left = live.get_mut(&survivor).expect("candidate left live");
        merge_regions(left, &mut right);
        for label in labels.iter_mut() {
            if *label == removed {
                *label = survivor;
            }
        }
        trace.push(candidate);
        let score_q = trace
            .iter()
            .try_fold(0i64, |score, step| {
                let cost = i64::try_from(step.total_cost_q).ok()?;
                score.checked_add(cost.checked_sub(POLICY.selector_merge_credit_q as i64)?)
            })
            .ok_or_else(|| {
                PpError::UnsupportedVectorContent("illustration selector score overflow".into())
            })?;
        selector.consider(trace.len(), score_q, labels, live);
        if trace.len() >= MAX_FINE_REGIONS - 1 {
            stop_reason = "merge-limit";
            break;
        }
    }
    Ok(AdaptiveMergeResult {
        trace,
        selector,
        protected_boundaries: protected,
        rejected_hard,
        rejected_cost,
        rejected_resource: 0,
        evaluations,
        queue_entries,
        reason_counts,
        stop_reason,
    })
}

fn record_queue_entry(entries: &mut usize, maximum: usize) -> PpResult<()> {
    if *entries >= maximum {
        return Err(PpError::UnsupportedVectorContent(
            "illustration merge queue exceeds bounded entries".into(),
        ));
    }
    *entries = entries.checked_add(1).ok_or_else(|| {
        PpError::UnsupportedVectorContent("illustration queue accounting overflow".into())
    })?;
    Ok(())
}

fn hard_reason(left: &FineRegion, right: &FineRegion) -> Option<ProtectionReason> {
    for reason in [
        ProtectionReason::Island,
        ProtectionReason::Thin,
        ProtectionReason::Endpoint,
        ProtectionReason::Corner,
        ProtectionReason::Junction,
    ] {
        if left.protections.contains(&reason.index()) || right.protections.contains(&reason.index())
        {
            return Some(reason);
        }
    }
    if (left.plateau() && right.plateau()) || (!left.fragment() && !right.fragment()) {
        Some(ProtectionReason::PlateauBoundary)
    } else {
        None
    }
}

fn merge_cost(
    left: &FineRegion,
    right: &FineRegion,
    boundary: BoundaryStats,
) -> Option<MergeCandidate> {
    let distance_q = perceptual_distance_q(left.color, right.color)?;
    let left_area = u64::try_from(left.area()).ok()?;
    let right_area = u64::try_from(right.area()).ok()?;
    let ward_cost_q = distance_q
        .checked_mul(left_area)?
        .checked_mul(right_area)?
        .checked_div(left_area.checked_add(right_area)?)?;
    let boundary_cost_q = 1_000u64.checked_div(u64::try_from(boundary.shared_length).ok()?)?;
    let alpha_cost_q = u64::from(left.color[3].abs_diff(right.color[3])).checked_mul(1_000)?;
    let structure_cost_q = 100u64.checked_div(u64::try_from(boundary.shared_length).ok()?)?;
    let fragment_credit_q = 400;
    let total_cost_q = ward_cost_q
        .checked_add(boundary_cost_q)?
        .checked_add(alpha_cost_q)?
        .checked_add(structure_cost_q)?
        .saturating_sub(fragment_credit_q);
    Some(MergeCandidate {
        left: left.id.min(right.id),
        right: left.id.max(right.id),
        boundary,
        representative_rgba: representative_color(left, right)?,
        ward_cost_q,
        boundary_cost_q,
        alpha_cost_q,
        structure_cost_q,
        fragment_credit_q,
        total_cost_q,
    })
}

fn perceptual_distance_q(left: [u8; 4], right: [u8; 4]) -> Option<u64> {
    let weights = [299u64, 587, 114];
    let mut sum = 0u64;
    for channel in 0..3 {
        let delta = u64::from(left[channel].abs_diff(right[channel]));
        sum = sum.checked_add(weights[channel].checked_mul(delta.checked_mul(delta)?)?)?;
    }
    sum.checked_div(1_000)
}

fn merge_regions(left: &mut FineRegion, right: &mut FineRegion) {
    let representative = representative_color(left, right).expect("bounded existing paint medoid");
    for (paint, area) in &right.paints {
        *left.paints.entry(*paint).or_insert(0) += area;
    }
    left.pixels.append(&mut right.pixels);
    left.min_x = left.min_x.min(right.min_x);
    left.max_x = left.max_x.max(right.max_x);
    left.min_y = left.min_y.min(right.min_y);
    left.max_y = left.max_y.max(right.max_y);
    left.color = representative;
}

fn representative_color(left: &FineRegion, right: &FineRegion) -> Option<[u8; 4]> {
    let mut paints = left.paints.clone();
    for (paint, area) in &right.paints {
        *paints.entry(*paint).or_insert(0) += area;
    }
    paints
        .keys()
        .copied()
        .min_by_key(|paint| {
            paints.iter().try_fold(0u64, |sum, (other, area)| {
                perceptual_distance_q(*paint, *other)?
                    .checked_mul(u64::try_from(*area).ok()?)
                    .and_then(|value| sum.checked_add(value))
            })
        })
        .or(Some(left.color))
}

fn canonical_trace_bytes(trace: &[AdaptiveMergeStepEvidence]) -> Vec<u8> {
    let mut bytes = b"perfectpixel.adaptive-trace/1\0".to_vec();
    bytes.push(TRACE_ENCODING_VERSION);
    bytes.extend_from_slice(&(trace.len() as u64).to_be_bytes());
    for step in trace {
        for value in [
            step.sequence as u64,
            step.survivor_id as u64,
            step.removed_id as u64,
            step.shared_boundary_length as u64,
            step.ward_cost_q,
            step.boundary_cost_q,
            step.alpha_cost_q,
            step.structure_cost_q,
            step.fragment_credit_q,
            step.total_cost_q,
        ] {
            bytes.extend_from_slice(&value.to_be_bytes());
        }
        bytes.extend_from_slice(&step.representative_rgba);
    }
    bytes
}

fn runs_for_snapshot(
    image: &Raster,
    labels: &[usize],
    live: &BTreeMap<usize, FineRegion>,
) -> PpResult<RegionRuns> {
    let (width, height) = (image.width() as usize, image.height() as usize);
    let mut runs = BTreeMap::new();
    let mut emitted = 0;
    for y in 0..height {
        let mut x = 0;
        while x < width {
            let index = y * width + x;
            if labels[index] == usize::MAX {
                x += 1;
                continue;
            }
            let id = labels[index];
            let color = live.get(&id).expect("live label").color;
            let start = x;
            x += 1;
            while x < width && labels[y * width + x] == id {
                x += 1;
            }
            emitted += 1;
            if emitted > MAX_PATHS {
                return Err(PpError::UnsupportedVectorContent(format!(
                    "illustration has more than {MAX_PATHS} emitted path runs"
                )));
            }
            runs.entry((color, id)).or_insert_with(Vec::new).push((
                start as u32,
                y as u32,
                (x - start) as u32,
            ));
        }
    }
    Ok(runs)
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

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn canonical_trace_is_stable() {
        let trace = vec![AdaptiveMergeStepEvidence {
            sequence: 1,
            survivor_id: 0,
            removed_id: 1,
            shared_boundary_length: 2,
            representative_rgba: [1, 2, 3, 255],
            ward_cost_q: 4,
            boundary_cost_q: 5,
            alpha_cost_q: 0,
            structure_cost_q: 6,
            fragment_credit_q: 7,
            total_cost_q: 8,
        }];
        assert_eq!(
            candidate::sha256_identity(&canonical_trace_bytes(&trace)),
            candidate::sha256_identity(&canonical_trace_bytes(&trace))
        );
    }
    #[test]
    fn medoid_is_existing_weighted_paint() {
        let a = FineRegion {
            id: 0,
            color: [0, 0, 0, 255],
            pixels: vec![0],
            min_x: 0,
            max_x: 0,
            min_y: 0,
            max_y: 0,
            paints: BTreeMap::from([([0, 0, 0, 255], 1), ([10, 10, 10, 255], 4)]),
            protections: BTreeSet::new(),
        };
        let b = FineRegion {
            id: 1,
            color: [20, 20, 20, 255],
            pixels: vec![1],
            min_x: 1,
            max_x: 1,
            min_y: 0,
            max_y: 0,
            paints: BTreeMap::from([([20, 20, 20, 255], 1)]),
            protections: BTreeSet::new(),
        };
        assert_eq!(representative_color(&a, &b), Some([10, 10, 10, 255]));
    }
    #[test]
    fn protected_reasons_reject_before_costing() {
        let protected = FineRegion {
            id: 0,
            color: [1, 1, 1, 255],
            pixels: vec![0],
            min_x: 0,
            max_x: 0,
            min_y: 0,
            max_y: 0,
            paints: BTreeMap::from([([1, 1, 1, 255], 1)]),
            protections: BTreeSet::from([ProtectionReason::Island.index()]),
        };
        let adjacent = FineRegion {
            id: 1,
            color: [2, 2, 2, 255],
            pixels: vec![1, 2],
            min_x: 1,
            max_x: 2,
            min_y: 0,
            max_y: 0,
            paints: BTreeMap::from([([2, 2, 2, 255], 2)]),
            protections: BTreeSet::new(),
        };
        assert_eq!(
            hard_reason(&protected, &adjacent),
            Some(ProtectionReason::Island)
        );
    }
    #[test]
    fn selector_can_restore_snapshot_zero() {
        let labels = vec![0];
        let live = BTreeMap::new();
        let mut selector = SnapshotSelector::new(&labels, &live);
        selector.consider(1, 1, &[1], &live);
        assert_eq!(selector.selected().id, 0);
        assert_eq!(selector.selected().labels, labels);
        assert_eq!(selector.retained_count(), 1);
    }

    #[test]
    fn selector_retains_early_minimum_with_constant_full_state_count() {
        let live = BTreeMap::new();
        let mut selector = SnapshotSelector::new(&[0], &live);
        selector.consider(1, -10, &[1], &live);
        selector.consider(2, -10, &[2], &live);
        for id in 3..=MAX_RETAINED_SNAPSHOTS * 512 {
            selector.consider(id, -9, &[id], &live);
        }

        assert_eq!(selector.selected().id, 1);
        assert_eq!(selector.selected().score_q, -10);
        assert_eq!(selector.selected().labels, vec![1]);
        assert_eq!(selector.retained_count(), 2);
        assert_eq!(selector.retained_count(), MAX_RETAINED_SNAPSHOTS);
    }
    fn stable_solid_classification(colors: &[[u8; 4]]) -> PpResult<()> {
        let mut pixels = Vec::new();
        for color in colors {
            for _ in 0..POLICY.protected_thin_pixels {
                pixels.extend_from_slice(color);
            }
        }
        let image = Raster::new(
            (colors.len() * POLICY.protected_thin_pixels) as u32,
            1,
            pixels,
        )?;
        let (regions, labels) = build_fine_regions(&image)?;
        classify_stable_solid_model(&image, &regions, &build_adjacency(&image, &labels)?)
    }

    #[test]
    fn stable_solid_rejects_smooth_and_striped_ramps() {
        assert!(stable_solid_classification(&[
            [20, 20, 20, 255],
            [30, 30, 30, 255],
            [40, 40, 40, 255],
            [50, 50, 50, 255],
        ])
        .is_err());
        assert!(stable_solid_classification(&[
            [20, 40, 80, 255],
            [30, 50, 90, 255],
            [40, 60, 100, 255],
            [50, 70, 110, 255],
        ])
        .is_err());
    }
    #[test]
    fn stable_solid_rejects_alternating_plateau_stripes() {
        assert!(stable_solid_classification(&[
            [0, 0, 0, 255],
            [255, 255, 255, 255],
            [0, 0, 0, 255],
            [255, 255, 255, 255],
            [0, 0, 0, 255],
        ])
        .is_err());
    }

    #[test]
    fn stable_solid_rejects_high_contrast_plateau_stripes() {
        assert!(stable_solid_classification(&[
            [0, 0, 0, 255],
            [255, 0, 255, 255],
            [0, 255, 0, 255],
            [255, 255, 0, 255],
            [0, 255, 255, 255],
        ])
        .is_err());
    }

    #[test]
    fn stable_solid_accepts_bounded_multi_paint_plateaus() -> PpResult<()> {
        stable_solid_classification(&[
            [255, 0, 0, 255],
            [0, 255, 0, 255],
            [0, 0, 255, 255],
            [255, 255, 0, 255],
        ])
    }

    #[test]
    fn queue_entry_limit_is_cumulative_and_exact() -> PpResult<()> {
        let mut entries = 0;
        record_queue_entry(&mut entries, 2)?;
        record_queue_entry(&mut entries, 2)?;
        assert_eq!(entries, 2);
        assert!(record_queue_entry(&mut entries, 2).is_err());
        assert_eq!(entries, 2);
        Ok(())
    }
}
