#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TARGET_DIR="${CARGO_TARGET_DIR:-$ROOT/target}"
cd "$ROOT"

if find "$ROOT" \
  -path "$ROOT/.git" -prune -o \
  \( -type d -o -type l \) -path '*/.github/workflows' -print -quit | grep -q .; then
  echo "VERIFY: forbidden .github/workflows found" >&2
  exit 4
fi

if ! command -v cargo >/dev/null 2>&1; then
  echo "VERIFY: cargo is required" >&2
  exit 3
fi

if ! command -v python3 >/dev/null 2>&1; then
  echo "VERIFY: python3 is required for the static agent-only contract gate" >&2
  exit 3
fi
python3 scripts/verify-agent-only.py

cargo metadata --no-deps --format-version 1 >/dev/null
cargo fmt --all -- --check
cargo check --locked --workspace --all-features
cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
cargo test --locked --workspace --all-features
cargo build --locked --release -p agent-cli -p agent-review-cli

BINARY="$TARGET_DIR/release/agent"
REVIEW_BINARY="$TARGET_DIR/release/agent-review"
STATE_ROOT="$(mktemp -d)"
cleanup() { rm -rf "$STATE_ROOT"; }
trap cleanup EXIT

"$BINARY" profiles >/dev/null
"$REVIEW_BINARY" --help >/dev/null
"$BINARY" --skills-root "$ROOT/skills" skills >/dev/null
"$BINARY" schema task-v3 >/dev/null
"$BINARY" --state-root "$STATE_ROOT/state" --skills-root "$ROOT/skills" doctor >/dev/null

# Strict mbox host-adapter contract is independent of an installed mbox binary.
bash "$ROOT/scripts/test-run-with-mbox.sh"

echo "VERIFY: PASS"
