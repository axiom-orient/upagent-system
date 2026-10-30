# Adaptive Color Simplification Target

Status: Promoted for the deliberately bounded compact `bounded-illustration` scope through
`stable-solid-illustration` backend v2, `perfectpixel.vector-evaluation/3`, and its digest-bound,
content-addressed authority.

Normative: Yes for the implemented bounded scope. This document also records deferred research that
is not public behavior.

Implemented: Backend v2 performs deterministic adaptive fragment simplification with bound
policy/selector/trace identities, hard work limits, independent protected-geometry checks, and
auditable selection evidence. Its compact route is promoted; evaluation reports remain evidence and
do not themselves authorize publication.

## Execution readiness

Generated corpus authority, prepared authority, foundation bootstrap, fresh family calibration, and
reuse calibration have closed for the compact route. The promoted
`stable-solid-illustration/2` route and `family-calibration/bounded-illustration/1` bundle bind the
v3 evaluation schema and report digest. No ad hoc threshold or separate bundle authorizes this
route.

## Deferred broader target

Beyond the promoted fragment scope stated below, the following target behavior is deferred research
and does not enlarge the compact route.

The product does not take a fixed color count as its default simplification objective.

The intended behavior is:

- detect paint fragments that do not represent meaningful source regions, including anti-aliasing
  mixtures and small near-color fragments;
- merge only adjacent, compatible regions when the merge preserves meaningful boundaries, alpha,
  topology, and protected features;
- stop automatically when the next merge is unsafe or causes disproportionate visual loss;
- report the selected color and region counts as outputs of the decision.

An already clean icon may remain unchanged. Another image may simplify substantially, slightly, or
not at all. No result is approved merely because it has fewer paints.

## Product boundary

The supported compact target is limited to stable opaque or simply transparent solid-region
fragments in bounded artwork:

- pixel-boundary artwork;
- nearly solid flat icons;
- bounded-complexity flat illustrations;
- bounded multi-color illustrations with stable region interiors.

Only a qualifying small fragment may merge into a compatible adjacent stable plateau. Stable
plateau-to-plateau merging, gradients, textures, photos, interior translucency, arbitrary palette
mapping, and research extensions are unsupported. Inputs outside this fragment model fail closed
rather than weakening the route.

## Separate modes

Adaptive simplification and locked-palette mapping are different operations:

| Mode | Input | Output decision |
| --- | --- | --- |
| Adaptive simplification | Image plus calibrated policy/profile | Runtime chooses how many safe merges to accept. |
| Locked palette | Explicit `allowedPalette`/`requiredPalette` | Candidate paints must satisfy an exact allowlist; current 0.3.1 behavior is validation only. |

The default adaptive route chooses its result from measured quality and structural complexity. A
future explicit palette-mapping feature may map regions to an arbitrary user palette, but it must be
kept separate from automatic simplification and receive its own versioned contract.

## Required pipeline

### 1. Paint-model eligibility

Normalize transparent RGB, work in linear light, and derive perceptual coordinates such as Oklab.
Estimate background and visible region interiors. Classify whether the source has stable solid
plateaus, anti-aliased boundaries, gradients, texture, or meaningful partial alpha.

If no stable solid-paint explanation exists, return `NO_STABLE_SOLID_PAINT_MODEL`; do not force the
image through color reduction.

### 2. Fine regions and adjacency

Create a deterministic over-segmentation of visible pixels, then build a region-adjacency graph.
Palette histogram entries alone are insufficient: two similar colors on opposite sides of a real
edge are not a merge candidate, while several anti-aliasing mixtures along one boundary may belong to
the same intended paint.

Each region records at least area, robust interior paint, alpha model, internal variance, neighbors,
shared-boundary length, edge persistence, components, holes, and protected thin-feature evidence.

### 3. Anti-alias reconstruction

Boundary mixture pixels must be distinguished from region interiors before counting effective
paints. Prefer reconstruction toward neighboring plateau paints when the local alpha/color mixing
model explains the fringe. Do not promote every anti-aliased RGB value to an SVG paint and do not use
dithering in vector preprocessing.

### 4. Merge eligibility

Only adjacent regions with compatible solid-paint and alpha models are eligible. A candidate merge is
forbidden when it would:

- cross a persistent perceptual or alpha boundary;
- alter connected-component or hole topology beyond the approved zero budget;
- erase a protected thin feature, endpoint, junction, corner, or small semantic island;
- combine solid and gradient/translucent paint models;
- make a protected source boundary unrecoverable in render-back.

Nonadjacent regions may share one SVG paint without unioning their geometry. Adjacent same-paint
regions may be geometrically unioned only when no protected boundary is required.

### 5. Merge cost and hierarchy

For every eligible adjacent pair, compute a deterministic cost with separately reported terms:

