# ADR 0001 — 결정적 asset authority와 선택 provider

상태: **확정된 기존 제품 결정의 통합 기록**. 새 dependency 선택이나 미래 작업 승인이 아니다.

## 맥락

외부 encoder/inference/backend마다 성공 정책을 가지면 같은 입력의 acceptance와 publication
의미가 달라진다. 기존 roadmap/research 문서의 지속적 판단만 이 ADR로 통합한다.

## 결정

core의 pixel/alpha/geometry/document/verification과 checked publisher가 성공 의미를 소유한다.
KTX·Vision·vector backend는 후보만 제공한다. CLI/MCP는 presentation/admission이며 generic
command runner를 추가하지 않는다. 공통 Operation의 의미를 공유하되 transport inventory가
같아야 하는 것은 아니다.

ICC bytes/provenance는 보존하되 검증 corpus 없는 색 변환 engine과 SSIMULACRA2를 추가하지
않는다. ICC conversion은 Unsupported이며 missing color를 sRGB로 위조하지 않는다.
CIEDE2000와 exact/alpha/geometry/region gates는 서로 다른 측정 의무로 유지한다.

KTX는 pinned executable와 structure/decode roundtrip, Vision은 pinned helper/request revision/
provenance/current work identity와 deterministic mask cleanup, vector는 기존 route/quality
acceptance를 유지한다. 외부 VTracer·SAM/ViTMatte/libvips·새 Vision operations·OpenRaster는
확정된 reachable 요구가 없는 상태에서 추가하지 않는다.

## 결과

필요 없는 dependency·성공 정책·fallback이 늘지 않는다. optional native capability는 별도의
운영 gate가 필요하며 default core 성공과 구분한다. 범위를 확장하려면 실제 요구·계약·품질
corpus·실패/취소/publication 검증을 먼저 결정해야 한다.

## 원 결정의 참고 자료

아래는 이전 연구가 사용한 primary references이며 이번 작업에서 모두 재검증한 목록이 아니다.
현재 구현 판단은 source/계약에 따른다.

- [Apple foreground instance mask](https://developer.apple.com/documentation/vision/generateforegroundinstancemaskrequest)
- [Khronos KTX tools](https://github.khronos.org/KTX-Software/ktxtools/ktx.html)
- [image decoder ICC](https://docs.rs/image/latest/image/trait.ImageDecoder.html)
- [LittleCMS Rust wrapper](https://docs.rs/lcms2/latest/lcms2/)
- [SSIMULACRA2 후보 원본](https://github.com/imazen/fast-ssim2)
