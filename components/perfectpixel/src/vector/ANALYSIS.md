# Vector policy/quality 분석

## Verdict·책임 경계

**MAINTAIN / IMPROVE.** raster→SVG의 candidate 선택·geometry/alpha/topology quality gate를
소유하는 public domain unit이다. CLI와 Rust caller가 사용한다. 외부 vectorizer wrapper가
아니며 candidate와 acceptance를 구분한다. 구조 확신도 높음, runtime/corpus 검증 NOT_RUN.

## 공개 경계

| Surface·활성 조건 | Caller→Handler | Input·검증 | State/Effect owner | Output·Failure | 근거 |
|---|---|---|---|---|---|
| `vector` | CLI→vector handler→public domain | bounded request·profile·policy·input digest | route/threshold authority | verified SVG 또는 rejection | [handler](../application/handlers/vector.rs), [API](mod.rs) |
| `vector-analyze` | CLI→analysis route | input/profile·report option | policy decision; SVG candidate 없음 | analysis/report; `--report`는 write 가능 | handler, [routes](authority/routes.rs) |
| compile/quality library | host→candidate/quality | exact input/profile/scope | internal route·resvg verification | structured outcome/report | [policy](policy.rs), [quality](quality/mod.rs), [outcome](outcome.rs) |
| SVG/report publication | accepted outcome→application I/O | request/source/policy/destination guard | checked publisher | bytes 또는 precondition error | handler 및 ../../io |

## 실제 흐름·owner

Input/profile → authority route → exact_grid/pixel_art/line_art/flat_icon/illustration candidate →
resvg roundtrip + alpha/geometry/topology gates → outcome → checked writer.
후보 backend 성공 또는 contour 개수만으로 acceptance를 선언하지 않는다.

## 기능 현황

| 기능 | 연결 상태 | 계약 충족 상태 | 검증·적용 범위 | 근거 |
|---|---|---|---|---|
| vector compile/analyze | REACHABLE | SATISFIED | CLI/API 경계 조사; Rust NOT_RUN | handler, mod.rs |
| policy/quality public API | PUBLIC_LIBRARY | SATISFIED | exports·compile tests 존재; 실행 NOT_RUN | request.rs, policy.rs, tests |
| external VTracer route | ABSENT_IN_SCOPE | NOT_IMPLEMENTED | N/A: reachable registered backend 아님 | backends/mod.rs, authority/routes.rs |

## 실패·취소·복구·findings

source/policy/destination이 바뀌면 기존 candidate를 publish하면 안 된다. library는 acceptance
결과를 반환하며 파일 commit/recovery는 상위 I/O owner가 맡는다. 외부 process state를 pure
library에 발명하지 않는다.

문제 → 근거 → 원인 → 영향 → 방향: optional backend 이름을 새 구현 권한으로 오인할 위험 →
route와 public outcome은 이미 quality 소유 → 별도 backend success의 권한 승격 → 품질 gate
약화 → KEEP, corpus 근거가 있을 때만 동일 authority 아래 개선. 모든 profile의 시각 품질·성능
수치는 이번에 측정하지 않았다. 기존 [상세 계약](../../docs/vectorize/README.md)을 보존한다.
