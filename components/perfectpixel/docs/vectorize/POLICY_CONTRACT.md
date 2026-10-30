# Vector Policy Contract

Status: Current normative policy contract for the 0.3.1 vector runtime.

Normative: Yes — `perfectpixel.vector-policy/1`, grounded in the approved Vector/SVG Transition Roadmap.

Implemented: Yes. The validated policy and embedded authority are active.

## Document identity and authority

A policy document has schema identifier `perfectpixel.vector-policy/1`. It is a selection/tightening input, not an authority source. Canonical route and threshold registries are embedded, digest-verified authority; callers, policy files, paths, and environment variables cannot supply or override thresholds, routes, registry entries, backend identities, or authority digests.

The generation CLI may load exactly one bounded `.json` policy document through `--policy`; analysis may load the same document type. The library accepts `VectorPolicy` through validated constructors. Required fields are `schema`, nonblank caller `version`, `allowedPalette`, `requiredPalette`, `rejectUnmapped`, and `allowDropNoise`; unknown or missing fields reject. Policy documents reject CLI-owned preset, profile, numeric request fields, and every threshold/route authority field. The same normalizer is mandatory for library generation, CLI generation, and analysis normalization; parser-local gate arithmetic is prohibited.

## Universal no-waiver rule

For every publishing route, including `foundationActive`, approval is:

```text
approved output = candidate-bound private evaluation
  AND fixed gates
  AND protected gates
  AND resource gates
  AND requested output-profile predicates
  AND approved digest-bound calibrated threshold bundle
  AND selected promoted/foundation route
```

No request, explicit preset, policy document, report, diagnostic, human review, exact-grid backend, or route rollback may waive any conjunct. An infeasible request rejects. The exact-grid backend preserves only its deterministic generator/configuration/candidate bytes; an evaluated gate failure uses its specific `*_GATE_FAILED` code, domain unavailability uses `UNSUPPORTED`, and invalid embedded authority is an operational construction error. Human review is evidence only and has no override.

## Request controls

| Control | Class | Effective rule |
| --- | --- | --- |
| `preset` | route selection only | Selects family; never waives a gate. `auto` with low confidence or conflicts is `PRESET_REQUIRED`. |
| `profile` | predicate selection only | `compact` is default; `motion-structure-ready` is explicit opt-in. Common gates remain unchanged. |
| `detail` | candidate selection only | Selects a bounded backend candidate; it never changes path, quality, loss, protected, resource, or profile authority. |
| `minimum_quality` | tighten only | `max(approved, requested)`; a relaxation attempt is recorded. |
| `maximum_quality_loss` | tighten only | `min(approved, requested)`. |
| `maximum_paths` | tighten only | `min(fixedSafety, approvedRoute, requested)` and never replaces motion gates. |
| policy constraints | selection/tighten only | Infeasibility rejects; protected content cannot be dropped. |

Generation reports record candidate selection plus requested, approved, effective, applicability, measurement, and attempted-relaxation evidence. Analysis records candidate-free normalized profile/policy, recommendations, route availability, and route reasons; it has no approval authority.

### Locked palette document fields

`allowedPalette` and `requiredPalette` contain canonical six-digit RGB paints in `#RRGGBB`
form; comparison is ASCII case-insensitive and serialized policy identity binds the complete
validated document. Duplicate or malformed colors reject. When `allowedPalette` is non-empty,
every effective candidate paint must belong to it. Every `requiredPalette` paint must appear, and
must also be allowed when an allowlist exists.

`rejectUnmapped: true` additionally requires every non-transparent source RGB paint to remain
represented in the candidate. `allowDropNoise` is not a waiver: because the runtime has no
approved protected-noise classifier, setting it to `true` fails the locked-palette gate rather than
silently dropping source paint or protected structure. The default policy has no palette
restriction, does not request unmapped-source rejection, and does not allow noise dropping.

### Adaptive simplification boundary

`allowedPalette` is an exact allowlist of arbitrary valid length; its length is not a requested or
maximum output color count. `requiredPalette` states paints that must occur. Neither field asks the
runtime to discover similar colors, assign a source region to its nearest allowed paint, or merge
region geometry.

The locked-palette gate validates a candidate produced elsewhere; it does not transform the
candidate to make the policy feasible. The promoted compact `bounded-illustration` route separately
performs bounded, topology-preserving fragment reduction. It may merge a qualifying small fragment
into a compatible adjacent stable plateau; it does not merge stable plateaus, map arbitrary source
regions to an allowlist, or support broader paint models. The implemented fragment scope and deferred
extensions are specified in [ADAPTIVE_COLOR_SIMPLIFICATION.md](ADAPTIVE_COLOR_SIMPLIFICATION.md).

## Gate classes

**Fixed gates** are zero-waiver safety invariants: bounded parsing/resources, no raster payload, required output validity, and the common quality envelope. They apply to every publishing route.

**Protected gates** are zero-budget preservation invariants: alpha, topology, protected edges/features/endpoints/junctions, and locked palette. A protected loss rejects; it is not traded against aggregate quality.

**Calibrated gates** use an approved, version-compatible, digest-bound threshold bundle. The foundation bootstrap is separate from later family bundles. A route without a compatible approved bundle is unsupported.

**Resource gates** impose fixed bounds on input/decode, SVG parsing/rendering, output, elements/attributes/tokens/coordinates/references, paths, and diagnostic artifact transaction ownership. The CLI bounds policy and SVG control-document reads before allocation. A caller limit can only reduce an applicable bound.

Profile predicates are requested output-profile requirements in addition to common gates. Compact is vector-quality owned. Motion structure is explicit, tri-state, motion-owned, hash-bound, and non-semantic; it is never inferred or assessed for an unrequested compact request.

## Support and preset-required outcomes

`PRESET_REQUIRED` is a reportable generation rejection (exit 4) when automatic classification is low-confidence or conflicting. Analysis remains successful (exit 0) and records abstention plus route reasons rather than a generation rejection code; it cannot publish.

`unsupported` is a reportable generation rejection (exit 4) when content, output profile, route, bundle, backend, or proof is unavailable or fails. Explicit presets select a route family only; they do not convert unsupported content into supported content. An unpromoted motion key yields `PROFILE_NOT_PROMOTED` with motion `notEvaluated`; withdrawn or invalid-predecessor routes are unsupported.

Route states are `foundationActive`, `candidateShadow`, `promoted`, and `unsupported`. Only `foundationActive` and `promoted` can be considered for publishing, and both must pass the universal envelope. Route key identity is `(inputFamily, policyVersion, profileVersion, outputProfile)`. Promotion and rollback change only intended route keys beneath the stable public API/CLI; rollback selects an approved predecessor or becomes unsupported.
