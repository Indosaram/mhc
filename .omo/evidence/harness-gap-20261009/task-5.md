# QA Evidence: Task 5 (Bound memory mirror git commands)

## Summary
- Target: Plan todo 5 (GAP-2, GAP-3 / T21)
- Worktree: `/home/projects/mhc-wt/hg-lane-2005`
- Base commit: `b3c3c94f2cf0220a7de0d3ac1f9434461fc14c02`
- Source tree hash: `32ade4991507d20105c926359836f6c516240f3d`

## Changes
- Modified `crates/omo/memory-core/src/sync/mirror.rs`: In `MirrorSync::run`, routed the default (`exec: None`) execution through `self.repo.exec()`. `GitMemoryRepo::exec()` uses `GitExec` (via `create_git_exec`), which guarantees:
  - Command timeout honoring with active watchdog thread killing and waiting (reaping) the child process upon deadline expiration.
  - Setting `GIT_TERMINAL_PROMPT=0` on child execution.
  - Returning `GitExecResult` with errors that pass through `redact_url` in `push()`.
- Added tests in `crates/omo/memory-core/src/sync/mirror_tests.rs` (`mod bounded_exec`):
  - `test_push_honors_timeout_ms_and_redacts_error_on_exec_none_path`: Stalled fake git process blocking on a FIFO is killed at deadline and reaped, returning a redacted timeout error with no credentials leaked.
  - `test_push_passes_terminal_prompt_and_redacts_credentials_on_timeout`: Checks `GIT_TERMINAL_PROMPT=0` environment setting and timeout value observation with redacted errors.
- Added parity fragment `crates/omo/memory-core/parity.d/2005.md`.

## Verification Commands & Output

### 1. Parity Audit
Command:
```bash
bun tools/parity-audit.mjs --crate maho-memory-core
```
Exit code: `0`
Output:
```
PASS maho-memory-core: 15 files checked
```

### 2. Nextest Suite
Command:
```bash
cargo nextest run -p maho-memory-core
```
Exit code: `0`
Output:
```
Summary [  86.592s] 725 tests run: 725 passed, 0 skipped
```

### 3. Clippy Check
Command:
```bash
cargo clippy -p maho-memory-core --all-targets -- -D warnings
```
Exit code: `0`
Output:
```
    Checking maho-memory-core v0.0.0 (/home/projects/mhc-wt/hg-lane-2005/crates/omo/memory-core)
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 5.94s
```

## QA Scenarios

### Happy QA Scenario: Stalled Git Killed & Reaped at Deadline
Command:
```bash
cargo nextest run -p maho-memory-core -E 'test(test_push_honors_timeout_ms_and_redacts_error_on_exec_none_path)' --nocapture
```
Exit code: `0`
Result: PASS (0.112s)
Details: Stalled fake git blocked on FIFO was killed by timeout watchdog, child reaped, returned `pushed: false` with detail containing `timed out after 100ms` without credential leak (`secret` and `user:` absent).

### Failure QA Scenario: Fast Success Path Unchanged & Redacted Error on Timeout
Command:
```bash
cargo nextest run -p maho-memory-core -E 'test(test_push_passes_terminal_prompt_and_redacts_credentials_on_timeout)' --nocapture
```
Exit code: `0`
Result: PASS (0.035s)
Details: Fast path executes cleanly, verified `GIT_TERMINAL_PROMPT=0` is passed to git process, and credential URL (`https://alice:supersecret@example.com/mem.git`) never appears in error text (`supersecret` and `alice:` absent).

## Cleanup Receipt
- All temporary directories (fifos, fake git scripts, temporary test repos) created during test execution are managed via RAII `tempfile::tempdir()` and cleaned up automatically on process exit.
- Background test processes reaped; no orphan processes or background bash sessions left running.
