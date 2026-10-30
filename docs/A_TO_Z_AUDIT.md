# A→Z Connectivity and Contract Audit

이 문서는 **2026-10-03 정적 연결 감사**의 범위를 기록한다. 계정 기반 E2E나
이후 소스의 최신 실행 결과는 [통합 구현 현황](IMPLEMENTATION_STATUS.md)에서 구분한다.

## 판정

등록·노출된 production capability 중 **정적으로 연결되지 않아 사용할 수 없는 기능은 발견되지 않았다.** 미구현 항목은 실행 surface에 등록하지 않은 PLANNED/ABSENT_IN_SCOPE로만 존재한다.

## Canonical flow

```text
Input/Event
→ UpAgent task/state transition
→ selected typed tool
→ trusted effect boundary
→ Vergerail | PerfectPixel | promptGen
→ structured result / artifact / receipt
→ UpAgent result event
→ stale/cancel/duplicate fence
→ candidate/evidence state transition
→ Output
```

## 전수 연결 결과

- UpAgent workspace: 15/15 reachable
- promptGen workspace: 6/6 reachable
- UpAgent effect tools: 8/8
- memory tools: 2/2
- image tools: 6/6
- ImageMaker tools: 3/3
- UpAgent CLI: 11/11
- recovery CLI: 4/4
- approval CLI: 3/3
- Vergerail provider operations: 2/2 (`model_turn`, `image_generate`)
- PerfectPixel product operations: 16/16
- PerfectPixel agent operations: 5/5
- promptGen CLI commands: 8/8
- promptGen MCP tools: 4/4
- promptGen Web routes: 7+
- Rust literal includes: missing 0
- JSON documents: parse error 0

## 실제 수정된 계약 공백

`upagent.image-prompt-artifact/3`의 `compilerSha256`은 소비 시 현재 composition에 pin된 PromptGen executable SHA와 반드시 일치해야 한다. task/content digest만 맞는 다른 compiler 산출물은 effect 전에 거부한다. PromptGen adapter가 없는 composition도 compiled artifact를 소비할 수 없다.

## Schema

Draft 2020-12 schema와 checked-in fixture를 정적 검증한다. 동일 `$id`를 공유하는 AXD projection alias 3개는 bytes가 동일한 immutable projection이며 서로 다른 의미 정본이 아니다.

## 범위 밖 / NOT_RUN

PromptGen 5.0.0 source gate, UpAgent workspace gate, four-workspace source build와 PromptGen
`prompt-only` smoke는 2026-10-03에 PASS했다. PromptGen checkout에 1.4 historical image
evidence가 없어 그 archive audit만 receipt에 `[NOT_RUN]`으로 남겼다. 실제 Vergerail/Codex
계정 효과와 image E2E는 실행하지 않았으며, static/unit/build 결과는 이를 증명하지 않는다.
상세 항목은 [현재 구현 현황](IMPLEMENTATION_STATUS.md)과 [미실행 항목](UNRESOLVED.md)을
따른다.
