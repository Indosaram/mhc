# Implementation: omo-28

All 4 port units (covering 24 port-needed census entries) have been fully implemented in Rust matching upstream omo TIP v5.1.47 behavior.

## Implementation

| Unit | Status | Files touched |
|---|---|---|
| Paused state, pause sequence, and compatible persistence | DONE | `crates/omo/senpi-task/src/dag/types.rs`, `crates/omo/senpi-task/src/dag/store.rs`, `crates/omo/senpi-task/src/dag/events.rs`, `crates/omo/senpi-task/src/dag/handle.rs`, `crates/omo/senpi-task/src/dag/types_tests.rs`, `crates/omo/senpi-task/src/dag/events_tests.rs`, `crates/omo/senpi-task/src/dag/handle_tests.rs`, `crates/omo/senpi-task/src/dag/paused_workflow_tests.rs` |
| User-stop event reduction and safe amendments | DONE | `crates/omo/senpi-task/src/dag/manager.rs`, `crates/omo/senpi-task/src/dag/scheduler.rs`, `crates/omo/senpi-task/src/dag/paused_workflow_tests.rs` |
| Paused-run controls and node messaging/retries | DONE | `crates/omo/senpi-task/src/dag/node_control_context.rs`, `crates/omo/senpi-task/src/dag/node_send.rs`, `crates/omo/senpi-task/src/dag/node_retry.rs`, `crates/omo/senpi-task/src/dag/paused_controls.rs`, `crates/omo/senpi-task/src/dag/node_send_tests.rs`, `crates/omo/senpi-task/src/dag/node_retry_tests.rs`, `crates/omo/senpi-task/src/dag/paused_workflow_tests.rs` |
| Paused child recovery and resume installation | DONE | `crates/omo/senpi-task/src/dag/recovery.rs`, `crates/omo/senpi-task/src/dag/paused_child_recovery.rs`, `crates/omo/senpi-task/src/dag/scheduler.rs`, `crates/omo/senpi-task/src/dag/recovery_tests.rs`, `crates/omo/senpi-task/src/dag/scheduler_tests.rs`, `crates/omo/senpi-task/src/dag/e2e_tests.rs`, `crates/omo/senpi-task/src/dag/perf_bench.rs`, `crates/omo/senpi-task/src/dag/paused_workflow_tests.rs` |

CROSS-CRATE:
- `crates/omo/senpi-task/src/manager/types.rs`: Added `Held { pause_seq: u64 }` variant to `OwnedStartResult` and optional `resume_credential` to `ManagerStartSpec` for held task admission compatibility.

Compiler risk:
- None identified. Types, imports, function signatures, serde attributes, and test harnesses match workspace invariants and call sites. Intermediate builds were not executed per instructions.

## Rebase onto parity

Rebased in sequence onto current parity and reconciled overlapping pause contracts. Cargo validation intentionally deferred. rustfmt checks and git diff --check used; this is not compiler verification.

Node-send conflicts were equivalent exhaustive paused-status matches. Retained parity cases. Kept omo-21 sidecar controls and adapted checkpoint cancellation callers; retained lane paused recovery and amendment modules. Current parity moved during work, so a final explicit parity merge reconciles typed optional mutation records rather than discarding removal notifications.

Remaining: checkpoint pause and sidecar pause representations still need execution-level validation together. No Cargo commands run.