```text
mergeCost(A, B) =
    interiorVarianceIncrease(A, B)
  + boundaryPersistencePenalty(A, B)
  + alphaPenalty(A, B)
  + calibratedStructurePenalty(A, B)
  - antiAliasOrSpeckleEvidence(A, B)
```

The interior term may use an area-weighted Ward increase in Oklab/Delta E OK space. Shared-boundary
length, region area, and robust interior variance must participate explicitly; a single global color
distance threshold is not sufficient. Hard protection rules remain infinite-cost barriers, not soft
terms that another score can cancel.

Accepting the lowest safe merge creates a fine-to-coarse hierarchy. Store deterministic snapshots at
each material change in paint, region, or geometry complexity.

## Automatic stopping and selection

Color count is an observed result. Selection uses quality-constrained rate-distortion, not a fixed K:

1. Render every retained snapshot through the same bounded SVG pipeline.
2. Reject snapshots that fail alpha, topology, protected-feature, edge, perceptual-color, resource,
   or output-profile gates.
3. Remove dominated snapshots: one candidate is dominated when it is no simpler and no more faithful
   than another.
4. Among passing nondominated snapshots, choose the calibrated Pareto knee where another simplification
   produces a disproportionate rise in distortion.
5. Stop with zero merges when no safe improvement exists.

The ranking model may combine normalized distortion and structural cost, but it cannot compensate for
a failed hard gate. Structural cost must include at least effective paint count, connected region
count, paths/subpaths, actual curve segments/control points, and serialized SVG size. This prevents a
nominal paint reduction from being approved when it explodes paths or curves.

Three error layers must be measured separately:

- source raster to simplified label/paint raster: simplification loss;
- simplified raster to SVG render-back: tracing loss;
- source raster to final render-back: total user-visible loss.

This separation prevents a good aggregate render score from hiding a bad color merge or bad tracing.

## Required evidence

The analysis and evaluation schema for the target route must add versioned fields for:

- source effective paint count and selected effective paint count;
- source and selected region counts;
- accepted and rejected merge counts;
- protected-boundary count and conflict reasons;
- each retained snapshot's paint, region, path, segment/control-point, byte, and distortion metrics;
- selected snapshot identity and selection reason;
- simplification, tracing, and total error measurements;
- whether the result used zero merges.

Required target failure reasons include `NO_STABLE_SOLID_PAINT_MODEL`, `NO_SAFE_COLOR_MERGE`,
`PROTECTED_BOUNDARY_CONFLICT`, `TOPOLOGY_DRIFT`, and `ADAPTIVE_SIMPLIFICATION_GATE_FAILED`.
Names become normative only when the report schema version is approved.

## Calibration and acceptance

Thresholds and the Pareto-knee rule must be calibrated independently by content family and locked in
digest-bound bundles. The corpus must contain clean icons, anti-aliased resizes, compression/color
noise, small islands, holes, thin lines, touching near-colors, low-contrast true boundaries, alpha
edges, gradients, textures, and adversarial cases where similar colors must not merge.

Acceptance requires all of the following:

- clean-palette fixtures do not gain paints and may validly receive zero merges;
- anti-alias/color-fragment fixtures reduce unnecessary paints without losing protected structure;
- similar colors separated by a true boundary remain separate;
- output paint reduction does not cause an unbounded increase in paths, curve segments, or bytes;
- calibration and holdout results meet independently locked quality and resource gates;
- every automatic decision is deterministic and auditable from the report.

## Research basis

The target follows region-merging vectorization research, which treats vectorization as segmentation
and couples fine-to-coarse region merging with curve smoothing. It also follows anti-aliased clip-art
work showing that raster boundary mixtures can obscure intended colors and topology. Perceptual color
costs use Oklab/Delta E OK rather than raw encoded RGB distance. Quality-driven quantizers demonstrate
that a quality target can determine a variable palette size, but pixel palette quantization alone is
not sufficient because it does not protect vector-region topology.

- [A Formalization of Image Vectorization by Region Merging](https://doi.org/10.1137/24M1696469)
- [Subpixel Deblurring of Anti-Aliased Raster Clip Art](https://research.nvidia.com/publication/2023-05_subpixel-deblurring-anti-aliased-raster-clip-art)
- [Image Vectorization via Gradient Reconstruction](https://research.adobe.com/publication/image-vectorization-via-gradient-reconstruction/)
- [CSS Color 4: Delta E OK](https://www.w3.org/TR/css-color-4/#color-difference-OK)
- [libimagequant quality-controlled quantization](https://pngquant.org/lib/)

## Current promotion boundary

The promoted compact bounded-illustration route is `stable-solid-illustration` v2 with
`perfectpixel.vector-evaluation/3` and its digest-bound, content-addressed authority. It authorizes
only safe fragment reduction in the stable opaque/simple-transparent solid-region model: positive
safe reductions and conservative zero-merge results are both valid outcomes. Stable
plateau-to-plateau merging and every broader paint-model or palette-mapping extension remain
unsupported research work.
