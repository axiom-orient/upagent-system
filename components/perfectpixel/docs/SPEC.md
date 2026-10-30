# PerfectPixel SPEC

## 필수 기능·계약 원본

[CONTRACT](CONTRACT.md)가 raster/document/edit/publication의 상세 규범,
[MCP_CONTRACT](MCP_CONTRACT.md)는 fixed root transport, [MOTION_CONTRACT](MOTION_CONTRACT.md)는
motion/scene identity, [NORMALIZE_REVIEW_CONTRACT](NORMALIZE_REVIEW_CONTRACT.md)는 normalize
후보/승인, [vector 계약](vectorize/README.md)은 route/quality policy를 소유한다.

## 입력·결과·실패

public operation의 typed 입력을 검증하고 immutable source snapshot/precondition을 잡는다.
외부 effect result는 current identity로 결합한 뒤 core acceptance를 통과해야 한다. 실패·취소·
timeout·precondition conflict를 구분하고 destination을 무조건 덮어쓰는 fallback을 두지 않는다.

## 색·픽셀 계약의 구분

일반 codec inspection은 PixelSpec/ICC provenance를 관찰하고 미확정 색은 Unknown이다.
agent image protocol의 working descriptor는 RGBA8/sRGB/straight라는 별도 제한된 계약이다.
이 선언은 임의 입력 PNG의 ICC→sRGB 변환을 증명하지 않는다. 두 경계의 의미를 혼동하지 않는다.
ICC transform·SSIMULACRA2·새 semantic matting adapter를 존재하는 것처럼 노출하지 않는다.

## 선택 기능·수용 기준

KTX는 pinned executable→encode/inspect/extract/decode roundtrip 후 게시, Vision은 pinned
helper→provenance/current work identity→deterministic mask cleanup/verification 후 게시한다.
PSD structural readback과 native application 호환성은 구분한다. MCP/CLI 공통 operation의
의미는 동일해야 하지만 tool inventory 전체가 같아야 하는 계약은 아니다.
