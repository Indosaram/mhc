# Baseline regression result

Identical test in detached `/home/projects/mhc-wt/hg-hang-red` at pre-fix commit `0dec5825`. Only new file is `crates/maho-core/tests/extension_lock_release.rs`.

Command: `CARGO_TARGET_DIR=$HOME/.cargo-target/maho-code/hg-integration CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0 cargo nextest run -p maho-core --test extension_lock_release`.

Exit 100. Summary: 1 test run, 0 passed, 1 failed, 0 skipped. Failure duration 2.056 seconds. Panic: `renderer snapshot blocked while handler was pending`, at test line 73, after its two-second snapshot timeout. This is the requested deadlock failure, not a compile or fixture failure.

Same test passed on fixed code separately (green-result.md) and in the aggregate suite (nextest.log: 2249/2810, 0.315 seconds). Aggregate suite: 2810 passed, 0 skipped, exit 0.
