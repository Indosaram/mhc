# Failure selection — clean-room spec

Behaviour reference: `crates/omo/memory-core/src/facts/failures_selection.rs`. Implemented in
`crates/maho-memory/src/failure_selection.rs`.

Which queued batches may launch now, given the failure ledger. With an empty ledger everything
launches and nothing is reported as skipped.

## Two kinds of "not yet" propagate differently

This is the whole point of the module:

| record state | range skipped as | blocks successors? |
|---|---|---|
| `Parked` | `parked` | **no** |
| `Backoff` before `next_eligible_at` | `backoff` | yes |
| after a blocking skip | `blocked-by-predecessor` | yes |

A range in backoff will be retried soon, so the ranges after it in the same conversation must wait:
launching a successor would make the extractor read the conversation out of order. A **parked** range
is abandoned, so holding its successors hostage would strand the rest of that conversation forever.
Once a bucket is blocked, later ranges stay `blocked-by-predecessor` even if a parked range follows.

Blocking is per conversation. A failure in one conversation never touches another.

## Undated records

A `Backoff` record with no `next_eligible_at`, or with one that does not parse, is treated as **still
backing off**. The absent timestamp means "not known to be eligible", and a corrupt timestamp must not
turn a deliberate backoff into an immediate relaunch — which is exactly what defaulting the parsed
value to zero would do.

## Result shape

`selected` preserves input order (the caller's queue order is meaningful); `skipped` is a `BTreeMap` keyed
by `selection_key` so the report is stable and each range appears once. `selection_key` is
`conversation_id \0 end_message_id`, the same key the queue deduplication and the ledger use.
