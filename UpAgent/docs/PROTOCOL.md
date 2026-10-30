# CLI and task protocol

## Role

This document defines the JSON boundary of the `agent` binary: ingress values,
composition admission, durable task evidence, result statuses, and exit codes.
The source of truth is `crates/agent-cli/src/main.rs` and the public types in
`crates/agent-core/src/{composition,task}.rs`.

## Non-role

Provider frames are described in [AXD integration](AXD_INTEGRATION.md), review
campaign JSON is described in [Review campaign](REVIEW_CAMPAIGN.md), and RAX
operation rules are described in [Architecture](ARCHITECTURE.md). This page
does not define an external composition producer.

## Public surface

The binary is `agent` (`crates/agent-cli`). Global options are accepted before
or after the subcommand:

| option | environment | behavior |
|---|---|---|
| `--state-root <PATH>` | `AGENT_STATE_ROOT` | durable runtime root |
| `--skills-root <PATH[,PATH...]>` | `AGENT_SKILLS_ROOT` | skill discovery roots |
| `--maximum-parallel-tasks <N>` | — | `1..=64`, default `4` |

The stock CLI ingress is a provider-neutral `AgentSpec` schema version `5`, a
task-only `TaskEnvelopeV3` schema version `3`, and a linked provider plugin
envelope. The envelope has `pluginId`, `manifestSha256`, and plugin-owned
`configuration`; `agent plugins` lists valid linked provider/profile IDs and
manifest digests. `agent compose --spec <PATH> --provider-config <PATH> --request
<PATH>` resolves the policy, verifies the selected catalog entry, derives the
provider `ProviderBinding`, and resolves the task without opening provider
transport or runtime state. `agent run` repeats the same resolution, requires
the exact digest, persists the create-only composition receipt, then connects
the selected provider. Embedding hosts may call the public composition API with
a different linked-Rust `CompositionPluginCatalog`; the selected provider
plugin returns the `ProviderBinding` and `ProviderPlanes` used by the matching
runtime. No dynamic dylib, PATH, or Wasm loader is part of this contract.

| command | input/output |
|---|---|
| `profiles` | built-in profile manifests as JSON |
| `plugins` | linked provider/profile plugin IDs and manifest digests |
| `skills` | discovered skill catalog as JSON |
| `compose` | provider-unconnected, provider-bound `CompositionReceipt` as JSON |
| `schema task-v3|agent-spec|composition|result|profile|shared-contract` | generated JSON Schema or shared-contract catalog |
| `doctor` | environment and workspace diagnostics as JSON |
| `run` | task execution result as JSON |
| `inspect --session <ID> --task <ID>` | read-only task evidence as JSON |
| `recover task-list` | incomplete task records |
| `recover task-resolve --session <ID> --task <ID> --resolution acknowledged|discarded|retry_with_new_task` | resolve one incomplete task |
| `recover effect-list --workspace <PATH>` | unresolved effect runs |
| `recover effect-resolve --workspace <PATH> --run-id <ID> --resolution verified_succeeded|verified_failed|manual_review_required [--note <TEXT>]` | resolve one effect outcome |
| `memory snapshot --scope <SCOPE>` / `memory recall --scope <SCOPE> --query <TEXT> [--maximum <N>]` | scoped memory operations; recall defaults to `20` |
| `approvals list` / `approvals approve --subject <SHA256>` / `approvals deny --subject <SHA256>` | operator decisions |

Provider selection is a generic envelope, not a Vergerail-shaped top-level
object. The linked catalog owns the configuration schema and the host checks
the manifest digest before calling it:

```json
{
  "pluginId": "upagent.provider.vergerail",
  "manifestSha256": "<linked-manifest-sha256>",
  "configuration": {
    "protocolSchema": "vergerail.upagent/2",
    "executable": "/absolute/path/to/provider",
    "executableSha256": "<sha256>",
    "runtimePackage": "/absolute/path/to/runtime",
    "managedHome": "/absolute/path/to/home",
    "model": "model-id",
    "workspace": "/absolute/path/to/workspace"
  }
}
```

