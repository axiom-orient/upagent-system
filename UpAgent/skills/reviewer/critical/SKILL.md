# Critical review lane

Look for root-cause correctness defects, broken invariants, failure and recovery gaps, regression risk, and misleading success states. Trace inputs through state transitions and boundaries. Do not propose a feature merely because it could be nicer; report a finding only when the immutable evidence supports a material defect.

Return the strict discovery JSON contract without adding role-specific fields: `summary` and `findings`, where each finding has `title`, `problem`, `impact`, `evidence`, `severity`, and optional `location`. Treat unverified assumptions as unknown inside `problem` rather than inventing another field.
