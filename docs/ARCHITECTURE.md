# Architecture

## Authority

| Fact / decision | Authoritative owner | Derived consumers |
|---|---|---|
| user task, intent version, current candidate, approval | UpAgent | CLI, agent context, receipts |
| model/image provider response | Vergerail/provider | UpAgent observation |
| deterministic pixel result | PerfectPixel output bytes | UpAgent artifact store |
| pixel verification evidence | PerfectPixel inspect/compare | UpAgent qualification/review |
| image prompt compilation | externally pinned PromptGen source + pure compiler result | UpAgent artifact store |
| semantic edit receipt/output | explicitly approved PromptGen Codex effect | UpAgent |
| executable/config identity | host composition | UpAgent runtime adapters |

## Explicit flows

### General LLM

```text
Input → UpAgent task/session → provider model_turn effect
→ typed provider result event → currentness check → assistant output
```

이미지 configuration이 없더라도 일반 model turn은 독립적으로 구성 가능해야 한다.

### Generate and deterministic edit

```text
ImageTask(generate_new)
→ [optional] promptgen_compile(generate-mode image request)
→ image-prompt-artifact/3 {task/compiler/raw/normalized/outcome/prompt digests + size/background/detail intent}
→ image_generate authorization/precondition
→ Vergerail dispatch
→ provider image bytes + observed metadata
→ UpAgent validates PNG and stores artifact
→ PerfectPixel inspect
→ candidate state
→ image_render(operations/text/affine/source-over)
→ PerfectPixel receipt + published artifact
→ independent inspect/compare
→ UpAgent stale/cancel/currentness check
→ candidate/review/final output
```

prompt artifact 경로를 사용할 때 width/height/background는 compiler artifact가 소유하고 caller override와 충돌하면 dispatch 전에 거절한다. PromptGen `detail`은 prompt intent이며 Vergerail quality control로 사용하지 않는다. caller가 provider-native quality를 따로 명시할 수 있다. PromptGen이 지원하지만 Vergerail이 지원하지 않는 크기는 usable artifact로 만들지 않는다.

### Semantic edit

`image_edit_semantic`은 deterministic render와 다른 외부 effect다. PromptGen executable/source identity, Codex runtime path/SHA/home, source artifact/revision, approval이 필요하다. PromptGen이 base PNG를 snapshot/hash한 뒤 Codex 구독 편집을 수행하고, UpAgent는 prompt/output/reference/fidelity receipt를 다시 검증한다. 한 semantic edit 승인에서는 image candidate를 하나만 요청한다. dispatch 이후 결과가 불확실하면 자동 재전송하지 않고 기존 recovery/ResolutionRequired 경로로 간다.

## State model

- **Constant:** schema/protocol IDs, capability names, error code vocabulary.
- **Policy:** approval mode, effect budget, retry/deadline limits.
- **Configuration:** executable paths/digests, provider runtime/model/workspace.
- **Settings:** 사용자 선택 표시/작업 옵션만. 권한을 만들지 않는다.
- **Feature flag:** 현재 제품에는 새 전역 flag를 추가하지 않는다.
- **State:** task/candidate revision, operation state, receipts, current intent generation.

각 material fact의 owner는 하나다. provider status 또는 모델 설명은 UpAgent 상태 권위가 아니다.
