# UpAgent — 정체성과 발전 원칙

## 정체성

UpAgent는 host가 고정한 의도·계약·권한을 한 Agent runtime으로 실행하고, 실제 결과를 독립적인 artifact·receipt·journal 증거로 확인하는 시스템이다. caller는 CLI 사용자, 로컬 도구와 embedding host다. 모델은 다음 행동과 평가 근거를 제안하고 프로그램은 admission·effect·완료를 결정한다.

핵심 가치는 모델의 주장, 도구 실행, 데이터 저장, 계약 수용을 구분하는 데 있다. provider·profile·skill·memory·tool은 일반 Agent의 capability로 연결하되 내부 책임은 합치지 않는다. 독립 `agent-review`는 별도 review 제품이다. PerfectPixel은 외부 deterministic image mechanism이며 promptGen은 선택적 prompt compiler/semantic edit mechanism이다.

범위는 명시적 composition 이후의 task 실행·근거·복구다. GUI editor, 전역 OS sandbox, 임의 plugin code loader, 외부 제품의 승인/attempt owner가 되는 것은 비목표다.

## 변하면 안 되는 것

Host의 task와 선택된 provider/profile binding이 실행 전에 고정되어야 한다. 입력 digest가 달라지면 실행을 거절해야 한다. source artifact·기존 byte identity·required exact assertions·승인 범위를 몰래 대체하거나 약화하지 않는다.

한 concern·scope에 authoritative owner를 하나만 둔다. task 종료는 SessionMachine, image qualification은 ImageDirectorState, 저장 바이트는 ArtifactStore, durable 사건은 journal이 소유한다. 관찰은 승인과 다르다. 효과 확인은 요구 충족과 다르다. 불확실한 외부 효과를 성공·실패 확정으로 위장하거나 자동 재전송하지 않는다.

공개 provider-neutral ports, 독립 review, deterministic `image_render.operations`, 유효한 optional capability, LICENSE/NOTICE/attribution을 보존한다. proposal·문서·mock을 production evidence로 승격하지 않는다.

## 변경 가능한 것

같은 계약·권한·byte identity·failure semantics를 보존하면 내부 알고리즘, UI host, provider plugin, persistence mechanism, package 구성과 optional adapter를 교체·확장할 수 있다. 외부 executor 교체는 명시적 identity·capability·receipt 수용 검사를 통과해야 한다.

현재 구현의 모든 bool·파일명·모듈 배치를 영구 불변으로 선언하지 않는다. 파생 projection과 cache는 재구성 가능하지만 원 증거와 권한은 잃지 않는다. 변경 허용은 추가 작업 승인이나 미결정 제안의 채택을 뜻하지 않는다.

## 발전 방향

우선 판단 기준은 필수 의미 보존, 실행 경로의 짧음, 권한의 명확함, 결과의 독립 검증 가능성이다. 새 capability는 Agent에서 발견·선택·실행 가능하되 새로운 state machine이나 암묵 activation이 필요하지 않아야 한다.

이미지 기능은 의도와 provenance, deterministic pixel evidence, 모델의 시각 판단을 분리하면서 연결한다. 정확한 화면 배치는 typed deterministic contract로 다루고 생성형 품질은 관찰 evidence와 명시적 수용으로 다룬다. 선택 기능은 코어의 필수 의존성이 되지 않는다. 일정·완료 이력·남은 작업은 이 문서의 범위가 아니다.
