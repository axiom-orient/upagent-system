# Vector Evaluation and Analysis Report Schemas

Status: Current normative report contract for the 0.3.1 vector runtime.

Normative: Yes — REPORT-001, grounded in the approved Vector/SVG Transition Roadmap.

Implemented: Yes. The runtime emits these schemas.

## Schema identities and immutability

Generation evaluation reports use `perfectpixel.vector-evaluation/3`; analysis reports use
`perfectpixel.vector-analysis/1`. Both are deterministic JSON evidence documents. They are immutable
once serialized, are never rewritten after artifact commits, and cannot deserialize, construct, or
authorize `ApprovedVectorOutput`. `digests.report` is the runtime/authority-owned schema identity
`sha256-962c423b7aa6e6de99010847b38943e3c0c9c9683e84d53ec264d9bb7152bcef`.

Generated authority binds the v3 schema identity and report digest through the verified embedded
closure. Reports remain evidence only and cannot promote or publish a route.

## `perfectpixel.vector-evaluation/3`

An evaluation report is a closed object with exactly these top-level fields:

- `schema`, `inputDigest`, `preset`, `profile`, and factorized `evidence`;
- `constraints` and `pathLimit`, each carrying requested/approved/effective/applied/applicability/relaxation evidence;
- `digests` for backend, route, registry, registry entry, threshold bundle, normalized request,
  policy, profile, foundation, report, optional motion assessment, and optional candidate bytes;
- nullable `candidateFacts`;
- `fixedGates`, `protectedGates`, `calibratedGates`, `resourceGates`, and `profilePredicates`;
- `gateMeasurements`, grouped by fixed/protected/calibrated/resource/output-profile family, with typed actual values, comparator, approved/requested/effective thresholds, applicability, and a descriptive reason;
- `actualDecision`, `rejectionCodes`, and `rejectionReasons`;
- `artifactIntent`, which contains metadata and hashes but no transaction-completion claim; and
- `environmentMeasurements` exactly as below.

```json
"environmentMeasurements": {
  "renderTimeMs": null,
  "peakMemoryBytes": null,
  "throughputPixelsPerSecond": null,
  "machineProtocolId": null
}
```

All four environment keys are always present and JSON `null` in product reports. Wall-clock, memory, throughput, and machine measurements belong only in separate benchmark evidence and must never populate this schema.

## Candidate facts and adaptive/topology evidence

When a candidate exists, `candidateFacts` has `candidateSha256`, `candidateByteCount`,
`renderBackSha256`, `width`, `height`, `pathCount`, `colorCount`, `nodeCount`,
`curveSegmentCount`, `closedPathCount`, `adaptive`, and `topology`. `topology` records the
render-back comparison as `sourceComponents`, `candidateComponents`, `sourceHoles`,
`candidateHoles`, and `mismatchedMaskPixels`.

`candidateFacts.adaptive` is JSON `null` for every family except bounded illustration; it is present
only when the illustration v2 backend produces a candidate. Its identity and eligibility fields are
`policyIdentity`, `modelIdentity`, `modelVersion`, `selectorIdentity`, `traceIdentity`,
`traceEncodingVersion`, and `modelEligible`. Its exact policy limits are `fragmentPixels`,
`protectedThinPixels`, `minimumPlateauCoveragePercent`, `maximumCostQ`,
`selectorMergeCreditQ`, `maximumSourceColors`, `maximumFineRegions`,
`maximumAdjacencyEdges`, `maximumMergeEvaluations`, `maximumRetainedSnapshots`,
`maximumQueueEntries`, and `maximumEmittedPaths`.

Adaptive outcome facts are `sourcePaintCount`, `selectedPaintCount`, `sourceRegionCount`,
`selectedRegionCount`, `adjacencyEdgeCount`, `protectedBoundaryCount`, `acceptedMerges`,
`rejectedHardMerges`, `rejectedCostMerges`, `rejectedResourceMerges`, `mergeEvaluations`,
`queueEntries`, `retainedSnapshotCount`, `selectedSnapshotId`, `zeroMerges`, and `stopReason`.
Protection-reason counts are `rejectedIslandMerges`, `rejectedThinMerges`,
`rejectedEndpointMerges`, `rejectedCornerMerges`, `rejectedJunctionMerges`, and
`rejectedPlateauBoundaryMerges`. `trace` is the canonical ordered merge-step array; every step has
`sequence`, `survivorId`, `removedId`, `sharedBoundaryLength`, `representativeRgba`, `wardCostQ`,
`boundaryCostQ`, `alphaCostQ`, `structureCostQ`, `fragmentCreditQ`, and `totalCostQ`.
`traceDigest` identifies the canonical trace encoding. Snapshot selection is real evidence:
`selectedSnapshotId: 0` requires `zeroMerges: true`, rather than a synthetic merge.

