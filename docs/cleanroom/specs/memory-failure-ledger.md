# Facts failure ledger — clean-room spec

Behaviour reference: `crates/omo/memory-core/src/facts/failures_schema.rs` (with the streak policy in
`failures_backoff.rs`). Implemented in `crates/maho-memory/src/facts_failures_schema.rs`, policy in
`crates/maho-memory/src/failure_backoff.rs`.

The ledger is `failures.json`: one record per conversation *endpoint* that failed extraction, carrying the
streak, the reason, and when it may be retried.

## Why fail-closed

A corrupt ledger must never be read as "no failures". Reading it that way retries every parked conversation at
once and recreates the outage that parked them. Every structural problem is therefore a typed
`FactsFailuresCorruptError` with a reason string; the caller decides to skip the batch, never to proceed as if
the file were empty.

## File shape

```json
{
  "version": 1,
  "updatedAt": "2026-11-11T00:05:00.000Z",
  "entries": [ ... ]
}
```

| Field | Rule |
|---|---|
| `version` | must be exactly `1`; anything else (including absent) is corrupt |
| `updatedAt` | string in the exact millisecond-precision RFC 3339 form this crate writes |
| `entries` | array of objects |

`render_failures_file` always emits a trailing newline and sorts entries, so rewriting an unchanged ledger
produces no diff.

## Record shape

Field names are mixed by upstream design and must not be "fixed": `conversationId`, `firstFailureAt`,
`lastFailureAt`, `lastReason`, `lastFailureId`, `nextEligibleAt`, `parkedAt` are camelCase, while
`end_message_id` and `end_snapshot_line` are snake_case. These are persisted names read by other tooling.

| Field | Rule |
|---|---|
| `conversationId` | non-empty string |
| `end_message_id` | non-empty string |
| `end_snapshot_line` | non-negative integer |
| `state` | `backoff` or `parked` |
| `streak` | integer ≥ 1 |
| `firstFailureAt`, `lastFailureAt` | exact-form instants |
| `lastReason` | one of the twelve known reasons |
| `lastDetail` | absent, or a **non-empty** string |
| `lastFailureId` | non-empty string |
| `nextEligibleAt` | backoff: required instant; parked: must be `null` |
| `parkedAt` | parked: required instant; backoff: must be absent |

An empty `lastDetail` is rejected rather than normalised to absent: a writer that meant "no detail" should omit
the field, and accepting `""` would make two encodings of one state.

### State and timestamps are mutually exclusive

A parked record has `parkedAt` and a null `nextEligibleAt`; a backoff record has `nextEligibleAt` and no
`parkedAt`. Enforcing disjointness keeps "when may this retry" answerable from one field instead of from a
combination the reader has to reconstruct.

## Instants

Parsed with chrono RFC 3339, then re-rendered in millisecond form and compared to the input. A value that parses
but re-renders differently (`2026-11-11T00:00:00Z`) is rejected. Two spellings of one instant would otherwise
compare unequal, and the backoff comparison would treat a record as eligible early or late depending on who wrote
it.

## Reasons

`quick_category_unavailable`, `sandbox_unavailable`, `child_exit`, `deadline_exceeded`, `invalid_extraction`,
`memory_write_lock_exhausted`, `parent_dirty`, `unknown_liveness`, `payload_envelope_oversize`,
`payload_entry_oversize`, `secret_like_content`, `other`.

An unknown reason is corrupt, not silently mapped to `other`. A new reason means a newer writer; downgrading it
to `other` hides which failure is actually happening.

## Sorting

Entries sort by conversation id, then snapshot line, then end message id. The order is total, so the rendered
file is a pure function of its contents.
