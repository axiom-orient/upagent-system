# PerfectPixel 정체성과 발전 방향

## 정체성

raster/document/sprite/vector/texture/bounded motion을 처리하는 결정적 로컬 asset compiler·검증 엔진이다. caller는 CLI, fixed MCP adapter, 외부 Rust host 및 UpAgent다. 외부 inference/encoder는 후보 생산자이지 제품 성공 owner가 아니다.

## 변하면 안 되는 것

pixel/alpha/geometry/document 의미, content identity, exact/region/quality verification, guarded publication·explicit error, public CLI/API 및 선택 capability를 보존한다. 관찰되지 않은 색 provenance를 발명하지 않고 ICC transform 부재를 숨기지 않는다.

## 변경 가능한 것

동일 acceptance와 failure 의미 아래 codec/backend·publication 구현·transport·성능을 교체할 수 있다. 새 native/ML dependency는 reachable 요구와 검증 corpus가 있어야 한다. motion·PSD·MCP를 imageMaker의 미사용 코드로 삭제하지 않는다.

## 발전 방향

더 많은 후보 backend보다 정확한 domain authority·측정·precondition·publication을 개선한다. 선택 provider는 core 밖에서 후보를 만들고 core가 검증한다. route 품질과 public contract를 보존하는 단순화를 우선한다.
