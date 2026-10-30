# PerfectPixel

PerfectPixel is a deterministic local image asset compiler and verification engine. It transforms raster, document, sprite, vector, texture, and bounded motion assets; external Vision/tool processes may produce candidates, but PerfectPixel owns semantics, verification, and publication.

For GPT image generation followed by UpAgent-driven pixel editing, start with the
[imageMaker usage guide](../../docs/OPERATIONS.md). Current live results and automation
limitations are recorded in the [integration status](../../docs/IMPLEMENTATION_STATUS.md).

## Architecture

```text
CLI / MCP
    │
    ▼
typed Operation
    │
    ▼
deterministic domain transition
    │
    ├─ no Effect required ───────────────┐
    │                                    │
    └─ Effect request → Result/Event ────┤
                                         ▼
                                  verification
                                         │
                                         ▼
                                checked atomic publish
```

One responsibility has one authoritative owner. CLI and MCP are presentation/admission adapters; they do not independently own product validation, success policy, timeout policy, or publication semantics.

## CLI

```text
perfectpixel schema
perfectpixel agent-schema
perfectpixel agent-inspect --request <agent-inspect-request.json>
perfectpixel agent-extract --request <agent-extract-request.json> --out-dir <dir>
perfectpixel agent-render --request <agent-render-request.json> --out-dir <dir>
perfectpixel agent-compare --request <agent-compare-request.json> --out-dir <dir>
perfectpixel inspect <input.png|jpg|jpeg|webp>
perfectpixel convert <input> --out <output> [options]
perfectpixel upscale <input> --out <output> --scale <n> [options]
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

`perfectpixel schema` is the machine-readable capability surface. `--help` is presentation only.

## Core boundaries

- **Operation authority** — typed `Operation` plus `operation_specs()` metadata; transports do not reconstruct argv.
- **Artifact identity** — content SHA-256, not pathname.
- **Inspection** — exact source `ArtifactRef`, codec-observed `PixelSpec`, ICC byte evidence, and deterministic raster facts. A bare `Raster` does not invent color provenance.
- **Pixel semantics** — RGBA8 plus explicit alpha/color provenance; missing color remains `Unknown`.
- **ICC** — embedded profile bytes/digest are preserved. This release has no ICC conversion engine and fails explicitly instead of assuming sRGB.
- **Verification** — exact dimensions/pixel/alpha/artifact, mask geometry, region assertions, and CIEDE2000 evidence.
- **Alpha / geometry / composite** — deterministic core operations shared by documents, masks, sprites, and verification.
- **Document** — `DocumentIR` owns layered semantics; the PSD adapter serializes them and does not own merged appearance.
- **Texture** — a SHA-pinned KTX executable is an external Effect; candidate bytes must pass structure and decode-roundtrip verification before publication.
- **Vector** — PerfectPixel's verified route/policy and raster roundtrip gates decide acceptance. Disconnected optional backends are not exposed.
- **Apple Vision** — inference is an external Effect. Work identity, helper provenance, deterministic mask cleanup, and verification precede checked publication.
- **Publication** — destination/input preconditions are captured before expensive work; only checked atomic publication owns visible mutation.

## MCP

```text
perfectpixel-mcp --root /absolute/non-symlink/workspace
```

The MCP adapter is local stdio with a fixed root. Protocol-local typed `*Params` resolve directly into the same `Operation` authority used by the CLI. It is not a generic command runner.

## Failure semantics

Failure, timeout, dependency failure, verification failure, precondition conflict, and stale Effect results remain distinct. External process completion never means product success by itself. A failed, stale, or unverified candidate cannot acquire artifact authority.

## Documentation

- [Architecture](docs/ARCHITECTURE.md)
- [Product contract](docs/CONTRACT.md)
- [Function matrix](docs/FUNCTION_MATRIX.md)
- [MCP contract](docs/MCP_CONTRACT.md)
- [Motion contract](docs/MOTION_CONTRACT.md)
- [Normalize review contract](docs/NORMALIZE_REVIEW_CONTRACT.md)
- [Vector documentation](docs/vectorize/README.md)
- [남은 계획](docs/PLAN.md)
- [설계 결정](docs/adr/0001-asset-authority.md)
- [운영·검증](docs/OPERATIONS.md)
- [Documentation index](docs/README.md)
- [Third-party notices](THIRD_PARTY_NOTICES.md)

## Verification

Run on the target macOS checkout:

```bash
cargo fmt --all -- --check
cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
cargo test --locked --workspace --all-targets --all-features
```

Apple Vision, KTX executable integration, and native layered-PSD interoperability require the corresponding local tools/frameworks. They are not inferred from source inspection.

PerfectPixel intentionally supports Unix targets only; non-Unix targets fail at crate compilation.

## 정본 색인

[정체성과 발전 방향](docs/IDENTITY_AND_EVOLUTION.md) · [SPEC](docs/SPEC.md) ·
[ARCHITECTURE](docs/ARCHITECTURE.md) · [IMPLEMENTATION_STATUS](docs/IMPLEMENTATION_STATUS.md) ·
[PLAN](docs/PLAN.md)
