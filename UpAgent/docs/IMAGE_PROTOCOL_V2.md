# Image Artifact & Observation Protocol v2

## Status

Protocol v2 is implemented through the M0–M8 contract boundary. M3 adds deterministic
extraction/object artifacts, M4 adds stateless raster/object composition, M5 adds
deterministic comparison, M6 binds a task-owned comparison specification to the
current candidate revision, M7 adds explicit precision/colour and text contracts,
and M8 removes the runtime compatibility editor after canonical parity coverage.
Semantic Vision/matting remains an explicit adapter boundary; no native backend is
bundled in this repository.

## Authority split

- `agent-core`: logical artifact, observation, pixel and receipt contracts.
- `agent-runtime`: content-addressed artifact persistence and just-in-time observation resolution.
- `agent-provider`: transport materialization for the canonical `vergerail.upagent/2` contract.
- PerfectPixel: deterministic image execution and operation receipts.

A durable `ModelMessage` never stores an absolute artifact path or base64 image. It stores an `ArtifactRef`. Immediately before provider dispatch, the runtime re-reads that reference from the content-addressed `ArtifactStore`, verifies bytes against SHA-256, validates PNG geometry, then constructs a bounded transport sidecar.

## M0 compatibility rule

PerfectPixel operation compatibility is semantic and capability-subset based,
not exact-list based: composition admission checks that every capability
required by the selected operation is present with the required compatible
major version and determinism. Additive capabilities do not fail that
operation-level subset check.

This semantic compatibility check is separate from the exact M8 composition
identity. The canonical capability manifest is part of the pinned composition
identity, so *any* manifest change—including an additive capability or a
version/determinism change—changes the identity and requires recomposition.

Required M2 PerfectPixel capabilities:

- `inspect.basic`
- `artifact.content_addressed`
- `artifact.dependency_closure`
- `artifact.pin_set`

The runtime manifest contains no compatibility editor capability. Geometric and
edge-background operations are represented as ordered `operations` on an
`image_render` node and execute through the same bounded PerfectPixel raster core.

## M8 executable and capability identity

An image-capable `AgentSpec` (schema v5) must carry the absolute pinned
PerfectPixel executable, its lowercase SHA-256, protocol schema/version,
behavior version, implementation version, and capability-manifest SHA-256. The
runtime re-hashes the executable and probes `agent-schema` before each image
operation. It rejects protocol, behavior, implementation, or capability
identity drift before request execution.

The capability digest is computed over the canonical strict manifest JSON with
capabilities sorted by `(name, version, determinism)`:

```text
perfectpixel.agent-image/capability-manifest/2\0<canonical manifest JSON>
```

Capability names must be unique. Required operation capabilities use
same-major-version minimum checks and exact determinism checks; this is the
semantic subset rule used for operation admission. Independently, the
manifest digest is exact composition identity: any manifest byte or entry
change requires a new composition, even when the changed entry is additive
and would be harmless under the subset rule. The executable SHA-256 is
rechecked at every child-process
boundary; the remaining file-read/exec interval is a bounded host TOCTOU risk.

## M1 multimodal observation

Logical message part:

```json
{
  "type": "image_observation",
  "image": {
    "artifact": {"id":"...","sha256":"...","mediaType":"image/png","byteLength":123,"relativePath":"..."},
    "role": "full",
    "detail": "auto",
    "caption": "Generated image artifact"
  }
}
```

Provider admission is capability-based in core: image tools require
`model/image_observation`, and `image_generate` additionally requires
`model/image_generation`. The stock Vergerail adapter maps the single
`vergerail.upagent/2` contract to text, typed local-image observations, and
image generation. There is no silent downgrade from image observation to
metadata-only text. Observation requests are admitted only after final
serialization (64 MiB), with at most 16 images, 8 MiB per image, and 32 MiB
aggregate decoded bytes. UpAgent allocates a request-specific private
`stagingRoot`; Vergerail receives it in the wire request and consumes typed
local-image paths, while UpAgent removes the exact root after child reap on
all process and cancellation paths.

## M2 artifact descriptor

Every new image output exposed to the model is projected with:

- protocol version
- immutable `ArtifactRef`
- artifact kind
- pixel spec
- direct dependencies
- canonical dependency-closure SHA-256
- retention class

