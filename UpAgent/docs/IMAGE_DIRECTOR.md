# Image Director — 계약·검토·효과 경계

## 정체성·계약 owner

UpAgent는 image intent, admission, orchestration, acceptance를 소유한다. PerfectPixel은 deterministic pixels·검사·합성·비교를 소유한다. Vergerail은 `image_generate` provider effect, promptGen은 pure image prompt compile과 명시적 semantic-edit mechanism을 담당하며 둘 다 ImageDirectorState를 직접 수정하지 않는다.

`TaskEnvelopeV3.imageTask`의 `upagent.image-task/5`는 host ingress다. `provided { artifact }` 또는 `generate_new`, goal, attempts, preserve/visual criteria, mandatory visual review와 optional typed deterministic verification specification을 고정한다. source를 첫 tool call에서 추정하지 않는다. 계약 digest는 composition task digest 및 runtime provenance에 묶는다.

`perfectpixel.agent-image/verification-spec/1`은 beforeArtifact, afterKind, ordered assertions와 unique IDs, required/advisory severity·threshold를 소유한다. 최소 하나의 required assertion을 요구한다. specification digest와 enclosing task digest는 다른 식별자다.

## 상태와 admission

ImageDirectorState는 contract/digest, current typed candidates/revision, authorized pixel closure, observed revision, compare binding/results, pending repair, semantic-mask request identity, revision requests와 visual evidence를 보유한다. task terminal은 SessionMachine의 책임이다.

`image_inspect`는 inspection-only task에서 사용할 수 있다. standalone `image_compare`는 active qualification을 임의 변경하지 않는다. candidate producers는 미리 묶인 image contract가 필요하다. generation은 `provided`에서 거절하며 deterministic editing 이후 재생성으로 provenance를 갈아끼우지 않는다.

pixel-bearing source는 task-authorized closure 안에 있어야 한다. masks/fonts는 control dependency로 다루고 foreign raster를 candidate로 세탁하지 않는다. compare는 current full artifact identity·typed kind·revision·baseline·assertion 세트·call ID에 바인딩하여 한 번만 수용한다. pending compare/semantic 효과가 있는 checkpoint를 깨끗한 restart 지점으로 만들지 않는다.

## 관찰과 visual qualification

모든 current candidate의 관찰 전달이 필요하다. 일부 후보 또는 이전 revision의 관찰만으로 승인하지 않는다. 기준은 visualCriteria의 순서에 따라 `visual-1`부터 부여한다. 기준 개수 1..32, 각 기준과 evidence는 명시적 byte bound를 만족해야 한다.

모델은 내부 제어 응답 `IMAGE_REVIEW:` 뒤에 strict JSON을 반환한다. 필드는 contractDigest, candidateRevision, candidateSetDigest, criteria다. 각 criterion은 criterionId, decision(PASS/FAIL), 비어 있지 않은 evidence를 포함한다. 모든 ID를 정확히 한 번 다룬다. contract/revision/candidate-set digest가 모두 현재 값과 일치해야 한다.

후보 집합 digest는 전체 typed identity를 serialize→sort→deduplicate한 뒤 domain separator와 길이를 포함해 SHA-256으로 계산한다. 임의 첫 artifact SHA나 Debug 문자열을 사용하지 않는다. 새 후보는 이전 evidence를 무효화한다. duplicate/stale/unknown/malformed review와 tool call이 섞인 review는 거절한다.

모든 criterion PASS는 visual blocker만 제거한다. exact assertions와 pending repair 등 다른 blocker는 그대로다. review 자체를 user-facing 최종 응답으로 내보내지 않고 별도의 정상 final turn을 요구한다. model 판단의 내용적 정확성은 host/provider qualification의 대상이지 hash가 증명하는 성질이 아니다.

## generation·repair

유효한 PNG지만 요청 크기/배경과 다르면 effect receipt의 확인과 품질 수용을 구분한다. 바이트를 보존하고 `generationStatus: generated_needs_repair`, typed mismatches, `repairRequired: true`를 반환한다. 이를 무조건 transport failure나 성공 완료로 바꾸지 않는다. 자동 재생성·silent resize/matte/stretch를 하지 않는다.

