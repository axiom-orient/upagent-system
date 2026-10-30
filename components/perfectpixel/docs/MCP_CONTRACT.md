# perfectpixel-mcp Contract

**Status: normative adapter contract for PerfectPixel 0.3.1.**

`perfectpixel-mcp` is a local stdio adapter. It owns protocol syntax, fixed-root admission, and MCP result envelopes. It does **not** own product semantics, publication policy, retries, timeout policy, or verification.

## Protocol

- Rust `cfg(unix)` only.
- MCP protocol version: `2026-07-28`.
- stdio transport only.
- no HTTP/WebSocket/network service.
- no generic command runner.
- no `initialize` compatibility path for the retired handshake.

Startup accepts exactly:

```text
perfectpixel-mcp --root <ABSOLUTE_EXISTING_DIRECTORY>
perfectpixel-mcp --help
```

The configured root is canonicalized once and remains fixed for the process.

## Canonical dispatch

```text
MCP typed *Params
-> root/path admission
-> canonical scalar constructors/parsers
-> Operation
-> shared application dispatch
-> domain / Effect
-> verification
-> checked publication
-> ApplicationOutput
-> MCP result envelope
```

There is no `ApplicationRequest`, argv reconstruction, CLI subprocess, or CLI parser re-entry.

CLI and MCP can expose different inventories. Operations present in both transports reach the same typed `Operation` and application implementation.

## Tool inventory

Exactly ten tools are exposed:

| Tool | Parameters | Mutation |
| --- | --- | --- |
| `perfectpixel_schema` | `{}` | none |
| `perfectpixel_inspect` | `inputPath` | none |
| `perfectpixel_convert` | `inputPath`, `outputPath`, optional transform fields | checked file publication |
| `perfectpixel_upscale` | `inputPath`, `outputPath`, `scale`, optional transform fields | checked file publication |
| `perfectpixel_normalize` | `requestPath`, `outputDir` | managed generation publication |
| `perfectpixel_bundle` | `requestPath`, `outputDir` | managed generation publication |
| `perfectpixel_vector` | raster/vector options | approved vector publication |
| `perfectpixel_vector_analyze` | analysis options | optional report publication |
| `perfectpixel_motion_scaffold` | `inputPath`, `outputDir` | managed generation publication |
| `perfectpixel_motion_build` | `requestPath`, `outputDir` | managed generation publication |

Compiler-only CLI operations such as layered PSD, texture compilation, and Apple Vision foreground instances are intentionally not added to the fixed MCP v1 inventory.

## Path authority

Every MCP path is root-relative and must remain inside the configured root.

The adapter rejects:

- empty or overlong paths;
- NUL and backslash forms;
- absolute paths supplied as tool parameters;
- `.` / `..` traversal;
- symlink escapes;
- non-regular input files;
- output targets whose existing parent authority escapes root.

Nested paths in normalize, bundle, and motion requests are preparsed through bounded typed request readers when possible and are checked against the same root authority. Malformed product JSON is delegated to the shared application layer so the product failure taxonomy remains canonical.

Admission retains the SHA-256 of those exact request bytes. Application execution checks its request snapshot against that digest before reading nested inputs. A changed request fails with `precondition_failed` and cannot publish output; an unchanged malformed request keeps the canonical product parsing error.

The MCP adapter does not use process CWD or MCP Roots to enlarge filesystem authority.

## Semantic validation

Transport deserialization owns only protocol shape. Product scalar validation uses the same canonical constructors/parsers as the CLI:

- positive dimensions;
- scale `>= 2`;
- JPEG quality `1..=100`;
- supported resampling filter;
- vector preset/profile/detail;
- finite unit scores;
- positive path-count limits;
- `#RRGGBB` background parsing.

Root/path rejection is an MCP preparation error. Semantic invalidity is rendered through the shared application failure taxonomy.

## Result contract

Dispatched operations use:

```json
{
  "schema": "perfectpixel.mcp-tool-result/1",
  "ok": true,
  "operation": "perfectpixel_inspect",
  "exitCode": 0,
  "result": {}
}
```

`result` is parsed from the shared application JSON output. MCP never interprets message text to decide product success.

- `exitCode == 0` means success.
- any non-zero application exit code is failure and is preserved.
- root/path admission, busy state, malformed application output, and worker failure use explicit MCP error envelopes.
- invalid application JSON can never be fabricated into success.

## Concurrency and cancellation

One MCP process permits one active product operation. A concurrent call is rejected as `busy`; calls are not queued.

Synchronous product execution runs in `spawn_blocking`. Cancelling the MCP future does **not** forcibly stop an already-running in-process publication worker. Its permit remains held until that worker completes. The caller must treat publication visibility as unknown after transport cancellation and inspect before retrying.

KTX/Vision process Effects have an explicit timeout completion and process-group termination boundary.

## Exclusions

The MCP adapter intentionally does not provide:

- generic shell/CLI passthrough;
- network transport;
- runtime root replacement;
- authentication/session state owned by PerfectPixel;
- prompts/resources/sampling surfaces;
- an independent publication implementation.

See [CONTRACT.md](CONTRACT.md), [ARCHITECTURE.md](ARCHITECTURE.md), and [FUNCTION_MATRIX.md](FUNCTION_MATRIX.md) for product semantics.
