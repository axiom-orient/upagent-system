# Operations

## 1. Static source verification

```bash
python3 scripts/verify-static.py
```

## 2. Build

현재 검증 기준은 Rust 1.99.1이며 각 workspace의 lockfile이 필요하다.

```bash
./scripts/build-local.sh
```

기본 출력은 `dist/bin/`이다. 빌드는 PATH fallback을 제품 실행 계약으로 만들지 않는다. 생성된 binary의 절대 경로와 SHA-256을 AgentSpec/provider envelope에 넣는다.

GitHub 소스 checkout은 별도 `../promptGen` dependency를 포함하지 않는다. 공개 릴리스의
소스 패키지는 검증된 PromptGen 작업 트리와 receipt를 함께 담는다. 이를 새 디렉터리에
풀고 `upagent-system/`에서 위 명령을 실행하면 기본 상대 경로가 맞는다. PromptGen의
Git base commit만 clone하면 고정된 로컬 변경까지 재현되지는 않는다.

macOS arm64 바이너리 패키지는 `bin/`, `skills/`, `examples/`, `licenses/`를 포함한다.
패키지를 푼 디렉터리에서 `./bin/agent --skills-root ./skills doctor`로 실행한다.
배포 파일의 SHA-256과 `bin/SHA256SUMS`를 확인한다. 이 릴리스는 Developer ID 공증을
수행하지 않았으며, 외부 모델·이미지 작업에는 별도의 런타임과 인증이 필요하다.

## 3. General LLM

아래 명령은 통합 패키지 루트에서 실행한다.
`UpAgent/examples/agent-spec-creator.template.json`은 그대로 사용할 수 있는
provider-neutral policy 입력이다. 별도 spec 생성 단계는 필요 없다. 일반 대화는
PerfectPixel 또는 PromptGen 설정에 종속되지 않는다.

먼저 실행할 `agent`의 linked plugin과 AgentSpec schema를 확인하고, 사용할 provider
실행 파일의 SHA-256을 계산한다.

```bash
./dist/bin/agent plugins
./dist/bin/agent schema agent-spec
shasum -a 256 /absolute/path/to/upagent-system/dist/bin/vergerail-upagent-provider
```

`plugins` 출력의 `id`가 `upagent.provider.vergerail`인 항목에서
`linkedManifestSha256`을 가져온다. 아래 JSON의 모든 `<...>`와
`/absolute/path/...`를 실제 값으로 바꾸고, caller가 선택한 provider envelope 파일에
저장한다. `manifestSha256`은 plugin manifest의 digest이고 `executableSha256`은
위에서 계산한 provider 실행 파일의 digest다.

```json
{
  "pluginId": "upagent.provider.vergerail",
  "manifestSha256": "<linkedManifestSha256 from agent plugins>",
  "configuration": {
    "protocolSchema": "vergerail.upagent/2",
    "executable": "/absolute/path/to/upagent-system/dist/bin/vergerail-upagent-provider",
    "executableSha256": "<provider executable SHA-256>",
    "runtimePackage": "/absolute/path/to/audited-codex-runtime-package",
    "managedHome": "/absolute/path/to/dedicated-authenticated-codex-home",
    "model": "<account model ID>",
    "workspace": "/absolute/path/to/task-workspace"
  }
}
```

envelope의 세 필드와 `configuration`의 일곱 필드는 모두 필수다. 알 수 없는 필드는
거절한다. 네 경로는 절대경로여야 하며 `..`를 포함할 수 없다.
`model`에는 실제 계정에서 사용할 모델 ID를 명시한다.

같은 spec, envelope, task로 먼저 composition digest를 얻은 뒤 `run --expect-digest`에
전달한다.

```bash
PROVIDER_ENVELOPE=/absolute/path/to/provider-envelope.json
digest="$(./dist/bin/agent --skills-root ./UpAgent/skills compose \
  --spec UpAgent/examples/agent-spec-creator.template.json \
  --provider-config "$PROVIDER_ENVELOPE" \
  --request UpAgent/examples/creator.json \
  | python3 -c 'import json,sys; print(json.load(sys.stdin)["compositionDigest"])')"
./dist/bin/agent --state-root /absolute/path/to/task-state --skills-root ./UpAgent/skills run \
  --spec UpAgent/examples/agent-spec-creator.template.json \
  --provider-config "$PROVIDER_ENVELOPE" \
  --request UpAgent/examples/creator.json \
  --expect-digest "$digest"
```

