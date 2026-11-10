# Journal — clean-room spec

Behaviour reference: the journal in `crates/omo/memory-core/src/journal/`. Implemented from scratch
in `crates/maho-memory/src/journal.rs`.

Every store change is recorded as an event before the state it implies is trusted, so an interrupted
write can be replayed to the state it was meant to reach.

## Entries

A journal is a sequence of entries, each with a monotonic `seq` starting at 1 and an event:

- `remember` — id and the stored content (already redacted, see [memory-redact](memory-redact.md));
- `forget` — id;
- `revalue` — id and the new importance.

The events serialize as JSON lines with `op` carrying the kind, so a line is self-describing and a
partially written file is diagnosable by eye.

## Appending

`append` refuses a `seq` that does not equal `next_seq()`; the journal never silently renumbers,
because a renumbered entry would claim a position in history it did not occupy. `record` is the
convenience form that uses the next number.

## Loading

`parse` reads JSON lines. A damaged **final** line is dropped: that is exactly what a crash
mid-write leaves behind, and refusing to load the whole journal would lose every good event before
it. A damaged line **anywhere else** is reported as `Malformed { line, reason }` — corruption in the
middle is not a torn write, and pretending otherwise would silently discard real history.

Entries are sorted by `seq` after loading so replay sees them in order.

## Replay

`replay` folds the events:

- `remember` inserts or replaces the id's content;
- `forget` removes the id, and also drops its importance;
- `revalue` sets importance.

An event that cannot apply — forgetting an absent id, revaluing an id that was never remembered — is
**discarded with a reason** rather than aborting. One lost tail event must not hide the events
before it, and a caller needs to know which events were skipped.

## Compaction

`compact_through` drops everything up to and including `seq` and leaves `next_seq()` continuing from
the last kept entry. Compaction is only safe once the resulting state is persisted elsewhere; the
journal does not decide that for the caller.
