# Transcript cursor — clean-room spec

Behaviour reference: `crates/omo/memory-core/src/journal/cursor.rs`. Implemented in
`crates/maho-memory/src/cursor.rs`.

Reflection is incremental, so progress is stored as a **message id**, never a line number. Compaction
rewrites the transcript; a line number would afterwards point at different content and a run could skip a
conversation or read one twice. Byte offsets are derived from the id for reporting, not used as the anchor.

## What counts as a step

One `assistant` entry with a non-empty `source_message_id` and non-whitespace text. `user` and
`tool_call` entries are context, not progress. `total_completed_steps` is **derived from the entries**
every time, never incremented, so a retried write or a replanted journal cannot inflate progress.

`reflected_completed_steps` is clamped to the total: compaction can remove already-reflected messages, and
a cursor past the end would make `steps_since_last_successful_reflection` underflow.

## Capturing the next slice

Start searching after the reflected message id (whole transcript when there is no anchor), then at the
first canonical assistant entry.

The budget cut happens **between messages**, never inside the lines that share one `source_message_id`:
the extractor reads a captured conversation as one contiguous span, so half a message would be attributed
to the wrong run. Rules:

1. A group that would exceed the budget and is not the first ⇒ stop before it, leaving the rest as
   `backlog_remaining`.
2. A single group larger than the budget **is still returned when it is the first group**. Refusing it
   would stall reflection forever on one long message.
3. If everything fits, the snapshot runs to the end and `backlog_remaining` is absent.

## Finalizing

| Outcome | Effect |
|---|---|
| failure | cursor unchanged; only counters recomputed |
| success | cursor moves to the snapshot's end message, byte offset recomputed, `reflected_completed_steps` recount, `consecutive_failures = 0`, `next_eligible_at` cleared, success timestamp set |

A failure must not advance the cursor: the same slice has to be readable again on the retry. Zeroing the
failure counters on success is what lets a recovered conversation be scheduled normally instead of being
held back by its own history.

## Parsing stored state

Every absent field takes its empty value — a partial write must not make a conversation permanently
unreflective. Negative and non-numeric counters coerce to 0 for the same reason: a corrupt counter would
otherwise block that conversation forever.

Three things do fail or normalise:

- A non-object payload is an error: `[]`, `3`, and non-JSON are not a state at all.
- An empty `reflected_through_message_id` parses as **absent**, because `""` would match no entry and
  silently mean "nothing reflected" via a different path than `None`.
- `schema_version` is always re-stamped to the current rule (`v3_assistant_steps`) on read, so counters
  written by an older counting rule are never compared against new ones.
