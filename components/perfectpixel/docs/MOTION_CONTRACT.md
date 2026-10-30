# Agent-authored motion contract

PerfectPixel motion v1 turns an already-vectorized SVG into an agent-editable scene and compiles a
bounded transform/opacity animation. It does not guess semantic body parts or invent poses.

## Commands

```text
perfectpixel motion-scaffold <input.svg> --out-dir <dir>
perfectpixel motion-build --request <motion-request.json> --out-dir <dir>
```

`motion-scaffold` validates that the SVG is raster-free, flattens supported geometry into
transform-free paths, assigns deterministic `pp-path-NNNN` identifiers, and writes:

- `scene.svg`
- `layers.json`
- `motion-request.json`
- `layer-inspector.html`

The agent uses the inspector and layer list to group path IDs into semantic parts. Each layer
includes paint, stacking order, parsed bounds, shape count, and `relatedPathIds` suggestions for
overlapping similarly colored boundary bands. Shift-click in the inspector selects those
suggestions together. A part can be a mouth variant, eye, head, leg, tail, whole pose, or any other
authored unit; suggestions are evidence, not automatic semantic truth.

## Request schema

```json
{
  "schema": "perfectpixel.motion/1",
  "name": "dog-walk",
  "sourceSvg": "scene.svg",
  "sourceSvgSha256": "7b58cbe26c4f0f7e9627c0e9673a0fd1514ac10b53bc3f4e6bd9c6ea838f760d",
  "fps": 30,
  "durationMs": 1600,
  "loop": true,
  "authoredPaths": [
    {
      "id": "tear-left",
      "d": "M485 690 C465 720 470 748 485 755 C500 748 505 720 485 690 Z",
      "fill": "#4bb6ff",
      "fillOpacity": 1
    }
  ],
  "parts": [
    {
      "id": "front-leg",
      "pathIds": ["pp-path-0042", "pp-path-0043"],
      "anchor": [612, 890]
    }
  ],
  "tracks": [
    {
      "part": "front-leg",
      "interpolation": "linear",
      "keyframes": [
        { "atMs": 0, "translate": [0, 0], "rotateDeg": -12, "scale": [1, 1], "opacity": 1 },
        { "atMs": 400, "translate": [4, 0], "rotateDeg": 15, "scale": [1, 1], "opacity": 1 },
        { "atMs": 800, "translate": [0, 0], "rotateDeg": -12, "scale": [1, 1], "opacity": 1 }
      ]
    }
  ],
  "markers": [
    { "name": "walk", "fromMs": 0, "toMs": 800 }
  ]
}
```

`authoredPaths` is optional. It lets an agent add bounded path-only variants such as a smile, closed
eye, or tear without rewriting the generated `scene.svg`; those IDs participate in `parts` exactly
like source path IDs. All keyframes contain a complete pose. Expressions use authored variant parts
and opacity tracks: for example `mouth-neutral`, `mouth-smile`, and `mouth-cry`. Walking uses
separate leg/body parts or authored pose variants. The compiler does not infer which pixels form a
mouth or leg. Track `interpolation` is `linear` by default. Use `hold` for mutually exclusive
replacement layers such as expression mouths or eyes; the compiler emits CSS `steps(1,end)` and
Lottie hold keyframes so two variants do not cross-fade into a duplicate expression.

The scaffold writes `sourceSvgSha256` from the exact reparsed `scene.svg`; `motion-build` verifies that provenance binding before authored-path injection or output allocation. The digest does not grant vector publication authority.

## Build outputs

- `animated.svg`: dependency-free CSS-keyframe preview preserving SVG stacking order.
- `animation.json`: classic Lottie JSON accepted by Lottie/dotLottie players.
- `dotlottie/manifest.json`: dotLottie v2 manifest.
- `dotlottie/a/<name>.json`: the animation at the v2 package path.
- `motion-report.json`: dimensions, parts, tracks, markers, and compatibility facts.
- `preview.html`: local animated-SVG preview plus an optional dotLottie Web canvas preview. A
  browser imports dotLottie Web from jsDelivr when the file is opened; if that external import
  fails, the animated-SVG preview remains available.

The `dotlottie/` directory is an exploded, package-ready v2 layout. v1 does not claim to create a
compressed `.lottie` archive. A later pack step may use dotLottie tooling to Deflate this directory.

## Invariants

- Source and outputs must contain path geometry and no raster payload.
- IDs and part names use lowercase ASCII letters, digits, and single hyphens.
- Path IDs exist in the exact `sourceSvg` bytes bound by `sourceSvgSha256`; one path belongs to at most one animated part.
- Authored path IDs are unique, use safe path data, and cannot replace a source path ID.
- `fps` is 1 through 120; duration is 1 through 600,000 milliseconds.
- Keyframes are strictly increasing, start at 0, and do not exceed duration.
- Track interpolation is `linear` or `hold` and is applied consistently to SVG and Lottie output.
- Translate/rotate values are finite; scale values are finite and positive; opacity is 0 through 1.
- Markers are unique and remain within the animation duration.
- Unassigned SVG paths remain static.
- Lottie v1 supports shape paths with line, cubic, smooth cubic, quadratic, and close commands.
  Elliptical arcs and unsupported SVG transforms fail with an actionable error.

## Compatibility boundary

Motion v1 supports layer transform and opacity animation. It intentionally excludes automatic
semantic segmentation, automatic path invention, path morphing, bones/IK, physics, expressions,
masks, filters, text, audio, and direct `.lottie` ZIP creation. These features require separate
versioned contracts because renderer support differs.
## Opt-in structural assessment

`perfectpixel.motion-assessment/1` records motion-structure acceptance without granting vector
publication authority.

Assessment authority is embedded in the Vectorizer route table. Only that internal flow can bind a
promoted route to the exact backend candidate bytes and produce a pass. The public
`assess_motion_structure(MotionAssessmentRequest)` surface is intentionally non-authoritative:
unrequested calls return `notEvaluated/unrequested`; requested calls return
`notEvaluated/prerequisiteUnavailable` and never emit approved output. Assessment requests are not
deserializable.

The authorized assessor checks the source byte limit before hashing or copying, consumes the bounded
shared SVG IR, requires pre-existing unique stable path IDs, and bounds paths and geometry. It does
not assign IDs or infer labels, hints, parts, poses, or starter motion.

A pass SHA-256-binds `sourceSceneSha256`, `scaffoldSha256`, `assessedSceneSha256`, and
`motionContractVersion`; approved motion bytes are byte-for-byte identical to the reparsed assessed
candidate. Failed or non-authoritative requests contain no approved output bytes. Assessment and
scaffolding share the same safety cap: 1,048,576 source bytes, 512 paths, and 524,288 aggregate
path-data bytes.
