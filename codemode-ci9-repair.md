# CI9 codemode repair

## Scope and result

Worktree: `/home/projects/mhc-wt/nonwindows-20261020`.

Applied source repairs for all five codemode Clippy findings and the stale interrupt API consumer. No cargo, tests, CI, or commits were run. All edits used `apply_patch`. Compilation and runtime behavior remain unverified. The patch tool emitted an automatic LSP notice for the test file: `server cancelled the request`; that is not a usable diagnostic result, and no separate diagnostic run was requested.

Changed files:

- `crates/maho-codemode/src/kernels/js/context_manager.rs`
- `crates/maho-codemode/src/tool/types.rs`
- `crates/maho-codemode/tests/host_package_installs.rs`
- This explicitly requested report.

## Complete diagnostic evidence

Evidence directory: `/home/projects/mhc/.omo/nonwindows-20261020/ci-39432997402/`.

Read the complete five-error codemode diagnostic sequence in `clippy.log:1928-2009`, including the terminal `could not compile maho-codemode (lib) due to 5 previous errors`. The same locations are also present in `macos.log`.

| Historical source location | Diagnostic | Repair |
| --- | --- | --- |
| `context_manager.rs:904` | `possible_missing_else` | Put recovery cancellation and host-entry abort on separate lines, preserving both independent operations. |
| `context_manager.rs:924` | `possible_missing_else` | Expand worker-error cleanup into separate statements, preserving memory marking, host abort, query failure delivery, run settlement, tool-call cleanup, and retirement order. |
| `context_manager.rs:684` | `type_complexity` | Name the oneshot sender contract `InterruptResponse`; use it for both `Command::Interrupt` and the pending response vector. |
| `context_manager.rs:912` | `collapsible_if` | Use a short-circuit let-chain for process isolation, result frames, and a successful footprint read. Non-process and non-result messages still do not invoke the async footprint read. |
| `tool/types.rs:79` | `type_complexity` | Name the existing callback type `PackageRootResolver`; keep its return type and `Send + Sync` bounds unchanged. |

Read the complete missing-method diagnostic in `test__4_.log:1986-2004`: `E0599` at `tests/host_package_installs.rs:284`, with the compiler suggestion to use `interrupt`, followed by the terminal failure for test target `host_package_installs` and exit code 101.

The interleaved `maho-agent` diagnostics in the Clippy log are outside the codemode grant and were not changed.

## Interrupt API inventory

Searched the entire codemode Rust source and tests inventory, including inline test modules and `context_manager/startup_tests.rs`, rather than only the failing integration target. The direct API/handle search covered 203 Rust files and found 36 matching lines in 12 files; a broader interrupt-reference inventory was paginated through its final page.

Current contracts:

- Concrete `JavaScriptKernel::interrupt` awaits the actor's oneshot response and returns `Result<(bool, Option<String>), String>`.
- Trait `EvalKernel::interrupt` returns `KernelInterruptHandle`. Its JavaScript adapter awaits the concrete operation and wraps the already resolved retained-state result in the handle future.
- `interrupt_outcome` remains a valid managed-cell watch field; it is not a method on `JavaScriptKernel`.

Consumers and fixtures inspected:

| Location | Inventory result |
| --- | --- |
| `tests/host_package_installs.rs` | Sole stale `interrupt_outcome(...)` method call. Changed to the concrete `interrupt(...)` API. |
| `tests/js_kernel.rs` | Five concrete interrupt consumers already await the tuple API: cooperative retention, nested host-wait interruption, forced retirement, tracked-child retirement, and unknown-cell handling. No migration needed. |
| `src/kernels/js/context_manager/startup_tests.rs` | Startup interruption uses `Command::Interrupt`, awaits the oneshot tuple, and awaits stopped-run settlement. No migration needed. |
| `tests/eval_tool_lifecycle.rs` | `CapacityKernel` fixture already implements the trait handle API. No migration needed. |
| `tests/detached_cell_manager.rs` | Disposal fixture already implements the trait handle API and exercises dequeue-before-interrupt ordering. No migration needed. |
| `src/tool/types.rs` | JavaScript trait adapter already maps the concrete tuple into `KernelInterruptHandle`. No migration needed. |
| `src/tool/cell_execution.rs` | Cancellation retains the in-flight interrupt operation after its notification deadline, stores the eventual handle, and signals handle readiness. No migration needed. |
| `src/tool/detached_cell_manager.rs` | Detached stop awaits the handle's retained-state future before publishing final state; notification snapshots await the managed-cell completion watch. No migration needed. |
| `src/tool/run_eval_cell.rs` | Timeout reporting awaits interrupt-handle readiness and then retained-state completion. No migration needed. |
| `src/tool/managed_cell.rs` | `interrupt_outcome` is the notification-completion watch field, not an obsolete method consumer. Keep it. |

Other interrupt-named test references concern bounds, notes, state snapshots, protocol values, or queue results rather than the changed method contract.

## Cancellation completion preserved

The host cancellation test still subscribes to the started and cancelled channels before triggering interruption. It now bounds the concrete interrupt await to five seconds and checks `cancelled_rx.try_recv() == Ok(())` immediately after interruption completes. This asserts that cooperative host cancellation has completed before the API returns, rather than allowing a late event to hide a completion-order regression. The pending cell result is also awaited with a five-second bound, and the existing retained JavaScript-global check remains in place.

The actor still awaits `HostEntry::abort()` before releasing and settling the active host run and sending the interrupt response. No cancellation, startup recovery, reset, close, retirement, queue admission, JS-only language, or callback thread-safety contract was changed. The earlier owned run-frame lifetime repair is retained.

## Verification boundary

This lane delivers source changes and existing-log evidence only. No passing Clippy, compilation, or test result is claimed. The final integrated gate must establish those results; this lane did not launch it under the explicit execution restrictions.
