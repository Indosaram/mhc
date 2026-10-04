# Machine-consumed negative fixtures (task 18)

Each file is a real input the verifier must REJECT. They are consumed by
`tools/verify-session2-residual.mjs --self-test`; none of them is executed against the product.

| Fixture | Defect it encodes | Verifier guard |
| --- | --- | --- |
| `parity-hidden-row.md` | parity table split by an intra-table blank line (Task 8: 85 parsed of 155 declared, 70 hidden) | declared-vs-parsed row coverage |
| `nextest-skipped.log` | a `SKIP` status line and a nonzero skipped count | zero-skip + executed-PASS requirement |
| `nextest-missing-required.log` | a required test absent from the log (discovered but not executed) | per-test execution lookup |
| `placeholder-required-tests.json` | prose placeholders posing as exact test identifiers | exact-identifier validation |
