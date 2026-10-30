# Third-party notices

PerfectPixel keeps the direct Rust dependency surface intentionally bounded. The canonical declaration is [`Cargo.toml`](Cargo.toml); [`Cargo.lock`](Cargo.lock) records the graph used by `--locked` local verification.

## Direct Rust dependencies

| Component | Requirement | Purpose |
| --- | --- | --- |
| `binpack2d` | `=1.0.1` | deterministic sprite atlas packing |
| `fast_image_resize` | `=6.1.0` | bounded raster resampling |
| `image` | `0.25` | PNG/JPEG/WebP decode and encode boundary |
| `libc` | `0.2` | Unix process-group termination and cleanup at external Effect timeout |
| `resvg` | `0.47`, default features disabled | SVG render-back verification |
| `rmcp` | `=3.1.4`, default features disabled | typed local stdio MCP transport |
| `roxmltree` | `=0.21.1` | bounded XML/SVG parsing support |
| `schemars` | `1` | MCP/request schema generation |
| `serde` | `1` | typed serialization |
| `serde_json` | `1` | strict JSON contracts and receipts |
| `sha2` | `=0.10.9` | artifact, input, and executable SHA-256 identities |
| `rustybuzz` | `=0.20.1` | deterministic font shaping |
| `ttf-parser` | `=0.25.1` | bounded font and glyph metadata parsing |
| `unicode-script` | `=0.5.8` | script classification for text validation |
| `svgtypes` | `=0.16.1` | SVG path parsing |
| `thiserror` | `1` | typed error definitions |
| `tokio` | `1`, `macros`/`rt-multi-thread`/`sync` | MCP async transport/admission boundary |

The pinned package metadata licenses `rustybuzz` under MIT and `sha2`, `ttf-parser`, and
`unicode-script` under MIT OR Apache-2.0. Their source packages retain the corresponding
license files.

`moxcms` may appear transitively through the selected `image` crate; PerfectPixel does not call it or treat it as a color-semantic authority. `fast-ssim2` is not a dependency in this release. Embedded ICC provenance is retained and a required ICC conversion fails explicitly rather than silently assuming sRGB. Perceptual color verification uses the in-repository CIEDE2000 implementation together with exact, alpha, geometry, and region gates.

Cargo.lock may contain other transitive/build packages required by the direct crates above. A transitive package does not own PerfectPixel semantics or success policy.

## External runtime tools

KTX2 compilation uses a caller-supplied `ktx` executable pinned by SHA-256 in each request. It is treated as an external Effect candidate producer, not bundled product authority.

Apple Vision integration uses the helper source under `tools/apple-vision-helper/`. The locally built helper executable is pinned by SHA-256 in the request; Vision inference does not decide product success.

## Source-only publication limits

This repository publishes source, manifests, lockfile, tests, documentation, and Apple Vision helper source. It does not claim environment-specific build/runtime evidence unless that evidence was actually produced by the verifying host.
