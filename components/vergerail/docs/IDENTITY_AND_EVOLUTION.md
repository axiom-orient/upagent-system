# Vergerail 정체성과 발전 방향

## 정체성

고정 OpenAI Codex app-server와 Rust caller 사이의 좁은 runtime/protocol bridge다. library와 UpAgent/IFSC용 one-shot adapter를 제공한다. 범용 HTTP SDK·계정 저장소·daemon이 아니다.

## 변하면 안 되는 것

embedded runtime/schema lock, initialize/session/turn identity, typed approval·multimodal input, bounded frame/deadline·uncertain outcome, 공식 계정 custody를 보존한다. private compatibility adapter를 public API 지원 보장으로 표시하지 않는다.

## 변경 가능한 것

검증된 version migration 절차로 runtime pin·native guardian·transport 내부 구현을 바꿀 수 있다. caller의 wire·auth·failure contract가 바뀌면 명시적 versioned decision이 필요하다. API-key fallback을 몰래 추가하지 않는다.

## 발전 방향

최신 버전 추종보다 pinned provenance·좁은 권한·정확한 process lifecycle·실계정 증거를 먼저 확보한다. text planning, pixel generation, 외부 caller용 IFSC를 독립 경계로 유지한다.
