# UpAgent

UpAgent는 host가 선택한 `AgentSpec`과 task JSON을 먼저 고정한 뒤, 하나의
`AgentRuntime`에서 provider turn, typed tool, RAX effect, memory, artifact와
durable result를 실행하는 Rust workspace다. 공개 실행 파일은 `agent`와 독립
리뷰 실행기 `agent-review`다.

## 범위

UpAgent는 composition 이후의 agent 실행과 독립적인 읽기 전용 review campaign을
소유한다. provider executable, runtime package, managed home, PerfectPixel 실행 파일,
외부 host adapter는 명시적인 process 계약으로만 연결된다. 외부 composition adapter,
GUI/TUI, 동적 코드 loader, 전역 workspace writer fence는 이 workspace의 실행 표면이
아니다.

## 구조

직접 관리하는 디렉터리는 다음과 같다.

| 경로 | 역할 |
|---|---|
| `contracts/` | 외부 계약의 immutable JSON projection과 hash index |
| `crates/` | Rust workspace의 kernel, runtime, provider, review 구현 |
| `docs/` | 책임별 설계·프로토콜·검증 문서 |
| `examples/` | `AgentSpec`, task, batch 입력 예시 |
| `scripts/` | 검증·정리·host wrapper 명령 |
| `skills/` | profile별 `skill.json`과 `SKILL.md` |

crate 책임은 manifest와 public entrypoint 기준으로 다음과 같다.

| crate | 책임 |
|---|---|
| `agent-core` | 계약 DTO, ID, 상태·이벤트 전이, 결과 |
| `agent-plugin-kernel` | typed plugin manifest와 graph contract (tools/verifiers/provider/profiles) |
| `agent-profiles` | profile manifest, track policy, model/tool plan |
| `agent-skills` | bounded skill catalog와 data-only extension |
| `agent-memory` | scoped memory, provenance, revision CAS |
| `agent-provider-api` | replaceable `ModelPort`/`ImagePort`, generic provider failure contract |
| `agent-provider` | canonical `vergerail.upagent/2` Vergerail adapter와 linked-Rust provider plugin |
| `agent-effects` | semantic tool을 RAX operation으로 변환 |
| `rax-core` | RAX operation, plan, protocol, result contract |
| `rax-runtime` | workspace effect 실행, process/Git, receipt·recovery |
| `agent-runtime` | task session, journal, artifact, approval, tool loop |
| `agent-cli` | `agent` binary, composition, JSON I/O, signal·exit code |
| `agent-review` | provider-neutral 독립 `ReviewCampaign` library |
| `agent-review-cli` | linked provider/profile plugin composition root + `agent-review` binary |

## 요구 환경

`rust-toolchain.toml`이 지정하는 Rust `1.99.1`, `rustfmt`, Clippy가 필요하다.
`AgentSpec` schema v5는 provider-neutral agent policy다. stock CLI에서 provider를
선택할 때는 별도 provider plugin envelope JSON에 `pluginId`, `manifestSha256`, 그리고
plugin-owned `configuration`을 채운다. 현재 linked-Rust stock provider의 configuration에는
절대경로 executable과 SHA-256, pinned runtime package, managed home, model,
workspace가 들어간다. `protocolSchema`는 유일한 `vergerail.upagent/2` 계약이다.
`CompositionPluginCatalog`가 전체 linked-Rust provider/profile
manifest graph를 검증하고 envelope의 plugin ID·manifest digest를 확인한 뒤 해당 plugin에
configuration을 전달한다. 선택된 provider/profile plugin ID와 manifest SHA-256은
composition receipt에 봉인된다. `managedHome`은 upstream Codex가 읽는 `CODEX_HOME`으로
provider process에 명시적으로 전달된다. embedding host는 같은
`ProviderPlugin`/`ProfilePlugin`과 `ProviderBinding + ModelPort` 계약을 만족하는 다른
linked-Rust 구현으로 교체할 수 있다. 동적 dylib/PATH/Wasm loader는 제공하지 않는다.
semantic natural-image mask를 사용할 때만 `AgentSpec.semanticMask`에 절대경로
helper, executable SHA-256, helper identity/version, `timeoutMs`를 명시한다.
구성하지 않으면 semantic adapter는 명시적 unavailable 결과를 반환하며, `PATH`
자동 탐색은 없다.

## 빌드·검증·정리

canonical 전체 검증은 다음 한 명령이다.

```bash
./scripts/verify.sh
```

개별 빌드와 저장소 정리는 다음 명령을 사용한다.

```bash
cargo build --locked --workspace
cargo build --locked --release -p agent-cli -p agent-review-cli
./scripts/cleanup_inventory.sh .
./scripts/clean.sh
```

`clean.sh`는 Cargo 산출물과 저장소가 명시한 재생성 가능한 임시 파일만 정리한다.
검증 순서와 smoke 범위는 [Verification](docs/VERIFICATION.md)에 있다.

