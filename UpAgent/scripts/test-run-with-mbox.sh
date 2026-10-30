#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
WRAPPER="$ROOT/scripts/run-with-mbox.sh"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT
mkdir -p "$TMP/work" "$TMP/read" "$TMP/write" "$TMP/tmp"
chmod 700 "$TMP/tmp"
printf 'auth-test\n' > "$TMP/read/auth.json"
printf 'auth-special\n' > "$TMP/read/auth,prod=.json"

cat > "$TMP/mbox" <<'FAKE'
#!/usr/bin/env bash
set -euo pipefail
if [[ "${1:-}" == "--version" ]]; then
  if [[ -n "${FAKE_MBOX_VERSION_SIDE_EFFECT:-}" ]]; then
    printf 'version-executed\n' > "$FAKE_MBOX_VERSION_SIDE_EFFECT"
  fi
  echo "${FAKE_MBOX_VERSION:-mbox 1.1.0-test}"
  exit 0
fi
printf '%s\n' "$@" > "${FAKE_MBOX_ARGS:?}"
FAKE
chmod +x "$TMP/mbox"

ARGS_FILE="$TMP/args"
MBOX_BIN="$TMP/mbox" \
FAKE_MBOX_ARGS="$ARGS_FILE" \
UPAGENT_WORKSPACE="$TMP/work" \
UPAGENT_EXTRA_READ_PATHS="$TMP/read/auth.json:$TMP/read/auth,prod=.json:$TMP/read" \
UPAGENT_EXTRA_WRITE_PATHS="$TMP/write" \
UPAGENT_TMP_PATH="$TMP/tmp" \
UPAGENT_PROVIDER_DOMAINS="api.openai.com,chatgpt.com" \
UPAGENT_ALLOW_ENV="OPENAI_API_KEY,HOME" \
UPAGENT_MBOX_SHA256="$(shasum -a 256 "$TMP/mbox" | awk '{print $1}')" \
"$WRAPPER" -- /bin/echo hello

WORKSPACE="$(realpath -- "$TMP/work")"
AUTH_FILE="$(realpath -- "$TMP/read/auth.json")"
SPECIAL_AUTH_FILE="$(realpath -- "$TMP/read/auth,prod=.json")"
READ_DIR="$(realpath -- "$TMP/read")"
WRITE_DIR="$(realpath -- "$TMP/write")"
TMP_DIR="$(realpath -- "$TMP/tmp")"
grep -Fx -- '--cwd' "$ARGS_FILE" >/dev/null
grep -Fx -- "$WORKSPACE" "$ARGS_FILE" >/dev/null
grep -Fx -- '--read' "$ARGS_FILE" >/dev/null
grep -Fx -- "$AUTH_FILE" "$ARGS_FILE" >/dev/null
grep -Fx -- "$SPECIAL_AUTH_FILE" "$ARGS_FILE" >/dev/null
grep -Fx -- "$READ_DIR" "$ARGS_FILE" >/dev/null
grep -Fx -- '--write' "$ARGS_FILE" >/dev/null
grep -Fx -- "$WORKSPACE" "$ARGS_FILE" >/dev/null
grep -Fx -- "$WRITE_DIR" "$ARGS_FILE" >/dev/null
grep -Fx -- '--tmp' "$ARGS_FILE" >/dev/null
grep -Fx -- "$TMP_DIR" "$ARGS_FILE" >/dev/null
grep -Fx -- '--deny-write' "$ARGS_FILE" >/dev/null
grep -Fx -- "$WORKSPACE/.git" "$ARGS_FILE" >/dev/null
grep -Fx -- '--allow-net' "$ARGS_FILE" >/dev/null
grep -Fx -- 'api.openai.com' "$ARGS_FILE" >/dev/null
grep -Fx -- 'chatgpt.com' "$ARGS_FILE" >/dev/null
grep -Fx -- '--env' "$ARGS_FILE" >/dev/null
grep -Fx -- 'OPENAI_API_KEY' "$ARGS_FILE" >/dev/null
grep -Fx -- 'HOME' "$ARGS_FILE" >/dev/null
grep -Fx -- '--set-env' "$ARGS_FILE" >/dev/null
grep -Fx -- 'UPAGENT_SANDBOX_MODE=mbox-required' "$ARGS_FILE" >/dev/null
grep -Fx -- '/bin/echo' "$ARGS_FILE" >/dev/null
grep -Fx -- 'hello' "$ARGS_FILE" >/dev/null

