# Verification

## 전체 검증

저장소의 canonical local gate는 다음 명령이다.

```bash
./scripts/verify.sh
```

`verify.sh`는 순서대로 다음을 확인한다.

1. `.github/workflows` 금지와 zero-legacy 경계: `vendor/RustProviderKit` 부재,
   workspace/root/member Cargo manifest의 package·dependency·path·source 및
   `[patch.<registry>]`·`[replace]` 항목에서 case-insensitive
   `rust-provider-kit`/`RustProviderKit` 부재,
   active Rust source에서 해당 legacy 명칭 부재
2. Cargo metadata와 `cargo fmt --all -- --check`
3. `cargo check --locked --workspace --all-features`
4. `cargo clippy --locked --workspace --all-targets --all-features -- -D warnings`
5. `cargo test --locked --workspace --all-features`
6. `cargo build --locked --release -p agent-cli -p agent-review-cli`
7. release `agent`의 `profiles`, `skills`, `schema task-v3`, `doctor` smoke와 `agent-review --help`
8. workspace tests의 stopped-task `inspect` projection과 CLI parser 오류 JSON stdout contract
9. `agent-provider`의 deterministic Vergerail JSONL model/tool/image process contract, bounds, home lock, cancellation, and no-retry tests
10. creator image module fake provider→strict PerfectPixel artifact handoff tests, including multimodal observation, extract/render/compare protocol shaping and profile reasoning propagation
11. fake-mbox wrapper의 argument와 fail-closed contract
12. provider-unconnected compose와 `run --expect-digest` mismatch의 pre-state/provider-connection boundary
13. `scripts/verify-agent-only.py`의 workspace/manifest/static boundary

성공 시 마지막 줄은 `VERIFY: PASS`다. Vergerail managed-home credential, 외부 AXD
adapter, 실제 provider network 호출은 이 gate에 포함되지 않는다.

## 플랫폼 경계

현재 지원·검증 기준은 Apple silicon macOS (`aarch64-apple-darwin`)다.
Windows는 명시적인 비지원 범위이며 build, runtime, signing, security
acceptance 대상에서 제외한다. Linux도 현재 release 증거에는 포함되지 않으며
별도 지원을 주장하지 않는다.

## agent-only 경계 검증

```bash
python3 scripts/verify-agent-only.py
```

이 명령은 workspace member·path dependency·front implementation reference와
contract projection hash를 정적으로 확인한다. 또한 하나의 canonical zero-legacy
invariant로 `vendor/RustProviderKit` 경로, root와 workspace member Cargo manifest의
package·dependency·path·source 및 `[patch.<registry>]`·`[replace]` 항목, active Rust
source를 case-insensitive하게 확인한다. generated target, recovery archive와 historical external path는 이
검사 범위가 아니다. 전체 검증은 이 static gate를 포함하는 `scripts/verify.sh`로
실행한다.

checker의 path·manifest·Rust source 재도입 회귀는 다음 temp-copy mutation test로
검증한다.

```bash
python3 scripts/test-verify-agent-only.py
```

## CLI smoke와 실제 사용

