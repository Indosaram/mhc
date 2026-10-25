# omo-31

## Implementation

Branch: `p5147/omo-31`
Commit: `d607abcc feat(parity): port omo-31 run pause custody from omo v5.1.47`

Units=6 DONE=6 SKIPPED=0 PRESENT=0.

| Unit | Status | Files / implementation |
| --- | --- | --- |
| admission-barrier.ts | DONE | `crates/omo/senpi-task/src/pause/admission_barrier.rs`: reject sequence mismatch in both Closed and Resuming; private shared identity token prevents reconstruction and invalidates credentials after restore/replacement. |
| ports.ts | DONE | `crates/omo/senpi-task/src/pause/ports.rs`: engine hold state/input/inherit/resume contracts, observed holds, native child pause port, exact resumed-turn receiver, run-fence context, staged child creation and guarded activation contracts. |
| guidance.ts | DONE | `crates/omo/senpi-task/src/pause/guidance.rs`: immutable revisioned draft edits/removal, duplicate/frozen errors, stable sorted scoped dispatch, freeze save before delivery, per-delivery acknowledgements, queued childless targets retain dispatching custody, same input ID/revision on crash recovery. |
| native-child-pause.ts | DONE | `crates/omo/senpi-task/src/pause/native_child_pause.rs`, `src/manager/child_handle.rs`: optional native engine controls on managed handles; production adapter forwards every operation, refuses unavailable engines/stale resumes, preserves other holds, returns exact native resumed-turn outcome. No local simulated engine. |
| registry.ts | DONE | `crates/omo/senpi-task/src/pause/registry.rs`: validated parent paths, private atomic JSON replacement, synchronous close before I/O, guarded participant mutations, ledger restore/rebuild, persisted phases, frozen resume cutoffs, installed acknowledgements, barrier credentials and parked-first admission order. |
| run-controller.ts | DONE | `crates/omo/senpi-task/src/pause/run_controller.rs`: synchronous admission closure at pause invocation, snapshots before effects, quiesce/fences, two propagation passes around creation settlement, uncertain hold observation/lost recovery, guidance-before-turn resume, transaction identity/supersession checks, install/ack then explicit release, newer child holds, recoverable lost resume failures/retries, cancellation before custody removal. |

Exports: `crates/omo/senpi-task/src/pause/mod.rs`.
Dependency features: existing tokio dependency enables sync/macros/rt for exact-event coordination and async tests; no new crates.

## Tests

`crates/omo/senpi-task/src/pause/parity_tests.rs` ports regression cases for credential replacement/restore and wrong Resuming sequence; registry write/restore/rebuild, ordering, frozen cutoffs and stale updates; guidance drafts, crash redelivery and persisted acknowledgements/queued targets; native unavailable/wrong-parent/stale/other-hold and exact resumed result; controller close-before-effects, guidance/install/ack/release order, child hold cutoffs, Stop-2 supersession and cancel-before-removal. Existing admission-barrier tests retained.

Async supersession subscribes to oneshot events before starting work and awaits bounded event/completion timeouts. No sleeps or polling delays.

## CROSS-CRATE

None. Managed handle API change stays in maho-senpi-task and has a default optional implementation. Concrete runner transport/native engine implementations remain owned by their runner census lanes, as specified in this census. The staged creation types are contracts, matching upstream ports.ts; concrete activation wiring belongs to the manager/runner lanes.

## Verification / compiler risks

Reopened every edited Rust file and inspected the manifest/export diff. `rustfmt --edition 2024 --check` succeeded for pause modules and managed child handle. `git diff --check` succeeded. No cargo build/check/test/clippy or compilation was run, as explicitly prohibited; tests are written but not executed. Compiler/type verification remains deferred to the orchestrator's merged build. Public native controls follow the existing blocking ManagedChildHandle convention; resumed outcomes use a dedicated channel rather than reusing the prior completed handle turn. New controller effects use boxed Send futures to permit concurrent supersession.

## Rebase onto parity

Rebased onto parity/omo-5.1.47 at 53340019. Resolved pause/ports.rs by retaining parity admission tickets, guarded creation, borrowed ChildPausePort and deferred exact-turn outcome while adding lane engine hold state, native receipts, refusal and fence contracts. Native adapter keeps typed native errors through inherent methods and maps errors at the existing HostError port boundary. The native regression test still awaits the dedicated exact-turn channel with a bounded timeout through the deferred outcome closure.

Duplicate scan across senpi-task and components finds one definition of each pause port type. runners/task_parent_control.rs has a separate existing wire-format RunRef/RunPauseOwner (string kind and camelCase serialization); these are transport DTOs, not duplicate pause engine contracts. No attempt was made to conflate their distinct wire shape.

Validation: rustfmt --edition 2024 --check on ports.rs, native_child_pause.rs and parity_tests.rs passed; git diff --check passed. Cargo intentionally not run.
