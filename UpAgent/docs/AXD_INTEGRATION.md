# AXD integration boundary

## 역할

외부 composition producer와 UpAgent 사이의 입력 경계를 정의한다. producer는 승인과
provenance를 해석해 provider-neutral `AgentSpec` schema v5와 task-only
`TaskEnvelopeV3`를 만든다. provider transport는 host composition root가 generic
provider plugin envelope를 linked catalog에서 확인한 뒤 `ProviderBinding`으로 축약한다.

## 비역할

외부 contract를 UpAgent 입력으로 변환하는 producer/adapter는 이 workspace에 없다.
`contracts/`의 JSON은 projection 보관소이며 실행 가능한 adapter가 아니다. UpAgent core는
Vergerail executable, credential, managed home 또는 다른 provider-specific 설정을 해석하지
않는다.

## Public surface

| 외부 값 | UpAgent authority |
|---|---|
| objective | task `objective` |
| profile/track | `AgentSpec.profile`, `AgentSpec.workflow` |
| task/session identity | task `taskId`, `sessionId` |
| caller workspace | task `workspace` |
| model/process route | host-owned concrete config → `ProviderBinding` |
| skill/tool selection | `AgentSpec.skills`, `AgentSpec.tools` |
| memory/effect/review policy | `AgentSpec.memory`, `effects`, `effectApproval`, `review` |
| approval grants | task `approvals` |
| informational provenance | task `context` |

이미지 작업은 `TaskEnvelopeV3.imageTask`의 `upagent.image-task/5`가 source와 verification
authority를 소유한다. `provided`는 source `ArtifactRef`를 task digest에 봉인하고,
`generate_new`는 성공한 generation 전 deterministic pixel producer를 허용하지 않는다.
`AgentSpec.image`는 PerfectPixel executable/protocol/behavior/implementation/capability
identity를 봉인한다.

semantic mask를 쓰는 composition은 `AgentSpec.semanticMask`에 absolute executable,
SHA-256, helper identity/version, bounded `timeoutMs`를 명시한다. 이 helper는 image Effect
boundary이며 provider abstraction과 독립이다.

`context`는 profile, tool, effect 또는 provider authority를 부여하지 않는다. stock CLI는
`pluginId`, `manifestSha256`, plugin-owned `configuration`을 담은 provider envelope를
`CompositionPluginCatalog`의 linked-Rust provider plugin으로 검증해 `ProviderBinding`으로
축약하고 receipt에 봉인한다. profile도 같은 catalog의
linked-Rust `ProfilePlugin`에서 공급되므로 실행·review가 동일한 선택 경계를 사용한다.
다른 host는 동일한 `ProviderBinding`과 `ModelPort` 계약을 만족하는 provider를 사용할 수
있다.

## Primary flow

```text
external composition
  → AgentSpec v5 + TaskEnvelopeV3
  → host provider plugin envelope → catalog verification → ProviderBinding
  → agent compose (provider connection/state 없이 resolution)
  → compositionDigest 확인
  → agent run --expect-digest <compositionDigest>
  → runtime verifies ModelPort.binding == receipt.provider
  → AgentResult
```

Stock Vergerail CLI:

```bash
cargo build --locked --release -p agent-cli

digest="$(./target/release/agent --skills-root ./skills compose \
  --spec /absolute/path/agent-spec.json \
  --provider-config /absolute/path/provider-envelope.json \
  --request /absolute/path/task-v3.json \
  | python3 -c 'import json,sys; print(json.load(sys.stdin)["compositionDigest"])')"

./target/release/agent --state-root /absolute/path/state --skills-root ./skills run \
  --spec /absolute/path/agent-spec.json \
  --provider-config /absolute/path/provider-envelope.json \
  --request /absolute/path/task-v3.json \
  --expect-digest "$digest"
```

`compose`와 `run`은 같은 resolver와 동일한 provider binding을 사용한다. spec/task/route가
달라 digest가 바뀌면 provider connection 전에 종료한다. 실제 provider 호출 성공은 별도
운영 증거다.

## Verification

```bash
python3 scripts/verify-agent-only.py
./scripts/verify.sh
```

첫 명령은 projection/workspace 정적 경계를 확인한다. 두 번째는 Rust toolchain이 있는
환경에서 build/test와 stock adapter smoke를 실행한다. 외부 composition translation parity와
실제 provider/effect 운영 결과는 별도 증거다.
