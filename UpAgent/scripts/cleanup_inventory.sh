#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 1 ]]; then
  echo "usage: $0 <repository-root>" >&2
  exit 2
fi

ROOT="$(cd "$1" && pwd -P)"
if [[ ! -f "$ROOT/Cargo.toml" ]]; then
  echo "not a Rust repository root: $ROOT" >&2
  exit 2
fi

git_state="unavailable"
if git -C "$ROOT" rev-parse --show-toplevel >/dev/null 2>&1; then
  git_state="available"
fi

printf 'root=%s\n' "$ROOT"
printf 'git=%s\n' "$git_state"
printf 'working_tree_bytes='
du -sk "$ROOT" | awk '{ print $1 * 1024 }'
printf 'working_tree_files='
find "$ROOT" -type f -not -path "$ROOT/.git/*" | wc -l | awk '{ print $1 }'

report() {
  local path="$1"
  local kind="$2"
  local relative="${path#"$ROOT/"}"
  local size
  size="$(du -sk "$path" | awk '{ print $1 * 1024 }')"

  local state="$git_state"
  if [[ "$git_state" == "available" ]]; then
    if git -C "$ROOT" check-ignore -q -- "$relative"; then
      state="ignored"
    elif git -C "$ROOT" ls-files --error-unmatch -- "$relative" >/dev/null 2>&1; then
      state="tracked"
    else
      state="untracked"
    fi
  fi
  printf 'candidate\tkind=%s\tbytes=%s\tgit=%s\tpath=%s\n' "$kind" "$size" "$state" "$relative"
}

while IFS= read -r -d '' path; do
  report "$path" "cargo-target"
done < <(
  find "$ROOT" \
    -path "$ROOT/.git" -prune -o \
    -type d -name target -print0
)

while IFS= read -r -d '' path; do
  report "$path" "reproducible-file"
done < <(
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
    \) -print0
)