The stock linked provider is Vergerail today; another trusted linked-Rust
provider can be registered in `CompositionPluginCatalog` without changing this
CLI/core envelope contract. Dynamic dylib, PATH, and Wasm loading are not
supported.

The JSON task fields include `taskId`, `sessionId`, `objective`, optional
`parentTaskId`, `imageTask`, `history`, `workspace`, `context`, `budget`, and
`approvals`. `AgentSpec` supplies the profile and authority fields. When image
tools are selected, its `image` field binds the absolute PerfectPixel
executable, executable SHA-256, protocol/behavior versions, implementation
version, and capability-manifest SHA-256. An optional `semanticMask` field
selects the external natural-image mask Effect explicitly:

```json
{
  "semanticMask": {
    "executable": "/absolute/path/to/semantic-mask-helper",
    "executableSha256": "<64 lowercase hexadecimal characters>",
    "helperIdentity": "vendor.semantic-mask/1",
    "helperVersion": "2026.08",
    "timeoutMs": 30000
  }
}
```

`semanticMask` is valid only when `image_extract` is selected. Composition
canonicalizes and hashes the regular non-symlink executable before creating
the receipt; `timeoutMs` is bounded to `1..=600000`. Runtime construction
passes the same artifact-store root to the pinned process adapter. If the
field is absent, `image_extract` uses an explicit unavailable adapter. No
executable is discovered from `PATH`, and helper digest drift fails before
image execution. Task context does not grant capabilities. Unknown fields are
rejected by the public typed inputs.

The task limits are enforced before execution:

| value | bound |
|---|---|
| objective | `1..=256 KiB` |
| history | at most `128` turns, each `1..=256 KiB`, total `8 MiB` |
| requested skills | at most `32` |
| context keys | at most `128`; encoded context at most `256 KiB` |
| approvals | at most `16` |
| transitions / model calls / tool calls | `1..=1024` / `1..=64` / `0..=256` |
| output / timeout | `1 KiB..=64 MiB` / `1 s..=24 h` |

`AgentResult` returns `schemaVersion`, task/session identity, profile and track,
state/status, optional output, artifact and effect receipt references,
composition digest, review subject bundle, approval request, failure, and
metrics. Statuses are `succeeded`, `suspended`, `awaiting_input`,
`awaiting_approval`, `failed`, `cancelled`, and `resolution_required`.

| result status | process exit |
|---|---:|
| `succeeded` | `0` |
| `suspended`, `awaiting_input`, `awaiting_approval` | `10` |
| `resolution_required` | `20` |
| `failed`, `cancelled` | `1` |
| CLI parse or execution error | `2` |

Results and errors are JSON on stdout. Help and version output are the plain
text exceptions. A stopped task can be inspected without changing its evidence;
an unresolved external outcome is represented by `resolution_required` and is
handled through the `recover` commands.

Runtime state is rooted at `state-root`:

```text
compositions/<sessionId>/<taskId>/composition.json
sessions/<sessionId>/<taskId>/        request, events, result, locks
artifacts/                             content-addressed task artifacts
approvals/                             durable operator decisions
memory/                                scoped memory records
effects/<workspace-digest>/           effect receipts and recovery records
```

Suspension checkpoint의 내부 `schemaVersion`은 **3**이다. v3는 image repair를 bool이 아니라 exact typed obligation으로 직렬화한다. 안전하게 복원할 수 없는 v2 pending-repair checkpoint는 clean-break로 거절하며 자동 migration이나 추정 복구를 하지 않는다.

## Primary flow

```text
AgentSpec v5 + ProviderPluginEnvelope + TaskEnvelopeV3
  → typed validation and profile/skill/tool/provider-capability resolution
  → `compose` prints the composition digest
  → `run --expect-digest` checks the same digest
  → create-only composition receipt
  → task journal, provider/effect execution, artifacts and result
  → `inspect` or `recover` reads/resolves durable evidence
```

Cancellation is delivered to the runtime; provider cancellation drains the
child process and does not retry it. Task and effect recovery remain explicit
when the outcome cannot be established.

## Verification

