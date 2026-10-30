# UpAgent System

텍스트와 이미지 작업을 실행하고, 승인·상태·결과를 관리하는 에이전트 시스템입니다.

- **UpAgent**: 작업 실행과 승인, 결과 관리
- **Vergerail**: 모델 호출과 이미지 생성
- **PerfectPixel**: 이미지 편집과 검증
- **PromptGen**: 선택적 프롬프트 컴파일과 semantic edit

[Releases](https://github.com/axiom-orient/upagent-system/releases)에서 macOS arm64 실행 파일과 소스 패키지를 받을 수 있습니다.
소스 패키지에는 빌드에 필요한 고정 PromptGen 스냅샷이 포함됩니다.

## 빌드

Rust 1.99.1, Python 3.11 이상과 `jsonschema`가 필요합니다. 소스 패키지를 푼 뒤 실행합니다.

```bash
cd upagent-system
./scripts/build-local.sh
./dist/bin/agent --skills-root ./UpAgent/skills doctor
```

모델·이미지 생성에는 별도 런타임과 인증 설정이 필요합니다.
[운영 안내](docs/OPERATIONS.md) · [검증 기록](docs/IMPLEMENTATION_STATUS.md)

라이선스: [UpAgent](UpAgent/LICENSE) · [Vergerail](components/vergerail/LICENSE) · [PerfectPixel](components/perfectpixel/LICENSE). PromptGen 라이선스는 소스 패키지에 포함됩니다.