FAKE_MBOX_ARGS="$ARGS_FILE" \
MBOX_BIN="$TMP/mbox" \
UPAGENT_WORKSPACE="$TMP/work" \
UPAGENT_ALLOW_GIT_METADATA=1 \
"$WRAPPER" -- /bin/true
if grep -Fx -- '--deny-write' "$ARGS_FILE" >/dev/null; then
  echo 'git deny rule remained in allow-git mode' >&2
  exit 1
fi

if UPAGENT_EXECUTION_MODE=native-trusted "$WRAPPER" -- /usr/bin/true 2>/dev/null; then
  echo 'native mode succeeded without explicit acknowledgement' >&2
  exit 1
fi
UPAGENT_EXECUTION_MODE=native-trusted \
UPAGENT_ACK_NATIVE_TRUST=I_ACCEPT_UNSANDBOXED_EXECUTION \
"$WRAPPER" -- /usr/bin/true

if MBOX_BIN="$TMP/missing" UPAGENT_WORKSPACE="$TMP/work" "$WRAPPER" -- /usr/bin/true 2>/dev/null; then
  echo 'missing mbox silently fell back' >&2
  exit 1
fi

if FAKE_MBOX_VERSION=not-mbox MBOX_BIN="$TMP/mbox" \
  UPAGENT_WORKSPACE="$TMP/work" "$WRAPPER" -- /usr/bin/true 2>/dev/null; then
  echo 'unsupported mbox version silently accepted' >&2
  exit 1
fi

if MBOX_BIN="$TMP/mbox" UPAGENT_WORKSPACE="$TMP/work" \
  UPAGENT_EXTRA_READ_PATHS="$TMP/read:" "$WRAPPER" -- /usr/bin/true 2>/dev/null; then
  echo 'empty extra read path silently accepted' >&2
  exit 1
fi

VERSION_SIDE_EFFECT="$TMP/version-side-effect"
if FAKE_MBOX_VERSION_SIDE_EFFECT="$VERSION_SIDE_EFFECT" \
  MBOX_BIN="$TMP/mbox" UPAGENT_WORKSPACE="$TMP/work" \
  UPAGENT_MBOX_SHA256="$(printf '%064d' 0)" "$WRAPPER" -- /usr/bin/true 2>/dev/null; then
  echo 'wrong mbox digest silently accepted' >&2
  exit 1
fi
if [[ -e "$VERSION_SIDE_EFFECT" ]]; then
  echo 'bad mbox digest executed --version before rejection' >&2
  exit 1
fi

HOST_SPEC="$TMP/host-spec.json"
printf '%s\n' '{"isolation":{"kind":"host_adapter","adapter":"mbox","policySha256":"0000000000000000000000000000000000000000000000000000000000000000","adapterSha256":"0000000000000000000000000000000000000000000000000000000000000000"}}' > "$HOST_SPEC"
: > "$ARGS_FILE"
if MBOX_BIN="$TMP/mbox" \
  FAKE_MBOX_ARGS="$ARGS_FILE" \
  UPAGENT_WORKSPACE="$TMP/work" \
  "$WRAPPER" -- "$TMP/agent" run --spec "$HOST_SPEC" 2>/dev/null; then
  echo 'host_adapter hash mismatch silently accepted' >&2
  exit 1
fi
if [[ -s "$ARGS_FILE" ]]; then
  echo 'host_adapter mismatch started the child command' >&2
  exit 1
fi

echo 'mbox wrapper contract: PASS'
