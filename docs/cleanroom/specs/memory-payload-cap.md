# Facts payload cap — clean-room spec

Behaviour reference: `crates/omo/memory-core/src/facts/payload_cap.rs`. Implemented in
`crates/maho-memory/src/payload_cap.rs`.

A facts-extraction payload over 128 KiB is rejected by the provider, so the batch is cut before it is
written. The cut preserves two invariants:

1. **Conversation contiguity.** Entries from one conversation are never interleaved with another's.
   The extractor reads each conversation as a contiguous span, so a split conversation would produce
   two half-reads of the same material.
2. **Oversize is reported, not skipped.** An entry whose own payload alone exceeds the cap is returned
   as `OversizedEntry { header, bytes }` with its body cleared. Dropping it silently would retry it
   forever at the head of the queue; the caller parks or fails it instead.

## Sizing

The measurement uses the same serializer that writes the on-disk payload, so the number checked is the
number written. Two details matter:

- Entries are measured with the envelope present. A tiny entry still costs the envelope's bytes, so a
  cap smaller than the envelope selects nothing and sets `envelope_oversized`.
- Appending is incremental: `single_bytes - envelope_bytes` is the entry's marginal cost, minus two
  bytes for every entry after the first. Pretty-printed array elements are identical at any position;
  the first replaces `[]` and later ones reuse the separator line.

## Ordering

Conversations are ordered by fairness, not arrival:

- a conversation waiting at least 24 hours (`FACTS_STARVATION_MS`) outranks every non-starved one
- among starved conversations, **the oldest goes first** — hence `right_wait.cmp(&left_wait)`
- among non-starved conversations, the longest wait goes first

An enqueue time that does not parse counts as zero wait rather than sorting first or last, so a
corrupt timestamp cannot pin a conversation at the head forever.

Within a conversation, entries are ordered by `range.end_snapshot_line`.

## Loading

`select_capped_batch_with` takes headers and a `load` callback, so only the selected batch and oversize
candidates stay resident. A `load` returning `None` stops the current bucket: continuing past a missing
entry would let a follower leapfrog the hole and reorder the conversation.

PERF-DIVERGENCE(upstream): the upstream retains every oversized entry fully in memory until
classification. This keeps only the header plus the measured byte count. Parking and failure semantics
are identical.
