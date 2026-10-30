# Vector Quality Contract

Status: Current normative quality and route-activation contract for the 0.3.1 vector runtime.

Normative: Yes — quality and gate rules grounded in the approved Vector/SVG Transition Roadmap.

Implemented: Yes. These gates protect every publishing route.

## Approval model

Every raster-to-SVG publication through `vector`, including output produced by the frozen exact-grid
backend, requires candidate-bound evaluation through the universal policy envelope. Machine
hard gates and a compatible approved calibrated bundle run before visual review; visual review cannot
override a failure. An approval is about exact candidate bytes, not a serializable report.

A publishing route is eligible only when its embedded route entry, private backend source-closure digest, foundation/report digests, requested profile predicate, and approved threshold-bundle digest form a verified closure. No route may lower a gate. Domain unavailability or an incompatible/withdrawn route yields `UNSUPPORTED`; invalid embedded authority prevents `Vectorizer` construction and is never converted into a domain fallback.

## Fixed and protected gates

The following are universal zero-waiver gates:

- **Alpha:** preserve required alpha behavior; coverage-AA and interior translucency failures reject.
- **Topology:** preserve components, holes, bridges, and protected topology; no topology loss budget exists.
- **Protected geometry:** preserve protected edges, features, endpoints, junctions, polarity/background constraints, and required shared boundaries.
- **Palette:** preserve locked palette feasibility; infeasibility rejects instead of dropping protected color constraints.
- **Representation:** no raster payload; output must be bounded, parseable/renderable SVG under the shared neutral SVG IR limits.
- **Safety/resources:** at most 16,777,216 input pixels, 65,536 canonical source RGBA colors (all fully transparent pixels canonicalize to one color), 1,048,576 SVG bytes, and 64 publishing paths; shared SVG parsing additionally caps depth at 64, elements at 16,384, attributes at 65,536, tokens and coordinates at 262,144, path segments at 131,072, canvas extents at 8,192, and coordinate/transform magnitude at 1,000,000. Request limits only tighten applicable bounds.

Quality scores, render-back measures, path counts, or detail selection never compensate for a protected failure.

## Calibrated and route-specific gates

Calibrated thresholds come only from embedded, approved, digest-bound bundles. `THRESH-BOOT-001` supplies a non-weakened foundation-baseline bundle before family promotion; it is distinct from family bundles. Later family backends require their own corpus-calibrated bundle. A bundle requires compatible policy/profile/foundation versions, controlled defects, calibration, untouched disjoint holdout, and machine-pass-only review before entry into canonical authority.

Route-specific quality predicates may add restrictions but cannot replace fixed/protected/resource gates. `minimum_quality` uses the higher of approved/requested values; `maximum_quality_loss` uses the lower; applicable `maximum_paths` uses the lowest fixed, approved-route, and requested limit. Detail levels `1..=5` select among bounded backend candidates and never alter or bypass any gate.

## Family and profile constraints

Families use these implemented methods under the same gates:

- `pixel-art`: logical-grid orthogonal regions.
- `flat-icon`: connected-color analysis emitted as deterministic filled horizontal runs with exact boundary evidence.
- `line-art`: two-paint polarity analysis emitted as bounded filled horizontal runs with endpoint/junction evidence.
- `bounded-illustration`: promoted `stable-solid-illustration` v2 safe fragment reduction for stable opaque or simply transparent solid-region content, with adaptive policy/selector/trace/work-limit closure and independent protected-geometry checks.
- `exact-grid`: private frozen exact emitter family; its bytes remain frozen but its approval is not grandfathered.

For bounded illustration, positive safe reductions and conservative zero-merge results are valid.
Stable plateau-to-plateau merging, gradients, texture, photos, interior translucency, arbitrary
palette mapping, and research extensions are unsupported and fail closed. Other continuous-tone
content cannot publish without a new versioned backend and contract. Auto classification with low
confidence or conflicts is `PRESET_REQUIRED`; explicit presets select only a family and still pass
all gates.

`compact` is the default output profile and is owned by vector quality. `motion-structure-ready` is an explicit, independent predicate. It requires motion-owned, hash-bound scaffold assessment and approval of exactly the scene bytes; unrequested or unavailable motion is `notEvaluated`, not a compact failure or a silent motion run. Semantic motion, hints, and starter content are excluded.

## Activation and rollback

Family × output-profile route keys activate independently only after the foundation, corpus, and approved bundle requirements are met. `candidateShadow` never publishes. `foundationActive` and `promoted` publish only on a complete gate pass. Withdrawal, invalid predecessor, or failed proof makes the affected key unsupported.

Route rollback changes the intended key to an approved predecessor backend/bundle while preserving non-target keys, public API/CLI, frozen exact-grid goldens, and all universal gates. It cannot restore removed commands, removed Rust types, weaker approval, or a public compatibility surface.
