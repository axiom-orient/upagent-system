# UpAgent·Vergerail·PromptGen 공통 계약

## 판단

공통 process 경계의 타입·스키마·검증은 통합할 수 있고, 같은 Rust 원본으로 연결했다.
서로 다른 제품 상태와 외부 protocol은 각각의 권위를 유지한다. 전체 JSON을 한 구조로
평탄화하면 승인·컴파일·실행의 의미가 섞이므로 채택하지 않았다.

실제 중복은 UpAgent provider의 request/response/tool/usage DTO, Vergerail의 같은
provider 값, UpAgent가 복제한 PromptGen compilation·semantic receipt에 있었다.
PromptGen core의 JSON·diagnostic·compiler detail/background와 Codex receipt를
`../promptGen/crates/agent-contract`로 옮겼다. 소비자는 같은 타입을 직접 사용한다.
기존 공개 module은 공통 원본을 re-export한다. 외부 Codex structured-output을
provider 값으로 바꾸는 adapter는 실제 protocol 차이를 처리하므로 유지한다.

## 선택 근거

| 방법 | 결과 | 판단 |
|---|---|---|
| 프로젝트별 DTO와 수동 변환 | 이름·enum·default·한도가 따로 변경됨 | 폐기 |
| 범용 JSON envelope만 공유 | compile-time 타입과 도메인 의미가 약해짐 | 미채택 |
| 순수 공통 타입 + serde/schema 선택 feature | 한 원본에서 producer·consumer·schema 생성 | 채택 |
| 전체 runtime과 상태까지 공통 framework로 합침 | 인증·승인·복구와 mechanism 책임이 결합됨 | 미채택 |

Serde의 명시적 enum 표현과 unknown-field 거절을 사용한다. schema는 wire 형식과
구조 제약을, runtime validator는 byte 크기와 실제 admission을 확인한다.
JSON Schema의 object 제약만으로 파일 권한·PNG·승인 여부까지 증명할 수는 없다.
([Serde enum 표현](https://serde.rs/enum-representations.html),
[JSON Schema object 제약](https://json-schema.org/understanding-json-schema/reference/object))

Cargo는 repository 내부 crate를 Git dependency로 선택하고 `rev`를 고정할 수 있다.
PromptGen에 순수 crate를 함께 배포하면 외부 프로젝트가 그 crate만 소비할 수 있으며
PromptGen을 UpAgent runtime에 종속시키지 않는다. 현재 통합 빌드는 고정 sibling
source와 lock/tree 검증을 사용한다.
([Cargo dependency 명세](https://doc.rust-lang.org/cargo/reference/specifying-dependencies.html))

## 공통 범위와 보존 경계

| 공통 원본 | 그대로 유지하는 authority |
|---|---|
| message·tool call·usage·reasoning | UpAgent turn/state와 Vergerail native execution |
| provider request·response·failure·한도 | operation별 capability·account·파일/PNG 검증 |
| artifact ID/ref·image observation·native image option | artifact bytes·보관·후보 승격 |
| compilation outcome·diagnostic·detail/background | PromptGen request 의미·컴파일 계산 |
| semantic edit·fidelity·refinement receipt | UpAgent 승인·candidateRevision·복구 |
| 같은 catalog 생성 | native Codex JSON-RPC·IFSC·task/state 계약 |

`detail`은 compiler 프롬프트 의도, `quality`는 provider-native 실행 설정이다.
compiler background의 `auto/opaque`와 provider의 `auto/transparent/opaque`는 실제
지원 범위가 다르므로 별도 enum을 유지한다. Codex의 pinned generated schema와
runtime lock은 수정하지 않았다.

공통 crate는 기본 feature에서 외부 dependency가 없다. serde/schema는 선택 feature다.
I/O·HTTP·process·인증·승인·상태 전이는 포함하지 않는다. `agent-contract` 0.1.0의
카탈로그는 `agent.shared-contract/1`, 기존 provider wire는 `vergerail.upagent/2`다.
기존 compiler JSON과 진단·Studio 결과 형식도 보존한다.

## 배포·검증 계약

세 executable은 `promptgen schema shared-contract`, `agent schema shared-contract`,
`vergerail-upagent-provider --schema`로 같은 카탈로그를 출력한다. 통합 build의
`audit-shared-contract.py`가 실제 세 출력을 비교하고 실제 pure compile을 schema로
검증한다. ID의 ASCII 문법·줄바꿈·길이도 검사한다. UpAgent의 추가 통합 검사는
SHA-256으로 고정한 실제 PromptGen binary를 production adapter로 호출한다.

Vergerail의 `cargo package`는 path dependency를 registry 명세로 정규화한다.
source-only 배포에서 package 검증은 동일 sibling crate를 명시적인 Cargo patch로
연결하여 추출된 package를 실제 빌드한다. 단독 `.crate`의 registry 배포를 주장하지
않는다. root source archive에는 PromptGen과 공통 crate가 함께 들어간다.
([Cargo package](https://doc.rust-lang.org/cargo/commands/cargo-package.html),
[Cargo patch](https://doc.rust-lang.org/cargo/reference/overriding-dependencies.html))

2026-10-03 당시 Vergerail timeout fixture는 제한된 PATH에서 `sleep`을 찾지 못해
exit 127이 발생했다. `/bin/sleep`으로 고쳐 timeout/종료 검사를 복원했고, 당시 보안
gate가 거절한 `rustls 0.23.43`을 `0.23.45`로 갱신했다. 이는 과거 변경 기록이며,
현재 의존성 버전은 `components/vergerail/Cargo.lock`이 정본이다.
([rustls 공식 advisory](https://github.com/rustls/rustls/security/advisories/GHSA-2mjx-qc3c-rqvc))

검증 결과와 게시 identity는 [현재 구현 현황](IMPLEMENTATION_STATUS.md),
[source lock](../components.lock.json), [PromptGen 통합](PROMPTGEN_INTEGRATION.md)에 기록한다.
계정 기반 이미지 생성·semantic edit·시각 품질은 pure compile과 fixture로 입증하지 않는다.
이 세 source에는 SwiftPM target이 없으므로 macOS SwiftPM 구현을 추가하지 않았다.
