#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
PROMPTGEN_SOURCE="${PROMPTGEN_SOURCE:-$ROOT/../promptGen}"
PROMPTGEN_SOURCE="$(cd "$PROMPTGEN_SOURCE" && pwd -P)"
export PROMPTGEN_SOURCE
command -v cargo >/dev/null 2>&1 || { echo 'cargo is required (Rust 1.97+)' >&2; exit 127; }
python3 "$ROOT/scripts/verify-static.py"
DIST="$ROOT/dist/bin"; mkdir -p "$DIST"
TARGET="${CARGO_TARGET_DIR:-$ROOT/.build-target}"
export CARGO_TARGET_DIR="$TARGET"
cargo build --manifest-path "$ROOT/UpAgent/Cargo.toml" --locked --release -p agent-cli -p agent-review-cli
cargo build --manifest-path "$ROOT/components/perfectpixel/Cargo.toml" --locked --release --bin perfectpixel --bin perfectpixel-mcp
cargo build --manifest-path "$ROOT/components/vergerail/Cargo.toml" --locked --release --bin vergerail-upagent-provider --bin ifsc_text_provider
cargo build --manifest-path "$PROMPTGEN_SOURCE/Cargo.toml" --locked --release -p promptgen-cli
install -m 0755 "$TARGET/release/agent" "$DIST/agent"
install -m 0755 "$TARGET/release/agent-review" "$DIST/agent-review"
install -m 0755 "$TARGET/release/perfectpixel" "$DIST/perfectpixel"
install -m 0755 "$TARGET/release/perfectpixel-mcp" "$DIST/perfectpixel-mcp"
install -m 0755 "$TARGET/release/vergerail-upagent-provider" "$DIST/vergerail-upagent-provider"
install -m 0755 "$TARGET/release/ifsc_text_provider" "$DIST/ifsc_text_provider"
install -m 0755 "$TARGET/release/promptgen" "$DIST/promptgen"
python3 "$ROOT/scripts/audit-shared-contract.py" \
  --agent "$DIST/agent" --promptgen "$DIST/promptgen" \
  --vergerail "$DIST/vergerail-upagent-provider" \
  --image-input "$PROMPTGEN_SOURCE/examples/image-app-icon.json"
( cd "$DIST" && shasum -a 256 agent agent-review perfectpixel perfectpixel-mcp vergerail-upagent-provider ifsc_text_provider promptgen > SHA256SUMS )
echo "built: $DIST"
