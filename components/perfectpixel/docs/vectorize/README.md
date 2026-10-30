# Vector/SVG Authority

Status: Current authority guide for the 0.3.1 vector runtime.

Normative: Yes — lifecycle and authority vocabulary grounded in `docs/CONTRACT.md`.

Implemented: Yes. The breaking API/CLI transition and compact route promotions are active.

> **Status: current authority, 0.3.1.** This document describes the implemented vector transition. The normative public contract is [../CONTRACT.md](../CONTRACT.md); this document records its authority vocabulary and lifecycle.

## Public surface

`Vectorizer` is the public Rust capability API. `Vectorizer::new()` verifies embedded authority;
`run(&Raster, &VectorRequest)` returns `VectorOutcome::{Approved, Rejected}`; and
`analyze(&Raster, &VectorAnalysisRequest)` returns deterministic candidate-free evidence.

`perfectpixel vector` is the sole raster-to-SVG quality-gated publication command.
`perfectpixel vector-analyze` is analysis-only: it cannot generate SVG, diagnostics, or a publication
transaction. `motion-build` may publish derived `animated.svg` from an accepted raster-free SVG
source; it is not raster-to-SVG generation. There are no old command or API aliases, redirects,
deprecated wrappers, compatibility modules, or feature flags.

Related contracts:

- [PROFILE_AND_SCOPE.md](PROFILE_AND_SCOPE.md) — factorized evidence, uncertainty, support boundaries, and output-profile scope.
- [POLICY_CONTRACT.md](POLICY_CONTRACT.md) — validated selection/tightening policy.
- [ADAPTIVE_COLOR_SIMPLIFICATION.md](ADAPTIVE_COLOR_SIMPLIFICATION.md) — promoted bounded
  topology-aware fragment simplification for compact bounded illustrations; broader paint models remain unsupported.
- [QUALITY_CONTRACT.md](QUALITY_CONTRACT.md) — universal fixed, protected, calibrated, resource, and profile gates.
- [REPORT_SCHEMA.md](REPORT_SCHEMA.md) — immutable analysis/evaluation evidence and transaction truth.
- [CALIBRATION.md](CALIBRATION.md) — corpus, bootstrap, family bundles, and route promotion.
- [`examples/`](examples/) — validated policy and request examples.

## Authority and versions

The embedded, content-addressed authority is immutable at runtime and is the sole route and
publication authority. Caller input, reports, diagnostics, filesystem paths, environment values,
and historical plans cannot approve a route or waive a gate. The runtime generation report is
`perfectpixel.vector-evaluation/3`, with report-schema digest
`sha256-962c423b7aa6e6de99010847b38943e3c0c9c9683e84d53ec264d9bb7152bcef`. The promoted compact
bounded-illustration route binds that schema and digest to `stable-solid-illustration` backend v2;
reports are evidence, never authority.

Output profile keys are:

| CLI profile | Route key profile | Role |
| --- | --- | --- |
| `compact` | `perfectpixel.svg-editable/1` | Editable SVG publication profile. |
| `motion-structure-ready` | `perfectpixel.svg-motion-structure/1` | Motion-structure analysis profile; currently non-publishing. |

A route is content-addressed and binds family, profile, foundation, threshold bundle, corpus/bootstrap/family evidence, and predecessor state. The foundation thresholds are universal and cannot be weakened. A policy may constrain paint handling; it cannot provide routes, thresholds, digests, backend identity, or waivers.

## Route lifecycle

| Family | Current v3 compact publication state | Motion-structure route |
| --- | --- | --- |
| `pixel-art` | promoted | `candidateShadow` |
| `exact-grid` | active | `candidateShadow` |
| `flat-icon` | promoted | `candidateShadow` |
| `line-art` | promoted | `candidateShadow` |
| `bounded-illustration` | promoted through `stable-solid-illustration` v2 for bounded safe fragment reduction | `candidateShadow` |

Compact routes publish only through their verified content-addressed authority. Generated authority
binds the v3 schema/report digest, corpus and bootstrap closure, family calibration, and promotion
evidence; reports remain evidence rather than publication capabilities.

## Evaluation and publication

All candidate families emit through shared bounded SVG IR. Raster embedding and raster image payloads
are forbidden. Evaluation evidence and diagnostic artifacts are immutable; they are not publication
authority. CLI transaction truth is authoritative: only an `Approved` outcome atomically writes the
requested SVG. A rejected result writes no final SVG; when diagnostics were explicitly requested, it may still expose non-publishable candidate and render-back evidence.

Unsupported continuous-tone content—including photos, textures, complex gradients, and interior
translucency—is diagnostic/non-publishing. Unpromoted motion routes, ambiguous profiles, and all
unsupported content likewise fail closed for publication. An explicit preset can select a family but
cannot waive a gate.
## Behavior matrix

This matrix covers `Vectorizer`, `perfectpixel vector`, and `perfectpixel vector-analyze`.

| Required case | Current behavior | Contract or verification |
| --- | --- | --- |
| Approved generation | A compact request publishes only from `ApprovedVectorOutput` after its evaluation passes. | [CLI transaction tests](../../tests/vector_cli_contract.rs) |
| Rejected generation | A rejected evaluation never exposes publishable output or writes a final SVG; explicit diagnostics remain evidence only. | [Rejected transaction tests](../../tests/vector_cli_removed_commands.rs) |
| Preset-required analysis | `auto` may abstain; generation then requires an explicit preset, while analysis remains evidence-only and exit-neutral. | Non-JSON behavior: [PROFILE_AND_SCOPE.md](PROFILE_AND_SCOPE.md#automatic-selection) |
| Unsupported route | Unsupported content and unpromoted routes fail closed; neither is a compact publication fallback. | Non-JSON behavior: [PROFILE_AND_SCOPE.md](PROFILE_AND_SCOPE.md#supported-scope) |
| Policy tightening/non-weakening | A policy can select or tighten allowed request constraints, never supply authority or waive a gate. | [Policy authority boundary](POLICY_CONTRACT.md) |
| Outcome ownership/non-publication | Evaluation evidence does not own transaction state; only the CLI transaction can commit the final SVG. | Non-JSON behavior: [Report ownership](REPORT_SCHEMA.md#report-ownership-versus-cli-transaction-ownership) |
| Artifact intent/diagnostics | Reports record requested diagnostic artifact intent, not artifact commit or SVG publication. | [Evaluation report contract](REPORT_SCHEMA.md#schema-identities-and-immutability) |
| Motion-structure limitations | Motion-structure-ready is `candidateShadow`; compact does not assess motion. | Non-JSON behavior: [Output profiles](PROFILE_AND_SCOPE.md#output-profiles) |
| Adaptive similar-color simplification | The promoted illustration v2 compact route performs bounded safe fragment reduction, including valid conservative zero-merge outcomes; stable plateau-to-plateau merging and broader paint models remain unsupported. | [Adaptive scope](ADAPTIVE_COLOR_SIMPLIFICATION.md#current-promotion-boundary) |

Transaction claims are covered by the cited CLI tests. Rows marked **Non-JSON behavior** describe a
fail-closed command/API boundary rather than a stored output example. Reports are generated from
the actual input and embedded authority; their digest-bearing fields must not be hand-edited.