```bash
python3 scripts/verify-agent-only.py
cargo test --locked -p agent-core -p agent-cli -p agent-runtime
./scripts/verify.sh
```

The static check validates member and projection invariants. The focused tests
cover schema, admission, journal, recovery, and exit-code behavior; the full
script additionally builds release binaries and runs the CLI smoke commands.

## Image protocol M0–M8

Image-capable compositions require provider capabilities `model/image_observation` and, when generation is selected, `model/image_generation`. The stock Vergerail adapter exposes these through the canonical `vergerail.upagent/2` contract. Logical model messages keep only content-addressed `ArtifactRef` values in `contentParts`; runtime-owned bytes are re-resolved and verified immediately before provider dispatch. See [IMAGE_PROTOCOL_V2.md](IMAGE_PROTOCOL_V2.md).

An image task is an explicit `TaskEnvelopeV3.imageTask` using
`upagent.image-task/5`. Its typed `perfectpixel.agent-image/verification-spec/1`
owns the baseline artifact, candidate kind, ordered assertions, severity, and
thresholds. The runtime binds the specification digest and current candidate
revision/full `ArtifactRef` plus typed kind to each compare call; stale, duplicate, malformed, or
identity-drifted results cannot qualify completion. AgentSpec schema v5 binds
the PerfectPixel executable and the canonical capability-manifest digest, so
an image operation revalidates the complete M8 identity before execution. The
The image boundary carries an explicit `PixelSpec` and a content-addressed
deterministic `TextNode` contract. UpAgent accepts `rgba8/srgb/straight` working
pixels. Optional PNG RGBA16/Display-P3 conversion belongs to standalone
PerfectPixel and is not exposed by the UpAgent image tools.

The stock Vergerail adapter exposes text, local-image observation, and image
generation through the single `vergerail.upagent/2` contract. An image-capable
turn carries verified `observations` plus an UpAgent-owned request-specific
`stagingRoot`; the provider materializes typed local-image inputs and UpAgent
removes that exact private root after process reap on every success, failure,
timeout, write, drain, and cancellation path. The final serialized request is
admitted at 64 MiB, with at most 16 observations, 8 MiB per observation, and
32 MiB aggregate decoded bytes. `managedHome` is passed as `CODEX_HOME`; provider
processes receive no second home-owner environment mapping.

## Optional promptGen와 image review extension

AgentSpec schemaVersion 5의 optional `promptgen` object는 `executable`, `executableSha256`, `timeoutMs`, optional `codexRuntime`을 갖는다. `codexRuntime`은 semantic edit가 명시적으로 선택된 경우에만 필요하며 absolute `executable`, `executableSha256`, `home`을 고정한다. unknown field는 거절하며 tool 선택과 분리한다. public JSON schema는 `agent schema agent-spec`의 Rust type projection이 원본이다.

`promptgen_compile`은 inputJson XOR inputArtifact의 **promptGen image generate request**만 허용하고 `upagent.image-prompt-artifact/3`을 만든다. artifact는 task/compiler/raw-input/normalized-request/compiler-outcome/prompt digest와 width/height/background/detail을 결합한다. detail은 프롬프트 의도이며 provider-native quality 설정이 아니다. `image_generate`는 raw prompt XOR promptArtifact+promptSha256을 사용하고 artifact의 size/background를 적용한다. 품질은 별도의 optional provider control이다. `image_edit_semantic`은 artifact/candidateRevision/inputJson을 사용해 PromptGen `codex-imagegen`을 명시적 Codex runtime SHA/home과 후보 1회 제한으로 호출한다. after-dispatch uncertainty는 기존 resolution result를 사용한다. screen compile과 PromptGen standalone Web/MCP/Codex state는 UpAgent tool contract가 아니다. 전체 기준은 [Image Director](IMAGE_DIRECTOR.md)를 따른다.

`IMAGE_REVIEW:`는 active image task의 internal model response이고 일반 user-facing result나 tool name이 아니다. strict bound evidence가 기록된 후 별도 final turn을 요구한다. ResolutionRequired event에 포함된 optional artifact refs는 callId와 결합된 forensic evidence이며 정상 candidate와 구별한다.
