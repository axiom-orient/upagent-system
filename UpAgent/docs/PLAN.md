# 남은 실환경 qualification

로컬 workspace gate·release build·CLI 검증의 현재 결과는
[구현 현황](IMPLEMENTATION_STATUS.md)과 [통합 기록](../../docs/IMPLEMENTATION_STATUS.md)을 따른다.
현재 버전의 실계정 실행·외부 실행 파일 검증은 별도 입력과 증거가 필요하다.

- 명시적으로 선택한 provider·PromptGen·PerfectPixel identity로 모델 호출·이미지 생성·
  단일 승인 semantic edit를 실행하고 원본·결과·영수증·후보 승격을 연결한다.
- timeout·취소·부분 파일·결과 미확정 상태를 실제 실행과 통제된 fixture로 구분한다.
- UpAgent의 `rgba8/srgb/straight` 범위를 넘는 정밀도·색공간을 묵시적으로 변환하지 않는다.
- 공개 배포 서명·공증과 다른 host·계정의 사용 가능 여부는 해당 배포 환경에서 확인한다.

## 검증 순서와 금지되는 대체 증거

정적 통과는 build 통과가 아니다. 테스트 source의 존재는 테스트 실행이 아니다. shell shim/fake model은 계약 회귀 도구이지 실제 provider 품질이나 설치된 sandbox의 증거가 아니다. 이전 ZIP의 Darwin PASS를 이 source revision에 재사용하지 않는다.

로컬 검증 통과를 실제 선택 기능의 새 실계정 실행이나 공개 배포 완료로 확대하지 않는다.
환경상 미실행 항목을 우회 fallback으로 성공 처리하지 않는다.
