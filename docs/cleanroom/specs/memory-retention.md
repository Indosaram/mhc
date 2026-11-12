# MIT store retention — clean-room spec

Implemented in `crates/maho-memory/src/retention.rs` (policy) and `Store::prune` in
`crates/maho-memory/src/store.rs`.

## Why this exists

The MIT store writes to `working_memory` and `episodic_memory` and, before this, never removed anything. Phase 0
found the SUL memory store at 3.4 GB and reclaimed 1.46 GB by enforcing age limits there. The MIT store had the
same design gap: a long-lived bank grows without bound, and an agent's own notes eventually dominate the file.

Unlike SUL retention, this is *not* a scheduler. It is an explicit, idempotent call that drops what policy allows,
so a caller decides when to spend the I/O.

## Policy

| Field | Default | Meaning |
|---|---|---|
| `working_days` | 30 | age at which an inactive working row becomes eligible |
| `episodic_days` | 90 | age at which an episodic row becomes eligible |
| `episodic_keep_importance` | 900 | permille at or above which a row is never dropped |

`clamped()` bounds both windows to 1..3650 days and importance to ≤1000. The lower bound matters: a settings file
with `0` must not mean "delete everything now" — it means one day.

## Imported rows are exempt

A row with `source = 'import'` is never dropped, whatever its age or importance.

This is not a convenience. `mhc memory import-legacy --history` writes one row per *file revision*,
stamped with that revision's commit date, so an imported row is ancient the moment it is written —
that is the entire point of importing history. Combined with the importance mapping (`system/` is
0.9, but `notes/`, `project/`, `reference/` and `people/` land at 0.6–0.7, below the 900 keep
threshold), a single `mhc memory prune` after an import deleted the whole imported corpus. This was
an actual defect, caught by importing a 2020-dated store and pruning it.

The exemption is on `source`, not on age or a flag the importer would have to remember to set:
the row says what it is. A row the agent writes through `remember` keeps `source =
"conversation"` (or `"coding-agent-retain"`) and ages out normally.

## The conditions a row must meet to be dropped

All of them, or the row stays:

0. **It is not imported.** `source = 'import'` is never dropped; see above.
1. **This session owns it.** `session_id = current`. A bank shared across sessions must not lose another agent's
   memory because this session stopped using it.
2. **It is old enough**, measured against the policy window for its table.
3. **It is not still valid.** A row whose `valid_until` is in the future is live regardless of age.
4. **Its importance is below the keep threshold.** Importance is converted to permille and compared as an integer,
   so the boundary cannot wobble on a float. `NaN` counts as zero (droppable); `+∞` counts as 1000 (pinned), and a
   negative score as zero.

## Timestamps: two formats, one comparand

`timestamp` is ISO-8601 (`2026-11-11T13:45:00.000Z`); `created_at` is SQLite's default
(`2026-11-11 13:45:00`). A plain string comparison between them is silently wrong — it is only correct for one
format. Both therefore go through `julianday` in SQL. This was an actual bug caught by the tests: the first version
compared strings, and no row ever aged out.

Age is measured from `timestamp` with `created_at` as the fallback, because `timestamp` is what the writer sets and
`created_at` is only the insert time.

## Report

`PruneReport { working, episodic }` carries the counts actually deleted, so a caller can log or surface "reclaimed N
rows" instead of guessing.
