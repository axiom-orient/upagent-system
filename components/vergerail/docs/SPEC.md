# Vergerail SPEC

## 계약 원본

[PROTOCOL_CONTRACT](PROTOCOL_CONTRACT.md)는 고정 native wire/schema,
[PROVIDER_CONTRACT](PROVIDER_CONTRACT.md)는 `vergerail.upagent/2`,
[IFSC_TEXT_PROVIDER](IFSC_TEXT_PROVIDER.md)는 별도 ScreenProgram 입력·출력을 소유한다.
이 SPEC은 동일 필드를 복사해 다른 정본으로 만들지 않는다.

## 필수 입력·수용 조건

runtime package/model/workspace를 명시하고 해당 pinned artifacts를 검증한다. 세션은 initialize
이후 typed events와 명시적 approval로 실행한다. imageOptions를 typed request로 전달하며
출력 bytes는 bounded PNG 구조·CRC·zlib·길이·치수 검증을 통과해야 한다.
관찰된 PNG 치수와 요청 size는 구분하고 실패를 자동 resize로 은폐하지 않는다.

## 인증·오류·취소

공식 app-server가 선택한 계정과 명시적 auth RPC만 사용한다. disk credentials를 새로 읽거나
복제하는 custom account authority를 만들지 않는다. Debug에는 token/account 식별값을 노출하지
않는다. dispatch 뒤 결과가 불명확한 요청은 `OutcomeUnknown`이며 일반 재실행은 자동화하지 않는다.
HTTP 401의 한 번 auth refresh 정책과 일반 operation retry를 혼동하지 않는다.

## 선택 기능·비목표

IFSC는 별도 public adapter이며 UpAgent request로 강제 통합하지 않는다. generated schema와
native helper는 provenance를 유지한다. unsupported native platform을 stub 성공으로 처리하지 않는다.
