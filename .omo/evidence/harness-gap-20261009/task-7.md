# Task 7: Port telemetry product properties (T17) Evidence

## Plan Reference
- Plan: `.omo/plans/omo-native-rs-tui-parity.md`
- Refs: `.omo/plans/harness-gap-closure-20261009.md`
- Task: Todo 7 (GAP-1 / T17: Port telemetry product properties)
- Upstream References: `oh-my-openagent@adb672532 packages/telemetry-core/src/events.ts:44,82,181`, native `crates/omo/telemetry-core/src/events.rs`

## Source Tree State
- Worktree: `/home/projects/mhc-wt/hg-lane-2007`
- Branch: `lane/2007`
- Base HEAD: `b3c3c94f2cf0220a7de0d3ac1f9434461fc14c02`
- Source tree hash: `32ade4991507d20105c926359836f6c516240f3d`

## Verification Commands and Exit Codes

### 1. Verification of Tests (cargo nextest)
```bash
export TMPDIR=$HOME/.cache/mhc-lanes/2007
export CARGO_TARGET_DIR=$HOME/.cargo-target/maho-code/lane-2007
$HOME/.cache/mhc-lanes/cargo-slot.sh cargo nextest run -p maho-telemetry-core -p maho-omo-telemetry
```
- Exit code: 0
- Output summary: 211 tests run: 211 passed, 0 skipped.
- Newly ported tests in `crates/omo/telemetry-core/tests/all/events.rs`:
  - `additional_properties_ride_on_shared_block_without_overriding_fixed_keys` (PASS)
  - `native_events_client_effective_transport_options_disable_geoip_false_while_guards_hold` (PASS)

### 2. Linter Verification (cargo clippy)
```bash
export TMPDIR=$HOME/.cache/mhc-lanes/2007
export CARGO_TARGET_DIR=$HOME/.cargo-target/maho-code/lane-2007
$HOME/.cache/mhc-lanes/cargo-slot.sh cargo clippy -p maho-telemetry-core -p maho-omo-telemetry --all-targets -- -D warnings
```
- Exit code: 0
- Output: 0 warnings, 0 errors.

### 3. Parity Audit
```bash
bun tools/parity-audit.mjs --crate maho-telemetry-core
bun tools/parity-audit.mjs --crate maho-omo-telemetry
```
- Exit code: 0
- Output:
  - PASS maho-telemetry-core: 7 files checked
  - PASS maho-omo-telemetry: 17 files checked

## QA Scenarios

### QA Scenario 1 (Happy Path)
- **Goal**: Local capture transport receives expected properties in pinned override order (additionalProperties spread first, fixed identity keys override, disable_geoip fixed false).
- **Execution**: Programmatic harness in `qa_task7`.
- **Observed Results**:
  - Transport options `disable_geoip`: false (fixed false, immune to product override).
  - Transport options `flush_at`: 20 (`EVENT_FLUSH_AT` overrides product tuning).
  - Transport options `flush_interval`: 10000 (`EVENT_FLUSH_INTERVAL_MS` overrides product tuning).
  - Captured properties verify pinned order:
    - `install_id`: `"ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff"` (rides along from product.additional_properties)
    - `surface`: `"cli"` (rides along from product.additional_properties)
    - `platform`: `"omo-senpi"` (fixed identity key overrides product "spoofed-platform")
    - `product_name`: `"omo-native"` (fixed identity key)
    - `package_version`: `"5.0.0"` (fixed identity key)
    - `schema_version`: 3 (fixed identity key)
    - `$process_person_profile`: false (fixed identity key)
    - `$session_id`: `"sess-456"` (payload event property)
    - `reason`: `"startup"` (payload event property)
- **Verdict**: PASS

### QA Scenario 2 (Failure / Opt-out)
- **Goal**: Opt-out sends zero events.
- **Execution**: Programmatic harness with `DO_NOT_TRACK=1` in environment.
- **Observed Results**:
  - `client.enabled()`: false.
  - Captured event attempt: 0 events captured by transport.
- **Verdict**: PASS

## Cleanup Receipt
- Temporary test harness `crates/omo/telemetry-core/examples/qa_task7.rs` removed.
- Build artifacts in `$CARGO_TARGET_DIR` preserved in designated lane target dir.
- No child processes, daemon servers, or background tasks left running.
