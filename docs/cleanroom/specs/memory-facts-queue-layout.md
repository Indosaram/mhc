# Facts queue layout — clean-room spec

Behaviour reference: `crates/omo/memory-core/src/facts/schema.rs`. Implemented in
`crates/maho-memory/src/facts_schema.rs` (paths, watermarks, entry parsing).

The queue is a directory of JSON files, not an append-only log. Every path must therefore be derivable
from its contents alone, and any file may be replaced by a double-write loser without corrupting the
queue.

## Layout

Under `factsQueue`:

| Path | Contents |
|---|---|
| `<ts>-<convHash12>-<endHash8>.json` | one queued batch |
| `cursor/<convHash12>.json` | per-conversation watermark |
| `consumed.json` | consumed watermarks across conversations |
| `failures.json` | extraction failure records |
| `parked/` | entries parked after repeated failure |

Conversation and message ids enter file names **only as SHA-256 hex prefixes** (12 and 8 characters).
They are caller-controlled text; a raw id containing `/` or `..` would move the file out of the queue.

`<ts>` is `YYYYMMDDTHHMMSSmmmZ`: the enqueue time with `-`, `:` and `.` stripped, because colons are not
portable in a Windows file name. The timestamp is part of the name so re-enqueuing the same range creates a
**new** file; overwriting would delete a batch an extractor may already be reading.

## Cursor

Two independent watermarks per conversation:

```json
{"version":1,
 "enqueued_through_message_id":null, "enqueued_through_snapshot_line":-1,
 "consumed_through_message_id":null, "consumed_through_snapshot_line":-1}
```

They are separate because enqueue and consume run on different timers: a batch can be enqueued before any
extractor has run, and consuming one batch must not make the next range look already enqueued.

`-1`, not `0`, is the initial snapshot line. Line 0 is a real position; `0` would claim the first line is
already handled and skip it.

A corrupt or wrong-version cursor is **not** an error. It falls back to the initial state: that re-reads
some already-extracted messages, which costs work, while failing would stall that conversation's extraction
permanently. Empty strings are treated as absent, and negative line numbers as `-1`.

## Consumed watermark

`consumed.json` maps conversation id to `{end_message_id, end_snapshot_line, consumedAt}`. Parsing skips a
bad **record** rather than discarding the file: the other conversations' progress is still true, and losing
it would re-extract every one of them.

## Queue entry

A scheduling-usable entry needs `identity`, `sessionId`, `conversationId`, `enqueuedAt`, and a `range` of
`{start_message_id, end_message_id, start_line, end_snapshot_line}`. Parsing is all-or-nothing: a batch
missing its range or conversation cannot be safely extracted, and one missing its identity would be written
into the wrong store. A `version` other than 1 means the file is not this queue's entry.

The transcript payload is *not* parsed here. `parse_queue_entry` reads only what scheduling needs, so
scanning a queue of large batches does not load every message body.

## Position

`canonical_position` returns the index of the last canonical (`assistant`, non-empty text, non-empty
message id) entry carrying the given message id, or `-1`. It searches from the end because one message can
span several lines and the last line is the boundary — the next read starts after it.
