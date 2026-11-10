# Ledger — clean-room spec

Behaviour reference: the recall ledger in `crates/omo/memory-core/src/recall/ledger.rs`.
Implemented from scratch in `crates/maho-memory/src/ledger.rs`.

Recall never repeats itself inside a session. Each session keeps one small JSON file listing the
paths already shown, so the next turn's candidate set can exclude them
(see [memory-select](memory-select.md)).

## File

```
<dir>/<sanitized session id>.json
```

```json
{
  "version": 1,
  "surfaced": {
    "notes/facts/2026-11.md": { "hash": "<sha256 hex>", "at": "<rfc3339>" }
  }
}
```

`surfaced` is a map keyed by path, so the rendered file is byte-identical for the same set of
entries regardless of insertion order.

## Session id sanitization

The session id becomes a file name component, so it is restricted to ASCII alphanumerics, `.`, `_`
and `-`. Everything else collapses to a **single** dash (runs never produce repeated dashes), and an
unsafe character at the very start produces nothing.

Trailing dots and dashes are stripped. Windows silently drops trailing dots from file names, so
keeping them would let `abc` and `abc...` resolve to the same file — that is a collision between two
different sessions, which would suppress recalls the second session never saw. Leading dots are
**kept**, so `.hidden` and `hidden` stay distinct.

A name that ends up empty, `.` or `..` falls back to `session`. The result is truncated to 80 chars
and trimming is applied again afterwards, so truncation cannot leave a trailing dot or dash.

## Content identity

A path's hash is the hex SHA-256 of its content at the moment it was surfaced. A path whose content
has since changed is **not** considered surfaced: the memory was edited, so it carries information
the session has not seen.

`mark_surfaced` keeps one row per path — re-surfacing replaces the hash and timestamp rather than
appending a second row.

## Reading

A ledger whose `version` this build does not know, or whose JSON does not parse, reads as **empty**
rather than failing. Recall degrading to "nothing was surfaced yet" re-shows memories that the user
already saw, which is a mild annoyance; refusing to load the ledger would break recall entirely.
