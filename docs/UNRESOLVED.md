# 미실행·미검증 항목

아래 과거 실행 표는 2026-10-25까지의 기록이다. 최신 로컬 검증은
[통합 구현 현황](IMPLEMENTATION_STATUS.md)의 2026-10-30 항목을 따른다.
과거 실계정·서명·아카이브 실행을 이번 소스의 새 실행 결과로 해석하지 않는다.

## 2026-10-30 경합 후보 재현·수정

기존 세 후보를 순서대로 재현하고 수정했다. 파일 검사와 실행 사이에 실제 파일을
교체하는 순서를 고정했으며, 수정 전 실패와 수정 후 통과를 각각 확인했다.

| 후보 | 재현한 문제 | 수정·회귀 증거 |
|---|---|---|
| [RAX 복사·롤백](../UpAgent/crates/rax-runtime/src/fs/copy.rs) | 대상 확인 뒤 생성된 파일은 덮어쓰고, 디렉터리는 생성 실패 후 삭제함 | 소유한 임시 복사본을 검증한 뒤 no-replace rename으로 설치. 경쟁 대상 보존·실패 정리·정상 내용/권한/출력 경로 4개 통과 |
| [MCP 요청](../components/perfectpixel/src/mcp/server.rs) | 검사 직후 요청을 교체하면 검사하지 않은 외부 symlink 입력으로 결과를 게시함 | 검사한 bytes의 SHA-256을 Application snapshot과 비교한 뒤 중첩 입력을 읽음. 세 요청 operation의 교체 거부와 정상 요청, 기존 MCP 단위·stdio 계약 8개 통과 |
| [메모리 파일](../UpAgent/crates/agent-memory/src/private_fs.rs)·[도구 결과](../UpAgent/crates/agent-runtime/src/memory_tools.rs) | 임시 symlink가 외부 핸들 열기 검사를 통과함. 실제 rename 뒤 fsync 오류는 새 내용이 보이는데도 효과 없음으로 분류됨 | 보유 디렉터리 기준 no-follow open/rename과 핸들 identity 검사. rename 이후 오류는 CommitUncertain으로 전달해 resolution을 요구. 파일·도구·runtime 메모리 검증 12개 통과 |

fsync 실패는 실제 snapshot 교체 후 `/dev/null` 핸들에 fsync를 호출해 발생시켰다.
실제 디스크 장애를 유발한 검사는 아니다. 이번 수정이 모든 same-user hostile process
경합을 제거했다는 주장은 하지 않는다. 현재 지원 host의 보존 동작과 위 사건 순서만
검증했으며, Linux·Windows 실행은 새로 검증하지 않았다.

후속 수정에서 PerfectPixel의 `chunks_exact_to_as_chunks`·`obfuscated_if_else` 경고도
해결했다. 경고 제외 없는 전체 target/feature Clippy와 fmt, 관련 기존 테스트 101개가
통과했다. 소스 변경으로 달라진 backend/bundle/route/promotion 해시 연결도 갱신했다.
SVG 바이트와 백엔드 식별자의 기존 기대값, 승인 조건·임계값·과거 평가 기록은
유지했다. 전체 테스트·외부 효과 E2E·corpus 재평가는 반복하지 않았다.

## 2026-10-25까지의 실행 증거

| 항목 | 상태 | 필요한 증거 |
|---|---|---|
| UpAgent Rust workspace | PASS | Rust 1.99.1 최종 `scripts/verify.sh`: check/fmt/clippy, 테스트 404개, release/CLI/mbox smoke; 실제 계정 E2E 별도 완료 |
| PromptGen 5.0.0 source gate | PASS, 명시적 미실행 있음 | 현재 sibling의 `./scripts/verify.sh`: 311 tests, live 1 ignored; 과거 1.4 image evidence audit 입력은 없음 |
| 통합 source 빌드 | PASS | override 없이 기본 `scripts/build-local.sh`가 네 Rust source workspace에서 binary 생성 |
| PromptGen image `prompt-only` runtime | PASS | source-built binary로 image-app-icon 요청을 valid 결과로 컴파일 |
| source archive를 푼 뒤 재검증·재빌드 | PASS | SHA sidecar 확인, `.git` 없는 archive static gate, extracted PromptGen release build와 pure compile smoke |
| PerfectPixel positive text raster runtime | PASS | 2026-10-25 source-built binary + 명시적 Arial.ttf artifact, render receipt/layout/dependency evidence |
| 현재 개발 환경의 sibling PromptGen | PASS | 기존 변경·index 보존, 실제 full gate 후 receipt 갱신, 명시적 local pin으로 기본 정적 검증·빌드 통과 |
| 비 UTF-8 실행 파일 경로 | NOT_RUN_ENVIRONMENT | 실제 파일명 생성이 errno 92로 거절됨; 소스의 PathBuf 보존만 확인 |
| Vergerail 실제 model turn / image generation | PASS | 실제 ChatGPT model turn 및 생성 PNG 1개, verified UpAgent effect receipt |
| Codex subscription semantic edit | PASS | 단일 승인 consumed, 실제 source/output/runtime SHA binding, fidelity PASS와 출력 PNG |
| generated → PerfectPixel edit/compare E2E | PASS, 단계별 | 원본 1254² → 보정/flip 1024² → semantic edit → 고정 changed_ratio 비교·visual review → accepted; 실패 task 기록도 보존 |
| macOS 로컬 서명/runtime | PASS | 공식 runtime hash와 실제 실행, Rust 실행 파일 7개 ad-hoc signature strict 검증 |
| macOS 공개 배포 공증 | NOT_RUN | Developer ID Application identity 없음(Apple Development만 있음); 공개 배포/공증 성공은 주장하지 않음 |

Static 검사·fake process fixture·source build 결과는 실제 provider 효과나 시각 품질 증거로
승격하지 않는다. GitHub 공개 저장소 배포는 사용자 검증 전이다.
