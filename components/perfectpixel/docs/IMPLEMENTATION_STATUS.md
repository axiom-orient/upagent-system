# PerfectPixel 구현 현황

현재 계약은 [CONTRACT](CONTRACT.md), 책임·실행 경계는 [ARCHITECTURE](ARCHITECTURE.md)에 있다.
본 문서는 제품별 정본 색인과 검증 범위를 기록한다.

| 정본 | 역할 |
|---|---|
| [정체성](IDENTITY_AND_EVOLUTION.md) | 목적·보존 조건·발전 원칙 |
| [SPEC](SPEC.md) | 필수·선택 계약 및 원본 참조 |
| [ARCHITECTURE](ARCHITECTURE.md) | 경계·owner·실행 모델 |
| [PLAN](PLAN.md) | 남은 검증·결정 |

2026-10-30 두 Clippy 경고를 동작 보존 방식으로 수정했다. 경고 제외 없는 전체
target/feature Clippy `-D warnings`와 fmt, 관련 기존 테스트 101개가 통과했다.
고정 SVG 바이트·digest와 백엔드 식별자의 기대값은 유지했다. 백엔드 소스 해시에
의존하는 승인 데이터의 해시 연결을 갱신했으며, 임계값·승인 조건·기존 평가 기록은
보존했다. corpus를 다시 평가했다는 증거로 사용하지 않는다.

직전 MCP 경합 수정에서는 단위 검사 5개와 stdio 계약 검사 3개가 통과했다. 이 검사와
과거 전체 테스트 통과를 이번 수정의 전체 검증으로 승계하지 않는다. 실제 외부 계정·
KTX·Vision·폰트 렌더 E2E와 전체 테스트는 반복하지 않았다. 최종 검증 범위는
[루트 Current](../../../docs/IMPLEMENTATION_STATUS.md)에 있다. 수신한 과거 실행 로그와
출처 메타데이터는 현재 소스의 검증 결과를 대신하지 않는다.
