# CI8 codemode lifetime repair

## Scope and status

Worktree: `/home/projects/mhc-wt/nonwindows-20261020`.

Source change: `crates/maho-codemode/src/kernels/js/context_manager.rs`, in the run-posting block of `run_actor` (around line 777).

Status: source repair applied; compilation and runtime behavior remain unverified. No cargo command, test, CI run, diagnostic server, or commit was invoked. All file changes used `apply_patch`. The only file outside `crates/maho-codemode/**` authored by this lane is this explicitly requested report.

## Existing CI evidence

Evidence directory: `/home/projects/mhc/.omo/nonwindows-20261020/ci-39430684876/`.

Read both complete compiler diagnostic blocks, including their trait bounds and terminal compile failures, in:

- `test__3_.log:1848-1914`: Linux rejects `tokio::spawn(run_actor(...))` at historical `context_manager.rs:294:34`; the captured reference is identified at historical `context_manager.rs:792:69`. Both diagnostics say `&PendingJavaScriptRun` is not `Send` because its referent is not `Sync`.
- `windows-test__1_.log:1095-1167`: Windows reports the same two failures, ending with `could not compile maho-codemode (lib) due to 2 previous errors` and exit code 101.

The two non-`Sync` members named by the compiler are:

```text
dyn FnOnce() -> Result<String, String> + Send
dyn FnOnce(HostCellContext) -> Pin<Box<...>> + Send
```

These are `PrepareCell` (`run_queue.rs`) and `HostCellExecutor` (`tool/types.rs`). Their contract permits moving callbacks between threads without requiring concurrent shared access.

The evidence search also found duplicate codemode diagnostics in Linux shards 4 and 6. `build.log` and `windows.log` do not contain this codemode diagnostic in the available snapshots: their terminal failures concern `maho-omo` and `maho-chord`, respectively. Those failures are outside this lane and were not changed. The available Windows shard 2 and 3 logs have no matching codemode diagnostic.

## Root cause

After selecting and preparing the active run, the actor obtained `run: &PendingJavaScriptRun`. Its nested posting future captured that shared reference to construct the run frame after awaiting the tool-name frame post. A shared reference is `Send` only when its referent is `Sync`; the queued one-shot callbacks intentionally do not satisfy that stronger bound. The outer actor therefore failed the existing `tokio::spawn` requirement `Future + Send + 'static`.

This is a borrowed queue-entry lifetime problem, not a mutex/watch guard problem. Taking the callbacks before posting does not change the static auto-trait properties of `PendingJavaScriptRun`, even when those options happen to be empty.

## Repair and preserved contracts

The actor now computes the existing deadline and builds an owned `serde_json::Value` run frame inside a synchronous lexical block. The shared queue-entry borrow ends with that block. The posting future references only the owned frame rather than the `PendingJavaScriptRun` containing the callbacks.

The source change preserves:

- The `Send`-only callback aliases, without adding `Sync`, unsafe code, or suppressions.
- The existing `tokio::spawn` actor and its JS-only kernel contract; no local task runtime or alternative language kernel was introduced.
- FIFO admission and the active run's ownership by `JavaScriptRunQueue`; the run is neither removed nor replaced to send its frame.
- Preparation and host-executor dispatch before JS posting.
- Deadline calculation before tool-name resolution/posting, and tool-name posting before run posting. Building the owned run frame introduces no suspension or callback execution.
- Posting-error settlement and retirement, along with the existing queued cancellation, interrupt, timeout, reset, close, startup cancellation, and recovery paths.

Existing queue cancellation/reset tests and actor startup/close/refusal tests were read for the relevant contracts, but were not modified or run. The production `tokio::spawn(run_actor(...))` remains the compile-time `Send` enforcement point, so the original regression cannot compile unnoticed once the final integration gate runs.

## Validation boundary

Evidence consists of the existing CI diagnostics and source-level tracing only. No post-change passing build or test result is claimed. Final integrated compilation and behavioral verification belong to the parent gate under the requested no-cargo/no-tests/no-CI restriction for this lane.
