# PerfectPixel 남은 PLAN

## P1 — operation 및 색 의미의 경계 정합성

**Current:** 전용 agent-* dispatch가 일반 Operation 경계를 통과하지 않는다.
working sRGB 선언과 일반 codec의 observed color provenance는 다르다.
**보존:** public schemas·deterministic edit·Unknown color·MCP subset·기존 source pixels.
**결정 필요:** typed registry 통합 범위, 임의 입력 color rejection/normalization 정책.
현재 UpAgent bridge 정책은 working raster를 `rgba8/srgb/straight`로만 게시하고 embedded ICC/P3는
자동 변환 성공으로 취급하지 않는다. UpAgent bridge의 자동 색 변환은 지원하지 않는다.
**완료 조건:** 위협모델/계약 결정 후 CLI/schema/timeout/capability/색 corpus/bridge 회귀를
통과해야 한다. 임의 sRGB 재라벨링이나 ICC fallback으로 해결하지 않는다.

## P0/P1 — 실제 binary·선택 native 검증

로컬 build·테스트 결과를 선택 native 기능의 성공으로 승격하지 않는다.
[운영 gate](OPERATIONS.md)의 KTX/Vision/PSD와 UpAgent bridge는 해당 대상 환경에서 확인한다.
기존 LICENSE/NOTICE를 유지하며 release/legal approval은 별도로 검토한다.
