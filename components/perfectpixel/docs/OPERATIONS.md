# PerfectPixel 로컬 운영·검증

## 기본 gate

```bash
cargo build --locked --workspace --all-targets --all-features
cargo test --locked --workspace --all-targets --all-features
cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
cargo fmt --all -- --check
cargo run --locked --bin perfectpixel -- schema
```

lockfile mismatch는 dependency edge를 조사한다. gate를 통과시키기 위해 lockfile/golden/
테스트를 임의 변경하지 않는다. `tests/docs_contract.rs`는 실제 CLI/schema inventory와
FUNCTION_MATRIX를 대조한다. 정적 문자열 검사는 이 gate의 대체 증거가 아니다.

## 선택 capability

| 경계 | 필수 전제·실행 | 확인할 관찰 결과 |
|---|---|---|
| layered PSD | strict schemaVersion2/document.compile_psd, DocumentIR, exact source ArtifactRef/PixelSpec → `document-psd --request` | source/request drift 거절, merged RGBA/structure readback, 별도 native app opening |
| KTX2 | 호환 `ktx` 실제 실행 파일 SHA pin → `texture-compile --request` | color_srgb+CIEDE2000, linear maximum channel error, normal_map/UASTC, encode→inspect→extract→decode roundtrip, stale SHA 거절 |
| Apple Vision | `tools/apple-vision-helper/main.swift`를 대상 SDK로 빌드/해시 → `vision-foreground-instances --request` | request revision1, provider/adapter/OS provenance, L8 coverage→PP Mask, deterministic close/fill/feather, generation/operation/input identity |
| sprite/motion | normalize/bundle/scaffold/build의 실제 요청 | managed generation 전부 게시 또는 복구; 이전 scene의 build가 새 scaffold를 승인하지 않음 |
| MCP | `perfectpixel-mcp --root <absolute-real-directory>` | 고정 root, fixed tool inventory, 공통 operation 의미·거절 결과; 임의 argv/shell 없음 |

KTX/Vision의 성공 exit code는 제품 성공이 아니다. 실패·취소·timeout·stale result·검증 실패에서
최종 destination이 게시되지 않는지 확인한다. Vision 자체 pixels는 OS/model revision 사이에
bit-exact라고 가정하지 않고 PP가 소유한 후처리·검증만 결정성 claim에 포함한다.

## publication·복구·색

request/source/policy/destination precondition을 expensive work 이전에 잡고 publication 직전에
다시 확인한다. 최종 commit 경계와 취소 수용 시점을 구분한다. 파일 하나의 원자적 publish와
managed artifact-set transaction은 동일 scope가 아니다. 예외/강제 종료 후 부분 state의
복구 의미는 [제품 계약](CONTRACT.md)을 따른다.

unprofiled 일반 raster는 ColorSpec::Unknown이며 ICC bytes를 보존한다. ICC conversion engine은
없으므로 필요한 경우 명시적 Unsupported다. agent image의 선언된 working sRGB와 혼동하지 않는다.

## 실패 기록

operation·cause·context·retryability·failure/cancel/timeout·destination bytes 변경 여부를
함께 기록한다. 현재 실행 결과는 [Current](IMPLEMENTATION_STATUS.md)에만 둔다.
모든 gate 미실행은 NOT_RUN, 외부 환경/계정/배포 허가 미확인은 UNKNOWN으로 남긴다.
