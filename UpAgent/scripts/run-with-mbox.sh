#!/usr/bin/env bash
set -euo pipefail

fail() {
  printf 'upagent-sandbox: %s\n' "$*" >&2
  exit 64
}

canonical_existing() {
  local value="$1" kind="$2"
  [[ -e "$value" ]] || fail "$kind path does not exist: $value"
  [[ -f "$value" || -d "$value" ]] || fail "$kind path is not a regular file or directory: $value"
  command -v realpath >/dev/null 2>&1 \
    || fail "realpath is required to canonicalize $kind paths"
  realpath -- "$value"
}

canonical_dir() {
  local value="$1"
  [[ -d "$value" ]] || fail "directory does not exist: $value"
  canonical_existing "$value" workspace
}

validate_path_token() {
  local kind="$1" value="$2"
  [[ "$value" != *$'\n'* ]] \
    || fail "$kind contains a newline: $value"
}

validate_csv_token() {
  local kind="$1" value="$2"
  [[ "$value" != *$'\n'* && "$value" != *','* && "$value" != *'='* ]] \
    || fail "$kind contains a newline, comma, or equals sign: $value"
}

resolve_binary() {
  local requested="$1"
  if [[ "$requested" == */* ]]; then
    [[ -x "$requested" ]] || fail "mbox executable is not executable: $requested"
    realpath -- "$requested"
  else
    found="$(command -v "$requested" 2>/dev/null)" \
      || fail "mbox executable not found: $requested"
    [[ "$found" == /* && -x "$found" ]] \
      || fail "mbox command does not resolve to an executable path: $requested"
    realpath -- "$found"
  fi
}

sha256_file() {
  local path="$1"
  if command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$path" | awk '{print $1}'
  elif command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$path" | awk '{print $1}'
  else
    fail "neither shasum nor sha256sum is available for binary pin verification"
  fi
}

append_path_list() {
  local raw="$1" kind="$2"
  [[ -z "$raw" ]] && return 0
  [[ "$raw" != :* && "$raw" != *: && "$raw" != *::* ]] \
    || fail "$kind path list contains an empty path"
  local old_ifs="$IFS"
  local -a values
  IFS=':' read -r -a values <<< "$raw"
  IFS="$old_ifs"
  local value canonical
  for value in "${values[@]}"; do
    validate_path_token "$kind path" "$value"
    canonical="$(canonical_existing "$value" "$kind")"
    case "$kind" in
      read) READ_PATHS+=("$canonical") ;;
      write) WRITE_PATHS+=("$canonical") ;;
      *) fail "internal path-list kind is invalid: $kind" ;;
    esac
  done
}

append_csv_options() {
  local raw="$1" option="$2" kind="$3"
  [[ -z "$raw" ]] && return 0
  [[ "$raw" != *$'\n'* && "$raw" != ,* && "$raw" != *, && "$raw" != *,,* ]] \
    || fail "$kind list contains an empty value or newline"
  local old_ifs="$IFS"
  local -a values
  IFS=',' read -r -a values <<< "$raw"
  IFS="$old_ifs"
  local value
  for value in "${values[@]}"; do
    validate_csv_token "$kind" "$value"
    ARGS+=("$option" "$value")
  done
}

MODE="${UPAGENT_EXECUTION_MODE:-mbox-required}"
if [[ "${1:-}" == "--" ]]; then shift; fi
[[ $# -gt 0 ]] || fail "no UpAgent command supplied; use: run-with-mbox.sh -- <command> [args...]"

case "$MODE" in
  native-trusted)
    [[ "${UPAGENT_ACK_NATIVE_TRUST:-}" == "I_ACCEPT_UNSANDBOXED_EXECUTION" ]] \
      || fail "native-trusted requires UPAGENT_ACK_NATIVE_TRUST=I_ACCEPT_UNSANDBOXED_EXECUTION"
    printf 'upagent-sandbox: mode=native-trusted (explicitly acknowledged)\n' >&2
    exec "$@"
    ;;
  mbox-required) ;;
  *) fail "unsupported UPAGENT_EXECUTION_MODE: $MODE" ;;
esac

WORKSPACE="$(canonical_dir "${UPAGENT_WORKSPACE:-$PWD}")"
MBOX="$(resolve_binary "${MBOX_BIN:-mbox}")"
if [[ -n "${UPAGENT_MBOX_SHA256:-}" ]]; then
  [[ "${UPAGENT_MBOX_SHA256}" =~ ^[0-9a-fA-F]{64}$ ]] \
    || fail "UPAGENT_MBOX_SHA256 must be 64 hex characters"
  MBOX_DIGEST="$(sha256_file "$MBOX")"
  expected_digest="$(printf '%s' "$UPAGENT_MBOX_SHA256" | tr '[:upper:]' '[:lower:]')"
  MBOX_DIGEST="$(printf '%s' "$MBOX_DIGEST" | tr '[:upper:]' '[:lower:]')"
  [[ "$MBOX_DIGEST" == "$expected_digest" ]] \
    || fail "mbox binary digest mismatch: expected=$UPAGENT_MBOX_SHA256 actual=$MBOX_DIGEST"
fi

MBOX_VERSION="$("$MBOX" --version 2>/dev/null)" \
  || fail "mbox --version failed: $MBOX"
[[ "$MBOX_VERSION" =~ ^mbox[[:space:]][0-9]+\.[0-9]+ ]] \
  || fail "unsupported mbox version output: $MBOX_VERSION"

READ_PATHS=("$WORKSPACE")
WRITE_PATHS=("$WORKSPACE")
append_path_list "${UPAGENT_EXTRA_READ_PATHS:-}" read
append_path_list "${UPAGENT_EXTRA_WRITE_PATHS:-}" write

ARGS=(
  --cwd "$WORKSPACE"
)
for path in "${READ_PATHS[@]}"; do
  ARGS+=(--read "$path")
done
for path in "${WRITE_PATHS[@]}"; do
  ARGS+=(--write "$path")
done

if [[ -n "${UPAGENT_TMP_PATH:-}" ]]; then
  TMP_PATH="$(canonical_existing "$UPAGENT_TMP_PATH" temporary)"
  [[ -d "$TMP_PATH" ]] || fail "temporary path is not a directory: $UPAGENT_TMP_PATH"
  ARGS+=(--tmp "$TMP_PATH")
fi

case "${UPAGENT_ALLOW_GIT_METADATA:-0}" in
  0) ARGS+=(--deny-write "$WORKSPACE/.git") ;;
  1) ;;
  *) fail "UPAGENT_ALLOW_GIT_METADATA must be 0 or 1" ;;
esac

if [[ -n "${UPAGENT_PROVIDER_DOMAINS:-}" ]]; then
  [[ "${UPAGENT_PROVIDER_DOMAINS}" =~ ^[A-Za-z0-9.-]+(,[A-Za-z0-9.-]+)*$ ]] \
    || fail "UPAGENT_PROVIDER_DOMAINS must be a comma-separated exact-domain list"
  append_csv_options "$UPAGENT_PROVIDER_DOMAINS" --allow-net provider
fi

if [[ -n "${UPAGENT_ALLOW_ENV:-}" ]]; then
  [[ "${UPAGENT_ALLOW_ENV}" =~ ^[A-Za-z_][A-Za-z0-9_]*(,[A-Za-z_][A-Za-z0-9_]*)*$ ]] \
    || fail "UPAGENT_ALLOW_ENV must be a comma-separated environment-key list"
fi
append_csv_options "${UPAGENT_ALLOW_ENV:-}" --env environment
ARGS+=(--set-env "UPAGENT_SANDBOX_MODE=mbox-required")

# The wrapper owns the external mbox recipe. Hash the resolved executable and
# the exact ordered mbox policy arguments before the child is started. These
# values are compared to a host_adapter AgentSpec here; no forgeable child
# flag is used as security evidence and the kernel receipt remains declarative.
MBOX_DIGEST="${MBOX_DIGEST:-$(sha256_file "$MBOX")}"
POLICY_DIGEST="$(printf '%s\0' "${ARGS[@]}" | shasum -a 256 | awk '{print $1}')"
if [[ -n "${UPAGENT_MBOX_SHA256:-}" ]]; then
  EXPECTED_MBOX_DIGEST="$(printf '%s' "$UPAGENT_MBOX_SHA256" | tr '[:upper:]' '[:lower:]')"
  [[ "$MBOX_DIGEST" == "$EXPECTED_MBOX_DIGEST" ]] \
    || fail "mbox binary digest mismatch: expected=$UPAGENT_MBOX_SHA256 actual=$MBOX_DIGEST"
fi
if [[ -n "${UPAGENT_MBOX_POLICY_SHA256:-}" ]]; then
  [[ "${UPAGENT_MBOX_POLICY_SHA256}" =~ ^[0-9a-fA-F]{64}$ ]] \
    || fail "UPAGENT_MBOX_POLICY_SHA256 must be 64 hex characters"
  EXPECTED_POLICY_DIGEST="$(printf '%s' "$UPAGENT_MBOX_POLICY_SHA256" | tr '[:upper:]' '[:lower:]')"
  [[ "$POLICY_DIGEST" == "$EXPECTED_POLICY_DIGEST" ]] \
    || fail "mbox policy digest mismatch: expected=$UPAGENT_MBOX_POLICY_SHA256 actual=$POLICY_DIGEST"
fi

CHILD_ARGS=("$@")

validate_agent_host_adapter_spec() {
  local spec_path="$1"
  [[ -f "$spec_path" ]] || fail "AgentSpec path is not a regular file: $spec_path"
  command -v python3 >/dev/null 2>&1 \
    || fail "python3 is required to compare host_adapter hashes before agent execution"
  python3 - "$spec_path" "$MBOX_DIGEST" "$POLICY_DIGEST" <<'PY'
import json
import sys

spec_path, adapter_digest, policy_digest = sys.argv[1:]
try:
    with open(spec_path, "rb") as handle:
        spec = json.load(handle)
except Exception as error:
    raise SystemExit(f"cannot read AgentSpec for host_adapter admission: {error}")

isolation = spec.get("isolation")
if not isinstance(isolation, dict):
    raise SystemExit("AgentSpec isolation must be an object")
if isolation.get("kind") != "host_adapter":
    raise SystemExit(0)
if isolation.get("adapter") != "mbox":
    raise SystemExit("AgentSpec host_adapter adapter must be mbox")
if str(isolation.get("adapterSha256", "")).lower() != adapter_digest.lower():
    raise SystemExit("AgentSpec adapterSha256 does not match the measured mbox binary")
if str(isolation.get("policySha256", "")).lower() != policy_digest.lower():
    raise SystemExit("AgentSpec policySha256 does not match the measured mbox policy")
PY
}

if [[ "$(basename -- "${CHILD_ARGS[0]}")" == "agent" ]]; then
  run_selected=0
  spec_path=""
  for ((index = 0; index < ${#CHILD_ARGS[@]}; index += 1)); do
    child_arg="${CHILD_ARGS[index]}"
    if [[ "$child_arg" == "run" ]]; then
      run_selected=1
    elif [[ "$child_arg" == "--spec" ]]; then
      next_index=$((index + 1))
      [[ $next_index -lt ${#CHILD_ARGS[@]} ]] || fail "agent run --spec requires a path"
      spec_path="${CHILD_ARGS[next_index]}"
    elif [[ "$child_arg" == --spec=* ]]; then
      spec_path="${child_arg#--spec=}"
    fi
  done
  if [[ $run_selected -eq 1 ]]; then
    [[ -n "$spec_path" && "$spec_path" != "-" ]] \
      || fail "agent run through mbox requires a file-backed --spec for hash admission"
    spec_path="$(canonical_existing "$spec_path" AgentSpec)"
    validate_agent_host_adapter_spec "$spec_path"
  fi
fi

printf 'upagent-sandbox: mode=mbox-required workspace=%s git=%s network=%s tmp=%s\n' \
  "$WORKSPACE" "${UPAGENT_ALLOW_GIT_METADATA:-0}" \
  "${UPAGENT_PROVIDER_DOMAINS:-none}" "${UPAGENT_TMP_PATH:-none}" >&2
exec "$MBOX" "${ARGS[@]}" -- "${CHILD_ARGS[@]}"
