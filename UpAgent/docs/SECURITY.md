# Security

## 권한 모델

- profile manifest가 capability와 tool surface를 제한한다.
- skill은 profile allowlist와 required capability를 모두 통과해야 한다.
- trusted module은 compile-time composition root에서만 등록한다.
- public `ToolContext`는 workspace path나 mutation variant를 노출하지 않는다.
- workspace mutation은 RAX-backed semantic tool만 수행한다.
- filesystem/process/Git은 RAX workspace capability, plan, precondition, receipt를 사용한다.
- 같은 `AgentRuntime` 안에서 같은 workspace의 task grant는 한 writer로 직렬화한다.
- provider credential은 memory에만 두며 agent state store에 저장하지 않는다.
- process tool은 provider credential 환경변수를 상속하지 않는다.

## Cancellation과 실패 안전성

- caller-owned token은 task 또는 명시적 batch만 취소한다.
- 각 effect는 private RAX token을 사용해 다른 병렬 task로 취소가 누출되지 않는다.
- provider protocol 거절 시 active request를 취소한다.
- started mutation은 future를 버려 background에서 계속 실행시키지 않는다.
- external integration interruption은 실행 여부가 불명확하므로 `ResolutionRequired`다.
- terminal evidence 기록 실패는 irreversible effect 유무에 따라 `StorageFailed`와 `ResolutionRequired`를 구분한다.

## 저장

state, memory, journal, artifact root는 real directory여야 하며 symlink를 거절한다. StateStore::open은 workspaces, plans, runs와 이미 존재하는 workspace execution lock을 chmod 전에 검사하고, plan·lock 민감 경계에서도 다시 검사한다. pre-existing symlink는 SymlinkNotAllowed로 fail closed 한다. journal session/task directory 이름은 strict UTF-8 ID로 해석한다. Unix에서는 private directory/file permission을 강제한다. terminal result와 content-addressed artifact는 create-only 의미로 기록하고 기존 bytes를 재검증한다. recovery resolution은 task journal과 workspace execution lock이 해제된 뒤에만 기록한다.

ReviewCampaign도 request/recipe/lane/candidate/snapshot/result/claim을 create-only로
기록하고, cached result와 repair claim 전에 전체 lane raw/parsed evidence와
digest/reducer equality를 다시 검증한다. reviewer profile은 workspace, tool,
memory write를 받지 않는다. 같은 사용자의 동시 경로 교체에 대한 일반적인 OS
TOCTOU 한계는 capability storage를 이번 범위에서 재작성하지 않았으며 아래
residual로 남는다.

runtime writer lease와 RAX execution lock은 UpAgent가 소유한 실행끼리의 중복
mutation을 막는다. 별도 process, IDE, shell 또는 다른 사용자 프로세스는 이
process-local lease에 참여하지 않는다. host가 task 전용 workspace를 제공하지
않으면 Git status/diff와 filesystem observation은 한 시점의 증거이며, 이후 외부
변경까지 봉인하지 않는다.

## 명시적 제약

- RAX는 OS sandbox가 아니다. child process는 현재 사용자 권한으로 실행된다.
- `native` isolation은 신뢰된 host 실행 선언이며 containment 증명이 아니다. 선택적
  mbox wrapper도 실제 설치·정책 probe 없이는 OS containment 증거가 아니다.
- `ToolModule`은 신뢰된 linked Rust code이며 OS sandbox가 아니다. ambient OS API 사용을 기술적으로 차단하지 않는다.
- same-user hostile process가 경로를 동시에 교체하는 모든 TOCTOU를 플랫폼 전반에서 증명하지 못했다.
- Windows는 지원 범위가 아니다. Windows junction/reparse point와 atomic
  replacement에 대한 보안 보장을 제공하지 않는다.
- `task_grant`는 task-wide 사전 승인이다. 후속 task가 `plan_only` fingerprint를 cryptographically consume하는 계약은 아니다.
- synchronous filesystem syscall은 즉시 취소되지 않을 수 있다. runtime은 mutation 결과를 확인하기 전 성공·취소를 가정하지 않는다.
- external module의 공급망·credential·idempotency는 해당 integration이 별도로 증명해야 한다.

## 선택적 promptGen 경계

PromptGenSpec의 executable은 absolute regular executable, configured SHA-256과 일치해야 하며 invocation 전에 다시 확인한다. 이는 신뢰된 host 경계에서 identity drift를 거절하는 장치다. 같은 사용자 권한의 공격자가 check와 spawn 사이에 file을 교체하는 모든 TOCTOU를 제거했다고 주장하지 않는다.

Local compile/preflight child에는 provider credential을 전달하지 않는다. semantic edit는 AgentSpec에 pin된 Codex executable·SHA-256·`CODEX_HOME`으로 ChatGPT 구독 runtime을 실행하며 API key 경로를 사용하지 않는다. inherited `OPENAI_API_KEY`와 `CODEX_API_KEY`는 PromptGen child에 전달하지 않는다. raw child stderr는 외부 오류에 그대로 재노출하지 않는다. 같은 사용자 process가 Codex 인증 상태를 읽을 수 있다는 OS threat는 남는다.

output/receipt는 regular/non-symlink/nonempty/bounded file이어야 한다. Unix에는 O_NOFOLLOW/O_NONBLOCK도 사용한다. 부분 증거는 opaque로 저장하고 typed terminal ledger에 연결하되 image authority로 쓰지 않는다. Process-group cleanup과 bounded drain의 실제 OS 검증은 별도 qualification이며 test shim을 sandbox 증거로 쓰지 않는다.
