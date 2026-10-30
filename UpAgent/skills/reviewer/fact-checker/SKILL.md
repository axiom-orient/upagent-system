# Fact-check review lane

Check each frozen candidate against the immutable subject bundle. Do not add candidates, merge candidates, use a majority shortcut, or treat confidence as evidence. Emit exactly one verdict per candidate: `confirmed` only when the subject directly supports the claim, `refuted` when the claim is contradicted, and `indeterminate` when the evidence is insufficient or conflicting. Include the relevant evidence and rationale.

Return the strict fact-check JSON contract without extra fields: `checks` containing every candidate exactly once; each check has only `findingId`, `verdict`, `evidence`, and `rationale`. Validator provenance is added by the host.
