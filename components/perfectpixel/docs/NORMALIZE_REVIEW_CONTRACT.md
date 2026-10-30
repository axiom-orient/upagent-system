# Normalize implementation review contract

## Required invariants

1. A plan with `report.ok == false` must never emit `sprite-request.json` or normalized frame PNGs as successful output.
2. State order in request, report, frame output, and generated bundle request must remain identical.
3. A state preparation failure aborts the request; it must not be removed and then re-zipped with another state.
4. Any crop or placement that would discard non-transparent pixels is an explicit failure.
5. A request field must either affect observable output or be rejected. Accepted no-op configuration is forbidden.
6. Quality-gate failure is reported as `ok:false` and uses the quality-gate CLI exit class.
7. Existing `schema`, `inspect`, `bundle`, `vector`, and `vector-analyze` behavior remains unchanged.

## Current alignment constraint

Only the implemented `bottom` vertical alignment may be accepted. Alternate values must fail
explicitly rather than being silently ignored. A new vertical mode requires characterization
fixtures and placement tests before it can be added to the accepted request values.

## Verification matrix

- request/report state-order comparison;
- failure-output isolation;
- source alpha versus crop bounds;
- final frame dimensions;
- normalize-to-bundle smoke path;
- existing command regression;
- Rust format, all-target check, Clippy, and workspace tests.