Generation outcomes before a candidate (`PRESET_REQUIRED` or unsupported) use `actualDecision: "notApplicable"` and report their rejection/intent if a report was requested. Candidate evaluation has an actual approved/rejected decision before report serialization. A rejected report has no final-SVG accessor or authority.

## `perfectpixel.vector-analysis/1`

An analysis report is a closed object with exactly these top-level fields:

- `schema`, `inputDigest`, requested `preset`, requested `profile`, and `policyDigest`;
- factorized `evidence`, including confidence, conflicts, abstention, expected support, and deterministic `recommendedPresets` when classification supports an explicit family;
- nullable selected `route` plus deterministic `routeReasons`;
- `digests` for available authority identities, with unavailable route/backend identities represented as JSON `null`;
- `predicates`, including tri-state embedded-route availability;
- `normalization`, containing candidate-free normalized policy schema/version, tri-state applicability, and deterministic reasons; and
- the required all-null `environmentMeasurements` object.

Analysis validates, decodes, profiles, policy-normalizes, and looks up routes but does not create a candidate, render-back, motion scaffold, diagnostics directory, approval capability, or final SVG. A successfully produced analysis remains exit 0 when evidence abstains or a route is unavailable; its route reasons describe that state without emitting generation rejection codes. Its `--report` file is atomically the same immutable JSON emitted in stdout's `analysis` field.

## Tri-state profile predicates

Every profile-dependent predicate and availability uses exactly one of:

- `passed`: evaluated and satisfied;
- `failed`: evaluated and not satisfied;
- `notEvaluated`: deliberately unrequested, unavailable, or inapplicable under the normalized request.

`notEvaluated` is not pass, not failure, and not an implicit waiver. Compact requests do not assess motion. An unpromoted motion route reports `PROFILE_NOT_PROMOTED` and motion `notEvaluated` rather than silently evaluating or publishing.


## CLI result envelopes

`perfectpixel vector` writes one closed `perfectpixel.vector-result/1` object to stdout. Its exact top-level fields are `schema`, `ok`, `decision`, `report`, and `transaction`; post-decision transaction failures additionally include a typed `failure` object with `phase`, optional `path`, and `originalError`. `transaction` has exactly `report`, `diagnostics`, and `finalCommit`, each using `notRequested`, `notAttempted`, `committed`, or `failed`.

`perfectpixel vector-analyze` writes one closed `perfectpixel.vector-analysis-result/1` object with `schema`, `ok`, `analysis`, and `transaction`. Its transaction can commit only `report`; `diagnostics` and `finalCommit` remain `notRequested`. Every non-versioned CLI failure, including invalid requests and pre-transaction operational failures, uses the normative closed common error payload `{ok, message, phase, path, originalError}`. `phase` identifies `cli`, `vector`, or `vectorAnalyze`; `path` is JSON `null` when no source path is known; `originalError` retains the unmodified underlying error text. A post-decision report revalidation or commit failure instead uses its versioned analysis envelope, adds `failure`, and marks only `transaction.report=failed`. The CLI tests are the live transaction evidence; inner analysis/evaluation JSON is not transaction proof.
## Report ownership versus CLI transaction ownership

`EvaluationReport` and `VectorAnalysis` own deterministic evaluation/analysis evidence only. `CliTransactionOutcome` exclusively owns later artifact transaction state. Its per-artifact state is `notRequested`, `notAttempted`, `committed`, or `failed` for report, diagnostics, and final commit as applicable.

For generation: determine approval first; serialize immutable evaluation report; commit requested report; stage/hash-verify/commit requested diagnostics; commit exact approved SVG bytes last. Diagnostics replacement is allowed only for a bounded, non-symlink directory whose exact contents match `.perfectpixel-vector-diagnostics.json`, a `perfectpixel.vector-diagnostics-ownership/1` manifest listing zero, one, or both supported diagnostic artifacts and their SHA-256 digests. The marker and every listed artifact are regular files and are verified before replacement; lookalike directories without a valid marker are never claimed. Only `ApprovedVectorOutput` may reach the final writer. A report-commit failure means no report/diagnostics/final attempt; a diagnostics failure leaves any report truthful but no final replacement; a final-commit failure leaves the prior final SVG unchanged.

For analysis: the only possible persistence is the requested immutable analysis report. Diagnostics and final SVG are prohibited. Analysis transaction state cannot be transformed into generation approval.
