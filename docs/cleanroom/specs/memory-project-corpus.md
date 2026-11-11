# Project-scoped recall corpus — clean-room spec

Behaviour reference: `crates/omo/memory-core/src/recall/project_corpus.rs`. Implemented in
`crates/maho-memory/src/project_corpus.rs`.

A child project keeps its own memory, and a session may recall from it. Those notes join the session's corpus
under a qualified path: `@project/<identity-id>/<path>`.

## Why namespace at all

Recall keys its **ledger** and **surfaced set** by path. Two identities that both own `reference/deploy.md` would
collide on a bare path — one note would be marked surfaced and the other silently skipped, and the ledger would
record a hit against the wrong project. Qualification gives every note one key that is unique per identity.

## Rules

**Qualify** by prefixing `@project/<identity>/`. Description and body are copied untouched; only the path
changes. Input order and length are preserved.

**Parse** back at the **first** slash after the identity. A note path may be nested (`notes/2026/deploy.md`), so
splitting anywhere later would attribute the note to an identity that does not exist.

**Reject** a path that is outside the namespace, has an empty identity (`@project//notes/a.md`), or has an empty
note path (`@project/child/`, `@project/child`).

**Stay a plain relative path.** The qualified form has no leading slash and no `..` segment, so every existing
memory path check applies to it unchanged. This is what lets a project note flow through code that was written
before project memory existed.

## Round trip

`parse(qualify(id, doc).path) == (id, doc.path)` for every document with a non-empty identity and path. That
equality is the whole contract; it is what makes a qualified path safe to store and read back later.
