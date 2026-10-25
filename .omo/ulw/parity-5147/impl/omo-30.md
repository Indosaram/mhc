# omo-30 implementation

Branch: `p5147/omo-30`. Rebased onto `parity/omo-5.1.47`, including pause-gaps.

Implementation is **not yet parity-complete**. The concrete remaining behaviors are recorded below; no missing behavior is classified as PRESENT. No cargo build/check/test/clippy was invoked (global gate reserved by orchestrator). Tests were added, not executed. Rustfmt syntax/format checks and `git diff --check` passed.

Paths are relative to `crates/omo/senpi-task/src/` unless qualified.

| Unit | Status | Evidence / concrete remaining behavior |
| --- | --- | --- |
| 1 Preserve admission custody | DONE | `manager/concurrency.rs`: epoch leases, serializable lane/sequence admission, ordered withdrawal/requeue; custody tests. |
| 2 Suspend capacity-waiting Resume | PARTIAL | `manager/concurrency.rs::suspend_unpark` preserves warm token and settles superseded waiter. `manager/manager.rs::begin_native_pause`, `acquire_resume_capacity` wire live capacity. Fresh-manager saved lane/sequence persistence is absent in `TaskRecord`; cold capacity contention currently refuses instead of waiting with restored allocation. |
| 3 Extend normalized child seam | PARTIAL | Exactly one borrowed native `run_pause` method, one task-status method and durable-ID subscription in `manager/child_handle.rs`. `runners/rpc/handle.rs` adapts real wire controls; in-process session forwards its native seam. The concrete host-session transport still must implement durable-ID notifications/task-status publication (default client methods remain optional). |
| 4 Project held sends | DONE | `manager/continue_result.rs`: one Held arm maps Paused/FollowUp; retained `ContinueDelivery::Held` enum variant for compatibility but not returned by mapping. Projection test uses canonical pause_seq/entry_id fields. |
| 5 Carry trace context | DONE | Existing parity `TraceparentFor` and component engine wiring retained; `manager/helpers.rs::build_managed_spec` receives parent trace at launch; `manager/runner.rs::to_rpc_spec`, `runners/types.rs`, `runners/rpc/spawn.rs::TRACEPARENT_ENV` forward it. Inherited/member-env stale value stripped, exact delegating value inserted. Managed-spec persistence test and spawn env test added. Saved omo-21 patch inspected: correct propagation already present; invalid statement in respawn test and wrong OMO_TRACEPARENT env name NOT applied. Upstream uses OMO_SENPI_TASK_TRACEPARENT. |
| 6 Stage first prompt | PARTIAL | Canonical `StagedManagedChild/start_staged`, real session creation without prompt, manager activation, exact deferred initial outcome, refusal cleanup and component wrapper forwarding implemented. Real-engine Stop-crossing proof remains absent; underlying in-process first-turn closure still obtains generic handle outcome after follow_up instead of retaining a per-turn token. |
| 7 Teardown and tracing | DONE | `manager/runner.rs` forwards concrete RPC teardown; component exclusion/live wrappers forward staging and teardown. `manager/manager.rs::settle_launches_for_shutdown` waits initial teardown, launch-change condition, second teardown under shared deadline and final cleanup. Component SessionShutdown calls it in blocking worker. Throwing/panic runners logged. Trace spawn regression added; full shutdown failure test execution deferred. |
| 8 Filter fallback chains | PARTIAL | In-process staged/start/resume uses live provider exclusions; decorated-selector helper and component wrapper exist. Process respawn selector rebuild is still not wired to a live exclusion resolver. |
| 9 Refuse excluded selected model | DONE | `manager/parent_registry_context.rs`: live exclusion resolver checks resolved identity before registry work for start/resume; compatibility fixed-list constructor retained. Regression test refuses excluded model without registry lookup. |
| 10 Separate pause outcomes | PARTIAL | Epoch pending pause registered before engine call, acknowledged persistence parks capacity and fences old generation. Exact resume tracking wired through manager controller/component. Full refused-pause and immediate-resume race tests not yet mirrored. |
| 11 Terminal host status | PARTIAL | `manager/manager.rs::settle_waiters` publishes asynchronously and logs errors; shared publication mutex serializes with reopened-running publication, terminal worker re-reads fresh status. Concrete host transport implementation remains optional/no-op. |
| 12 Late durable identity | PARTIAL | Per-handle first stamp with handle and captured epoch ownership checks in `subscribe_child_facts`. Concrete host late-ID event source absent; full predecessor replacement regression cases not yet mirrored. |
| 13 Restore without turn | DONE | `manager/respawn.rs` suppresses continuation when Paused and refuses absent transcript. Added paused RPC respawn/no-follow-up and same-epoch reattach regression in `manager/manager_tests/respawn.rs`. |
| 14 Root admission fencing | PARTIAL | Canonical StartAdmission used before planner, residency checks before/after callback, queued ownership transfer and staging checks. Ordinary respawn refuses closed barrier and saved run hold. Ordinary respawn still lacks an entered root ticket spanning rebuild work. |
| 15 Held first launches | PARTIAL | Warm contexts, epoch withdrawals, queued ticket custody and saved sequence requeue implemented in manager. Fresh-manager launch reconstruction/CAS and durable allocation metadata absent. |
| 16 Run holds and held sends | PARTIAL | Existing controller integrated with manager pending pauses/capacity/exact turn tracking. RPC wire native bridge, idempotent receipts, observed-hold recovery and canonical Held response exist. Cold resume contention and full cutoff/lost-hold race behavior/tests remain incomplete. |
| 17 Paused ownership restoration | DONE | `manager/manager.rs::restore_paused_task/restore_paused_child`: newest local transcript (mtime then path), recorded host transcript precedence, live foreign-owner refusal, paused/status/epoch/host CAS, direct paused respawn under ownership recheck, native rehold before control-only reattach, cleanup on rehold failure, same epoch preserved. `ManagerRunPausePorts::restore` now invokes it. Added same-epoch/no-prompt regression. |
| 18 Parent rung eligibility | PARTIAL | `manager/credential_failure.rs` implements upstream usage/auth/403 classification, parent retention, spent-parent skip, stable account-provider ordering and chain_after_hop. Runtime fallback uses resulting chain. Classifier and real-record chain regressions added. Start-failure fallback advancement is not yet implemented. |
| 19 Parent fallback facts | PARTIAL | Native message/stream tool observation and active route accessor present. Runtime hop note exists. Manager-driven note still lacks shared streamed-tool facts and exact upstream reset/detail/reason builder. |
| 20 Epoch-tag live stats | PRESENT | Parity tracker `run_start_epoch` retained as single source instead of duplicate epoch map. `subscribe_child_facts`, `live_run_stats`, `reacquire_for_revive` use it; self-resume tags before record reopen. No duplicate live-stat definition remains. |
| 21 Reopened running status | DONE | `manager/manager.rs` self-resume publication shares serialization mutex with terminal worker, which reads fresh state inside that mutex. Running publication follows durable reopen. |
| 22 Cancel/shutdown cleanup | DONE | Accepted cancel settles waiters/releases task leases. Shared-deadline initial/final teardown settlement contains errors/panics, subscribes via launch condvar, parks/withdraws unadmitted contexts in final cleanup; component shutdown invokes it. |
| 23 Departed parent notes | DONE | `manager/runtime_fallback_event.rs`: fallback revert and operator model-change clear stale parent note; parent route retained, colon-containing selector regression. |

