# Product Contract

## Required vertical paths

1. `model_turn`: 일반 LLM 응답. PerfectPixel/promptGen 없이 구성 가능.
2. `promptgen_compile`: optional local pure compile. generate-mode PromptGen image request만 받아 `upagent.image-prompt-artifact/3`을 생성하며 외부 provider를 호출하지 않는다.
3. `image_generate`: `generate_new` image task에서만 허용. `model/image_generation` provider capability 필요. prompt artifact를 사용하면 size/background는 artifact 정본이며 quality는 별도 provider control이다. PromptGen의 `detail`은 prompt intent라 provider quality로 변환하지 않는다.
4. `image_render`: PerfectPixel `perfectpixel.agent-image/render/2`. raster/object source preprocessing, typed text node, affine transform, opacity, source-over를 담당.
5. `image_compare`: PerfectPixel `perfectpixel.agent-image/compare/2`. required/advisory assertion과 preview evidence를 담당.
6. `image_edit_semantic`: optional external effect. pinned PromptGen + pinned Codex runtime + image observation + approval + current source revision이 필요. 한 승인에서 생성 후보는 하나다.

## PerfectPixel v2 canonical capabilities consumed by UpAgent

`inspect.basic`, artifact content/dependency/pin, extract selectors/refinement, `render.composition_dag`, `render.affine_3x3`, `render.source_over`, `render.text_node`, `compare.basic`, `compare.exact_spec`, regions/masks/geometry/preview.

Deterministic preprocessing의 agent surface는 `image_render.operations`다. 별도 agent `edit.deterministic` capability를 병행하지 않는다. standalone `perfectpixel edit` CLI는 독립 public API로 보존한다.

## Failure / recovery

- schema/domain invalid → `not_applied`; 인자 수정 후 새 요청.
- revision/intent stale → conflict; re-inspect → re-plan.
- provider dispatch 전 취소 → known not applied.
- dispatch 후 timeout/cancel/connection loss → effect 여부가 확인되지 않으면 unknown/resolution path. 새 operation으로 무조건 재호출 금지.
- provider bytes가 valid라도 PerfectPixel 재검증 실패 → 정상 candidate로 승격 금지.
- compare required assertion 실패 → 이미지 작업 완료로 보고 금지.

## promptGen integration rule

UpAgent는 별도 GitHub PromptGen source checkout에서 빌드된 executable을 사용한다. `promptgen_compile`은 `image --mode prompt-only`만 호출한다. `image_edit_semantic`은 edit request를 먼저 prompt-only로 검증한 뒤 명시적으로 선택된 `image --mode codex-imagegen`을 `--codex-sha256`과 `--max-fidelity-attempts 1`로 호출한다. Codex image effect는 semantic edit에 한정되고 UpAgent가 승인, task/revision currentness, candidate promotion, unknown-outcome resolution을 계속 소유한다. API key 경로는 사용하지 않는다. screen compiler는 UpAgent image path에서 제거했다.
