# Facts anchor — clean-room spec

Behaviour reference: `crates/omo/memory-core/src/facts/queue.rs` (`effective_anchor`, `enqueue`).
Implemented in `crates/maho-memory/src/facts_anchor.rs`.

Extraction is incremental: each run enqueues only material newer than everything already handled. The
"already handled" point — the **anchor** — is derived from durable state, never from memory, because the
process that enqueued may have died long before the one that extracts.

## Sources

Four places can each know how far extraction got, and the anchor is the **highest** position any of them
names:

1. the cursor's `enqueued_through_message_id`;
2. the cursor's `consumed_through_message_id`;
3. `consumed.json` for this conversation;
4. every batch file still on disk — pending **and** parked — for this conversation.

Sources 1 and 2 are separate because a batch can be enqueued before any extractor has run. Source 4 is
needed because a batch that was enqueued but neither consumed nor cursor-advanced still represents handled
material: without it, a crash between publish and cursor advance would re-enqueue the same range forever.

Parked batches count. A parked batch is material an extractor already read and set aside; ignoring it
would make the anchor fall back and re-enqueue the identical range on every run.

## Candidate resolution

Candidates are **message ids**, resolved against the transcript being considered. Positions are never
stored, because compaction rewrites the transcript and any stored line number would drift.

- An id that appears several times resolves to its **last** canonical line: a message can span several
  lines and its boundary is the final one.
- An id **absent** from the transcript contributes nothing. The transcript was compacted past it; mapping
  it to `-1` would be indistinguishable from "nothing handled yet", which is the state that re-extracts
  everything.
- An id at or past the end contributes nothing: it names a message this snapshot does not extend beyond,
  so there is nothing after it to enqueue.
- With no usable candidate the anchor is `-1`, meaning "before line 0".

## Range

The new range **starts strictly after** the anchor, at the first **canonical** entry. Strictly after, not
at: including the anchor line would re-enqueue the same message on every run. Canonical means `assistant`
with non-empty text and a non-empty message id — the same predicate that decides reflected content
elsewhere, so a range never begins mid-message or at a line that carries no fact.

It **ends at the last canonical entry** in the snapshot. Leaving the tail out would re-anchor in the middle
of the conversation, and the batch would never reach the newest message.

If no canonical entry follows the anchor, the result is "nothing new" and no file is written.

## Cost

Positions are indexed once per transcript rather than once per candidate. A conversation with four
watermarks and a thousand retained batches would otherwise walk the same transcript five thousand times
per enqueue.