`compose`는 선언한 configuration과 provider 실행 파일의 identity를 검증한다.
provider를 연결하거나 credential을 읽지 않으며 `providerConnected=false`,
`stateWritten=false`를 반환한다. `runtimePackage`, `managedHome`, `workspace`의
실제 내용이나 runtime·인증·모델 사용 가능 여부는 검증하지 않는다. 실제 `run`에는
감사한 runtime package, 인증된 전용 managed home, 유효한 workspace와 모델이 필요하다.
composition 성공을 실제 provider 실행 성공으로 기록하지 않는다.

## 4. Image generation + deterministic edit

1. 필요하면 `promptgen_compile`을 명시적으로 선택해 generate-mode PromptGen image request를 `image-prompt-artifact/3`으로 컴파일한다. compile은 provider effect가 아니다. PromptGen은 `PROMPTGEN_SOURCE`가 가리키는 lock-pinned sibling Git checkout에서 빌드한다.
2. `generate_new` image task와 `image_generate`를 명시적으로 선택한다.
3. provider가 `model/image_generation`을 광고해야 한다.
4. PerfectPixel executable/path/SHA/protocol/manifest identity를 AgentSpec image block에 pin한다.
5. generated PNG는 UpAgent가 byte/PNG metadata를 검증하고 artifact로 저장한다.
6. `image_render`의 `operations`로 crop/rotate/flip/resize/background removal을 수행하거나 raster/object/text nodes를 합성한다.
7. output descriptor/receipt/dependency closure를 검증하고 다시 `image_inspect`/`image_compare`한다.
8. required assertion과 image review가 통과한 결과만 final candidate로 사용한다.

## 5. Semantic edit

`promptgen_compile` 또는 `image_edit_semantic`을 선택할 때만 PromptGen executable identity를 AgentSpec에 넣는다. `image_edit_semantic`에는 별도로 Codex binary SHA와 `CODEX_HOME` 경로를 pin해야 한다. semantic edit는 Codex 구독 외부 effect이며 후보를 1개로 제한한다. deterministic render와 같은 retry 규칙으로 취급하지 않고, dispatch 뒤 불확실하면 수동 resolution을 요구한다.

## 6. Runtime evidence

live credentials/runtime이 있는 환경에서는 다음을 별도로 기록한다.

- exact binary SHA-256 / protocol manifest
- model/runtime identity
- request/task ID and intent revision
- provider receipt/status
- generated/edited artifact SHA-256
- PerfectPixel inspect/compare result
- cancellation/retry/recovery outcome

mock 또는 static fixture는 production provider 동작의 증거가 아니다.

2026-10-25 실제 계정 검증은 [현재 구현 현황](IMPLEMENTATION_STATUS.md)의 E2E 기록을
따른다. 생성 크기가 요청과 달랐을 때 원본을 버리지 않고 별도 provided task에서
PerfectPixel로 보정했으며, semantic edit는 정확한 source/revision/input에 바인딩된
승인 한 건으로 실행했다. 각 의존 도구의 결과를 받은 뒤 다음 호출을 작성하고, task
전체에서 tool-call ID를 재사용하지 않는다. 고정 비교 계약이 있으면 모델에는 해당
assertion 타입만 노출되며, 정확한 threshold·ID·순서는 여전히 task digest로 검증한다.

## 7. Source packaging

```bash
./scripts/package-source.sh /tmp/upagent-system-final-2026-10-03.tar.gz
sha256sum -c /tmp/upagent-system-final-2026-10-03.tar.gz.sha256
```

패키징은 `components.lock.json`, PerfectPixel component manifest, root source manifest를 현재 bytes에서 다시 생성하고 static verifier가 통과한 경우에만 진행한다. 생성 archive는 build 산출물이 아니라 **검증된 source package**다.
