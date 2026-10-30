# PerfectPixel Product Contract

> Status: current public contract for `perfectpixel` 0.3.1.
> Runtime evidence that has not actually been produced on the target host is UNKNOWN.

PerfectPixel is a deterministic local image-asset compiler and verification engine. It accepts explicit local inputs, performs bounded deterministic domain work, optionally invokes pinned external Effects, verifies candidate artifacts, and publishes only accepted output.

The executable authority order is:

```text
runtime result > tests > implementation > configuration > documentation
```

Documentation never authorizes behavior that is not reachable from the implementation.

## Platform

The Rust crate is `cfg(unix)` only. Non-Unix targets fail at compilation. Apple Vision additionally requires a supported macOS Vision framework. KTX2 compilation requires a caller-supplied compatible `ktx` executable whose exact SHA-256 is declared in the request.

## Canonical operation boundary

`Operation` and the canonical `operation_specs()` slice own operation identity, risk, side-effect class, capabilities, and timeout metadata.

```text
CLI parser ---------> Operation ─┐
                                 ├── application dispatch
MCP typed *Params --> Operation ─┘
```

There is no intermediate `ApplicationRequest`, argv reconstruction, or CLI-parser re-entry. MCP `*Params` are protocol-local DTOs only. Root/path validation remains an MCP adapter responsibility; semantic scalar validation uses canonical PerfectPixel constructors/parsers before the `Operation` executes.

CLI and MCP inventories may differ. When an operation is exposed through both transports, both reach the same `Operation` implementation and neither transport may redefine success, retryability, publication, or domain validation.

Operation metadata is also the single timeout authority for external operations. Application dispatch passes that timeout into the Effect request; handlers do not own parallel timeout constants.

## Public CLI commands

```text
perfectpixel schema
perfectpixel inspect <input.png|jpg|jpeg|webp>
perfectpixel convert <input> --out <output> [options]
perfectpixel upscale <input> --out <output> --scale <integer>=2 [options]
perfectpixel edit --request <edit-request.json>
perfectpixel psd --request <psd-export-request.json>
perfectpixel document-psd --request <document-psd-request.json>
perfectpixel chroma-plan --request <chroma-plan-request.json>
perfectpixel normalize --request <normalize-request.json> --out-dir <dir>
perfectpixel bundle --request <sprite-request.json> --out-dir <dir>
perfectpixel texture-compile --request <texture-request.json>
perfectpixel vector <input> --out <output.svg> [options]
perfectpixel vector-analyze <input> [options]
perfectpixel vision-foreground-instances --request <vision-request.json>
perfectpixel motion-scaffold <input.svg> --out-dir <dir>
perfectpixel motion-build --request <motion-request.json> --out-dir <dir>
```

`perfectpixel schema` is the machine-readable capability surface. `--help` is presentation only. The schema identifies external inference as an Effect capability rather than claiming that the compiler itself is an inference model.

## Artifact and pixel contract

`ArtifactRef` identifies bytes by lowercase SHA-256 digest, media type, and byte count. A filesystem path is not artifact identity.

Current raster storage is RGBA8. `PixelSpec` explicitly records pixel format, alpha mode, and color semantics. Missing embedded color provenance remains `ColorSpec::Unknown`; it is never silently promoted to sRGB.

Embedded ICC bytes are retained and bound to their digest. This release does not expose an ICC conversion API. A path requiring conversion from an embedded profile returns explicit `Unsupported` rather than silently changing color semantics. An unprofiled source may be accepted only when the request explicitly declares the color semantics required by that operation.

The textual `#RRGGBB` matte syntax is parsed by the shared core sRGB8 literal parser; CLI and MCP do not maintain independent color-literal rules.

## Verification contract

`VerificationSpec` is strict typed input and rejects vacuous verification. `VerificationReport` is machine-readable evidence.

Supported verification includes:

- exact dimensions, `PixelSpec`, alpha bounds, artifact SHA-256, connected-component count, and content bounds;
- exact RGBA and alpha assertions over explicit regions;
- CIEDE2000 thresholds over complete or regional comparisons;
- mask coverage/component/bounds invariants;
- KTX2 structural and semantic assertions plus decode roundtrip;
- vector SVG safety/topology/render-back/quality gates;
- layered PSD structural readback against PerfectPixel-owned document semantics.

