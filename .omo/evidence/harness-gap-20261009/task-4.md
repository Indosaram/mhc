# Task 4 evidence - reflection integration receipts exactly (T23)

Worker st_01a12025 produced the source/test changes and crashed before committing; the orchestrator verified and committed them.

Pinned source: /home/indo/code/oh-my-openagent@adb672532 packages/memory-core/src/reflection/worktree-integration.ts
- findExactRunReceipt: rev-list baseSha..HEAD; per commit, %(trailers:key=Omo-Run,valueonly) must contain runId exactly; optional validatedTipSha must be an ancestor of the commit.
- probeReflectionIntegration: auto mode checks receipt first, then tip ancestry of HEAD.
- probeLegacyAutoRunReceipt: receipt scan without tip.

Native: crates/omo/memory-core/src/reflection/worktree_integration.rs (find_exact_run_receipt, has_exact_run_receipt, is_ancestor), worktree.rs (base_sha accessor).

Verification (orchestrator rerun, lane HEAD + uncommitted diff):
- cargo nextest run -p maho-memory-core -p maho-omo-memory --no-fail-fast: 1752 passed, 0 skipped, rc 0
- cargo clippy -p maho-memory-core -p maho-omo-memory --all-targets -- -D warnings: rc 0
- bun tools/parity-audit.mjs --crate memory-core: PASS (19 files)
- An intermittent failure in facts::queue::extra_tests::given_two_concurrent_enqueue_attempts... reproduced 2/3 on the integration base without this change (0/3 with it); tracked as plan todo 33.

QA: worktree_integration_tests cover exact trailer match, body-only run id rejected, wrong/non-ancestor tip rejected, out-of-range commit rejected, valid receipt idempotent.
Cleanup: tests use tempdir git repos dropped at scope end; no processes left.