수리에는 계약-authorized candidate의 `image_extract`/`image_render` 결과와 PP 검증을 사용한다. generation 시점에 명시된 exact size와 transparent/opaque 조건을 `ImageRepairRequirement`로 같은 `ImageDirectorState`에 저장한다. 수리 결과는 PP `agent-inspect`로 다시 관찰하며 candidate 전이가 성공한 뒤 `ImageRepairObservation`이 저장 조건 전체와 일치할 때만 pending repair를 해제한다. wrong-size, wrong-alpha, no-op success는 blocker를 제거하지 않는다. pending repair가 없어도 visual/required exact assertions를 생략할 수 없다.

## prompt artifact와 optional semantic edit

`promptgen_compile`은 generate-mode PromptGen image request만 받아 `upagent.image-prompt-artifact/3`을 만든다. artifact는 raw input digest, normalized typed request digest, complete compiler outcome digest, prompt digest, compiler identity, task ID와 output width/height/background/detail만 보존하며 raw input 전체를 다시 저장하지 않는다. `image_generate`는 raw prompt XOR artifact+expected digest를 소비하고 artifact 경로에서는 PromptGen이 고정한 size/background를 Vergerail request에 강제한다. detail은 prompt intent이며 provider-native quality로 변환하지 않는다. reference/edit 요청이나 Vergerail 미지원 크기는 artifact 생성 전에 거절한다. 숨은 latest-prompt state나 두 번째 ImagePromptBinding을 만들지 않는다.

semantic edit 입력은 artifact, candidateRevision, inputJson이다. host는 current provenance와 revision, request bound, explicit PromptGen pin, pinned Codex executable/hash/home, local prompt-only compile, approval을 external dispatch 전에 확인한다. 현재 adapter의 request subset은 edit mode, 하나의 reference index=1/role=base, `backend=codex-subscription`, PNG, bounded explicit dimensions와 `detail`이다. API key는 사용하지 않는다.

실제 Codex image child spawn 이후 effect가 발생했을 수 있다. receipt는 output과 독립 수집하고 subscription backend/task mode/detail, compiled prompt digest, pinned Codex runtime hash, base reference hash/bytes, output hash/path/dimensions, final fidelity pass와 시간 순서를 검증한다. 결과는 한 승인당 후보 1개로 제한한다. 완전 결과는 PP의 지원 raster 검사 후 normal tool→session→candidate 경로로만 승격한다.

## 실패·취소·재시도·복구 계약

| 경계 | effect 이전 실패 | effect 이후 실패 | 재시도·state |
|---|---|---|---|
| deterministic render/extract | 입력/권한 오류 | 기존 local effect/receipt 계약 | 검증 결과만 candidate; 기존 안전 정책 유지 |
| prompt compile | 입력/XOR/pin 오류 | local compiler error | image state 없음; 외부 이미지 effect로 분류하지 않음 |
| semantic edit | ordinary clean failure | timeout/cancel/partial/corrupt/unknown child → ResolutionRequired | 자동 재전송 금지; partial는 bounded opaque evidence |
| semantic mask | source/request validation | 기존 request/revision result binding | stale/duplicate 거절 |
| visual review | observation/current binding 위반 | N/A: 순수 evidence admission | current review만 qualification 변경 |

## public surface·비목표

Pixel tools는 image_generate, image_inspect, image_extract, image_render, image_compare, optional image_edit_semantic이다. ImageMaker에는 image_capabilities, imagemaker_prepare, promptgen_compile이 있다. 각 tool은 explicit selection·capability·approval 조건을 따른다.

Deterministic editing은 image_render node의 ordered operations로만 노출한다. 외부 PerfectPixel human CLI의 edit와 혼동하지 않는다. GUI/hidden mutable layers, native semantic segmentation backend, 전역 OS containment, deterministic aesthetic judgment는 제공 계약이 아니다. 외부 high-precision standalone capability를 이 Agent rgba8/srgb/straight 경로의 보장으로 확대하지 않는다.