There is no unused verification-profile selector in the public API. A concrete operation constructs the assertions it requires. A candidate that cannot be verified does not publish.

## Raster operations

`inspect` returns bounded numeric evidence over the exact source bytes it decoded.

`convert` and `upscale` use deterministic raster transforms and explicit encoders. PNG/WebP preserve RGBA. JPEG transparency requires an explicit background matte. Resize/upscale dimensions are positive and bounded. Output/input collisions fail before publication.

`edit` accepts a strict bounded JSON pipeline of deterministic operations: crop, quarter-turn rotation, horizontal/vertical flip, resize, exact keyed background removal, and deterministic edge-derived automatic background removal. It is not semantic segmentation, inpainting, outpainting, or generative editing.

`chroma-plan` is a deterministic fixed-palette color decision. It performs no semantic vision.

## PSD contracts

### `psd`

`psd` is the single-raster flattened PSD v1 export. It preserves its bounded raster/path export semantics and does not claim layered document semantics.

### `document-psd`

`document-psd` is the canonical layered path and maps to operation `document.compile_psd`.

The strict request has `schemaVersion: 2`, `operation: "document.compile_psd"`, a `DocumentIR`, exact artifact bindings, and one `.psd` output path. Unknown fields fail.

```text
request snapshot
-> DocumentIR validation
-> content-addressed raster bindings
-> explicit pixel/color validation
-> PerfectPixel deterministic merged composite
-> isolated PSD serialization
-> structural readback
-> request/input/output precondition check
-> checked atomic publication
```

`DocumentIR` owns layer/group/mask semantics. The PSD adapter owns byte encoding only. The adapter cannot replace PerfectPixel's merged appearance with an independently rendered success decision.

ICC-tagged DocumentIR raster bindings preserve profile provenance but return explicit `Unsupported` because no ICC transform authority exists in 0.3.1.

## Sprite contracts

`normalize` and `bundle` retain the existing deterministic sprite-generation workflow. Generated ownership is explicit and managed through `.perfectpixel-generation.json`; unowned files are not silently adopted or deleted.

Normalized/bundled sets publish through the recoverable artifact-set transaction boundary. A failed normalize gate may publish its explicit failure report but cannot leave a prior success set deceptively current.

## Texture contract

`texture-compile` maps to operation `texture.compile` and accepts one strict request:

- `schemaVersion: 1`;
- `operation: "texture.compile"`;
- `generation` work identity;
- input `path`, `ArtifactRef`, and `PixelSpec`;
- output `.ktx2` path;
- semantic `color_srgb`, `linear`, `normal_map`, or `mask`;
- encoding `basis_lz` or `uastc`;
- explicit mipmap policy;
- pinned `ktx` executable path and SHA-256;
- verification thresholds.

`normal_map` requires UASTC. `color_srgb` uses explicit CIEDE2000 thresholds. Linear, normal-map, and mask semantics use explicit maximum absolute-channel error.

ICC-tagged input is provenance-checked but explicitly unsupported until a real profile transform engine exists. Unprofiled input must explicitly declare the color semantics required by the texture semantic.

The flow is:

```text
immutable input snapshot
-> explicit pixel/color semantics
-> canonical PNG carrier
-> pinned KTX encode Effect
-> current EffectIdentity check
-> KTX2 structure/transfer/supercompression/mipmap verification
-> pinned KTX extract Effect
-> current EffectIdentity check
-> decode roundtrip verification
-> request/input/output precondition recheck
-> atomic publication
```

External process exit success is only a candidate event.

## Vector contract

`vector` is the sole raster-to-SVG publication command. The embedded content-addressed PerfectPixel vector authority owns route selection and acceptance.

A generated SVG must pass bounded SVG parsing, forbidden-resource checks, dimensions, resource limits, topology/protected-geometry checks, raster render-back, and configured quality gates before final publication. A rejected outcome cannot publish the requested final SVG.

`vector-analyze` is candidate-free analysis and cannot publish SVG.

