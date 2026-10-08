# Bounded runtime contract audit

Authoritative contract: `/home/projects/mhc/.omo/evidence/latest-omo-implementation-20261006/contracts/runtime.json`
Current root: `/home/projects/mhc-wt/main-integrated-latest-omo`

## Scope

Authoritative units: **7**
Expected contract-row references: **22**
Raw unit-row expansion observed: **23**
Unique contract-row IDs: **19**

The raw expansion is 23 because P507 and P593 are shared across RU-03a, RU-03b, and RU-03c. The bounded target counter remains the requested 22, with shared references normalized in the report.

## Superseded attempt

**REJECTED:** The prior 899-row runtime registry report is invalid for this node and is excluded. This report contains no separate 899-row registry.

## Counters

| Counter | Value |
|---|---:|
| missing | 0 |
| duplicates | 0 |
| extra | 0 |

## Units

### RU-01, tmux-core spawn-process closure
- Queue: P
- Contract rows: `md:runtime/owned-file-inventory.md:P163`
- Upstream: packages/tmux-core/src/tmux-utils/spawn-process.ts @pin (one-line re-export)
- Historical fact: candidate already re-exports utils::runtime::spawn at tmux_utils/mod.rs:51
- Disposition: **complete**
- Current consumer: crates/omo/tmux-core/src/tmux_utils/mod.rs:51 re-exports utils::runtime::spawn.
- Upstream evidence: packages/tmux-core/src/tmux-utils/spawn-process.ts at oh-my-openagent pin.
- Rationale: The bounded contract fact is present in current main.

### RU-02, boulder-state stale-work port + consumers
- Queue: L
- Contract rows: `md:runtime/COVERAGE-AND-UNKNOWNS.md:O46`, `md:runtime/DEEPENED-PER-FEATURE.md:B243`, `md:runtime/DEEPENED-PER-FEATURE.md:B244`, `md:runtime/DEEPENED-PER-FEATURE.md:B246`, `md:runtime/owned-file-inventory.md:P184`, `md:runtime/owned-file-inventory.md:P191`
- Upstream: packages/boulder-state/src/storage/stale-work.ts @latest (absent at pin)
- Historical fact: no stale_work module; no stale_since/parse_iso_to_ms/strip_session_platform/project_work_to_mirror; consumers ulw-execute-hook.ts:191 and boulder-eligibility.ts:25
- Disposition: **partial**
- Current consumer: crates/omo/boulder-state/src/storage/stale_work.rs exists in current main.
- Upstream evidence: packages/boulder-state/src/storage/stale-work.ts at latest upstream.
- Rationale: The cited module exists, but consumer parity remains unverified.

### RU-03a, supervisor re-entry dispatch in main.rs
- Queue: U
- Contract rows: `md:runtime/owned-file-inventory.md:P507`, `md:runtime/owned-file-inventory.md:P593`
- Upstream: senpi main.ts:930 if (await dispatchInternalSupervisor(args)) return
- Historical fact: spawned argv has no branch in main.rs -> parse_args rejects it ('Invalid CLI arguments')
- Disposition: **missing**
- Current consumer: crates/maho-cli/src/main.rs has no internal-rpc-host-supervisor dispatch branch.
- Upstream evidence: senpi packages/coding-agent/src/main.ts:930.
- Rationale: Supervisor re-entry remains absent from the current consumer path.

### RU-03b, multi-session host entry + HostRuntimeFactory seam
- Queue: U
- Contract rows: `md:runtime/owned-file-inventory.md:P507`, `md:runtime/owned-file-inventory.md:P593`
- Upstream: senpi main.ts:1114-1143 runMultiSessionHost({createRuntime: createCliRuntimeFactory(...)})
- Historical fact: --multi-session/--listen parsed but unconsumed; run_multi_session_host / create_cli_runtime_factory have zero callers
- Disposition: **partial**
- Current consumer: crates/maho-cli/src/cli/runtime.rs calls serve_multi_session_host.
- Upstream evidence: senpi packages/coding-agent/src/main.ts:1114-1143.
- Rationale: The current entry exists, but no runtime parity acceptance was run.

