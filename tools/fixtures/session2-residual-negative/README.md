# Machine-consumed negative fixtures (task 18)

Each file is a real input the verifier must REJECT. They are consumed by
`tools/verify-session2-residual.mjs --self-test`; none of them is executed against the product.

| Fixture | Defect it encodes | Verifier guard |
| --- | --- | --- |
| `parity-hidden-row.md` | parity table split by an intra-table blank line (Task 8: 85 parsed of 155 declared, 70 hidden) | declared-vs-parsed row coverage |
| `nextest-skipped.log` | a `SKIP` status line and a nonzero skipped count | zero-skip + executed-PASS requirement |
| `nextest-missing-required.log` | a required test absent from the log (discovered but not executed) | per-test execution lookup |
| `placeholder-required-tests.json` | prose placeholders posing as exact test identifiers | exact-identifier validation |
| `required-but-not-authored-orphan.json` | a required-but-not-authored test that is not linked as an exact (package, target, test) triple in `execution_coverage.required_tests` | orphan-triple rejection |
| `command-manifest-missing-hash.json` | a completed gate command whose entry has a log but no `log_sha256` | mandatory 64-hex raw-log hash |
| `command-manifest-malformed-hash.json` | a completed gate command with a non-64-hex `log_sha256` | mandatory 64-hex raw-log hash |
| `source-identity-dirty.json` | a begin source identity with a dirty worktree (`clean:false`, non-empty `dirty_sources`) | clean begin/end source identity |
| `server-row-missing-required-tests.json` | an accepted (non-excluded) server row citing no exact `required_tests` | server-row completeness (each non-excluded row must cite exact triples) |
| `server-row-unlinked-triple.json` | a server row citing a triple not linked into `execution_coverage.required_tests` | server-row triple integration (no parallel unverified list) |
| `server-row-uncovered-behavior.json` | a non-empty `server_row_manifest.uncovered_behavior` | uncovered behavior blocks success |
| `server-row-mcp-wrong-package.json` | server row 25 typing the cross-crate MCP test as `maho-server` | row-25 cross-crate triple must be typed `maho-ext-mcp` |
| `command-manifest-stale-binary-after-failed-build.json` | a command manifest where the workspace build failed (exit 101) yet a binary-dependent QA command (`package-native`/`run-qa`/`residual-qa`) still claims exit 0 | stale-binary acceptance: no binary-dependent command may pass after a failed build |
| `build-provenance-stale-mtime.json` | a build provenance whose `binary_mtime_ms` predates `run_started_at_ms` (and whose hash equals the pre-run baseline) | fresh-build provenance: the binary must have been produced by this run |
