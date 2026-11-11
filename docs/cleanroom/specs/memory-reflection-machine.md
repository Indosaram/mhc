# Reflection machine — clean-room spec

Behaviour reference: `crates/omo/memory-core/src/reflection/machine.rs`. Implemented in
`crates/maho-memory/src/reflection_machine.rs`.

## Model

A reflection run is never started directly. A journal reports what happened, the machine decides whether work is
warranted, and a reservation records it in one of two slots: `active` (running now) or `pending` (queued). Every
transition is a pure function of `(MachineState, event)`, so replaying an event produces the same reservation.

## Triggers

Four triggers, in priority order for both evaluation and merge:

| Trigger | Fires when |
|---|---|
| `Compaction` | `on_compaction` is on and the journal has `pending_compaction` |
| `StepCount` | `step_count > 0` and `steps_since_last_successful_reflection >= step_count` |
| `Dream` | `unreflected_bytes >= snapshot_max_bytes` |
| `Manual` | a caller asks directly |

Priority values used when merging: step-count `1.0`, idle/pressure dream `1.5`, compaction `2.0`, shutdown dream
`2.5`, manual `3.0`.

## Evaluation

`Settled { success: false }` reserves nothing. Otherwise the machine checks compaction, then the step threshold,
then byte pressure, and takes the **first** candidate. A candidate is skipped while a run of the same trigger is
already active or pending — so a queued compaction run is not duplicated on the next settle.

`CompactionAccepted` only sets `pending_compaction`; it does not reserve. Reserving happens on the settle that
follows, which keeps compaction from racing the turn that caused it.

`Manual` always reserves. Its conversation list defaults to the current journal when absent or empty, and
duplicates are removed in order.

## Reservations

`reserve_transition` fills `active` when it is empty; otherwise it writes `pending`. A second write to a non-empty
`pending` **merges** instead of replacing, keeping the original `run_id`:

- trigger, origin, focus and `target_doc` come from the higher-priority side (right wins ties);
- `origin` survives only for a dream trigger;
- `conversation_ids` are unioned preserving first-seen order;
- `snapshots` are keyed by conversation, and the right side wins a collision because it is the fresher capture;
- `recent_n` is the larger of the two.

## Pending cap

`REFLECTION_PENDING_MAX_CONVERSATIONS` = 32 and `REFLECTION_PENDING_MAX_BYTES` = 4 MiB. The cap drops the oldest
`conversation_id` first and drops any snapshot belonging to it, so the two lists never disagree about which
conversation is queued. If the byte cap cannot be met the loop stops when the list empties rather than spinning.

`make_request` captures only the journal it is handed, so a merged list may name conversations with no snapshot.
That is intentional: the run fetches those from disk.

## Completion

`complete_transition` refuses an unknown `run_id`. `Merged` and `NoChanges` are success; everything else leaves the
journals untouched so the work is retried.

On success:

- the active run's snapshots are returned as `finalize`;
- if the trigger was compaction, every conversation in that run whose `pending_compaction` is still true has it
  cleared — conversations the run did not name are left alone;
- for each finalized conversation, `reflected_completed_steps` advances by the captured completed-step count,
  clamped to `total_completed_steps`, and `steps_since_last_successful_reflection` is recomputed from the new
  cursor rather than decremented.

Then the pending run is checked against the **updated** journals: it launches only while its trigger is still
satisfied. Manual and dream always do; compaction needs a still-pending conversation; step-count needs the
threshold still met. A dropped pending run leaves both slots empty.
