# Profiles and development tracks

## Role

`agent-profiles` owns the built-in `ProfileDefinition` policies and the
`ProfilePlan` produced after a task, skill set, and profile context are
validated. A plan fixes the prompt, memory scope, allowed tools, reasoning,
worker topology, task-count limits, and post-mutation evidence requirements.

## Non-role

Profiles do not discover provider processes, read credentials, execute tools,
or decide whether a caller may write outside the runtime authority. The runtime
and effect planes enforce the plan; [Architecture](ARCHITECTURE.md) describes
their boundaries.

## Public surface

`agent_profiles::BuiltInProfilePlugin` contributes the stock four-profile catalog
through `CompositionPluginCatalog`; the catalog is the single selection boundary
shared by compose, run, and review. `ProfileRegistry::register`, `resolve`,
`manifests`, and `definitions` expose the provider-neutral profile contract;
`ProfileDefinition::prepare`
and `default_skills` produce a plan for one task.

| profile id | required selection | policy surface |
|---|---|---|
| `creator` | default skill `creator/chat` | text/image model; request input; image generate/inspect/extract/render/compare; file read/write only with a workspace; session memory; task-grant effects |
| `developer` | exactly one track | explicit workspace; `developer/<track>` skill; repository semantic tools; process/Git capabilities; session memory; task-grant effects |
| `reviewer` | one review lane | `reviewer/neutral`, `reviewer/critical`, `reviewer/adversarial`, or `reviewer/fact-checker`; no workspace, tools, memory, or effects |
| `twin` | `context.memoryScope` | `twin/conversation`; scoped long-term memory; `remember`, `forget_memory`, and `request_input` |

Developer tracks are policy identifiers, not separate binaries.

| track | workers / topology | count caps (transitions, model calls, tool calls) | evidence after a successful mutation |
|---|---|---|---|
| `prototyper` | 2 / `parallel_read` | 96, 6, 20 | none |
| `builder` | 1 / `single_with_independent_verifier` | 192, 12, 48 | `inspect_diff` |
| `sweeper` | 1 / `single_with_independent_verifier` | 128, 8, 24 | `inspect_diff`, `run_check` |
| `grower` | 2 / `parallel_read` | 192, 12, 40 | `inspect_diff` |
| `maintainer` | 1 / `single_with_independent_verifier` | 320, 20, 80 | `inspect_diff`, `run_check` |

All developer plans expose the semantic tools `repo_map`, `search_code`,
`read_file`, `write_file`, `replace_text`, `run_check`, `git_status`,
`inspect_diff`, and `request_input`. A reviewer plan exposes no tools. The
exact effect implementation is documented in [Architecture](ARCHITECTURE.md).

Runtime extensions are compile-time `ToolModule` values in
`crates/agent-runtime/src/extension.rs`. Each module declares a tool name,
side-effect class (`ReadOnly` or `External`), allowed profiles, required
capabilities, and activation skills. Read-only calls produce no effect receipt;
external calls must return receipts and artifacts through the runtime.

Profile and provider authorities are linked-Rust `ProfilePlugin` and
`ProviderPlugin` contributions selected by one immutable
`CompositionPluginCatalog`; skill authority remains the host-owned
`SkillCatalog`. The exact profile/catalog snapshot used by composition is reused
by run. Linked
`RuntimePlugin` extensions may add only tool modules or completion verifiers.
There is no generic process/Wasm plugin loader; external processes use explicit
versioned adapter boundaries such as PerfectPixel or the optional semantic-mask
Effect.

## Primary flow

```text
profile id + optional track
  → registry resolve and manifest validation
  → default skill selection and skill validation
  → ProfileDefinition::prepare
  → runtime preflight checks plan authority
  → selected tools, memory, effects, and evidence follow the plan
```

`developer` rejects a missing workspace or track. `reviewer` rejects a
workspace and requires the skill matching its lane. `twin` rejects a missing
`context.memoryScope`. These checks happen before provider execution.

## Verification

```bash
cargo test --locked -p agent-profiles -p agent-skills
./scripts/verify.sh
```

The first command covers profile and skill policy tests; the full script also
checks the workspace build, CLI smoke, review binary, and bounded effect paths.
