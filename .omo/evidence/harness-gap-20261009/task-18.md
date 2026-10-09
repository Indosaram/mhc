# Task 18 Evidence: Expose latest workflow tool contract (T04)

## Summary
- Upstream source: `oh-my-openagent @ adb672532` (`packages/omo-senpi/src/components/task/dag-tool.ts`, `packages/senpi-task/src/dag/*`)
- Implemented file-for-file parity in `maho-omo-task` and `senpi-task`.
- Tool name `workflow` registered with legacy `dag` alias via `TaskDagEngine::register_tool`.
- Supports all 8 actions: `start`, `attach`, `snapshot`, `wait`, `cancel`, `retry`, `send`, `amend`.
- Schema matches pinned upstream schema from `adb672532`.
- Validates target selection per node (`validate_task_target`), enforces `detach: false` blocking vs detached live snapshot default, lints node prompt contracts (`TASK:`, `STOP WHEN`, verification wave), and checks quiet child activity.
- Exposes full refusal vocabulary (`run_not_found`, `run_not_owned`, `run_still_active`, `invalid_arguments`, `node_not_found`, `node_not_retryable`, `node_has_no_task`, `node_not_continuable`, `invalid_amendment`, `amend_running_node`).

## Source Tree Hash
- Base HEAD: `b3c3c94f2cf0220a7de0d3ac1f9434461fc14c02`
- Working Tree Hash: `32ade4991507d20105c926359836f6c516240f3d`

## Commands & Exit Codes

### 1. Parity Audit
```bash
bun tools/parity-audit.mjs --crate maho-omo-task
# Exit code: 0
PASS maho-omo-task: 44 files checked

bun tools/parity-audit.mjs --crate maho-senpi-task
# Exit code: 0
PASS maho-senpi-task: 2 files checked
```

### 2. Clippy
```bash
$HOME/.cache/mhc-lanes/cargo-slot.sh cargo clippy -p maho-omo-task -p maho-senpi-task --all-targets -- -D warnings
# Exit code: 0
```

### 3. Nextest Suites
```bash
$HOME/.cache/mhc-lanes/cargo-slot.sh cargo nextest run -p maho-omo-task --test dag_tool
# Exit code: 0
# Summary: 42 passed, 0 skipped

$HOME/.cache/mhc-lanes/cargo-slot.sh cargo nextest run -p maho-omo-task -p maho-senpi-task
# Exit code: 0
# Summary: 2093 passed (1 slow), 0 skipped
```

### 4. QA Scenarios: `qa_task_18`
```bash
$HOME/.cache/mhc-lanes/cargo-slot.sh cargo run -p maho-omo-task --example qa_task_18
# Exit code: 0
```
Output:
```text
=== QA Task 18: Workflow Tool Contract ===
1. Tool registration verified: name=workflow

--- Happy Path Scenario ---
Step 1: start workflow
  start result: run_id=run-1
Step 2: snapshot workflow
  snapshot result: kind="snapshot"
Step 3: send message to node child
  send result: kind="sent", delivery="steer"
Step 4: retry node with prompt override
  retry result: kind="retried"
Step 5: amend workflow definition
  amend result: kind="amended"
Step 6: detach action
  detach result: kind="detached"
Step 7: cancel workflow
  cancel result: kind="cancelled"
Happy path successfully executed all workflow actions in order!

--- Failure Path Scenario ---
Step 8: unknown run_id rejection
  unknown run_id error: code="run_not_found", message="unknown dag run \"run-unknown\""
Step 9: invalid action rejection
  invalid action error: invalid dag action
Step 10: verify zero orphan child processes
  Cleanup complete: temporary directories removed, no detached processes.

=== QA Task 18 SUCCESS ===
```

## Cleanup Receipt
- All temporary directories cleaned up automatically via TempDir RAII guards.
- Zero orphan or detached child processes running (`ps aux` verified).
- No background daemons or PTY sessions leaked.
