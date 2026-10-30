# Repository instructions

- Keep the source surface small and use clean breaks. Do not add compatibility shims, deprecated aliases, migration paths, or legacy wrappers for removed APIs; update the canonical contract and its call sites directly.
- The only repository maintenance scripts are `scripts/verify.sh`, `scripts/clean.sh`, and the audit helper `scripts/cleanup_inventory.sh`.
- Never create, modify, suggest, or enable GitHub Actions, CI/CD workflows, release automation, or `.github/workflows` files. `scripts/verify.sh` must fail if `.github/workflows` appears.
- Preserve source, manifests, lockfiles, documentation that describes the current product, and user state. `scripts/clean.sh` may remove only reproducible Cargo output and repository-local caches or temporary files.

## Staged verification

- During implementation, run only focused tests/checks for the modules being changed.
- After the candidate is stable, run the canonical full verification once as the final code gate.
- Update documentation and regenerate checksums only after code is stable, and do those steps last.
- Add extra verification only for high-risk contract changes.
- Do not repeat the same full verification run during one change.