The default working-raster semantics remain deliberately compact:

```text
format      = rgba8
colorSpace  = srgb
alphaMode   = straight
```

UpAgent image tools accept only `rgba8/srgb/straight`. Standalone PerfectPixel
provides explicit `rgba16` and `Display-P3` PNG conversion; those opt-in contracts
are outside the UpAgent image-tool boundary.

## Dependency closure

The closure digest is SHA-256 over canonical JSON for direct content dependencies after validation, sorting and deduplication. Dependencies contain only content identity and media metadata, never filesystem paths.

This makes cache/provenance identity independent of checkout location, temp directories, process restarts or clean extraction.

## Receipt

An M2 operation receipt binds. UpAgent-generated orchestration receipts use `upagent.image-operation-receipt/2`; receipts emitted directly by PerfectPixel use `perfectpixel.agent-image/receipt/2`:

```text
request id
operation
COMMITTED / REJECTED
behavior version
implementation version
request SHA-256
dependency closure SHA-256
direct dependencies
optional output SHA-256
determinism class
```

PerfectPixel `agent-inspect` is bit-exact. Image generation is marked `inference_observed`; UpAgent does not mislabel generative output as deterministic.

When an explicit generation size or background differs from the observed valid PNG,
the provider returns `generated_needs_repair` with typed mismatch evidence. The
runtime keeps `is_error=false`, preserves the PNG and receipt, and emits a Korean
agent-visible repair message. `ImageDirectorState.pendingRepair` stores an exact
`ImageRepairRequirement` for every explicitly verifiable generation constraint
(size and/or transparent/opaque background). A subsequent PerfectPixel-backed
`image_extract`/`image_render` candidate is inspected again and clears the blocker
only when its `ImageRepairObservation` satisfies the complete stored requirement.
Tool success or `perfectpixelVerified=true` alone is insufficient. Structural invalid
PNG, oversize output, and unknown provider dispatch remain fail-fast or
`OutcomeUnknown`.

## Retention / pinning

M2 defines retention intent and PerfectPixel's strict `pin-set/2` contract:

- `task_scoped`
- `cacheable`
- `pinned`

M2 does **not** implement garbage collection. Future GC must preserve the transitive closure of the explicit pin set; absence of a GC implementation is not treated as deletion authority.

## Failure policy

Fail closed when any of the following changes:

- executable digest
- required PerfectPixel capability disappears
- request/source hash mismatch
- observation artifact cannot be re-read
- sidecar bytes differ from ArtifactRef
- PNG payload/header dimensions disagree (requested size drift is repairable evidence)
- receipt request/dependency hashes disagree
- stock Vergerail image composition configured with a retired provider protocol
- image-generation `ImagePort.binding()` differs from the provider binding
  sealed by composition, or changes after module admission

No path-based fallback and no metadata-only multimodal fallback are permitted.


## M3/M4 runtime wiring

`image_extract` and `image_render` are creator-profile built-in tools. They stage
only verified content-addressed artifacts, call the pinned PerfectPixel executable,
then revalidate returned bytes, descriptors, dependency closure and operation
receipts before artifact persistence. Render-node `operations` is the only
canonical deterministic preprocessing path.

## M5 deterministic comparison

`image_compare` is the verification authority for image invariants that can be expressed without semantic guessing. It calls PerfectPixel `agent-compare` and supports required/advisory assertions for exact equality, changed ratios, target-mask locality, unchanged regions, alpha IoU, mask leakage, object bounds/centroid/area, maximum channel error, and mean absolute error.

A failed required assertion is a **diagnostic result**, not a transport failure. PerfectPixel returns a rejected receipt plus bounded optional difference/mask-overlay previews. UpAgent revalidates the request hash, dependency closure, receipt status and preview descriptors before exposing the result.

## M6 exact contract-bound Image Director

Image work is coordinated by explicit `TaskEnvelopeV3.imageTask` ingress and durable `agent-core::ImageDirectorState`. There is no hidden mutable canvas. The task contract participates in the immutable task digest and is separately rebound to the runtime state before model execution.

