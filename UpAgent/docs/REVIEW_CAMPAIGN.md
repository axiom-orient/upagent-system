# Independent review campaign

`agent-review` is a separate executable backed by the provider-neutral `agent-review` library and the stock `agent-review-cli` composition root. The normal `agent` runtime
may emit a content-addressed review subject bundle, but it never runs the
review campaign itself.

## Contract

`ReviewRequest` uses `schemaVersion: 2` and rejects unknown fields. Its subject
is either:

- `inline`, with a bounded UTF-8 `mediaType`, `content`, and optional declared
  SHA-256; or
- `artifactBundle`, with producer `taskDigest`, `compositionDigest`, a full
  artifact reference, and an explicit `includeArtifactIds` allow-list.

The recipe is fixed and validated before provider setup:

- exactly three discoverers, one each for `neutral`, `critical`, and
  `adversarial`;
- two to four distinct `fact_checker` lanes;
- at least two confirmations for each finding, using non-self validators;
- required route/model diversity and bounded finding/lease limits.

The review domain receives only provider-neutral `ProviderBinding` values.
The stock `agent-review` binary accepts an independently versioned
`schemaVersion: 5` provider plugin route file. Each selected entry carries a
generic `ProviderPluginEnvelope`; the review host resolves its ID and verifies its
manifest digest against the linked catalog before passing configuration to the
plugin:

```json
{
  "id": "neutral-route",
  "provider": {
    "pluginId": "upagent.provider.vergerail",
    "manifestSha256": "<linked-manifest-sha256>",
    "configuration": {
      "protocolSchema": "vergerail.upagent/2",
      "executable": "/absolute/path/to/provider",
      "executableSha256": "<sha256>",
      "runtimePackage": "/absolute/path/to/runtime",
      "managedHome": "/absolute/path/to/home",
      "model": "model-id",
      "workspace": "/absolute/path/to/workspace"
    }
  }
}
```

Before campaign state is created, the binary validates the request, selects
only referenced routes, and derives their immutable provider-neutral bindings.
The selected plugin ID and manifest digest are verified at this host boundary;
the review recipe retains the resulting binding-derived route digest. Profile
ownership is resolved from the same catalog. Normal `agent` composition receipts
additionally seal explicit selected provider/profile plugin bindings.
Executable identity is checked only when provider setup begins. Credentials
remain outside both the request and `ReviewCampaign`; concrete managed-home,
runtime-package, executable, and environment semantics are owned solely by the
stock Vergerail adapter. An embedding host may provide the same campaign with
another `ModelPort` and matching `ProviderBinding` without changing review
domain code.

## Lifecycle

```text
validate request + routes
  → materialize immutable subject
  → acquire review lease
  → run neutral/critical/adversarial discovery
  → fact-check frozen candidate findings
  → pure evidence reducer
  → durable result / recoverable incomplete state
```

The campaign owns only review state and evidence. It does not edit the source
workspace, mutate the producer's task, or turn provider/setup failure into a
finding. Cancellation, suspension, recovery, and repair-claim handoff remain
explicit result states.

## CLI

```bash
target/release/agent-review --help
target/release/agent-review run \
  --request /absolute/path/review-request.json \
  --routes /absolute/path/routes.json \
  --state-root /absolute/path/review-state
```

The local `./scripts/verify.sh` gate builds the binary, checks its help path,
and runs the crate's deterministic fake-runner tests. Authenticated provider
execution and independent external review are separate evidence and are not
claimed by that local gate.
