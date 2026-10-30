# PerfectPixel 문서 색인

PerfectPixel은 독립적인 결정적 이미지·asset compiler다. 현재 계약은 [CONTRACT](CONTRACT.md), 책임 경계는 [ARCHITECTURE](ARCHITECTURE.md), 검증 상태는 [IMPLEMENTATION_STATUS](IMPLEMENTATION_STATUS.md)에 있다.

| 목적 | 정본 |
|---|---|
| 정체성·보존·발전 원칙 | [IDENTITY_AND_EVOLUTION](IDENTITY_AND_EVOLUTION.md) |
| 규범·수용 조건과 기존 계약 | [SPEC](SPEC.md), [CONTRACT](CONTRACT.md) |
| 책임·의존 방향 | [ARCHITECTURE](ARCHITECTURE.md) |
| public command별 현재 coverage | [FUNCTION_MATRIX](FUNCTION_MATRIX.md) |
| 남은 결정·구현·검증 | [PLAN](PLAN.md) |
| target Mac·선택 기능 검증 | [OPERATIONS](OPERATIONS.md) |
| 지속적 asset authority 결정 | [ADR](adr/0001-asset-authority.md) |
| MCP·motion·normalize | [MCP](MCP_CONTRACT.md), [MOTION](MOTION_CONTRACT.md), [NORMALIZE](NORMALIZE_REVIEW_CONTRACT.md) |
| vector 정책·schema | [vectorize](vectorize/README.md) |
| 외부 attribution | [THIRD_PARTY_NOTICES](../THIRD_PARTY_NOTICES.md) |

일반 command의 Operation 경계와 agent-*의 직접 dispatch는 구분한다. CLI/MCP가 모두 같은 Operation을 거친다는 주장은 현재 구조에 맞지 않는다. 자세한 경계와 남은 결정은 [ARCHITECTURE](ARCHITECTURE.md)와 [PLAN](PLAN.md)에 있다.

SOURCE_MANIFEST.sha256는 현재 파일 집합이다. PACKAGE_PROVENANCE.json은 변경하지 않은 입력 baseline provenance이며, 그 안의 과거 PASS는 현재 검증 결과가 아니다.
