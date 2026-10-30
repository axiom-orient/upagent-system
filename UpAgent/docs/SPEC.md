# UpAgent — 기능·수용 계약

이 문서는 규범을 소유한다. 구현 상태와 검사 결과는 [Current](IMPLEMENTATION_STATUS.md), 상세 wire format은 [Protocol](PROTOCOL.md), image 계약은 [Image Director](IMAGE_DIRECTOR.md)에 둔다.

## 필수 기능

| 계약 | 입력·전제조건 | 관찰 가능한 출력·수용 기준 | 원본 |
|---|---|---|---|
| 명시적 composition | AgentSpec v5, TaskEnvelopeV3, provider/profile binding | canonical digest; run은 expect-digest 불일치 시 provider/state 이전 거절 | [composition.rs](../crates/agent-core/src/composition.rs), [task.rs](../crates/agent-core/src/task.rs) |
| bounded task execution | selected profile/tools/skills, limits, effect approval | 한 terminal result, 명시적 failure/suspension/resolution, bounded loop | [event.rs](../crates/agent-core/src/event.rs), [Protocol](PROTOCOL.md) |
| effect authority | admitted typed call + 필요한 approval | 승인 없는 효과 금지; 검증된 receipt/실패 근거 | [Security](SECURITY.md), [RAX protocol](../crates/rax-core/src/protocol.rs) |
| durable artifacts/evidence | bounded bytes, validated identity | content-addressed 참조와 journal 복구 가능성; durability unknown은 성공 아님 | [artifact.rs](../crates/agent-runtime/src/artifact.rs), [journal.rs](../crates/agent-runtime/src/journal.rs) |
| provider/profile 교체 | matching public binding/manifest/capability | 구체 transport가 core policy의 owner가 되지 않음 | [provider API](../crates/agent-provider-api/src/lib.rs), [profile API](../crates/agent-profile-api/src/lib.rs) |

## 선택 기능

Image tools는 명시적 tool 선택과 PerfectPixel identity, 관찰 가능한 provider capability를 요구한다. candidate-producing tool은 `upagent.image-task/5`와 source provenance를 요구한다. `image_generate`는 generation capability와 effect approval이 필요하며 `provided` task에서 금지한다. deterministic preprocessing의 canonical surface는 `image_render.operations`다.

`promptgen_compile`은 **generate-mode image 전용 local compiler capability**다. `AgentSpec.promptgen`의 절대 executable 경로·SHA-256·timeoutMs와 tool 선택이 필요하다. inputJson XOR inputArtifact이며 raw input digest, PromptGen normalized request digest, 전체 compilation outcome digest, prompt digest, width/height/background/detail을 `upagent.image-prompt-artifact/3`에 결합한다. detail은 prompt intent라 provider quality로 변환하지 않는다. reference가 있거나 edit/composite/pose처럼 외부 reference effect가 필요한 요청은 artifact로 만들지 않고 전용 effect 경계를 사용한다. compile만 선택할 때 PerfectPixel이나 외부 image-effect approval을 요구하지 않는다. configuration만 두어 tool을 활성화하지 않는다.

`image_generate`는 raw `prompt` 또는 `promptArtifact` + `promptSha256` 중 하나를 소비한다. artifact를 소비할 때 task ID, compiler/content digest와 **PromptGen이 고정한 size/background**를 다시 읽고 caller가 덧씌운 값이 충돌하면 effect 전에 거절한다. quality는 provider-native 별도 옵션이며 PromptGen의 detail에서 추론하지 않는다. Vergerail이 지원하지 않는 PromptGen 출력 크기는 compile artifact 단계에서 fail closed하며 몰래 resize/crop하지 않는다. compiler digest는 공급망 서명이 아니다.

`image_edit_semantic`은 deterministic edit와 다른 외부 효과다. explicit selection, promptGen identity, PerfectPixel, image observation capability, source/revision/approval이 필요하다. credential은 external dispatch에만 전달한다. output·receipt를 독립 수집하고 실제 receipt schema/source/output binding과 PP raster 검증을 통과한 결과만 normal candidate 경로로 돌려보낸다. dispatch 후 불확실성은 ResolutionRequired이며 자동 재전송하지 않는다.

`image_capabilities`는 선택 정보를 조회하며 live availability를 증명하지 않는다. `imagemaker_prepare`는 v5 계약을 검증한 proposal만 반환하며 실행 중인 task를 재바인딩하지 않는다. semantic mask는 별도의 선택적 helper 계약을 유지한다.

Scoped memory, ASK, host adapter, ReviewCampaign은 각각 기존 명시적 scope/승인/독립 계약을 따른다. review는 [독립 제품 계약](../crates/agent-review/docs/SPEC.md)을 소유한다.

## 이미지 완료 수용

현재 provenance 내의 유효 후보, candidate revision에 맞는 전체 이미지 관찰, current contract·whole candidate-set digest·criterion ID에 묶인 explicit visual PASS가 필요하다. required exact verification specification이 있으면 동일 후보·typed kind·baseline·assertion 세트에 대한 모든 required assertion도 통과해야 한다. generation drift가 발생했다면 원 요청에서 검증 가능한 exact size/background를 저장한 `ImageRepairRequirement`가 남으며, PP가 재관찰한 최신 repair candidate가 그 조건 전체와 일치하기 전에는 완료할 수 없다. 단순 tool 성공이나 PP receipt만으로 pending repair를 제거하지 않는다.

빈 visual criteria, stale/malformed/duplicate evidence, plain observation, provider 성공 또는 `IMAGE_REVIEW` 자체를 사용자 최종 완료로 취급하지 않는다. 유효한 review 이후 별도의 정상 최종 응답을 요구한다. 시각 PASS는 주관적 품질의 수학적 증명으로 해석하지 않는다.

## 오류·취소·복구

유효하지 않은 입력/권한은 가능한 한 effect 이전 거절한다. side effect 이후 receipt/bytes/durability/child cleanup이 불명확하면 원인을 보존하고 resolution 상태로 남긴다. 재시도는 효과의 멱등성과 실제 상태를 확인한 명시적 정책만 허용한다. stale/duplicate/cancelled 결과는 current state를 변경하지 못한다.

외부 AXD/intake/front schema는 [contracts](../contracts/README.md)의 immutable projection을 원본 참조한다. projection을 로컬 구현으로 오인하거나 API 변경을 위해 임의 편집하지 않는다.
