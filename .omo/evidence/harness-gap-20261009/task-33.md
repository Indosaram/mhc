# QA Evidence: Task 33 (Fix flaky concurrent facts enqueue test)

## Summary
- Target: Plan todo 33 (GAP-2 / workspace gate prerequisite)
- Worktree: `/home/projects/mhc-wt/hg-lane-2033`
- Base commit: `d9b5ccb77c385b2e3e5c94d0ca9cbe87fa9f34a0`
- Issue: `facts::queue::extra_tests::given_two_concurrent_enqueue_attempts_for_same_delta_when_both_run_then_exactly_one_entry_is_published` failed intermittently with `Lock(Contention(LockContentionError))` because the enqueue critical section was holding the lock while performing multiple synchronous `file.sync_all()` calls and directory syncs. Under concurrent load and slow filesystem disk fsyncs (observed taking 1.5s - 5s on host storage), the lock was held longer than the 2000ms `LOCK_WAIT_MS` deadline.
- Resolution: Ported verbatim filesystem parity from pinned upstream `oh-my-openagent` (`packages/memory-core/src/facts/queue.ts`), which uses asynchronous non-flushing `writeFile` (atomic `.tmp` + rename under lock) without directory syncs or file syncs (compare with `packages/memory-core/src/fs/write-all.ts`). Removed the unpinned and extraneous `file.sync_all()` and `sync_dir` calls in `FactsQueue`.

## Changes
- Modified `crates/omo/memory-core/src/facts/queue.rs`:
  - Removed `file.sync_all()?` from `write_file_0600()`.
  - Removed redundant `sync_dir(parent)` calls following atomic rename in `publish()`, `advance_consumed()`, and `write_cursor()`.
  - Removed unused helper function `sync_dir()`.
- Added parity fragment `crates/omo/memory-core/parity.d/2033.md`.

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

### 2. Nextest 20-run Consecutive Stress Test
Command:
```bash
python3 -c "
import subprocess, time
start = time.time()
passes = 0
for i in range(20):
    cmd = [
        '/home/indo/.cache/mhc-lanes/cargo-slot.sh',
        'cargo', 'nextest', 'run', '-p', 'maho-memory-core',
        '-E', 'test(given_two_concurrent_enqueue)'
    ]
    p = subprocess.run(cmd, capture_output=True, text=True, env={...})
    if p.returncode == 0: passes += 1
print(f'{passes}/20 passed in {time.time() - start:.2f}s')
"
```
Exit code: `0`
Output:
```
20/20 passed in 21.42s
```

### 3. Full Nextest Suite
Command:
```bash
cargo nextest run -p maho-memory-core
```
Exit code: `0`
Output:
```
Summary [   5.678s] 725 tests run: 725 passed (1 leaky), 0 skipped
```

### 4. Clippy Check
Command:
```bash
cargo clippy -p maho-memory-core --all-targets -- -D warnings
```
Exit code: `0`
Output:
```
    Checking maho-memory-core v0.0.0 (/home/projects/mhc-wt/hg-lane-2033/crates/omo/memory-core)
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 11.40s
```

## QA Scenarios

### Happy QA Scenario: 20/20 Consecutive Nextest Passes
Command:
```bash
cargo nextest run -p maho-memory-core -E 'test(given_two_concurrent_enqueue)'
```
Exit code: `0` (repeated 20 times consecutively, 20/20 passed)
Details: Both threads concurrently attempt to enqueue the same delta; exactly one worker acquires the lock, publishes the queue file and advances the watermark, and the second worker observes the published delta / watermark and skips publication with `no-new-entries`.

### Failure QA Scenario: True Contention & Exact-One Publish Under Race
Command:
```bash
cargo nextest run -p maho-memory-core -E 'test(given_two_concurrent_enqueue)'
```
Details: Two threads race to publish identical delta simultaneously. Under previous implementation, holding the lock through slow disk `sync_all` caused the second thread to time out contending on the lock after 2000ms. With the fix, the critical section completes in milliseconds without lock contention timeout, and exactly one entry is published in the facts queue directory.

## Cleanup Receipt
- Verified no background bash sessions, monitor processes, or child cargo processes left running.
- Temporary files created during testing cleaned up via tempfile RAII.
