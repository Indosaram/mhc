# Ultrawork Engine Integration (Parent-Only Annex)

**NOTICE: THIS FILE IS A PARENT-ONLY ANNEX.**
This document contains proprietary SUL path references, internal component linkages, and consumer migration instructions. It is intended solely for parent integration work (plan todos 14, 15, 17).
**IT MUST NEVER BE EXPORTED TO THE CLEAN-ROOM IMPLEMENTER ENVIRONMENT.**

---

## 1. Architectural Scope & Crate Substitution

The new MIT crate `crates/maho-ultrawork` (clean-room implemented in Todo 14) replaces the following legacy proprietary components:
1. `crates/omo/components/maho-omo-ultrawork` (deleted in Todo 17)
2. `crates/omo/components/maho-omo-todo-fanout-reminder` (folded into `maho-ultrawork`, deleted in Todo 17)
3. `crates/omo/components/maho-omo-mass-ulw` (replaced by built-in mass mode, deleted in Todo 17)

The new crate is registered in both the standard `omo` profile and the `--no-default-features` (MIT) profile, ensuring that ultrawork mode works in the open-source MIT build as well.

---

## 2. SUL Consumer Inventory & Migration Mapping

The following components in `crates/omo/**` consume APIs or types from the ultrawork engine:

### 2.1 `crates/omo/components/maho-omo-telemetry/src/omo_native_prompt.rs`
- **Current Usage**:
  ```rust
  use maho_omo_ultrawork::{UltraworkClassification, arming_snapshot, classify_ultrawork_input};
  // Mutating arming calls in telemetry tests:
  maho_omo_ultrawork::shared_session_arming().lock().unwrap().mark_armed(Some(session));
  maho_omo_ultrawork::shared_session_arming().lock().unwrap().rearm_on_compact(Some(session));
  ```
- **Field Consumers**:
  `c.stage`, `c.effective`, `c.matched_ulw`, `c.matched_ultrawork`, `c.occurrence_count`, `c.suppression_reason`, `c.route`.
- **Migration Mapping**:
  - Replace `maho_omo_ultrawork` import with `maho_ultrawork`.
  - In `crates/maho-ultrawork`, export `UltraworkClassification` (or type alias `PromptClassification`) with exact matching public fields:
    - `matched_ulw: bool`
    - `matched_ultrawork: bool`
    - `occurrence_count: usize`
    - `effective: bool`
    - `stage: &'static str`
    - `route: &'static str`
    - `suppression_reason: &'static str`
  - Export public functions:
    - `classify_ultrawork_input(text: &str, source: InputSource, snapshot: ArmingSnapshot) -> UltraworkClassification`
    - `arming_snapshot(session_id: Option<&str>) -> ArmingSnapshot`
    - `shared_session_arming() -> Arc<Mutex<SessionArming>>`
    - `SessionArming` methods: `mark_armed(Option<&str>)`, `rearm_on_compact(Option<&str>)`, `track_session(Option<&str>)`, `is_armed(Option<&str>) -> bool`.

### 2.2 `crates/omo/senpi-task/src/dag/perf_bench.rs`
- **Current Usage**:
  ```rust
  use maho_omo_ultrawork::generated_directive::SENPI_ULTRAWORK_DIRECTIVE;
  use maho_omo_ultrawork::{ArmingSnapshot, classify_ultrawork_input, is_ultrawork_input, select_directive_text};
  ```
- **Migration Mapping**:
  - Replace `maho_omo_ultrawork` imports with `maho_ultrawork`.
  - Expose `is_ultrawork_input(text: &str) -> bool`.
  - Expose `classify_ultrawork_input` and `ArmingSnapshot`.
  - Provide `select_directive_text(text: &str, directive: &str) -> Option<String>` or update the benchmark to reference the new contract constant in `maho_ultrawork`.

### 2.3 `crates/omo/components/maho-omo-skill-commands/tests/skill_commands.rs`
- **Current Usage**:
  ```rust
  let ultrawork = maho_omo_ultrawork::UltraworkComponent {
      arming: Arc::new(Mutex::new(maho_omo_ultrawork::SessionArming::default())),
  };
  ```
- **Migration Mapping**:
  - Replace `maho_omo_ultrawork::UltraworkComponent` with `maho_ultrawork::UltraworkComponent` (or `UltraworkExtension`).
  - Provide `SessionArming` in `maho_ultrawork`.

### 2.4 `crates/omo/components/maho-omo-todo-fanout-reminder/`
- **Current Architecture**:
  Separate SUL crate that subscribed to `ToolResult`, checked `shared_session_arming().is_armed()`, and injected `TODO_FANOUT_REMINDER`.
- **Migration Mapping**:
  - The fanout reminder is integrated directly into `crates/maho-ultrawork` as part of its `ToolResult` handler.
  - The legacy crate `maho-omo-todo-fanout-reminder` is deleted in Todo 17.
  - SUL tests referencing `maho_omo_todo_fanout_reminder` are removed or re-targeted to `maho_ultrawork`.

