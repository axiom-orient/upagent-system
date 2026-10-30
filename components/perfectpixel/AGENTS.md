# Agent instructions

## Shared operating contract

### Project boundary

- This file applies to the independent project rooted at this directory. Treat
  sibling directories next to this project root as separate projects; do not
  infer their requirements or edit them unless the user explicitly asks for a
  cross-project change.
- Read this file first, then the project's README, manifest, relevant design
  documents, and canonical verification scripts before changing code.
- Inspect the current worktree before editing. Preserve unrelated user changes,
  use `apply_patch` for source and documentation edits, and keep changes
  minimal and reversible.
- Keep credentials, cookies, browser profiles, logs, generated outputs, and
  other user data outside the source tree and out of commits.

### Browser automation: Aside CLI

- Use Aside CLI as the only agent-operated browser automation surface for web
  navigation, UI interaction, screenshots, downloads, and browser QA. Read the
  official [Aside developer documentation](https://docs.aside.com/help/developers)
  when the workflow is unclear.
- Inspect the installed command before relying on options:
  `aside --help`, `aside exec --help`, and `aside repl --help`.
- Use `aside exec` for a task-level browser-agent request. Use `aside repl` for
  deterministic page inspection, screenshots, downloads, or exact UI steps;
  use its snapshot-based page inspection before acting. Use `--session` only
  to continue the intended Aside session, and use `aside mcp` only when an MCP
  client explicitly needs Aside as its browser server.
- Do not invoke Playwright directly (`npx playwright`, standalone Playwright
  scripts, or a separate Playwright browser). A Playwright-compatible API is
  permitted only inside an `aside repl` session launched through Aside CLI.
- If Aside is unavailable, unauthenticated, or cannot provide the required
  session, stop browser automation and report the blocker; do not silently
  substitute another browser tool or copy credentials from an external
  browser profile.
- Keep live browser state and downloaded evidence in the designated temporary
  or artifacts location, never in product source or committed output.

### Implementation and evidence

- Prefer the project's existing entry points, dependencies, and local gates.
  Do not install a new framework or validation utility without an explicit
  request.
- Verify the smallest relevant user flow after a change. Record commands that
  passed, were skipped because of the environment, or remain blocked; do not
  call an unverified result complete.

## Project-specific rules

- Treat `docs/CONTRACT.md`, the CLI schema, source, and tests as the product
  contract. Keep asset publication local, deterministic, bounded, and
  transaction-safe.
- Use Cargo's locked verification commands as the complete gate; do not add a
  service, CI, packaging, or publishing dependency.
