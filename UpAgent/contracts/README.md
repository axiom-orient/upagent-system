# Contract projections

## 역할

`contracts/`는 외부 composition 단계가 소유하는 JSON 계약의 immutable projection과
그 provenance hash를 보관한다. 이 디렉터리의 파일을 읽거나 실행하는 adapter는
포함하지 않는다.

## 비역할

UpAgent의 native task 실행 계약은 `agent-core`와 `agent-runtime`이 소유한다.
외부 계약을 검증·변환해 `AgentSpec`과 task-only `TaskEnvelope` v3를 만드는 단계도
이 workspace의 binary가 아니다.

## Public surface

현재 projection 파일은 다음과 같다.

- `axd-execution-contract.schema.json`
- `agent-axd-execution-contract.schema.json`
- `front-axd-execution-contract.schema.json`
- `approval.schema.json`
- `compilation-result.schema.json`
- `intake-request.schema.json`
- `work-item-handoff.schema.json`
- `schema-index.json`

`schema-index.json`은 각 projection의 역할, source label, SHA-256을 묶는다. source
label은 provenance 값이며 checkout 내부 경로로 해석하지 않는다. native runtime
schema는 release `agent`가 다음 `SchemaKind` 이름으로 생성한다.

```bash
target/release/agent schema task-v3
target/release/agent schema agent-spec
target/release/agent schema composition
target/release/agent schema result
target/release/agent schema profile
target/release/agent schema shared-contract
```

## Primary flow

`shared-contract`는 sibling PromptGen의 순수 `agent-contract`가 생성한다. 모델·tool·
usage·artifact·이미지 관찰·provider 요청/응답과 compiler/semantic receipt를
UpAgent와 Vergerail이 같은 Rust 타입으로 사용한다. task/state/approval 계약은
UpAgent에 남고 PromptGen runtime을 링크하지 않는다. source tree와 executable은
통합 lock 및 composition의 별도 SHA-256으로 고정한다.

외부 producer가 projection을 검증하고 host-owned `AgentSpec`과 task-only v3를 만든다.
그 뒤 root의 `agent compose`/`agent run`이 native composition과 실행을 담당한다.
필드 mapping과 boundary는 [AXD integration](../docs/AXD_INTEGRATION.md), native 입력·결과
의미는 [Protocol](../docs/PROTOCOL.md)에 있다.

## Verification

projection 존재 여부와 `schema-index.json`의 SHA-256은 root에서 다음 명령으로
확인한다.

```bash
python3 scripts/verify-agent-only.py
```
