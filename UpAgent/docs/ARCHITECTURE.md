# UpAgent — 아키텍처

## 확정 경계·의존 방향

```text
CLI / embedding host
  → AgentSpec + TaskEnvelopeV3 + CompositionPluginCatalog
  → immutable ProviderBinding + selected profile/tool binding
  → AgentRuntime / SessionMachine
       → ModelPort / ImagePort                  (external provider mechanism)
       → EffectPlane → RAX Engine               (workspace effects)
       → MemoryStore / interaction tools        (scoped I/O)
       → ImageToolModule → PerfectPixel        (deterministic pixels)
       → ImageMaker / PromptGenAdapter          (optional local/external effect)
       → ArtifactStore + SessionJournal         (bytes/evidence)
  → typed Result/Event → ImageDirectorState → AgentResult

agent-review CLI → independent ReviewCampaign → ModelPort → reducer/evidence
```

`agent-core`는 계약·분류·순수 transition을 제공한다. `agent-provider-api`/`agent-profile-api`는 public port와 catalog 계약을 유지한다. `agent-plugin-kernel`의 graph는 manifest 검증이지 executor나 서비스 locator가 아니다. runtime 등록은 tools/verifiers를 transactional하게 설치하며 실행 승인은 별도다.

독립 review는 일반 runtime의 숨은 mode가 아니며 producer task와 workspace를 수정하지 않는다. PerfectPixel/promptGen/Vergerail은 명시적 외부 mechanism 경계다. PromptGen runtime을 라이브러리로 링크하지 않으며, sibling 소스의 순수 `agent-contract` crate만 공유한다.

## 실행·상태 모델

```text
Input/Event → validation/admission → State decision → Effect/I/O
→ typed Result/Event → currentness/evidence validation → State transition → Output
```

Composition은 host 의도와 selected binding을 digest로 고정한다. compose는 provider/state를 열지 않는다. run은 같은 resolver·expect-digest를 사용하고 명시적 adapter identity를 확인한 뒤 create-only composition과 provider connection을 진행한다.

Task terminal authority는 SessionMachine이다. image 계약·authorized pixel closure·candidate revision·exact compare·semantic-mask·visual qualification·generation repair obligation은 ImageDirectorState가 소유한다. repair obligation은 별도 state가 아니라 원 generation의 exact size/background constraint이며 PP의 재관찰 결과가 전부 일치할 때만 해제된다. in-flight call bindings는 같은 task의 결과 admission 자료이며 두 번째 image state가 아니다. async effect는 state를 직접 쓰지 않고 typed result로 돌아와야 한다.

Review evidence는 current contract digest, revision, 전체 typed candidate identity set digest와 기준별 판단에 묶는다. current 후보 변화는 이전 qualification을 무효화한다. prompt artifact는 content-bound 값이며 latest-prompt state를 두지 않는다.

## Persistence·transaction·권한

ArtifactStore는 bytes를 content identity로 저장하고 검증한다. journal은 typed event와 artifact ledger를 통해 recovery evidence를 보존한다. append/durability가 확인되지 않았으면 terminal 성공을 발표하지 않는다. terminal resolution event의 opaque forensic artifact와 정상 image candidate는 구분한다.

EffectPlane/RAX는 operation plan·precondition·grant·execution lock과 효과의 receipt/recovery를 소유한다. ApprovalStore는 Agent의 선택한 effect approval을 소유한다. MemoryStore는 별도 scope/revision CAS를 소유하며 session/image revision과 합치지 않는다.

등록된 ToolModule은 신뢰된 linked code다. capability/approval은 프로그램 계약이지 OS sandbox가 아니다. 외부 executor의 교체는 configured canonical identity와 invocation preflight를 통과해야 한다.

## 동시성·취소·복구

동일 runtime의 task slots와 writer lease는 그 runtime scope에서 동작한다. 다른 process/IDE writer까지 잠그는 전역 fence가 아니다. typed stale/duplicate checks는 최신 candidate와 contract를 보존한다. 진행 중 효과가 있는 checkpoint를 깨끗한 재시도 상태처럼 저장하지 않는다.

Local compile preflight는 외부 image effect와 구분한다. semantic edit dispatch 이후 timeout/cancellation/partial output/child result 불명확은 ResolutionRequired로 수렴한다. output와 receipt는 독립적으로 보존하되 검증되지 않은 bytes를 candidate로 승격하지 않는다. process cleanup이 확인되지 않았으면 성공이나 무효과를 추정하지 않는다.

## 값의 의미·소유권

| 구분 | 의미·owner | 예 |
|---|---|---|
| Constant | 계약/안전 상한·identity; 정의 module | core tool names, schema tags, prompt byte limits, receipt/output bounds |
| Policy | 허용·선택·판단 규칙; core/profile/effect policy | external tool classification, generation admission, required criteria |
| Configuration | host가 고정한 mechanism identity·execution bound | PromptGenSpec, ImageSpec, provider plugin configuration |
| Settings | 사용자가 조정한 값이 있다면 composition 입력으로 고정 | 현재 별도 mutable settings owner를 추가하지 않음 |
| Feature Flag | 존재 시 승인된 activation owner에서만 결정 | 별도 ambient flag 없이 AgentSpec.tools로 명시 선택 |
| State | 실제 event로 변경되는 runtime 사실 | TaskState, ImageDirectorState, MemoryStore revision |

상수가 필요하다는 이유로 서로 다른 의미의 timeout/byte bound를 하나로 합치지 않는다. configuration을 설정했다고 capability가 자동 활성화되지 않는다. cache/projection/경계별 validation은 concern과 trust boundary를 확인한 뒤에만 단순화한다.

## 코어·선택 기능·확장 지점

일반 task kernel이 코어다. image, semantic mask, promptGen, memory와 host containment는 명시적 선택 기능이다. embedding host는 같은 provider/profile port나 linked ToolModule로 확장할 수 있다. dynamic Process/Wasm loader, GUI, 새로운 registry/image state machine을 전제로 하지 않는다. 세부 규범은 [Image Director](IMAGE_DIRECTOR.md), [Security](SECURITY.md), [Protocol](PROTOCOL.md)을 따른다.
