# senpi-67 implementation report

Target: senpi f2354d9705d37636825b7ccd60ee8f40bf0123ba (2026.10.23-1).
Branch: p5147/senpi-67. No Cargo commands executed.

## Implemented units

1. **Lifecycle ledger and facts - implementation added, runtime validation pending.**
   - session_lifecycle.rs now owns LifecycleLedger, LifecycleChildren and SessionWorkObligations rather than independently incrementing lifecycle epochs. Persistence callbacks and restored entries are explicit constructor inputs.
   - Controller admission/self-settle fencing, interrupted-run settlement, goal/run completion, custody flushing and terminal suppression use durable ledger state.
   - evaluate_turn_facts implements effective wake filtering, queued-human continuation, retry/monitor/recovery background facts, synthetic waits and provider failure facts.
   - AgentSession::refresh_lifecycle uses the same evaluator, typed reducer and durable custody facts. Work-obligation mutations and wake publishers recompute after dropping registry locks.
   - Goal turn-end publisher now sends structured input facts; the session subscribes to these rather than treating verdict labels as facts.
   - Live goal transitions publish goal_lifecycle_facts and are bound into AgentSession. The rebased controller retains integration's neutral GoalLifecycleService, including complete-then-clear observation; its regression test was adapted to the durable ledger constructor.
   - Focused tests cover ledger epoch/revision ownership, background custody, provider failure and queued-human/retry continuation.

2. **Extension move API release/import orchestration and cancellation - implementation added, runtime validation pending.**
   - maho-rpc/session_release_adapter.rs now calls AgentSession::emit_session_move; handler failures propagate into the existing release refusal path rather than being discarded on a fire-and-forget bus.
   - Release admission close/reopen/pending operations use the session's real ExternalAdmission.
   - AgentSession::bind_extensions invokes existing restore_moved_state after session_start. The existing attempt-isolated extension_state implementation preserves pending payload on failure and records/removes it only after successful import handlers.
   - Before-tool hooks propagate AbortSignal into the tool barrier and block execution after cancellation. Barrier cancellation awaits the signal's cancellation primitive, eliminating an abort-listener registration race. Added pre-aborted barrier test.
   - Existing move tests exercise attempt isolation, redelivery and sealed-attempt selection.

3. **Held input releasing-turn admission and settlement custody - implementation added, runtime validation pending.**
   - take_held_input_messages consumes recognized held commands before ordered execution, expands prepared text at delivery, suppresses manual-continue literal messages, and builds held-input custom messages for the releasing turn.
   - Prompt and custom-turn paths carry prepared held messages and record IDs only at admission; queued prompts steer the held messages into the admitted run.
   - AgentEnd consumes only IDs admitted under the current user-abort generation. A newer Stop retains durable held custody.
   - Inputs received beneath an inherited hold are stored before releasing direct Stop holds. pause_for_run/resume_for_run use the existing RunHolds seam; accepted inherited Resume triggers held delivery.
   - Resume no longer sends each held input as an independent human prompt.
   - Focused tests assert preparation does not consume custody, preserves input order, and excludes inputs already claimed by another turn.

4. **Lost-lock write fencing - previously implemented.** file_lock.rs, lockfile_policy.rs and credential/move write assertions.
5. **Tool interruption result - previously implemented.** interrupted_tool_result.rs and session stop integration.
6. **Footer subagent cost - previously implemented.** footer_data_provider.rs and interactive footer.

## Verification and remaining risk

Rustfmt checks and git diff --check are the available local gates. Cargo build/check/test/clippy were prohibited; no Rust test or executable validation is claimed. The new controller options are a Rust API change; repository search found only local constructor sites before the rewrite. All added test behavior remains unexecuted until the global Cargo gate.

Implementation is substantially extended, but full upstream equivalence is NOT verified. In particular, exhaustive command/reentrant prompt ordering, concurrent queued-turn consumption, and custom-tool workspace-effect propagation still need runtime coverage/review at the global gate. Do not infer tested parity from commit subjects containing 'finish'.
