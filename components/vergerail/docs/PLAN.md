# Vergerail 남은 PLAN

## 현재 소스의 실행 검증

첨부된 credential Debug·계정 생성 기록은 과거 증거다. 현재 소스의 로컬 Rust 검증과
실제 session·image 실행 여부는 [상위 Current](../../../docs/IMPLEMENTATION_STATUS.md)의
적용 범위를 따른다. 실제 실행의 완료 조건은 현재 binary/source identity와
입력·출력·실패·취소 증거가 연결되는 것이다.

## native release·IFSC·원격 효과

[VERIFICATION](VERIFICATION.md)의 target/native 및 [RELEASE](RELEASE.md)의 배포 의무를
별도로 확인한다. IFSC caller 앱은 첨부되지 않아 consumer compatibility는 UNKNOWN이다.
원격 취소/과금과 다른 계정의 지원을 로컬 자식 종료·과거 생성 성공으로 일반화하지 않는다.
서버/계정 권한이 없으면 API-key/private credential 우회로 완료를 흉내 내지 않는다.
