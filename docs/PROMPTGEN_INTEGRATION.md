# PromptGen Source and UpAgent Integration

## Source authority

Standalone PromptGen is the source of truth. UpAgent builds it from the sibling
source at `../promptGen`; `PROMPTGEN_SOURCE` can select another source tree only
when its `REVISION`, maintained-source SHA-256, Cargo lock, and verification
receipt match `components.lock.json`. A live Git checkout must also match the
pinned GitHub origin and HEAD commit. The origin kind distinguishes two inputs:

- `pinned-github-source` requires a clean checkout.
- `pinned-local-working-tree` includes local changes in the exact maintained-source
  SHA-256. Its commit is the Git base; the working tree is not claimed to be that
  published commit. A successful full verification receipt must attest the current
  source before integrity refresh. Staging changes alone does not change source
  identity, and verification never stages, commits, stashes, or resets user files.

Source archives omit `.git`, so their tree hash and current successful receipt
are checked against the same lock for either kind. The product source contains
only the maintained external PromptGen dependency; original input identities
remain recorded in `PACKAGE_PROVENANCE.json` and the existing source archives.

`scripts/build-local.sh` compiles the external Rust workspace from source and
installs the resulting `promptgen` executable in `dist/bin/`. UpAgent composition
pins that executable path and SHA-256 independently. A prebuilt executable alone
does not satisfy the source dependency.

For `pinned-github-source`, install the exact locked source in a fresh checkout
with the following commands, only when `../promptGen` does not already exist:

```bash
git clone https://github.com/axiom-orient/promptGen.git ../promptGen
PROMPTGEN_COMMIT="$(python3 -c 'import json; print(json.load(open("components.lock.json"))["components"]["promptGen"]["origin"]["commit"])')"
git -C ../promptGen checkout --detach "$PROMPTGEN_COMMIT"
./scripts/build-local.sh
```

The build runs `verify-static.py` first and checks the Git origin, commit,
maintained-source hash, and current PromptGen verification receipt. The source
archive contains the same Rust source when a sibling Git checkout is not used.
For `pinned-local-working-tree`, use the verified working tree or the combined
source archive. Cloning its base commit alone does not reproduce local changes.

## Shared contract source

`promptGen/crates/agent-contract` is the one source of provider messages, tools,
usage, artifact IDs/references, image observation/options, compiler outcome and
semantic edit receipt. UpAgent and the Vergerail provider import it directly.
PromptGen remains standalone: the crate has no UpAgent/runtime/I/O dependency,
and its default feature is std-only. JSON serialization and schemas are optional.

The three binaries emit the same `agent.shared-contract/1` catalog through
`promptgen schema shared-contract`, `agent schema shared-contract`, and
`vergerail-upagent-provider --schema`. The integration build compares all three
actual catalogs and validates an actual pure compile against the common schema.
UpAgent task/state/approval, Codex native JSON-RPC, and independent screen
contracts retain their existing owners. Prompt intent `detail` and native image
`quality` remain distinct.

Cargo path dependencies use the pinned sibling source layout. `PROMPTGEN_SOURCE`
must resolve to that shared source; an unrelated alternate compiler tree is
rejected to prevent mixing compiler and library identities. After GitHub
publication, external Rust projects can consume only `agent-contract` using a
Git dependency with the exact published commit as `rev`. No registry publication
is implied. The combined source archive includes the dependency closure.

## Prompt compilation

`promptgen_compile` invokes only:

```text
promptgen image --mode prompt-only --format json
```

The request must be a valid generate-mode image request without references. The
adapter checks the pinned executable, strict JSON result, normalized request,
`backend=codex-subscription`, PNG format, dimensions, background, and detail. It
stores `upagent.image-prompt-artifact/3`, binding the task ID, compiler SHA,
input and normalized-request hashes, complete compiler-output hash, prompt hash,
and delivery intent. PromptGen `detail` remains prompt intent; UpAgent does not
translate it into Vergerail's provider-native quality setting.

The artifact is immutable. UpAgent owns artifact storage and decides whether to
send its prompt to Vergerail. `image_generate` applies the artifact's supported
size and background. A provider-native quality value is a separate optional
control and cannot overwrite those pinned fields.

## Semantic edit

`image_edit_semantic` remains a separate external effect from deterministic
PerfectPixel rendering. Before dispatch, UpAgent checks the authorized source
artifact and current candidate revision, validates the strict edit request, and
uses PromptGen `prompt-only` to reject invalid input. The request must contain
one actual base reference at index 1 and explicit change and preserve clauses.

After approval, UpAgent invokes PromptGen `codex-imagegen` with the pinned Codex
binary, its SHA-256, and the caller-owned `CODEX_HOME`. It sets
`--max-fidelity-attempts 1`, so one approval causes at most one image candidate.
Codex runs with ChatGPT subscription authentication; the UpAgent adapter clears
the inherited environment and passes no API key.

The adapter validates the Codex execution receipt against the compiler prompt,
detail, pinned runtime hash, base-image hash and byte count, output path and
hash, dimensions, successful fidelity check, and execution timestamps. It then
re-inspects the PNG with PerfectPixel. UpAgent alone records the result and may
decide whether it becomes a candidate. Once PromptGen is spawned, a missing or
invalid result remains `ResolutionRequired`; it is not automatically retried.
The canonical `--result` file is `{compilation, execution}`. UpAgent strictly
validates the successful compilation and its exact preflight prompt before
validating the nested execution receipt. The complete original result bytes
remain available as evidence, including when only one output is observable.

## Composition fields

AgentSpec schema v5 keeps compiler and effect activation separate:

```json
{
  "promptgen": {
    "executable": "/absolute/path/to/promptgen",
    "executableSha256": "<sha256>",
    "timeoutMs": 60000,
    "codexRuntime": {
      "executable": "/absolute/path/to/codex",
      "executableSha256": "<sha256>",
      "home": "/absolute/path/to/codex-home"
    }
  }
}
```

`codexRuntime` is required only when `image_edit_semantic` is selected. It is
configuration, not activation. Selecting `promptgen_compile` alone requires no
Codex runtime, Pixel executable, or external-effect approval.

## Scope boundary

Vergerail remains UpAgent's only `image_generate` provider effect. PromptGen's
standalone Studio, MCP, screen compiler, and independent task state are not
registered as UpAgent surfaces. UpAgent consumes only its typed compiler output
and explicitly approved semantic-edit effect.
