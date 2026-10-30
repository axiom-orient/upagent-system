# Vector Profile and Scope Contract

Status: Current normative profile and support contract for the 0.3.1 vector runtime.

Normative: Yes — profile evidence, uncertainty, preset selection, and route availability.

Implemented: Yes. `Vectorizer::analyze` and `Vectorizer::run` use this factorized profile model.

## Purpose

Profiling describes raster evidence; it does not authorize publication. The same factorized analysis feeds generation and `perfectpixel vector-analyze`, while embedded route and threshold registries remain the only route authority.

## Factorized evidence

`ProfileEvidence` reports five independent groups:

| Group | Implemented signals | Use |
| --- | --- | --- |
| Geometry | hard-grid likelihood, horizontal-run ratio, strong-edge ratio | Distinguish grid, region, and line structure. |
| Paint | unique-color count, palette likelihood | Detect bounded palettes versus continuous paint. |
| Alpha | transparent-pixel ratio, partial-alpha ratio | Expose transparency without treating it as an approval waiver. |
| Complexity | pixel count, estimated region complexity | Bound the likely geometry and resource cost. |
| Source noise | isolated-pixel ratio, source-noise likelihood | Detect texture/noise that makes stable vector geometry unsafe. |

The report also records confidence, conflicts, abstention, and expected support. `recommendedPresets`
contains one deterministic explicit-family recommendation when profiling classifies supported input;
it is empty when profiling abstains or marks the input unsupported. Route availability and
deterministic `routeReasons` describe whether the selected route can publish. None of these fields
can be converted into `ApprovedVectorOutput`.

## Automatic selection

`auto` has three outcomes:

- a confident, non-conflicting supported classification selects one family;
- a low-confidence or conflicting classification returns `PRESET_REQUIRED` for generation;
- continuous-tone or otherwise unsupported content returns `UNSUPPORTED`.

A successful analysis remains exit-neutral. It exposes abstention/support evidence plus route availability and reasons; generation maps those facts to `PRESET_REQUIRED` or `UNSUPPORTED` when appropriate. Analysis never creates candidate bytes, diagnostics, a render-back, or an approval capability.

## Explicit presets

The exact preset vocabulary is:

- `pixel-art`
- `exact-grid`
- `flat-icon`
- `line-art`
- `bounded-illustration`

An explicit preset selects a family only. It does not bypass unsupported-content detection, fixed gates, protected-feature gates, calibrated quality thresholds, resource limits, policy feasibility, or the selected output-profile predicate.

## Supported scope

| Family | Compact behavior | Current route state |
| --- | --- | --- |
| Pixel art | Logical-grid orthogonal regions with exact RGBA paints. | `promoted` |
| Exact grid | Frozen exact raster-cell geometry under the universal gate envelope. | `foundationActive` |
| Flat icon | Bounded connected-color analysis emitted as deterministic filled horizontal runs, with exact boundary evidence. | `promoted` |
| Line art | Two-paint polarity analysis emitted as bounded filled horizontal runs, with endpoint/junction evidence used by protected gates. | `promoted` |
| Bounded illustration | Stable solid-region tracing for bounded palettes and complexity. | `promoted` |

Photos, continuous tone, texture, complex gradients, unstable high-complexity regions, and detected interior translucency are non-publishing. Supporting them requires a separately versioned backend, calibration bundle, and contract.

### Color-simplification boundary

The promoted compact `bounded-illustration` backend performs deterministic, bounded fragment
merging through `stable-solid-illustration` v2. Protected boundaries, quality gates, and resource
limits constrain the merges, and the selected result may contain zero merges. This does not provide
unrestricted similar-color or plateau-to-plateau merging, gradients, or continuous paint support.
The implemented subset, stopping rule, and promotion evidence are defined in
[ADAPTIVE_COLOR_SIMPLIFICATION.md](ADAPTIVE_COLOR_SIMPLIFICATION.md).

## Output profiles

`compact` maps to `perfectpixel.svg-editable/1` and is the default publishing profile. `motion-structure-ready` maps to `perfectpixel.svg-motion-structure/1`, is explicit-only, and is currently `candidateShadow` for every family. Compact evaluation does not run motion assessment.

Motion assessment is tri-state, hash-bound to the assessed scene bytes, non-semantic, and owned by `docs/MOTION_CONTRACT.md`. An unpromoted motion route returns `PROFILE_NOT_PROMOTED`; it cannot fall back to compact publication.
## Matrix boundary cases

The [behavior matrix](README.md#behavior-matrix) distinguishes evaluation JSON from non-JSON API/CLI guarantees. Analysis reports abstention and route reasons rather than generation rejection codes. Generation maps an abstained automatic selection to `PRESET_REQUIRED`; unsupported content and an unpromoted motion route become `UNSUPPORTED` or `PROFILE_NOT_PROMOTED`. None is a fallback selection or approval.

For `motion-structure-ready`, analysis may describe route availability, but it does not create a
motion scaffold or make compact output motion-assessed. Until a motion route is promoted, this
profile remains non-publishing for every family. Artifact intent and transaction ownership remain
defined by [REPORT_SCHEMA.md](REPORT_SCHEMA.md#report-ownership-versus-cli-transaction-ownership),
not by profile evidence.

## Policy and authority boundary

`perfectpixel.vector-policy/1` may constrain palette handling and may only tighten or select within the request contract. It cannot carry backend identities, route entries, thresholds, registry digests, foundation identities, or waivers. See [POLICY_CONTRACT.md](POLICY_CONTRACT.md), [QUALITY_CONTRACT.md](QUALITY_CONTRACT.md), and [README.md](README.md).