### RU-03c, watchdog lifetime binding + arming
- Queue: U
- Contract rows: `md:runtime/owned-file-inventory.md:P507`, `md:runtime/owned-file-inventory.md:P593`
- Upstream: senpi host-lifecycle.ts:505-544 (HOST_WATCH_FD_ENV=3, HOST_WATCH_PPID_ENV), multi-session-host.ts:470-490 (arm before listen), host-watchdog.ts:104
- Historical fact: arm_host_watchdog has zero callers AND the spawn passes no HOST_WATCH_FD/PPID -> read_host_watchdog_config_from_brand_env returns None; watchdog inert twice over
- Disposition: **partial**
- Current consumer: crates/maho-rpc/src/multi_session_host.rs:316 calls arm_host_watchdog; host_lifecycle.rs sets watchdog environment values.
- Upstream evidence: senpi host-lifecycle.ts:505-544 and multi-session-host.ts:470-490.
- Rationale: Watchdog wiring exists in current source, but behavior remains unverified.

### RU-04, DAG production registration
- Queue: U
- Contract rows: `rt:L218`, `rt:L219`, `rt:L220`, `rt:L221`, `rt:L222`, `rt:L223`, `rt:L224`, `rt:L225`
- Upstream: senpi omo-senpi task/index.ts:117-247 (createDagRuntime, registerTaskTools, wireDagLifecycle, registerDagTool)
- Historical fact: compose called only from examples/runtime_proof.rs:192 and tests/dag_registered_lifecycle.rs:94; component.rs never wires it
- Disposition: **verification_only**
- Current consumer: crates/omo/components/maho-omo-task/src/component.rs:127 composes the DAG and registers queries.
- Upstream evidence: packages/omo-senpi/src/components/task/index.ts:117-247.
- Rationale: Production registration is present, but source presence is not parity proof.

### RU-05, team query tools team_status / team_list
- Queue: U
- Contract rows: `rt:L280`, `rt:L281`
- Upstream: senpi features/team-mode/tools/query.ts (createTeamStatusTool, createTeamListTool)
- Historical fact: build_lead_team_tools publishes only Create/Delete/TaskCreate/TaskGet/TaskList/TaskUpdate
- Disposition: **verification_only**
- Current consumer: crates/omo/senpi-task/src/tools/team/query.rs defines team_status and team_list; team/index.rs registers both.
- Upstream evidence: Historical contract names senpi features/team-mode/tools/query.ts; no exact pinned path was needed for this bounded classification.
- Rationale: Current producers and registration exist, but no runtime invocation was run.

## Owner choices and unresolved prerequisites

Named owner choices and unresolved prerequisites are copied only from the authoritative runtime contract because the prompt allows them when they are unclosed.

### Owner choices

- **CHOICE-01**, Isolation backends, containment, merge policy, foreign-host scope (no Codex protocols / TS bridges). Maps to: dec-a-03.
- **CHOICE-02**, [CC] loader adoption and OpenClaw reply-listener interop incl. process ownership. Maps to: dec-a-04.
- **CHOICE-03**, OpenCode cross-process sidebar vs native footer/status surface. Maps to: rt:L304.
- **CHOICE-04**, Public SDK compatibility surface (thread addressing / CLI / RPC / SDK). Maps to: dec-a-06, dec-u-02.
- **CHOICE-05**, Whether the shipped binary enables the socket host by default and with which idle/empty-exit policy. Maps to: dec-u-01.
- **CHOICE-06**, Watchdog signal: ppid polling only vs the upstream fd-3 EOF pipe (blocked by unsafe_code='forbid'). Maps to: RU-03c.

### Unresolved prerequisites

- Authorization: dec-p-00 / dec-l-02 authorize per unit, not in bulk; every unit stays awaiting-authorization.
- Cross-lane ordering: RU-03a + RU-03b (assembly) before RU-03c (runtime); RU-04's registration step touches crates/maho-cli (assembly).
- RU-02 depends on dec-l-02 whose declared dependencies are dec-a-03 / dec-a-04.
- A/U decisions CHOICE-01..06.
- Not selected here: the per-hook, per-feature, agent-roster and team-core inventory rows remain U and need their own owner units.

## Verification

- Read method: JavaScript eval with `Bun.file(...).json()` for the authoritative contract and report parsing.
- The prior 899-row attempt is explicitly rejected and not used as the target.
- Expected counters are missing=0, duplicates=0, extra=0.

Final verification result: authoritative units 7, expected row references 22, missing 0, duplicates 0, extra 0. The prior 899-row attempt remains rejected.
