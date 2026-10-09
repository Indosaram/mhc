# Evidence: Task 3 - Port latest plan checklist grammar (T05)

## Metadata
- Plan: .omo/plans/omo-native-rs-tui-parity.md
- Refs: .omo/plans/harness-gap-closure-20261009.md
- Worktree: /home/projects/mhc-wt/hg-lane-2003
- Branch: lane/2003
- Base Commit: b3c3c94f2cf0220a7de0d3ac1f9434461fc14c02
- Source Tree Hash: 9f30427d817ab088dd7c69802b16197dff44257b

## Commands and Exit Codes

### 1. Parity Audit
Command:
```bash
bun tools/parity-audit.mjs --crate boulder-state
```
Exit Code: 0
Output:
```
PASS boulder-state: 6 files checked
```

### 2. Nextest Test Suite
Command:
```bash
$HOME/.cache/mhc-lanes/cargo-slot.sh cargo nextest run -p maho-boulder-state -p maho-omo-start-work-continuation
```
Exit Code: 0
Output:
```
129 tests run: 129 passed, 0 skipped
```

### 3. Clippy Verification
Command:
```bash
$HOME/.cache/mhc-lanes/cargo-slot.sh cargo clippy -p maho-boulder-state -p maho-omo-start-work-continuation --all-targets -- -D warnings
```
Exit Code: 0
Output:
```
Finished dev profile in 4.43s
0 warnings, 0 errors
```

## QA Scenarios

### Happy Scenario
Fixture plan markdown with T1, T1.1 ([~]), T1.2a, fenced code block skipping, F1, and H1:
```markdown
## TODOs
- [x] T1. Initial setup
- [~] T1.1 Blocked subtask
- [ ] T1.2a Next actionable implementation

```markdown
- [ ] T9. Fenced example should be skipped
```

## Final Verification Wave
- [ ] F1 — Plan compliance audit
- [ ] H1 — Human approval
```

Expected (matches pinned TS adb672532):
- `total`: 5
- `completed`: 1 (T1)
- `remaining`: 3 (T1.2a, F1, H1; T1.1 is [~] and counts toward total but neither completed nor next)
- `next_task_label`: Some("T1.2a Next actionable implementation")

Observed Rust output:
- `total`: 5
- `completed`: 1
- `remaining`: 3
- `next_task_label`: Some("T1.2a Next actionable implementation")
Status: PASS (verified in `tests/plan_checklist.rs::qa_scenarios_happy_and_failure`)

### Failure Scenario
Malformed labels (T0, T01, 0, missing titles, 1. with empty title, F0, H0) are ignored as in pinned TS:
```markdown
## TODOs
- [ ] T0. Invalid zero
- [ ] T01. Leading zero
- [ ] 0. Zero invalid
- [ ] Missing title
- [ ] 1.
- [ ] 2. Valid implementation
## Final Verification Wave
- [ ] F0. Zero invalid
- [ ] H0. Zero invalid
- [ ] F1. Valid verifier
```

Expected (matches pinned TS adb672532):
- `total`: 2 (only "2. Valid implementation" and "F1. Valid verifier" counted)
- `completed`: 0
- `remaining`: 2
- `next_task_label`: Some("2. Valid implementation")

Observed Rust output:
- `total`: 2
- `completed`: 0
- `remaining`: 2
- `next_task_label`: Some("2. Valid implementation")
Status: PASS (verified in `tests/plan_checklist.rs::qa_scenarios_happy_and_failure`)

## Cleanup Receipt
- No temporary background processes or daemons spawned.
- All temporary test directories in test runs cleaned up via standard tempfile scopes.
- Temporary files in /tmp (/tmp/adb_plan_checklist.ts, /tmp/markdown-fence.ts, /tmp/types.ts, /tmp/test_adb.ts, /tmp/test_adb_fail.ts) removed.
- Cargo target directory remained scoped to CARGO_TARGET_DIR=$HOME/.cargo-target/maho-code/lane-2003.