release binary를 직접 확인할 때는 다음을 실행한다. provider envelope 설정은
[통합 운영 가이드](../../docs/OPERATIONS.md#3-general-llm)를 따른다.

```bash
target/release/agent plugins
target/release/agent profiles
target/release/agent --skills-root ./skills skills
target/release/agent schema task-v3
target/release/agent schema agent-spec
target/release/agent schema result
target/release/agent schema profile
target/release/agent --state-root ./state --skills-root ./skills doctor
target/release/agent --state-root ./state inspect \
  --session "${SESSION_ID:?set SESSION_ID}" --task "${TASK_ID:?set TASK_ID}"
target/release/agent-review --help
```

`AgentSpec`은 provider-neutral schema v5이며 `isolation`은 `{ "kind": "native" }` 또는 명시적
`host_adapter` object다. `compose` 출력의 digest를 다른 값으로 바꾼 run은
composition state directory를 만들거나 credential/provider를 읽기 전에 실패해야
한다. 동일 검사는 selected skill 또는 Tier-2/3 JSON package의 1-byte drift에도
적용된다. `review: emit_subject_bundle` 성공은 result/inspect에서 bundle bytes와
task/composition/result/evidence hash를 다시 검증해야 한다.

실제 provider task는 authenticated managed home와 network가 필요하므로 별도 증거다. 먼저 같은
spec/request/provider plugin envelope로 provider-unconnected composition digest를 얻어야 한다.

```bash
PROVIDER_ENVELOPE=/absolute/path/to/provider-envelope.json
digest="$(target/release/agent --skills-root ./skills compose \
  --spec examples/agent-spec-creator.template.json \
  --provider-config "$PROVIDER_ENVELOPE" \
  --request examples/creator.json | \
  python3 -c 'import json,sys; print(json.load(sys.stdin)["compositionDigest"])')"

target/release/agent \
  --state-root ./state \
  --skills-root ./skills \
  run \
  --spec examples/agent-spec-creator.template.json \
  --provider-config "$PROVIDER_ENVELOPE" \
  --request examples/creator.json \
  --expect-digest "$digest"
```

실제 provider 호출을 실행하지 않았다면 provider flow를 `PASS`로 기록하지
않는다. 외부 AXD composition adapter와 production effect 또한 이 저장소의
local PASS로 대체할 수 없다.

같은 runtime 내부 writer 직렬화는 workspace tests에 포함된다. 별도 process나 IDE가
동일 Git workspace를 동시에 바꾸지 않는다는 보장과 `inspect_diff`의 원자적 snapshot은
이 gate에 포함되지 않는다. 운영 E2E 검증에는 host가 만든 task 전용 workspace를
사용하고, 결과의 Git 관찰을 한 시점의 증거로 기록한다.

## mbox wrapper

mbox를 설치하지 않아도 wrapper의 argument와 fail-closed 계약은 fake
adapter로 확인할 수 있다. 기본 wrapper는 workspace read/write, `.git`
default deny, optional existing 0700 `UPAGENT_TMP_PATH`, file 또는 directory인
추가 read/write path, explicit environment pass, binary SHA-256 pin을
전달한다. `UPAGENT_PROVIDER_DOMAINS`의 각 domain은 반복 `--allow-net`으로
전달된다. macOS mbox의 네트워크 보장은 초기 TLS ClientHello의 exact SNI와
CONNECT authority 일치까지이며, TLS 1.3 HRR 후속 ClientHello나 application
layer domain 검증까지를 의미하지 않는다.

```bash
./scripts/test-run-with-mbox.sh
```

실제 strict host containment가 필요하면 `scripts/run-with-mbox.sh`로
`AgentSpec`의 host_adapter run을 감싼다. wrapper는 실행 전 mbox binary와 ordered
policy arguments를 hash하고 AgentSpec의 `adapterSha256`/`policySha256`와 직접
비교한다. 불일치하면 child를 시작하지 않으며, 커널에는 spoofable launcher flag를
전달하지 않는다. receipt와 journal의 evidence는
`operator_declared_host_recipe`뿐이며 containment attestation이 아니다. 실제 macOS
containment는 별도 운영 증거다.
이 hash 비교는 file-backed `agent run --spec`의 직접 호출 경로에만 적용되며,
wrapper에 전달되는 임의 명령은 AgentSpec binding을 받지 않는다.

## 정리 검증

삭제 전에 후보와 크기를 확인한다.

```bash
./scripts/cleanup_inventory.sh .
./scripts/clean.sh
```

`clean.sh`는 루트의 Cargo `target/`, Python `__pycache__`/`.pyc`, `.DS_Store`와
명시된 임시 파일만 제거하며 source, manifest, `Cargo.lock`, 문서와 사용자
state는 보존한다. GitHub Actions,
CI/CD workflow, release automation은 검증 대상이나 생성 대상이 아니다.

## M0–M8 image gates

The image milestone requires all of the following when a Rust toolchain is available:

```bash
cargo test -p agent-core -p agent-provider -p agent-runtime
cargo clippy -p agent-core -p agent-provider -p agent-runtime --all-targets -- -D warnings
```

Additional contract checks:

- PerfectPixel required operations are subset-compatible with additive capabilities.
- image-tool compositions require the generic `model/image_observation` capability; `image_generate` also requires `model/image_generation`. The stock Vergerail adapter exposes both through the canonical `vergerail.upagent/2` contract.
- durable messages contain artifact references, not absolute paths or base64 payloads.
- provider sidecars are re-bound to artifact SHA-256 and PNG dimensions before dispatch.
- PerfectPixel v2 inspect receipts bind request hash and dependency closure.
- `TaskEnvelopeV3.imageTask` is `upagent.image-task/5`; a provided task binds its source `ArtifactRef` directly in the task contract, and deterministic producers accept only pixel-bearing inputs already in the task-local provenance closure. The contract binds an exact `perfectpixel.agent-image/verification-spec/1` when deterministic checks are required.
- AgentSpec schema v5 binds the PerfectPixel executable SHA-256, protocol and
  behavior versions, implementation version, and canonical capability-manifest
  SHA-256. The runtime rechecks these values before each image operation.
- `extract.semantic_mask` is an explicit Effect boundary. The unavailable local
  adapter returns a structured failure/cancellation result; `AgentSpec`
  `semanticMask` configuration is composition-pinned by absolute executable
  SHA-256/helper identity/version and bounded timeout, then injected by the CLI
  with the runtime artifact-store root. Its process adapter bounds JSON/PNG
  transport and proves exact process-group absence after graceful/forced
  cleanup on success, failure, cancellation, and timeout. Semantic effects bind a source revision derived from the current candidate revision; a result from an older candidate transition is stale and cannot change current state. On Unix, `waitid`
  with `WEXITED|WNOHANG|WNOWAIT` observes the leader without releasing its PID;
  exact numeric PGID cleanup and membership proof finish before the one leader
  reap. Source artifact,
  source revision, helper identity, request revision, and tool-call identity are
  revalidated before a mask can enter authoritative state.
- UpAgent image tools reject PixelSpec values outside `rgba8/srgb/straight`.
  Standalone PerfectPixel's opt-in PNG RGBA16/Display-P3 conversion is a
  separate contract. TextNode font bytes are SHA-256-bound with explicit
  missing-glyph/unsupported-script failure and no system-font fallback.
- Canonical render operations cover the retained crop/rotate/flip/nearest/
  Lanczos3/keyed/automatic-background parity corpus. The standalone human
  `edit` CLI is separate from the model-visible agent surface.
- The cross-process all-assertion contract test is run explicitly with the
  built PerfectPixel binary:

```bash
PERFECTPIXEL_SOURCE=/absolute/path/to/perfectpixel \
PERFECTPIXEL_BIN=/absolute/path/to/perfectpixel/target/debug/perfectpixel \
  cargo test --locked -p agent-runtime real_perfectpixel_round_trips_all_exact_compare_assertions \
  -- --ignored --exact --nocapture
```

This local gate does not prove authenticated provider-native multimodal
execution or host containment; those remain separate operational evidence.

## Recovery capability focused gates

변경 위험을 먼저 좁혀 검사한 뒤 canonical 전체 gate를 실행한다. 아래는 실행 방법이지 통과 기록이 아니다. 실제 결과는 [Current](IMPLEMENTATION_STATUS.md)에 있다.

```bash
cargo build --locked --workspace
cargo test --locked -p agent-core
cargo test --locked -p agent-runtime
cargo test --locked -p agent-cli
./scripts/verify.sh
```

관찰만으로 승인하지 않는지, 전체 candidate set digest의 순서 안정성, stale/duplicate/unknown review, compile XOR/digest/task binding, config-only/compile-only activation, semantic source/revision, receipt binding/partial opaque output/ledger를 확인한다. `runtime_flow`의 explicit IMAGE_REVIEW→separate final regression은 fake model 계약 검사로 한정한다. 실제 promptGen/PP/계정/sandbox 검증은 [PLAN](PLAN.md)의 별도 수용 기준을 따른다.