External VTracer is not part of this release. It may be added only when the verified route registry explicitly selects that backend and its candidate enters the same existing acceptance pipeline. Backend identity never grants approval.

Detailed vector policy/report behavior remains in [vector documentation](vectorize/README.md).

## Apple Vision contract

The only Vision product operation in this release is `vision.apple.foreground_instances`, exposed by CLI as `vision-foreground-instances`.

The strict request binds:

- `schemaVersion: 1` and canonical operation name;
- generation identity;
- input path and exact `ArtifactRef`;
- output directory;
- pinned Swift helper executable path and SHA-256;
- `requestRevision: 1`;
- deterministic mask-cleanup parameters;
- explicit mask verification bounds.

The Apple helper is an Effect adapter, not success authority. It records provider, adapter version, OS version, Vision request type/revision, instance IDs, executable digest, argument digest, and process evidence.

```text
request/input snapshot
-> EffectIdentity(generation, operationDigest, inputDigest)
-> pinned Swift helper / Apple Vision
-> EffectResult
-> current identity check
-> copy candidate masks into PerfectPixel-owned memory
-> remove helper staging state
-> deterministic Mask cleanup
-> coverage/component/bounds verification
-> request/input precondition recheck
-> checked artifact-set publication
```

Failed, timed-out, stale, or unverified Vision results cannot acquire artifact authority.

## Motion contract

`motion-scaffold` and `motion-build` retain the existing deterministic raster-free SVG motion contract. See [MOTION_CONTRACT.md](MOTION_CONTRACT.md). Motion does not grant raster-to-SVG vector approval and does not claim bones/IK, semantic segmentation, path morphing, or packed `.lottie` output unless explicitly implemented by that contract.

## External Effect contract

An external Effect receives explicit immutable input and a bounded staging destination. Executables are pinned by SHA-256 and revalidated around process execution. Ambient environment is cleared; required environment values are explicit.

Every completion returns one of:

```text
Succeeded(candidate)
Failed(error)
Cancelled
```

Timeout is an explicit failure category. Process Effects own a process group; timeout and normal leader exit terminate the complete group and reap the direct child before returning completion. `EffectIdentity` prevents stale results from changing current artifact state.

Cancelling an in-process MCP call does not force-stop an already executing synchronous publication worker. Its admission permit remains owned until that worker completes. External process Effects finish under their configured timeout and process-group cleanup boundary.

## Publication contract

A mutation follows the minimum complete sequence:

```text
capture destination/input preconditions
-> compute candidate
-> verify candidate
-> recheck preconditions
-> checked publication
```

Single-file outputs use the atomic file writer. Managed sets use the recoverable artifact-set publisher with lock, staging, backup evidence, journal, rollback/recovery, and final commit evidence.

There is no generic force overwrite, silent fallback, implicit stale-file adoption, or success result emitted before publication returns success.

## Failure contract

Failures preserve operation, cause, and relevant context. Invalid request, unsupported behavior, conflict/stale state, precondition failure, resource limit, timeout, cancellation, dependency failure, and verification failure remain distinguishable.

Retryability is explicit where the Effect boundary can determine it. A failure is not converted to success because cleanup, durability confirmation, or an optional backend is unavailable.

## Deliberately absent from 0.3.1

The following roadmap candidates are not hidden partial features and are not part of this release:

- embedded ICC conversion engine or public ICC transform API;
- SSIMULACRA2 dependency;
- external VTracer route;
- SAM/promptable segmentation;
- ViTMatte/neural matting;
- libvips large-image backend;
- OCR, contour, feature-distance, or optical-flow Vision expansions;
- OpenRaster backend.

They require a concrete product need before adding a new Effect or abstraction boundary.

## Verification contract

The target macOS checkout should run:

```bash
cargo fmt --all -- --check
cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
cargo test --locked --workspace --all-targets --all-features
```

Environment-specific follow-up:

- build/run the Apple Vision helper on the target macOS SDK;
- execute a pinned `ktx` encode/extract roundtrip;
- open/read back layered PSD with the desired independent/native application when native interoperability evidence is required.

Anything not actually executed remains UNKNOWN, never an inferred PASS.
