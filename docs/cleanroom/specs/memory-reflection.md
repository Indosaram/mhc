# Memory reflection and dream — clean-room spec

Behaviour reference: `crates/omo/memory-core/src/reflection/machine.rs` and one live run at
`~/.omo/memory/agents/indo-b00481cc/runtime/reflection/completions/reflection-run-22.json`.
This spec describes observable behaviour only; it is implemented from scratch in
`crates/maho-memory/src/reflection.rs`.

## Run record

A finished run is recorded as JSON, one file per run, named `reflection-run-<n>.json`:

```json
{
  "schemaVersion": 1,
  "runId": "reflection-run-22",
  "identity": "indo-b00481cc",
  "category": "memory",
  "model": "mahoquot/gpt-6-luna",
  "conversationIds": ["..."],
  "trigger": "dream",
  "origin": "shutdown",
  "outcome": "no_changes",
  "startedAt": "2026-11-10T08:16:07.119Z",
  "finishedAt": "2026-11-10T08:17:36.942Z",
  "durationMs": 89823,
  "filesChanged": 0,
  "consecutiveFailures": 0
}
```

`trigger` is one of `step-count`, `compaction`, `manual`, `dream`. `origin` is present only for
`dream` and is one of `manual`, `idle`, `shutdown`, `pressure`. `outcome` is one of `merged`,
`no_changes`, `parent_dirty`, `merge_conflict`, `dirty_uncommitted`, `failed`, `timed_out`.

## When a run is reserved

Runs are only considered after a settled turn; a failed turn reserves nothing. At most one run is
active and one pending; a trigger already present in either slot is not reserved again.

Priority order, first match wins:

1. `compaction`: enabled by config, a compaction was accepted since the last run, and no
   compaction run is already reserved.
2. `step-count`: `step_count` > 0 and the steps since the last successful run reach it, and no
   step-count run is already reserved.
3. `dream` with origin `pressure`: unreflected bytes reach the snapshot budget, and no step-count
   run is already reserved.

A manual trigger is reserved immediately, whatever else is pending, for the named conversations
(defaulting to the current one), and carries the optional focus text and `recent_n`.

## State carried between runs

- `steps_since_last_successful_reflection` resets to 0 only on a successful run.
- `pending_compaction` is set when a compaction is accepted, and cleared when the compaction run
  completes.
- `unreflected_bytes` accumulates journal bytes and resets on a successful run.
- `consecutiveFailures` counts completed runs with a failing outcome and resets to 0 on any
  non-failing outcome.

## Dream policy

`dream-policy.json` sits beside the run directories:

```json
{
  "version": 1,
  "people": { "enabled": true, "max_entries": 40, "max_entry_chars": 200 }
}
```

People dreaming is opt-in per policy: when `people.enabled` is false, no people observations are
written, and existing entries are left alone. `max_entries` and `max_entry_chars` bound what one
dream may add; over-limit candidates are dropped rather than truncated mid-sentence.

## Retention

Run directories live under `runtime/reflection/`, which is never projected into a prompt and is
safe to prune: `memory_retention.rs` keeps the last 7 days of runs, and completions are kept for
the same window so a resumed run can still find its record.
