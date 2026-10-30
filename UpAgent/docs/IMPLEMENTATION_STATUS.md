# UpAgent — 구현 현황

확인일: **2026-10-30**. UpAgent는 composition 이후의 task/state/approval/candidate authority를
유지한다. Vergerail이 model/image-generation effect, PerfectPixel이 deterministic pixel
작업, 외부 PromptGen이 image prompt compile과 선택적 semantic edit mechanism을 담당한다.

## 현재 이미지 경계

- `AgentSpec` schema v5는 PromptGen executable identity를 pin한다. semantic edit 선택 시에만
  추가 Codex executable·SHA-256·home이 필수다.
- `promptgen_compile`은 별도 source checkout에서 빌드한 PromptGen의 image `prompt-only`
  결과만 `upagent.image-prompt-artifact/3`으로 저장한다. artifact의 detail은 prompt 의도다.
- `image_generate`는 Vergerail provider operation이다. PromptGen `codex-imagegen`은 여기서
  사용할 수 없다.
- `image_edit_semantic`은 명시 선택·source/revision 검사·approval 후 Codex 구독 runtime을
  사용한다. 한 승인당 최대 후보 1개이며 output은 strict receipt 검증과 PerfectPixel 재검사
  뒤에야 후보 경로로 들어간다.
- process는 provider credential 환경을 상속하지 않는다. 이 semantic 경계는 API key를
  사용하지 않으며, spawn 뒤 불확실한 결과는 자동 retry하지 않는다.

## 검증

경합 수정 후 RAX 복사 4개·메모리 파일 8개·메모리 도구 2개·메모리 runtime 2개가
통과했다. 변경 모듈의 Clippy(`-D warnings`), fmt, release `agent`/`agent-review` 빌드와
CLI smoke도 통과했다. 전체 테스트는 반복하지 않았다. 이전 `./scripts/verify.sh`
통과를 이번 수정의 전체 gate로 승계하지 않는다. 실제 sibling source의 기존 변경과
staging 상태는 보존하며, source pin·verification receipt는 통합 lock에서 확인한다.
현재 검증과 과거 실계정 기록은 [통합 구현 현황](../../docs/IMPLEMENTATION_STATUS.md)에
날짜별로 구분한다. 이번 작업에서 실계정 E2E를 새로 실행하지 않았다.
비 UTF-8 실행 파일명 검사는 현재 macOS 파일시스템이 이름 생성을 거절하여 명시적으로
제외했으며 실행 검증은 남아 있다.

prompt-only 호출·검증은 어댑터 한 곳, semantic mask 실행 파일 정규화는 한 함수가
소유한다. 두 이미지 모듈 생성자도 동일한 PerfectPixel identity resolver를 사용한다.
같은 생성 과정의 중복 해시와 semantic edit 검증용 PNG 복사를 제거했지만,
dispatch 직전 재검증·취소·불확실한 실행의 evidence 보존은 유지했다. durable tool의
종료 결과도 같은 실패 정보를 담은 `ToolProgress` 하나로 전달한다. 공개 계약과
schema는 바꾸지 않았다.

2026-10-25에는 실제 ChatGPT 계정에서 model turn·이미지 생성·단일 승인 Codex 편집을 실행했다. 생성된
1254² 원본을 PerfectPixel로 1024²로 보정·반전했고, 편집 영수증의 원본·출력·runtime
SHA와 fidelity PASS를 확인했다. 최종 고정 changed_ratio 비교·visual review 후 별도
검증 task가 succeeded/accepted로 종료됐다. 초기 실패와 원본도 보존했으며 외부 이미지
효과를 자동 재전송하지 않았다. 고정 계약의 모델용 비교 schema를 좁히고 canonical
PromptGen 결과 wrapper를 strict 파싱하도록 수정했다. 이 증거는 same-user OS isolation이나
공개 배포 공증을 증명하지 않는다. 상세 실행 기록은 저장소 root의
[통합 구현 현황](../../docs/IMPLEMENTATION_STATUS.md)에 있다.
