# Facts batch lead — clean-room spec

Behaviour reference: `crates/omo/memory-core/src/facts/queue_lead.rs` (`lead_of`). Implemented in
`crates/maho-memory/src/facts_queue.rs`.

The extractor is shown one context row before the batch so it can tell which message the facts answer: "the
user complained about latency" lands differently when the preceding row is that complaint than when it is an
unrelated tool result.

## Selection

The **last** entry strictly before the batch start whose `kind` is not `reasoning`.

- `reasoning` is skipped because it is the model thinking to itself, not something the person said or was told.
  A fact attributed to a reasoning row is attributed to a turn nobody can see in the transcript.
- Skipping is *only* over reasoning. An intervening `user` or tool row is the real nearest context and wins,
  even when reasoning sits between it and the batch. Walking further back past visible rows would replace the
  actual context with a more distant one.

## Edges

- A batch starting at index 0 has no lead.
- A transcript with only reasoning before the batch has no lead.
- A start index past the end is clamped to the transcript length rather than panicking. The upstream indexer
  would panic on `entries[..start_index]`; a caller passing a stale index should get the whole transcript as
  context, not a crash during fact extraction.

## Ordering

Search is from the end backwards, so the first match is the *nearest* row. Searching forwards would attach the
oldest visible row in the transcript instead of the one the batch follows.
