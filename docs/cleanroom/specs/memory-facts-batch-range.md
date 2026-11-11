# Facts batch range — clean-room spec

Behaviour reference: `crates/omo/memory-core/src/facts/queue.rs` (`enqueue`'s start/end selection).
Implemented in `crates/maho-memory/src/facts_batch_range.rs`, on top of the `effective_anchor` inputs in
`memory-facts-anchor.md`.

One extraction batch covers one contiguous slice of a transcript. Consecutive runs must **tile** that
transcript: no message handled twice, no message skipped.

## Start

The first **canonical** entry strictly after the effective anchor.

- Strictly after, because the anchor line is already covered by a previous batch; including it would
  re-enqueue the same message on every run.
- Canonical, because a summary or a tool echo is not something facts can be extracted from.

## End

The **last** canonical entry in the transcript, searched from the end.

Searching from the end is what makes the batch reach the newest message. Taking the first canonical after the
start, or the last entry regardless of kind, would anchor in the middle: the next run would then start after a
mid-transcript line and the tail could be re-covered or lost depending on what arrived in between.

A trailing non-canonical entry (a user message after the last answer) is therefore **outside** the batch. The
batch's declared end message names a message, and a message is the unit of work, so the slice includes the end
index itself.

## No batch

`None` when there is no canonical entry after the anchor.

A run with nothing new must not enqueue an empty batch. An empty batch would be consumed, advance nothing and
re-enqueue on the next run forever — the queue head would never drain. The same applies to a transcript with
only user messages: there is nothing to extract, so nothing is queued.

## Anchor input

The anchor is an index into the snapshot the run was given, not a line number in a file. Compaction rewrites
the transcript, so a line number would point at different text next run.
