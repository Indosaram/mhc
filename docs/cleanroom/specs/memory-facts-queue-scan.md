# Facts queue scan — clean-room spec

Behaviour reference: `crates/omo/memory-core/src/facts/queue_scan.rs`. Implemented in
`crates/maho-memory/src/facts_queue_scan.rs`.

An extraction run needs the queue entries it should consider. The queue directory can hold thousands of
files and a conversation-scoped run wants only its own, so **filtering happens on the file name before any
payload is read**. Reading and parsing every file to then discard most of them pays for the whole queue to
use a handful.

## Ordering

Names are sorted before parsing. The enqueue timestamp is part of the file name
(`<timestamp>-<conversation hash>-<end hash>.json`), so name order is arrival order — callers get entries in
the order they were enqueued without sorting payloads.

## Selection

A file is an entry candidate when:

- it ends in `.json`;
- it is not one of the bookkeeping files `consumed.json`, `failures.json`, `claims.json`;
- when a conversation filter is given, its name contains `-<12-hex conversation hash>-`.

The hash comes from `queue_conversation_hash`, which is also what the writer uses to build the name. A filter
that matched nothing yields an empty list: an absent conversation is not an error.

## Tolerated states

- **Missing directory → empty result.** A conversation that has never produced facts has no queue
  directory; that is the normal first-run state, not a failure.
- **Unreadable or malformed entry → skipped, scan continues.** One corrupt file must not stop every other
  batch from being extracted, and the failure queue is what surfaces a bad entry to the operator. Failing the
  whole scan instead would let one truncated write block all extraction indefinitely.
- **Non-`.json` files → ignored.**

Every returned entry carries its `file_path` so the caller can remove exactly the file it consumed.
