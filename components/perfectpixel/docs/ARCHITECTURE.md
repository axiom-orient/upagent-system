# PerfectPixel 아키텍처

이 문서는 경계·의미·owner의 설계 계약이다. 상세 공개 규범은 [SPEC](SPEC.md),
실제 wiring 예외·기능 현황은 [Current](IMPLEMENTATION_STATUS.md)가 소유한다.

## Product invariant

PerfectPixel has one semantic authority per responsibility. Transport, codecs, platform frameworks, and external executables may carry input or produce candidates; they do not independently decide product success.

```text
Input / transport
      │
      ▼
 typed Operation
      │
      ▼
pure/bounded domain work
      │
      ├──────────── no external Effect ───────────┐
      │                                           │
      └─ Effect request -> Effect completion ─────┤
                                                  ▼
                                            verification
                                                  │
                                                  ▼
                                      checked publication
```

State mutation is visible only at the publication boundary. External success, helper output, or candidate encoding alone never changes artifact authority.

## Dependency direction

```text
core
  artifact / pixel / color / alpha
  geometry / composite / document
  raster / transform / verify / svg
         │
         ├─────────────┐
         ▼             ▼
      vector        adapters
  policy/gates     PSD / sprite / motion
         │             │
         └──────┬──────┘
                ▼
          application
       typed Operation dispatch
                │
        ┌───────┼────────┐
        ▼       ▼        ▼
       I/O    effects   MCP
                │
                ▼
             runtime
        bounded processes
```

`core` contains format-independent deterministic semantics. It does not parse command-line syntax, own MCP routing, invoke Apple Vision, invoke KTX tooling, or publish files.

`adapters` exist where a real external/interchange representation is required. `adapters::psd` serializes `DocumentIR`; it does not own document semantics or merged appearance. Sprite and motion adapters consume core values and retain their existing bounded packaging contracts.

`application` resolves transport-neutral operations into domain work, Effect requests, verification, and publication. It is orchestration, not a second semantic domain.

`effects` and `runtime` isolate external execution. They cannot publish final artifacts.

`io` owns byte/path capabilities and atomic publication mechanics. It does not decide domain acceptance.

## Operation authority

The `operation_specs()` slice is the only operation metadata authority. Every operation name is unique and binds summary, risk, side-effect class, capabilities, and optional timeout.

The CLI parses syntax directly into `Operation`. MCP keeps only its protocol-local typed `*Params` DTOs; each tool resolves root-bounded paths and canonical scalar values directly into the same `Operation`. There is no intermediate `ApplicationRequest`, argv reconstruction, or CLI-parser re-entry.

```text
CLI syntax ---------> Operation ─┐
                                 ├-> application dispatch
MCP typed *Params --> Operation ─┘
```

CLI and MCP inventory parity means semantic parity for operations exposed through both transports, not identical command counts. Compiler-only CLI operations do not silently expand the fixed MCP tool contract.

Operation metadata also owns external-operation timeouts. Application dispatch passes that value into the Effect request; Texture/Vision handlers do not maintain independent timeout constants.

## Authoritative owners

| Responsibility | Owner |
| --- | --- |
| operation identity / metadata / timeout | `Operation` / `operation_specs()` |
| artifact identity | `ArtifactRef` digest |
| raster pixel/color/alpha semantics | core pixel/color/alpha |
| mask cleanup | core alpha |
| geometry / compositing | core geometry/composite |
| verification policy / evidence | core verify |
| layered document semantics | `DocumentIR` |
| PSD byte serialization | PSD adapter |
| sprite semantics / generation | sprite plan + generation authority |
| texture acceptance | `texture.compile` verification path |
| vector route / acceptance | embedded PerfectPixel vector authority |
| Apple Vision inference | Apple Vision Effect adapter |
| KTX encoding/extraction | KTX process Effects |
| visible mutation | atomic publication runtime |
| CLI/MCP presentation | transport adapters |

No filename convention, helper result, README statement, or backend name may override these owners.

## Domain purity

Pure or deterministic modules operate on values and return values/errors. Examples include raster transforms, alpha operations, connected components, geometry, compositing, `DocumentIR` validation/composition, KTX2 inspection, verification calculations, vector policy/gate decisions, and generation ownership reducers.

Actors, locks, channels, and state machines are not added to pure computation. They exist only where there is actual concurrency or recoverable state.

## Real state machines

### Generated output ownership

Generated sprite/motion workflows use one pure generation authority state and explicit replacement events. The reducer decides retained, superseded, and stale managed artifacts; I/O adapters implement the resulting plan.

### Recoverable artifact-set publication

The artifact-set writer owns the durable transaction lifecycle. It stages new bytes, records exact backup evidence, verifies preconditions, mutates the managed set, verifies the installed set, records commit evidence, and performs rollback/recovery when required.

This state machine exists because multi-file replacement can be interrupted. Single-file transformations do not use it.

### Vector publication outcome

Vector generation retains its explicit ordered transaction outcome because report, diagnostics, and final SVG have distinct publication truth. Only an approved vector outcome can publish the final SVG.

## External Effect boundary

External Effects are candidate producers with explicit work identity.

`EffectIdentity` binds:

```text
generation
+ operation digest
+ input artifact digest
```

Completion is explicit:

```text
Succeeded(value)
Failed(error)
Cancelled
```

A result whose identity is not current is stale and rejected. Timeout is represented as a typed failure rather than a generic I/O error.

