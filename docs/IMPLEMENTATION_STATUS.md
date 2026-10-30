# 통합 구현 현황

## 2026-10-30 공개 배포 구성

- 배포 위치는 [GitHub 저장소](https://github.com/axiom-orient/upagent-system)와
  [v0.1.0 릴리스](https://github.com/axiom-orient/upagent-system/releases/tag/v0.1.0)다.
  README는 소개·빌드·운영 문서 링크로 줄였다.
- macOS arm64 패키지는 검증된 실행 파일 7개, skills·예제와 라이선스/출처 고지를
  포함한다. 다시 푼 파일의 내용·실행 권한·ad-hoc 서명과 CLI 실행을 확인했다.
  Developer ID 공증과 다른 플랫폼 실행은 수행하지 않았다.
- 소스 패키지는 고정 PromptGen source/receipt를 함께 포함한다. 소스와 바이너리
  배포 파일에는 SHA-256 체크섬을 제공한다. 계정·인증·과거 비공개 runtime evidence는
  배포하지 않는다. 이전 기능 테스트를 이번 패키징 작업에서 반복하지 않았다.

## 2026-10-30 통합 릴리스 바이너리 갱신

- 기본 `./scripts/build-local.sh`가 **PASS**다. 현재 lock의 네 source workspace에서
  릴리스 실행 파일 7개를 빌드하고 `dist/bin/`과 `SHA256SUMS`를 갱신했다.
  설치된 7개 파일은 `.build-target/release/` 원본 및 SHA-256 sidecar와 모두 일치한다.
- 실제 `agent`·PromptGen·Vergerail CLI의 공통 schema 7개와 PromptGen 순수 compile
  검사가 통과했다. 외부 계정·모델·이미지 생성 효과는 실행하지 않았다.
- 릴리스 `agent compose`에 새 `dist/bin/perfectpixel` 절대 경로, 현재 바이너리 SHA와
  관측한 capability manifest SHA를 넣어 구성 결과의 동일한 pin을 확인했다.
  compose는 provider 연결·상태 쓰기 없이 완료됐다. 이전 PerfectPixel 바이너리 SHA를
  넣은 별도 구성은 `agent run`의 이미지 모듈 입장에서 expected/observed SHA 불일치로
  거부됐으며, 외부 모델 turn은 요청하지 않았다.
- 새 바이너리를 `PERFECTPIXEL_BIN`으로 지정한 기존 UpAgent 실제 연결 검사 **2개**가
  통과했다. 로컬 PNG inspect·생성 결과 검증과 exact compare assertion 12종을 확인했다.
  이 PNG는 로컬 fixture이며 새 이미지 생성이나 provider 동작의 증거가 아니다.
- 전체 테스트·기존 Clippy 검사·외부 효과 E2E는 반복하지 않았다. 실행 파일 해시는
  [릴리스 체크섬](../dist/bin/SHA256SUMS), 소스 identity는 components lock과 소스
  매니페스트를 따른다.

## 2026-10-30 PerfectPixel Clippy 경고 수정

- `chunks_exact_to_as_chunks`는 같은 크기·순서·나머지 처리의 배열 순회로,
  `obfuscated_if_else`는 같은 결과의 명시적 조건식으로 바꿨다.
- 경고 제외 없이 `cargo clippy --locked --workspace --all-targets --all-features --
  -D warnings`와 `cargo fmt --all -- --check`가 통과했다.
- 관련 기존 테스트 **101개 통과**: 픽셀·알파·합성·CLI·품질 단위 67개,
  inspect 2개·PSD 8개·normalize 4개·승인 무결성 5개·SVG 바이트 golden 2개·벡터
  profile 7개·상태 전이 6개. SVG 바이트·digest와 백엔드 식별자의 기대값은 유지했다.
- 백엔드 소스 변경에 따라 family bundle·route·이전 상태·promotion의 해시 연결을
  갱신했다. 임계값·gate·승인 상태·기존 corpus/평가 기록은 바꾸지 않았다.
  전체 테스트·외부 효과·corpus 재평가는 반복하지 않았다.

## 2026-10-30 경합 후보 수정 후 검증

- `UNRESOLVED.md`의 RAX 복사·MCP 요청 교체·메모리 파일/효과 분류 후보를 순서대로
  재현·수정했다. 수정 전 덮어쓰기·오삭제·루트 검사 우회·효과 누락 실패를 확인했다.
- 필요한 회귀 검사 **24개 통과**: RAX 4개, 메모리 파일 8개, 메모리 도구 2개,
  `test-support` 메모리 runtime 2개, MCP 단위 5개와 stdio 계약 3개.
- UpAgent 변경 모듈의 Clippy `-D warnings`와 두 프로젝트 fmt가 통과했다.
  release `agent`/`agent-review` 빌드와 profiles/schema/review-help/doctor 실행도 통과했다.
- 이 단계에서 남았던 PerfectPixel Clippy 경고는 위의 후속 수정에서 해결했다.
  전체 테스트와 외부 효과 E2E는 사용자 요청에 따라 반복하지 않았다.
- 재현한 사건 순서와 검증 한계는 [미해결·검증 기록](UNRESOLVED.md)에 있다.

## 2026-10-30 이전 리팩터링 검증

- UpAgent `./scripts/verify.sh`: **PASS**. workspace 검사·테스트·Clippy,
  release `agent`/`agent-review` 빌드, CLI 실행 확인과 mbox 계약 검사를 통과했다.
- Vergerail `./scripts/verify.sh`: **PASS**. 로컬 계약 검사·Clippy·보안 감사와
  source package 재빌드를 통과했다.
- PerfectPixel `cargo test --locked --all-targets`: **PASS**.
- 이번 작업에서는 실계정 모델·이미지 생성, Vision·외부 KTX 실행 및 실제 폰트 렌더링
  E2E를 새로 실행하지 않았다. 로컬 결과를 이 실행의 증거로 사용하지 않는다.

아래는 날짜별 과거 실행 기록이다. 당시의 테스트 수·바이너리·의존성 버전을
현재 소스의 검증 결과로 승계하지 않는다. 현재 source identity는
[components lock](../components.lock.json)과 소스 매니페스트가 고정한다.

## 2026-10-25 실제 계정 이미지 E2E

공식 고정 Codex 0.150.1의 파일 집합·SHA와 ChatGPT 로그인을 확인하고, API key 없이
실제 모델·이미지 효과를 실행했다. 초기 계획과 PromptGen은 `gpt-5.6-luna`, 최종 비교
검증은 실제 catalog에 노출된 `gpt-5.6-sol`을 사용했다.

| 단계 | 관찰된 증거 |
|---|---|
| 실제 UpAgent model turn | PASS — 한 호출로 `UPAGENT_LIVE_TEXT_OK`, task succeeded |
| 실제 provider image_generate | PASS — PNG 1개와 verified effect receipt. 요청은 1024²였지만 실제 반환은 1254²라 최초 task는 승격을 거절함 |
| PerfectPixel 보정·편집 | PASS — 같은 원본을 1024² opaque로 resize + horizontal flip, inspect/visual review 후 accepted |
| semantic 승인 | PASS — plan_only에서 awaiting_approval·효과 없음, 발급한 single-use grant는 실제 consumed |
| 실제 Codex subscription edit | PASS — 원본 red mug를 blue로 편집 1회. source/output/runtime SHA, 1024² 정규화, fidelity 1회 PASS, UpAgent receipt 검증 |
| 실제 픽셀 비교·최종 승격 | PASS — changed_ratio 0.8859195709228516 ≥ 0.005, alpha IoU 1.0. task succeeded, stage accepted, required/verified spec digest 동일 |
| 마지막 PNG 재인코딩 | PASS — semantic output과 최종 canonical PNG의 decoded pixel exact_equal, changed ratio/MAE/max error 모두 0 |
| durable inspect·로컬 서명 | PASS — 최종 journal/result 검증, 실행 파일 7개의 ad-hoc signature strict 검증 |

이는 생성물과 영수증을 이어 쓴 **단계별 E2E**다. 최초 task들이 모두 성공한 것은
아니다. 실제 반환 크기 불일치, 모델의 중복 tool ID, 잘못된 assertion 및 미확정 후보
비교를 가드가 거절한 기록도 보존했다. 알려진 이미지 효과는 generation 1회와 semantic
edit 1회이며, 실패 task를 통과로 바꾸거나 외부 이미지 효과를 재전송하지 않았다.
관찰한 화소 변경 비율은 색 변경이 있음을 입증하며, 배경이 bit-exact로 보존됐다는
주장은 하지 않는다. 구도·대상 보존은 실제 픽셀 관찰과 별도 fidelity/visual review다.

실행 중 두 경계를 수정했다. semantic parser는 canonical PromptGen 결과
`{compilation, execution}`을 strict하게 읽고 preflight prompt와 nested receipt를
검증한다. 고정 deterministic 계약이 있으면 모델 요청의 image_compare schema에 그
계약의 assertion 타입만 제시한다. canonical/plugin 정의와 최종 digest/candidate
검사는 유지하며, 파생 모델 요청도 그대로 durable artifact에 저장한다.

수정 후 최종 UpAgent `scripts/verify.sh`는 PASS: **404 tests**, 실패 0, 기본 제외 5다.
현재 sibling PromptGen의 기존 변경 6개 파일과 Git index는 바이트/내용 비교로 보존을
확인했다. 해당 소스의 기존 full-gate receipt와 source pin도 유지한다.

과거 검증 bundle은 실행 host의 비공개 evidence 디렉터리에 보관한다. manifest,
실제 모델 요청/응답, 실패/성공 result, typed receipt, 원본·보정·편집·최종 PNG와
SHA256SUMS는 공개 소스와 배포 파일에 포함하지 않는다.

공개 macOS 배포 공증은 별도 미실행이다. 현재 유효 identity는 Apple Development이며
Developer ID Application identity는 없다. 비 UTF-8 파일명은 이 파일시스템의 errno 92로
검사 불가이고, 과거 1.4 원본 evidence 부재도 새 산출물로 대체하지 않는다.

## 2026-10-25 기본 경로 영수증 복구

기존 sibling PromptGen의 README·toolchain 변경과 Git index를 그대로 보존했다.
미커밋 변경을 포함한 소스를 `pinned-local-working-tree`로 명시하고, Git origin·HEAD,
정확한 maintained-source SHA-256과 현재 성공 영수증을 함께 확인한다. 기존 clean pin은
여전히 dirty checkout을 거절한다. 사용자 변경을 commit·stash·reset하지 않았다.

- Git base: `995fb74268022ee3e1e900ba3d38e96311e7cc3e`
- 검증된 작업 트리: `e61d21ca8b0ce10be8411969d4434c29de4eb0e579e7f89685da23a2aacd0473`
- PromptGen 기본 `./scripts/verify.sh`: **PASS**, 311 tests, 실제 계정 검사 1 ignored.
  check/test/fmt/Clippy/doc/release/audits/CLI·Web smoke 후 영수증을 생성했다.
  과거 image evidence 입력 부재는 영수증의 명시적 미실행 항목으로 유지한다.
- 무결성 회귀: **18개 통과**. 변경된 tree/receipt·잘못된 origin/HEAD·알 수 없는 kind를
  거절하고, staged/index 상태와 source identity의 분리를 확인했다.
- 기본 `python3 scripts/verify-static.py` 및 `./scripts/build-local.sh`: **PASS**.
  `PROMPTGEN_SOURCE`나 `CARGO_TARGET_DIR` override 없이 실제 sibling source와
  `.build-target`을 사용했다. 실행 파일 7개 및 실제 공통 schema/compile 검사를 통과했다.

Rust 1.99.1의 전체 gate를 통과하도록 PromptGen의 deprecated atomic 갱신을 동일한
메모리 순서의 compare-exchange loop로, SHA-256의 고정 크기 chunk 순회를 typed array
순회로 바꿨다. 동시 연결 상한 회귀와 기존 SHA 표준 벡터를 검증했으며 경고 검사는
완화하지 않았다. 공통 `agent-contract` 소스와 catalog 해시는 기존 값을 유지한다.

## 2026-10-25 리팩터링 검증 — 영수증 복구 전

- `scripts/source_integrity.py`가 로컬 tree digest·manifest·패키징 제외 규칙과 외부
  PromptGen snapshot/receipt 로딩을 소유한다. 기존 컴포넌트 해시 형식을 유지했다.
- PromptGen의 prompt-only 호출·결과 검증을 통합했다. 생성 전용 제한과 편집 preflight,
  원본 compiler 결과 digest, dispatch 전후 실패 의미는 유지한다.
- semantic mask 구성 한 번의 중복 executable hashing을 제거했다. compose→run 및 spawn
  직전의 별도 identity 재검증과 `Duration` 정밀도는 유지한다.
- semantic edit의 검증용 PNG 전체 복사 두 곳을 제거하고 생성 영수증의 PNG digest를
  재사용한다. 불확실한 결과의 원본 output/receipt와 candidate authority는 유지한다.
- 이미지 모듈의 두 공개 생성자가 하나의 PerfectPixel identity resolver를 사용한다.
  기본 생성자의 중복 해시를 제거하고 각 생성자의 provider/이미지 검사 우선순위와
  실제 `PathBuf`, 기본 adapter, 작업 직전 identity 재검증은 유지한다.
- durable tool 종료 결과는 `ToolProgress` 하나로 반환한다. 저장 실패도 기존과 같은
  실패 `AgentResult`를 전달한다. 공개 API와 wire/schema 변경은 없다.
- 존재하지 않는 spec renderer와 시작 문서 참조를 제거했다. 기존 creator template을
  직접 쓰며 provider envelope 설정은 운영 문서 한 곳에서 설명한다. 실제 `compose`로
  provider 미연결·state 미생성과 runtime/home 미접근을 확인했다.

Rust 1.99.1의 최종 `scripts/verify.sh`가 **PASS**다. fmt·workspace check·Clippy
(`-D warnings`)·전체 테스트 **396개**, release `agent`/`agent-review` 및 CLI smoke·
mbox wrapper 계약 검사를 통과했다. 기존 코드의 새 Clippy 경고는 테스트 assertion과
중복 종료 경로를 정리하여 해결했다.
공개 async `connect`는 실행을 future polling까지 지연하는 계약을 유지하므로 해당
함수에만 구체적인 async lint 예외를 둔다.

기본 테스트에서 제외되는 실제 바이너리 연결 검사 **4개**도 별도 통과했다: 고정
PromptGen 순수 컴파일, PerfectPixel PNG 검사, 비교 assertion 12종, 명시적 Arial.ttf
artifact를 사용한 텍스트 렌더링이다. 소스 무결성 회귀 테스트 **9개**도 통과했다.
이 검사는 실제 계정의 모델/이미지 생성·semantic edit나 시각 품질을 증명하지 않는다.
비 UTF-8 실행 파일명 검사는 현재 macOS 파일시스템이 해당 이름을 거절하여 명시적
`ignored`다. 실제 경로 보존은 소스에서 확인했지만 해당 실행 회귀는 미검증이다.

통합 `scripts/build-local.sh`도 통과했다. 실행 파일 7개와 SHA-256 sidecar를 생성했고,
실제 세 CLI의 공통 schema 7개와 순수 compile을 비교했다. catalog SHA-256은 기존
`6f0b4e0fef814eaf1e058a552887966b7332e71d42e387cfcac6ad2cb2aaa6a3`을 유지한다.
`dist/bin`에는 이 검증된 실행 파일을 반영했다. 고정한 Vergerail/외부 PromptGen의
기존 `fetch_update`는 Rust 1.99.1 빌드에서 deprecation 경고를 남기지만 빌드는 성공했다.

이 단계의 검증은 당시 lock의 PromptGen commit/tree를 별도 임시 디렉터리에 복원한
source closure에서 수행했다. 당시 기존 sibling의 receipt/tree 불일치와 기본 경로
검증 제한은 위의 후속 영수증 복구에서 해결했다.

## 2026-10-03 통합 기록

## 결론

UpAgent·Vergerail·PromptGen의 공통 process 계약을 `agent-contract` 0.1.0의 한 원본으로
통합했다. DTO 복사·tool ID 재파싱·usage 필드 재매핑·compiler/semantic receipt 복제를
제거하고 같은 Rust 타입과 schema catalog를 직접 사용한다. task/state/approval,
provider effect, compiler 의미와 native Codex protocol의 authority는 각각 유지한다.

Standalone PromptGen 5.0.0은 [공개 GitHub 저장소](https://github.com/axiom-orient/promptGen)에
게시했다. 원격 main과 고정 commit은 `97fdaf27c933e7eab78cce0613bed6c2a2e87c9c`, revision은
`promptgen-5.0.0-20261003.2`, maintained-source SHA-256은
`aff2e32d9bec96e9e7f65f05398d2ce4297bada37703cfd0072d0920dbc931b6`이다. 공개 여부와
원격 commit을 GitHub API 및 `git ls-remote`로 확인했고 fresh clone의 receipt/tree도 일치했다.

UpAgent는 이 공개 commit/tree에 고정한 sibling source로 빌드한다. 제품 소스에는
유지되는 외부 PromptGen 의존성만 포함한다. 원래 입력의 식별값과 소스는
`PACKAGE_PROVENANCE.json` 및 기존 소스 아카이브에 보존한다. 순수 공통 crate를 사용해도
PromptGen을 UpAgent runtime에 종속시키지 않는다. 별도 외부 Rust consumer가 같은 GitHub
commit에서 `agent-contract`만 Git dependency로 가져와 동일 catalog를 생성하는 검사도 통과했다.

## 실제 연결

- provider message·tool·usage·reasoning·request/response/failure·artifact/observation·이미지
  option과 공통 한도는 같은 crate의 타입을 사용한다.
- compiler outcome·diagnostic·detail/background·semantic/fidelity/refinement receipt도
  같은 원본이다. compiler `detail`은 provider `quality`로 변환하지 않는다.
- 카탈로그는 `agent.shared-contract/1`, 기존 provider wire는 `vergerail.upagent/2`다.
  기존 compiler JSON·진단·Studio 결과 형식을 유지한다.
- 기본 feature는 std-only다. serde/schema는 선택하며 I/O·인증·상태 전이·승인은 없다.
- AgentSpec v5와 immutable `upagent.image-prompt-artifact/3`의 digest binding을 유지한다.
  Vergerail은 `image_generate` owner, UpAgent는 승인·candidateRevision·승격·복구 owner다.
- semantic edit는 한 승인에서 후보 1개이며 SHA 고정 Codex 구독만 쓴다. spawn 후 결과가
  불명확하면 `ResolutionRequired`로 남고 자동 재전송하지 않는다.
- Vergerail의 실제 timeout fixture를 `/bin/sleep`으로 수정했고 보안 gate가 거절한
  `rustls`를 공식 수정 버전 0.23.45로 lock했다. native schema/runtime pin은 유지했다.

## 2026-10-03 검증

| 검사 | 증거 |
|---|---|
| PromptGen 전체 `scripts/verify.sh` | PASS — 310 tests, live 검사 1 ignored; check/test/clippy/doc/release/audits/CLI·Studio smoke |
| UpAgent 전체 gate | PASS — 15 crate, 385 tests, 3 ignored; check/clippy/test/release/CLI smoke |
| UpAgent 추가 실제 compiler 검사 | PASS — SHA 고정 production adapter가 로컬 및 GitHub 재빌드 PromptGen을 실제 호출 |
| Vergerail 전체 `scripts/verify.sh` | PASS — 248 tests, 4 ignored; check/test/clippy/doc/deny/native provenance/package 재빌드 |
| 실제 세 CLI catalog 비교 | PASS — 7 schemas 동일; 실제 pure compile schema 검증과 ID 문자·줄바꿈·길이 경계 검사 |
| GitHub fresh source clone | PASS — commit/receipt/tree 동일, release 재빌드, UpAgent 실제 compile adapter 연동 |
| Git dependency 외부 consumer | PASS — common crate만 독립 소비하며 현재 production CLI와 catalog 동일 |
| 통합 source release build | PASS — `dist/bin`의 7 executables 생성, actual catalog/compile audit 통과, SHA sidecar 검사 |
| historical 1.4 image evidence | NOT_RUN — 과거 evidence 입력이 없어 PromptGen receipt에 명시 |
| 실제 계정 기반 이미지 생성·semantic edit·시각 품질 | NOT_RUN — 이번 검증은 pure compile/계약/기존 fixture 범위 |

catalog의 정규화 SHA-256은
`6f0b4e0fef814eaf1e058a552887966b7332e71d42e387cfcac6ad2cb2aaa6a3`이다.

Source archive는 같은 sibling source closure와 verification receipt를 포함한다.
archive의 최종 무결성은 생성한 `.sha256` 및 추출본 static 검증으로 따로 확인한다.
unit/fake-process 결과는 계정 기반 실행이나 시각 품질의 증거로 승계하지 않는다.

상세 비교·선택 근거는 [공통 계약 분석](SHARED_CONTRACT_ANALYSIS.md), 운영 연결은
[PromptGen 통합](PROMPTGEN_INTEGRATION.md), 현재 identity는
[components lock](../components.lock.json)에 있다. GitHub Actions·CI/CD·release 자동화는 없다.