## 최소 compose → run 흐름

이 source package는 `bin/agent`/`bin/agent-review`를 포함하지 않는다. 먼저 위 release build를 수행한다.
[통합 운영 가이드](../docs/OPERATIONS.md#3-general-llm)에 따라 실행할 `agent`의
plugin manifest digest와 provider 실행 파일 SHA-256을 넣은 provider envelope를 준비한다.
실제 runtime·인증 검증 범위는 [Current](docs/IMPLEMENTATION_STATUS.md)를 확인한다.

`compose`는 provider process나 runtime state를 열지 않고 provider-neutral `AgentSpec` v5,
linked provider plugin envelope, task-only `TaskEnvelope` v3를 해석한다. envelope는
catalog의 ID와 manifest digest를 검증한 뒤 plugin-owned configuration으로 binding을
파생하며 실제 provider 연결/credential 접근은 하지 않는다. 출력의
`compositionDigest`를 그대로 `run --expect-digest`에 전달한다.

```bash
PROVIDER_ENVELOPE=/absolute/path/to/provider-envelope.json
digest="$(./target/release/agent --skills-root ./skills compose \
  --spec examples/agent-spec-creator.template.json \
  --provider-config "$PROVIDER_ENVELOPE" \
  --request examples/creator.json \
  | python3 -c 'import json,sys; print(json.load(sys.stdin)["compositionDigest"])')"
./target/release/agent --state-root ./state --skills-root ./skills run \
  --spec examples/agent-spec-creator.template.json \
  --provider-config "$PROVIDER_ENVELOPE" \
  --request examples/creator.json \
  --expect-digest "$digest"
```

`agent-spec-*.template.json`은 provider를 포함하지 않는 policy 입력이며 위 creator
예시는 별도 생성 없이 직접 사용한다. `compose`는 provider 실행 파일 identity를
검증하지만 runtime package나 credential의 유효성을 확인하지 않는다. 입력·출력 필드와
상태 의미는 [Protocol](docs/PROTOCOL.md), profile과 skill 확장은
[Profile development](docs/PROFILE_DEVELOPMENT.md)을 참조한다. 다른 provider나
agent policy는 `CompositionPluginCatalog`에 링크된 `ProviderPlugin`/
`ProfilePlugin` 구현을 등록해 교체한다.

## 명령 표면

`agent`의 주요 조회·진단 명령은 다음과 같다.

```bash
./target/release/agent plugins
./target/release/agent profiles
./target/release/agent --skills-root ./skills skills
./target/release/agent schema task-v3
./target/release/agent schema agent-spec
./target/release/agent schema composition
./target/release/agent schema result
./target/release/agent schema profile
./target/release/agent --state-root ./state --skills-root ./skills doctor
./target/release/agent --state-root ./state inspect \
  --session "${SESSION_ID:?set SESSION_ID}" --task "${TASK_ID:?set TASK_ID}"
```

승인과 복구 명령은 `approvals list|approve|deny`,
`recover task-list|task-resolve|effect-list|effect-resolve`, memory 명령은
`memory snapshot|recall`이다. 독립 review의 요청·route·상태 파일은
[ReviewCampaign](docs/REVIEW_CAMPAIGN.md)를 따른다.

## 문서 지도

정본: [정체성과 발전](docs/IDENTITY_AND_EVOLUTION.md) · [SPEC](docs/SPEC.md) · [ARCHITECTURE](docs/ARCHITECTURE.md) · [IMPLEMENTATION_STATUS](docs/IMPLEMENTATION_STATUS.md) · [PLAN](docs/PLAN.md).

선택적 promptGen의 별도 빌드·pin·tool 선택은 [통합 운영 가이드](../docs/OPERATIONS.md)를 따른다. 현재 이미지 계약은 [Image Director](docs/IMAGE_DIRECTOR.md), 독립 제품 정본은 [ReviewCampaign](crates/agent-review/README.md)에 있다.


- [Architecture](docs/ARCHITECTURE.md): crate 경계와 task 실행 흐름
- [Protocol](docs/PROTOCOL.md): 입력, 결과, cancellation, approval, recovery
- [Security](docs/SECURITY.md): authority, 저장 경계, host 제한
- [Profile development](docs/PROFILE_DEVELOPMENT.md): profile·skill·ToolModule 확장
- [ReviewCampaign](docs/REVIEW_CAMPAIGN.md): 독립 review request와 reducer
- [AXD integration](docs/AXD_INTEGRATION.md): 외부 composition 경계와 mapping
- [Implementation status](docs/IMPLEMENTATION_STATUS.md): 실제 연결·기능·검증·UNKNOWN
- [Performance](docs/PERFORMANCE.md): bounded resource와 cancellation 비용
- [Verification](docs/VERIFICATION.md): canonical command와 테스트 범위
- [Contracts](contracts/README.md): immutable projection boundary
