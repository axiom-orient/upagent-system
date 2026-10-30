# ADR-0001 — B 권한 구조와 독립 image capability

상태: **채택 — 사용자가 제공한 recovery 계약의 명시적 선택을 구현 경계에 적용**. 확인일: 2026-09-10.

## 맥락

B는 AgentSpec v5·ProviderBinding·image-task/5·독립 review·deterministic image_render.operations를 가진다. donor에는 image 기능과 함께 다른 provider/image state와 package 의존 구조가 있다. 단순 합집합은 동일 concern의 owner와 public edit surface를 복제한다.

## 결정

B kernel/state/authority를 보존한다. deterministic edit는 image_render.operations 하나로 유지한다. 도구 이름·분류는 core에 두고 configuration과 activation은 분리한다. optional promptGen runtime은 pinned external process이며 image state를 소유하지 않는다. sibling 소스의 순수 `agent-contract` crate는 공통 타입·검증·스키마 정본으로 공유한다. prompt compile 결과는 immutable artifact다.

Visual qualification은 ImageDirectorState의 evidence이며 observation과 분리한다. review는 전체 typed candidate set에 바인딩한다. semantic edit는 spawn 이전 validation과 이후 uncertainty를 구분하고 기존 ResolutionRequired를 재사용한다. 독립 ReviewCampaign/CLI와 provider/profile public ports를 유지한다.

## 결과

새 registry/state machine/extension crate/compatibility editor를 만들지 않는다. 외부 schema는 실제 mechanism contract에 맞춰 adapter가 검증한다. old lost-work 기록은 구현 증거로 쓰지 않는다. generation repair는 같은 ImageDirectorState의 typed obligation으로 보존하고 PP observation으로만 해제한다. 이 state serialization 변경은 checkpoint schema v3의 의도적 clean-break이며 v2 migration을 추가하지 않는다. 남은 실환경 qualification만 [PLAN](../PLAN.md)이 소유한다.
