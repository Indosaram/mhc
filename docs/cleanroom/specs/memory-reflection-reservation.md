# Reflection reservation persistence — clean-room spec

Behaviour reference: `crates/omo/memory-core/src/reflection/reservation.rs` (the persistence half).
Implemented in `crates/maho-memory/src/reflection_reservation.rs`.

The reservation is two files under the identity's `reflection/` directory:

- `active.lock` — the run that currently holds reflection,
- `pending.json` — the one run waiting for it.

Both are read by a process that may not be the one that wrote them, by definition: the whole point of the
record is that a *later* process finds out what an *earlier* one was doing.

## Missing is idle; corrupt is not

A missing file means "nothing here" — every identity that has never reflected is in that state, so it is not
an error.

A file that exists and does not parse is `Corrupt{path, detail}` and is **never** treated as absent. Silently
discarding it would make a damaged store look idle, and a second run would then start on top of a reservation
that is still real. The error names the path but never echoes file contents.

## One run cannot be both active and pending

When both files carry the same `run_id`, the active copy wins and the pending copy is dropped.

The pending entry is the queue. Re-queuing the run that is already running would make it run twice — and a
reflection run mutates memory, so running it twice is not idempotent.

## Atomic write

`write_json_atomic` writes `<name>.tmp-<random>`, `sync_all`s, then renames over the target.

- The temp name carries a **random** suffix. Two writers racing on the same reservation must not share a temp
  path; if they did, one would truncate the other's half-written file and the rename would publish garbage.
- `sync_all` comes **before** the rename. Without it the new name can become visible while its contents are
  still not on disk, and a crash restores an empty file — which `read_state` would then report as corrupt, on a
  reservation that was fine a moment ago.

`write_state` with `None` removes the file, and removing an already-absent file is not an error: the desired
end state (no file) holds either way.

## Dream bookkeeping

`write_dream_state` records `{last_dream_at, last_run_id}` at `<runtime>/dream/state.json` after a completed
dream run, using the same atomic write.

## `ReservationStore`: the coordinator

`ReservationStore` joins the ported pieces: it reads state, calls `reserve_transition` or
`complete_transition` from `reflection_machine`, stamps the launcher identity, and writes state back.

It does **not** take an OS lock. Callers that must exclude other processes wrap `reserve` and `complete` in
`lock_acquire::with_lock`. Keeping the lock out is what lets every transition here be tested against a plain
directory, with an injected clock, run id, and launcher identity.

The launcher identity is stamped on the **active** run only. A pending run records no owner because it has not
started: claiming a pid for it would make the liveness reaper treat a run that never launched as live, or as
dead, depending on when it looked.

`complete` takes the journal snapshots as a parameter rather than resolving them internally. Completion decides
whether the pending run launches from those snapshots, so a caller that cannot supply one per conversation must
not call it: an empty map does not fail loudly, it silently drops the pending run.

## Scope boundary

This module owns persistence and coordination. It evaluates no trigger policy (that is `reflection_machine`),
spawns no agent, and touches no git. Upstream keeps lock, persistence, transitions and journalling in one
492-line type; splitting them is what makes each rule testable without a repository, a clock, or a lock.

Two upstream signature differences remain, and both are absorbed by callers rather than by this module:

1. Upstream `with_lock(path, record, options, body)` vs this crate's
   `with_lock(path, record, liveness, local, now, body)` — this crate injects what upstream packs into
   `AcquireLockOptions`.
2. Upstream `journal::store::TranscriptJournal` exposes `get_state()`/`set_pending_compaction()`; this crate's
   `journal::Journal` is an in-memory replay log, so the snapshot map is passed in.

Identity is not a constraint: `MemoryIdentityPaths` already carries `reflection`, `locks` and `runtime`.
