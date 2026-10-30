# Performance

## 역할

이 문서는 현재 구현이 사용하는 resource bound, deterministic ordering, cancellation
비용을 기록한다. 수치는 source의 상수와 validation 코드에서 가져온다.

## 비역할

측정하지 않은 latency·throughput·provider network 성능을 약속하지 않는다. profile별
행동 정책은 [Profile development](PROFILE_DEVELOPMENT.md), 안전 한계는
[Security](SECURITY.md)에서 다룬다.

## Public surface

| 영역 | 현재 bound |
|---|---|
| task budget | transitions `1..=1024`, model calls `1..=64`, tool calls `0..=256`, output `1 KiB..=64 MiB`, timeout `1 s..=24 h` |
| task defaults | `128` transitions, `12` model calls, `32` tool calls, `8 MiB` output, `15 min` timeout |
| runtime parallelism | default `4`, configured `1..=64` |
| provider turn | messages `128`, message `128 KiB`, advertised tools `64`, tool calls `64` |
| provider response | one argument `256 KiB`, aggregate arguments `1 MiB`, decoded text `8 MiB`, JSONL frame `64 MiB` |
| provider image | decoded PNG `8 MiB`, response frame `16 MiB`, timeout at most `30 min` |
| skill catalog | skills `1024`, scan depth `4`, aggregate entries `100,000`, manifest `64 KiB`, instructions `256 KiB` |
| data extension | package `512 KiB`, skills/package `64`, selected extensions `32` |
| memory | entries `4096`, file `16 MiB`, entry content `1 MiB`, recall result `256` |
| RAX operations | read/output `64 MiB`, entries/matches `100,000`, depth `256`, edit count `100,000`, timeout `24 h` |
| runtime evidence | inline tool result `192 KiB`, preview `8 KiB`, artifact ledger `8192` entries, `256` uses/artifact |
| review | request `16 MiB`, lane output `512 KiB`, findings `64`, discovery lanes `3`, fact-check lanes `2..=4`, event journal `8 MiB` |

## Primary flow

`BTreeMap`/`BTreeSet`와 명시적 sort로 profile, skill, tool, plugin binding을 안정적인
순서로 만든다. `run_many_with_cancellation`은 task를 bounded semaphore로 실행한 뒤
입력 index 순서로 결과를 복원한다. 큰 tool result는 `agent-runtime`의 artifact
store에 저장하고 model에는 bounded preview와 reference만 보낸다.

model·ToolModule·RAX process/Git는 cancellation을 관찰한다. synchronous filesystem
mutation은 future를 버리지 않고 worker 결과를 기다리므로 wall-clock deadline을
넘을 수 있다. provider와 external ToolModule은 dispatch 뒤 결과를 확인할 때까지
자동 재호출하지 않는다.

## Verification

```bash
cargo test --locked --workspace --all-features
./scripts/verify.sh
```

상한 회귀는 `agent-core`, `agent-provider`, `agent-memory`, `agent-runtime`,
`rax-core`, `rax-runtime`, `agent-review`의 unit/integration tests가 담당한다.