Final counts: **10 DONE, 1 PRESENT, 12 PARTIAL**. Remaining units: 2,3,6,8,10,11,12,14,15,16,18,19.

## pause-gaps gap 3

Implemented exact resumed outcome ownership end-to-end:
- `manager/run_pause.rs::ResumeTurnResult::Resumed` and `TaskResumedTurn` own deferred `ExactTurnOutcome`, not just turn ID.
- `components/maho-omo-task/src/engine_run_pause.rs::NativeTaskPausePort` preserves `ResumedChildTurn.outcome`; `ManagerRunPausePorts::track` calls `TaskManager::track_resumed_turn` and the exact-generation outcome watcher.
- `ManagerRunPausePorts::restore` is no longer a no-op: calls transcript-only paused recovery above.
- `runners/rpc/handle.rs` captures expected turn generation and caches settled outcome per generation, preventing a resumed outcome closure from observing the previous or following generic turn.
- Focused tests: `resumed_turn_preserves_the_exact_deferred_outcome`, `exact_resumed_outcome_settles_without_waiting_for_old_generic_turn`, `paused_respawn_has_no_continuation_prompt_and_reattach_keeps_epoch`.
- No cargo execution: type/behavior acceptance remains for global gate.

## Verification

- All branch-changed Rust files: `rustfmt --edition 2024 --config skip_children=true --check <file>` PASS before final report.
- `git diff --check`: PASS.
- Merge tree checked after rebasing; exact final result recorded in final response.
- Integration worktree was read-only; this report is committed in shard worktree only.
