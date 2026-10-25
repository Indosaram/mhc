# omo-30 implementation

Branch: `p5147/omo-30`. Rebased onto `parity/omo-5.1.47`, including pause-gaps.

Implementation is **not yet parity-complete**. The concrete remaining behaviors are recorded below; no missing behavior is classified as PRESENT. No cargo build/check/test/clippy was invoked (global gate reserved by orchestrator). Tests were added, not executed. Rustfmt syntax/format checks and `git diff --check` passed.

Paths are relative to `crates/omo/senpi-task/src/` unless qualified.

| Unit | Status | Evidence / concrete remaining behavior |
| --- | --- | --- |
| 1 Preserve admission custody | DONE | `manager/concurrency.rs`: epoch leases, serializable lane/sequence admission, ordered withdrawal/requeue; custody tests. |
| 2 Suspend capacity-waiting Resume | DONE | Cold Resume enqueues/requeues saved lane/sequence/epoch; stale allocations are rejected before held-capacity fast paths. A dedicated capacity mutex serializes cold attempt registration/enqueue, replacement cleanup, and Stop withdrawal, preventing obsolete attempts from withdrawing replacement custody. Waits settle on grant/cancellation rather than an arbitrary 30-second capacity deadline. Parked token identity and epoch are checked. Regressions cover persisted FIFO/second Stop and stale allocation with held capacity. Implementation closure only: cargo/type and behavioral execution remain reserved for the global gate. |
| 3 Extend normalized child seam | DONE | `ManagedChildHandle` exposes optional borrowed `run_pause`, durable-ID subscription, and status publication. `InProcessChildHandle` forwards the real `ChildSession` pause capability; `NativeChild` implements core controls and receipt normalization. `RpcChildHandle` forwards all three controls to its protocol/native pause adapters. Optional defaults preserve engines without the capabilities. Inspected upstream `adaptInProcessHandle`/`adaptRpcHandle` and concrete implementations; native receipt/idempotence and RPC pre-subscription identity regressions are present, unexecuted. Status ordering and exact turn races are tracked separately in units 10/11/12/16. |
| 4 Project held sends | DONE | `manager/continue_result.rs`: one Held arm maps Paused/FollowUp; retained `ContinueDelivery::Held` enum variant for compatibility but not returned by mapping. Projection test uses canonical pause_seq/entry_id fields. |
| 5 Carry trace context | DONE | Existing parity `TraceparentFor` and component engine wiring retained; `manager/helpers.rs::build_managed_spec` receives parent trace at launch; `manager/runner.rs::to_rpc_spec`, `runners/types.rs`, `runners/rpc/spawn.rs::TRACEPARENT_ENV` forward it. Inherited/member-env stale value stripped, exact delegating value inserted. Managed-spec persistence test and spawn env test added. Saved omo-21 patch inspected: correct propagation already present; invalid statement in respawn test and wrong OMO_TRACEPARENT env name NOT applied. Upstream uses OMO_SENPI_TASK_TRACEPARENT. |
| 6 Stage first prompt | DONE | `InProcessManagedRunner::start_staged` creates without prompting and exposes first prompt/begin; manager checks entered admission after creation, either begins and tracks exact initial outcome or native-pauses/holds the first prompt and parks capacity. `InProcessChildHandle::start_initial_turn` retains numeric per-turn outcomes and settles superseded waiters. Added real `AgentSession` Stop-crossing staged regression with a panic if begin is called, plus first-result retention regression. Static path review and rustfmt only; tests remain unexecuted. |
| 7 Teardown and tracing | DONE | `manager/runner.rs` forwards concrete RPC teardown; component exclusion/live wrappers forward staging and teardown. `manager/manager.rs::settle_launches_for_shutdown` waits initial teardown, launch-change condition, second teardown under shared deadline and final cleanup. Component SessionShutdown calls it in blocking worker. Throwing/panic runners logged. Trace spawn regression added; full shutdown failure test execution deferred. |
| 8 Filter fallback chains | DONE | Live exclusions filter resolved and decorated persisted chains at runner start/resume and process rebuild. `RpcProcessRunner` probes `get_protocol_info` for `retry_fallback_command`, then sends `set_retry_fallback` / `retryFallback` within one configurable deadline (default 10 seconds). Unsupported capability warns once; refusal/timeout logs and retains manager fallback without preventing prompt. Dropped pending requests remove their response slots. Supported/refused/unsupported/silent-probe regressions are present, unexecuted; static wire/capability review completed. |
| 9 Refuse excluded selected model | DONE | `manager/parent_registry_context.rs`: live exclusion resolver checks resolved identity before registry work for start/resume; compatibility fixed-list constructor retained. Regression test refuses excluded model without registry lookup. |
| 10 Separate pause outcomes | DONE | Pause intent precedes native abort; acknowledgement gates outcome settlement, accepted pause invalidates old tracking and parks capacity without fallback/isolation terminalization. Exact resumed closures own a new tracking generation and stale generic aborts cannot settle it. Regressions `refused_pause_does_not_trap_ordinary_completion`, `exact_resumed_outcome_settles_without_waiting_for_old_generic_turn`, and `accepted_stop_and_immediate_exact_resume_ignore_the_old_abort` are present. Static generation/acknowledgement review completed; tests unexecuted. |
| 11 Terminal host status | PARTIAL | Manager serializes fresh terminal/reopened status. `RpcProtocolClient` now sends `set_session_context` with `task_status` and reports rejection. Concrete daemon-host-session transport and terminal teardown/republication proof remain missing; a process protocol implementation alone is not host transport parity. |
| 12 Late durable identity | PARTIAL | RPC client subscribes to `session_id`/`session_replaced` IDs. Added predecessor replacement/first-stamp-only manager regression. Ownership/epoch fencing retained. Concrete host event naming and early event buffering/replay still require upstream proof; fake delivery does not establish the host source. |
| 13 Restore without turn | DONE | `manager/respawn.rs` suppresses continuation when Paused and refuses absent transcript. Added paused RPC respawn/no-follow-up and same-epoch reattach regression in `manager/manager_tests/respawn.rs`. |
| 14 Root admission fencing | PARTIAL | Ordinary respawn now owns an entered root admission ticket spanning rebuild, checks it before launch, and releases via RAII. Existing staging/residency/queue fencing retained. Real respawn versus Stop race regression and execution remain outstanding. |
| 15 Held first launches | PARTIAL | `resume_held_task_with_allocation` rebuilds cold launch spec, enters credential-authorized admission, CASes Pending/epoch/host, requeues saved custody, and transfers scope. Launch checks the credential ticket rather than only root Open state. Withdrawal now persists `queued_admission`; parser roundtrip/legacy test added. Cold live foreign-host custody is refused. End-to-end reconstruction contention proof remains outstanding. |
| 16 Run holds and held sends | PARTIAL | Resume admission guard checks current attempt/ticket/status/epoch/live handle before and after engine call; idle revival uses conditional store mutation. Native forwarding and cold allocation persistence added. Full cutoff/lost-hold races, native no-input Resume race safety and deterministic real-surface proof remain outstanding. |
| 17 Paused ownership restoration | DONE | `manager/manager.rs::restore_paused_task/restore_paused_child`: newest local transcript (mtime then path), recorded host transcript precedence, live foreign-owner refusal, paused/status/epoch/host CAS, direct paused respawn under ownership recheck, native rehold before control-only reattach, cleanup on rehold failure, same epoch preserved. `ManagerRunPausePorts::restore` now invokes it. Added same-epoch/no-prompt regression. |
| 18 Parent rung eligibility | PARTIAL | Retryable typed start failures now advance failure-sensitive chain, persist model/attempts/remainder/parent note, and attach successful context. Added first failure/category advance/parent retained regression. Existing usage/auth/provider ordering tests remain. Full start-failure matrix and execution remain outstanding. |
| 19 Parent fallback facts | PARTIAL | Shared `manager/parent_model_fallback.rs` builder/classifier now serves native and manager hops; stores bounded detail/reset/interrupted tools. Manager collects streamed assistant tool facts and failed message. Builder regression added. Exact upstream reset parsing/detail/reason matrix and stream-reset semantics have not been proven against upstream. |
| 20 Epoch-tag live stats | PRESENT | Parity tracker `run_start_epoch` retained as single source instead of duplicate epoch map. `subscribe_child_facts`, `live_run_stats`, `reacquire_for_revive` use it; self-resume tags before record reopen. No duplicate live-stat definition remains. |
| 21 Reopened running status | DONE | `manager/manager.rs` self-resume publication shares serialization mutex with terminal worker, which reads fresh state inside that mutex. Running publication follows durable reopen. |
| 22 Cancel/shutdown cleanup | DONE | Accepted cancel settles waiters/releases task leases. Shared-deadline initial/final teardown settlement contains errors/panics, subscribes via launch condvar, parks/withdraws unadmitted contexts in final cleanup; component shutdown invokes it. |
| 23 Departed parent notes | DONE | `manager/runtime_fallback_event.rs`: fallback revert and operator model-change clear stale parent note; parent route retained, colon-containing selector regression. |