### Process runtime

Pinned executables are opened through the no-follow capability boundary, hashed, canonicalized, and revalidated around execution. Child execution receives an explicit environment after `env_clear`.

One Effect owns one process group. Timeout and normal leader exit terminate the complete group and reap the direct child before the Effect returns. Stdout/stderr are drained with bounded retained evidence so a verbose child cannot deadlock on a full pipe.

The remaining pathname check-to-exec window is a platform residual of Darwin/`std::process`; PerfectPixel does not claim an executable-fd primitive it does not have.

## Texture flow

```text
strict texture request
-> request/input/output preconditions
-> exact input ArtifactRef verification
-> explicit PixelSpec/color policy
-> canonical PNG carrier
-> pinned KTX encode Effect
-> current EffectIdentity check
-> KTX2 structural/semantic verification
-> pinned KTX extract Effect
-> current EffectIdentity check
-> deterministic decode-roundtrip verification
-> precondition recheck
-> atomic KTX2 publication
```

The external KTX executable owns encoding bytes only. PerfectPixel owns texture semantics and acceptance. Color-sRGB verification uses CIEDE2000; linear/normal/mask verification uses explicit channel-error policy. Normal maps are not judged with an unrelated sRGB metric.

ICC-tagged texture sources retain exact profile provenance but fail with explicit `Unsupported` because this release has no ICC transform engine. No profile is silently interpreted as sRGB.

## Apple Vision flow

Only `vision.apple.foreground_instances` is included.

```text
strict Vision request
-> request/input/output preconditions
-> EffectIdentity
-> pinned Swift helper
-> Apple Vision framework
-> EffectResult
-> current identity check
-> candidate mask bytes copied into PerfectPixel memory
-> helper staging removed
-> core Mask cleanup
-> coverage/components/bounds verification
-> request/input precondition recheck
-> checked directory publication
```

The helper receipt records provider, adapter version, OS version, request type, request revision, and instance mapping. PerfectPixel does not require opaque Vision output itself to be bit-exact across OS/model revisions; it gates the deterministic postconditions it owns.

## Document flow

The single-raster `psd` export and layered `document-psd` compiler are independent public contracts with different semantics.

The layered path is:

```text
strict request
-> DocumentIR validation
-> exact ArtifactRef bindings
-> explicit pixel/color semantics
-> PerfectPixel merged composite
-> PSD adapter serialization
-> structural readback
-> precondition recheck
-> atomic publication
```

The merged composite is computed before serialization and remains PerfectPixel authority. The PSD writer is never allowed to become the renderer-of-record.

ICC-tagged DocumentIR raster bindings retain provenance but fail explicitly until a real color transform engine is introduced behind a separately verified boundary.

## Vector flow

```text
raster snapshot
-> profile/request normalization
-> verified embedded route
-> compiled native candidate backend
-> bounded SVG parse/safety checks
-> topology/protected geometry gates
-> resvg render-back
-> quality/resource gates
-> Approved | Rejected
-> explicit publication outcome
```

External VTracer is deliberately absent from this release. Adding it before the verified route registry can select and identity-pin it would create a disconnected second path. If later adopted, its bytes must enter the exact same acceptance flow shown above.

## Color management boundary

Codec decode preserves embedded ICC bytes and binds them to `ColorSpec::Icc { digest }`. No-profile input remains `ColorSpec::Unknown`.

The current minimal release does not expose an ICC transform API. Operations that require conversion from an embedded ICC profile return explicit `Unsupported`; there is no silent sRGB fallback. A future color engine must be introduced as one explicit transform authority with corpus-backed verification rather than as a hidden codec behavior.

## MCP boundary

MCP is local stdio with one configured fixed root. Root/path validation, one-active-operation admission, typed protocol `*Params`, and MCP envelope rendering belong to this adapter only.

The protocol DTO maps directly to `Operation`. Root/path rejection remains a transport error; semantic scalar validation is rendered through the same application failure taxonomy used by the CLI.

Synchronous product work runs in `spawn_blocking`. Cancelling the MCP future does not force-stop an already-running in-process publication worker, and its permit is not released early. External process Effects finish under their configured timeout and process-group cleanup boundary.

## Publication boundary

### Single file

A single-file mutation captures destination/input preconditions before expensive work, creates and verifies bytes, rechecks preconditions, then commits through `AtomicFileWriter`. Failure before the rename preserves the previous destination. A durability failure after visibility is reported explicitly rather than rewritten as success.

### Managed set

Generated sets and Vision directory output use checked artifact-set publication. The writer coordinates cooperating publishers with an OS lock, exact stage/backup evidence, journaled transition, rollback/recovery, and verification before commit evidence.

A stale or unowned file is not silently deleted. A force flag is not part of the architecture.

## Bounded concurrency

MCP admission uses a semaphore because there is a real shared active-operation boundary. CPU-heavy independent per-item work may use the existing bounded scoped-thread helper; it joins synchronously and preserves deterministic lowest-input-index error selection.

No generic actor system, async domain layer, or channel graph is introduced.

## Deliberate release exclusions

The following are not partially wired features:

- embedded ICC conversion engine;
- SSIMULACRA2;
- external VTracer route;
- SAM / ViTMatte / libvips optional adapters;
- OCR / contour / feature-distance / optical-flow Vision operations;
- OpenRaster backend.

A future implementation must first establish a concrete product requirement and a single authority/effect boundary.
