#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
cd "$ROOT"

if [[ ! -f "$ROOT/Cargo.toml" ]]; then
  echo "not a Rust repository root: $ROOT" >&2
  exit 2
fi

printf 'CLEAN: inventory before\n'
"$ROOT/scripts/cleanup_inventory.sh" "$ROOT"

if [[ -d "$ROOT/target" ]]; then
  if command -v cargo >/dev/null 2>&1; then
    cargo clean --manifest-path "$ROOT/Cargo.toml"
  else
    printf 'CLEAN: cargo unavailable; removing the verified Cargo target only\n'
    rm -rf -- "$ROOT/target"
  fi
fi

while IFS= read -r -d '' path; do
  rm -rf -- "$path"
done < <(
  find "$ROOT" \
    -path "$ROOT/.git" -prune -o \
    -path "$ROOT/target" -prune -o \
    -type d -name '__pycache__' -print0
)

find "$ROOT" \
  -path "$ROOT/.git" -prune -o \
  -path "$ROOT/target" -prune -o \
  -type f \( \
    -name '.DS_Store' -o \
    -name '*.pyc' -o \
    -name '*.tmp' -o \
    -name '*.lock.tmp' -o \
    -name '*.swp' -o \
    -name '*~' \
  \) -delete

printf 'CLEAN: inventory after\n'
"$ROOT/scripts/cleanup_inventory.sh" "$ROOT"