Candidate-producing operations (`image_generate`, `image_extract`, `image_render`)
require the contract before admission. `provided` tasks seal one source `ArtifactRef` in the contract and seed the Image Director's pixel provenance closure with it. `generate_new` tasks start with no pixel source and establish one only after successful generation. Deterministic producers must consume only pixel-bearing inputs already in that closure, and successful outputs extend it. `provided` tasks cannot generate a new base, and generation is always forbidden after deterministic editing starts.

The `upagent.image-task/5` contract requires visual review and may contain a typed `perfectpixel.agent-image/verification-spec/1`. The specification owns the baseline artifact, candidate artifact kind, and complete ordered assertion set; at least one assertion must be required. Its canonical digest is bound independently from the enclosing task digest. Each producer records the current candidate's full `ArtifactRef` and typed `ImageCompareArtifactKind`; a task-bound compare must match both the contract's `afterKind` and that producer-bound state.

A task-bound `image_compare` is admitted only when its baseline, candidate kind, assertion order, severities and thresholds reproduce that exact digest and its `afterArtifact` plus kind match the current producer-bound candidate. The runtime binds the pending call to tool-call ID, candidate revision, full candidate artifact identity, candidate kind and specification digest, then consumes that binding once. Stale, duplicate, drifted or malformed results cannot qualify the task. Visual-only or standalone comparisons do not mutate deterministic qualification.

Completion is blocked when no PNG candidate exists, the candidate is outside the authorized pixel provenance closure, visual observation is stale, the exact required specification did not pass for the current revision, or the returned assertion identities/types/severities/aggregate disagree with the request. Suspension is rejected while an admitted comparison is pending.

## M7 explicit output fidelity

`PixelSpec` identifies output semantics. UpAgent requires
`rgba8/srgb/straight`; it does not accept opt-in precision or colour-space
conversion requests. Standalone PerfectPixel's explicit PNG convert boundary
can produce `rgba16` and/or `Display-P3` using a fixed-profile matrix and
deterministic quantization. An 8-bit source is never labelled as native 16-bit,
and lossy quantization is reported at that separate conversion boundary.

`TextNode` requires content-addressed TTF/OTF bytes and their SHA-256, explicit
direction/language/box/alignment/line-break/font-size/colour/PixelSpec, and a
pinned deterministic raster path. Missing glyphs and unsupported scripts fail
explicitly; there is no system-font fallback.

## M3 semantic effect boundary

`image_extract` accepts an explicit semantic selector only through the
`extract.semantic_mask` adapter. The request and result carry source artifact and
revision, helper identity, request revision, and tool-call identity owned by the
durable Image Director state. Success, failure, cancellation, stale, and
duplicate results re-enter the authoritative state transition. The default
adapter returns a structured unavailable result. A configured process adapter is
selected by `AgentSpec.semanticMask` and requires an absolute regular
non-symlink executable, pinned executable SHA-256/helper identity/version, and
bounded `timeoutMs` (`1..=600000`). Composition canonicalizes and verifies that
digest before receipt creation; the CLI constructs it with the runtime artifact
store and rechecks it before each invocation. The helper uses bounded JSON
request/result plus PNG output; every success, failure, cancellation, and
timeout path drains output, terminates the captured process group gracefully,
forces KILL when needed, and proves group absence before accepting a result. No
helper is discovered from `PATH`. Native model quality is [UNKNOWN] until an
adapter is externally qualified.

The current Apple Vision reference has a stable
[`GenerateForegroundInstanceMaskRequest`](https://developer.apple.com/documentation/vision/generateforegroundinstancemaskrequest)
for foreground-instance masks, while the iterative point/box/scribble API is
documented as beta in
[`GenerateIterativeSegmentationRequest`](https://developer.apple.com/documentation/vision/generateiterativesegmentationrequest).
The local product therefore does not bind correctness to that beta API; a
pinned external process adapter is the replacement boundary for any qualified
backend.

## M8 canonical parity and release boundary

The retained deterministic editor behaviours—crop, rotate, flip, nearest/Lanczos3
resize, keyed removal, and edge-background planning—are covered by ordered
`image_render.operations` parity fixtures. The UpAgent runtime and capability
manifest contain no legacy editor caller or capability. PerfectPixel's standalone
human CLI may retain its separate `edit` command; it is not part of the model
tool surface.