Current counts: **15 DONE, 1 PRESENT, 7 PARTIAL**. Remaining units: 11,12,14,15,16,18,19.

## pause-gaps gap 3

Implemented exact resumed outcome ownership end-to-end:
- `manager/run_pause.rs::ResumeTurnResult::Resumed` and `TaskResumedTurn` own deferred `ExactTurnOutcome`, not just turn ID.
- `components/maho-omo-task/src/engine_run_pause.rs::NativeTaskPausePort` preserves `ResumedChildTurn.outcome`; `ManagerRunPausePorts::track` calls `TaskManager::track_resumed_turn` and the exact-generation outcome watcher.
- `ManagerRunPausePorts::restore` is no longer a no-op: calls transcript-only paused recovery above.
- `runners/rpc/handle.rs` captures expected turn generation and caches settled outcome per generation, preventing a resumed outcome closure from observing the previous or following generic turn.
- Focused tests: `resumed_turn_preserves_the_exact_deferred_outcome`, `exact_resumed_outcome_settles_without_waiting_for_old_generic_turn`, `paused_respawn_has_no_continuation_prompt_and_reattach_keeps_epoch`.
- No cargo execution: type/behavior acceptance remains for global gate.

## Continuation evidence

The requested 12 units all received implementation or regression-test work. DONE denotes implementation closure after static contract review, not executable acceptance: no cargo build or test has run. The table retains PARTIAL where concrete behavior still needs review or implementation. Earlier continuation notes below describe historical gaps and are superseded by the per-unit table as each unit closes.

Additional changes in this continuation:
- Durable queued admission metadata is parsed through the actual store parser, with a legacy-compatible roundtrip test.
- Held launch reconstruction refuses a live foreign host before ownership CAS.
- Native hold observation reads owner and current ledger pause sequence atomically instead of guessing from direct holds' local sequence fields.
- Native resumed results are captured from subscribed turn events before release, not obtained from a later generic idle/last-text read. No-input Resume explicitly submits a continuation; exact concurrent admission safety still needs a real-engine regression.
- Superseded in-process first-turn waiters receive a terminal outcome rather than being abandoned.

No cargo, eval, DAG/workflow delegation, or integration-worktree writes were used. Added tests remain unexecuted and compilation/type correctness remains unverified.

## Verification

- All branch-changed Rust files: `rustfmt --edition 2024 --config skip_children=true --check <file>` PASS before final report.
- `git diff --check`: PASS.
- Merge tree checked after rebasing; exact final result recorded in final response.
- Integration worktree was read-only; this report is committed in shard worktree only.
