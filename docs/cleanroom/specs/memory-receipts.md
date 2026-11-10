# Receipts — clean-room spec

Behaviour reference: `crates/omo/memory-core/src/receipts/`. Implemented from scratch in
`crates/maho-memory/src/receipts.rs`.

A receipt records what a memory-maintenance run did. Receipts live in `<runtime>/receipts.jsonl`,
**outside** the memory repository: a receipt describes a run, not memory content, so it must survive
the repository being reset or re-imported.

## Line shape

One JSON object per line, append-only, never rotated and never truncated:

```json
{"v":1,"at":"<rfc3339>","host":"<host>","pid":<pid>,"kind":"reflection",
 "runId":"...","trigger":"...","generation":"3","event":"committed","sha":"..."}
```

A facts receipt carries `batchId` instead of `runId`/`generation`. `sha`, `reason` and `detail` are
omitted when absent.

Events: `started`, `committed`, `no-change`, `rolled-back`, `failed`. Only `started` is
non-terminal; every other event closes its run.

## Identity

The identity decides whether a receipt is a second copy of one already recorded:

- run: `kind`, `runId`, `event`, `generation`;
- facts: `kind`, `batchId`, `event`.

A run's generation is part of its identity, so a **retried** run records a second receipt for the
same event while a **re-observed** one does not. That is the whole point: reconciliation re-offers
every terminal run's receipt on each start, and those re-offers must collapse. A facts batch has no
generation, so its identity is just kind, batch and event — a batch that runs again is the same work.

## Detail bounding

`reason` and `detail` are redacted with the same secret scan as memory content
(see [memory-redact](memory-redact.md)) and bounded to **400 UTF-16 code units**, cut at a character
boundary and suffixed with `...`.

UTF-16 code units, not chars or bytes: the bound must mean the same thing on every platform, and a
surrogate pair must never be cut in half. Truncating at a raw UTF-16 index produces a lone surrogate,
which becomes `U+FFFD` when the line is read back.

## Reading

A line that does not parse, or names an event this build does not know, is skipped and counted —
never fatal. A torn tail line is the normal consequence of a crash mid-append.

A missing file is an empty journal, not an error: a fresh install has no history, and refusing to
start because of that would make the first run impossible.