### 2.5 `crates/omo/senpi-task/src/completion/notification.rs`
- **Current Usage**:
  ```rust
  pub const FINAL_RESPONSE_TRANSPORT_LIMIT: usize = 32_000;
  ```
- **Migration Mapping**:
  - In Todo 8, this constant is replaced with a dynamically loaded configuration parameter: `task.completion_transport_limit` (default: 8,000; clamped between 1,000 and 64,000).
  - Uses the UTF-16 code point boundary prefixing and `local://` spill mechanism specified in Section 9 of `docs/cleanroom/specs/ultrawork.md`.

### 2.6 `crates/omo/maho-omo/src/coordinator.rs`
- **Current Usage**:
  Unbounded accumulation of child completions into `omo-senpi:wake` custom message.
- **Migration Mapping**:
  - In Todo 8, serialize task completion entries while staying under `task.completion_batch_limit` (default: 24,000 UTF-16 units).
  - Spill overflow results to a single batch manifest file (`<state_dir>/completion-results/wake-overflow-<batch_id>.json`) and attach the summary line `N more task results: local://<path>`.

### 2.7 `crates/omo/maho-omo/src/component_list.rs`
- **Current Usage**:
  ```rust
  maho_omo_ultrawork::UltraworkComponent::default(),
  maho_omo_todo_fanout_reminder::TodoFanoutReminderComponent::default(),
  ```
- **Migration Mapping**:
  - In Todo 17, replace both entries with a single registration of the MIT component:
    `maho_ultrawork::UltraworkExtension::default(),`

---

## 3. Configuration, Wire Tags & Legacy Protocol Compatibility

1. **Disable Flags**:
   - Primary flag: `ultrawork-disabled`
   - Compatibility alias: The engine should check both `ultrawork-disabled` and legacy `omo-senpi-ultrawork-disabled`. If either is `true`, the engine is disabled.
2. **Todo Fanout Disable Flags**:
   - Primary flag: `todo-fanout-reminder-disabled`
   - Compatibility alias: `omo-senpi-todo-fanout-reminder-disabled`
3. **Custom Message Types & Wire Tags**:
   - Injected directive custom message type: `omo-ultrawork:directive`
   - Injected directive wrapper tag: `<ultrawork-mode>` and `</ultrawork-mode>`
   - Injected reminder custom text:
     `<omo-ultrawork-reminder>ultrawork mode is already armed for this session - the ultrawork directive above remains binding; re-read it and continue.</omo-ultrawork-reminder>`
   - Injected todo fanout reminder text:
     `<system-reminder>\nultrawork mode is active and this session just started its todo list. Before working any todo:\n1. SIZE the work: weigh the todo count, each task's scope, and the total effort.\n2. COMPUTE the fan-out decision: delegate to parallel subagents only when the parallelism gain beats spawn and coordination overhead - independent parts with disjoint write scopes fan out, interdependent or trivial parts do not.\n3. TELL the user the decision either way: which parts route to which categories and why fan-out pays off, or why you are working directly. Never delegate silently and never grind through a fan-out-shaped task silently.\n4. KEEP the todo list fresh: mark start/done the instant each task transitions, append newly discovered steps the moment they surface, drop abandoned ones. A stale todo list is a defect.\n</system-reminder>`
   - Completion wake custom message type: `omo-senpi:wake`

4. **Telemetry and Classification Wire Values**:
   - Stages (`stage`): `"first_arm"`, `"remention"`, `"post_compact_rearm"`, `"none"`
   - Routes (`route`): `"direct"`, `"embedded_directive"`, `"skill_expansion"`, `"skill_args"`, `"none"`
   - Suppression reasons (`suppression_reason`): `"none"`, `"extension_source"`, `"no_keyword"`, `"identifier_reference"`, `"negated_mention"`, `"embedded_directive"`, `"skill_expansion"`, `"skill_name_only"`
   - State fields: `was_armed: bool`, `compact_rearm_pending: bool`
   - Historical regex patterns for reference:
     - Boundary prefix: `(?<![A-Za-z0-9][-_.]|[/\\])`
     - Boundary suffix: `(?![-_.]?[A-Za-z0-9]*[/\\])` and `(?!(?:--|[-_](?!(?:loop|plan|research|execute|mass)(?![-_.]?[A-Za-z0-9])))[A-Za-z0-9]|\.[A-Za-z0-9])`


---

## 4. Worktree and Allowlist Boundary (Todo 13 Note)

When creating the clean-room implementer export in Todo 13:
- Include: `docs/cleanroom/specs/README-ulw.md` and `docs/cleanroom/specs/ultrawork.md`.
- EXCLUDE: This file (`docs/cleanroom/specs/ultrawork-integration.md`) and any `*-trace.md` files.
- The implementer sees only clean behavioral contracts and builds `crates/maho-ultrawork`.
- The parent applies the mappings documented in this annex during integration in Todo 17.
