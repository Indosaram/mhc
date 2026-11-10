# Memory facts queue — clean-room spec

Behaviour reference: `crates/omo/components/maho-omo-memory/src/facts_*.rs` and one live queue at
`~/.omo/memory/agents/<id>/runtime/facts-queue/`. Implemented from scratch in
`crates/maho-memory/src/facts_queue.rs`.

## Layout

```
runtime/facts-queue/
  <enqueued>-<session>-<batch>.json   one enqueued batch
  claims.json                          in-flight claims
  consumed.json                        per-conversation watermark
  failures.json                        failure ledger and parked entries
  cursor/<batch>.json                  per-batch read cursor
  parked/                              parked batches, pruned after 14 days
```

## Batch

```json
{
  "version": 1,
  "identity": "indo-b00481cc",
  "sessionId": "01a1c486-...",
  "conversationId": "01a1c486-...",
  "range": { "start_message_id": "e2cff149", "end_message_id": "aa4387e4",
             "start_line": 0, "end_snapshot_line": 4 },
  "enqueuedAt": "2026-11-10T08:16:05.773Z",
  "entries": [ { "kind": "user", "text": "run", "captured_at": "...", "source_line_id": "...", "source_message_id": "..." } ]
}
```

A batch is only complete when `range.end_message_id` is non-empty and
`range.end_snapshot_line` is greater than `range.start_line`; a batch describing an empty range
is dropped at enqueue time rather than written.

## Claims

`claims.json` maps `<conversationId>\0<messageId>` to `{claimId, claimedAt}`. A key already claimed
is never claimed again by the same claim id; a claim whose batch completed is removed. Claims are
the only thing that stops two consumers from drinking the same range.

## Consumed watermark

`consumed.json` records, per conversation, the last `end_message_id` and `end_snapshot_line` that
were applied. A batch whose range ends at or before the watermark is already consumed and is not
enqueued again.

## Failures

`failures.json`:

```json
{
  "version": 1,
  "updatedAt": "2026-11-10T08:11:11.546Z",
  "entries": [ { "conversationId": "...", "end_message_id": "...", "end_snapshot_line": 4,
                 "state": "parked", "streak": 5, "firstFailureAt": "...", "lastFailureAt": "...",
                 "lastReason": "child_exit", "lastFailureId": "...", "nextEligibleAt": null } ]
}
```

`state` is `retrying` while `streak` is below the park threshold and `parked` at or above it.
A parked entry is not retried until `nextEligibleAt` passes; `null` means never automatically.
`streak` resets to 0 and the entry leaves the ledger when a batch for that range succeeds.
