# Facts commit message — clean-room spec

Behaviour reference: `crates/omo/memory-core/src/facts/recovery.rs` (`commit_message`). Implemented in
`crates/maho-memory/src/facts_commit.rs`.

Every facts mutation lands as one commit. Its message is the only durable record of why the fact exists and
what the writer had to change to store it, so the trailer spelling is a contract with every reader that greps
history — not a format choice.

## Shape

```
chore(facts): extract <N> fact|facts

Generated-By: facts-extractor
Omo-Writer: facts-extractor
Omo-Facts-Batch: <batch id>
[Omo-Provenance-Stripped: <path>]…
[Omo-Agreement-Evicted: <id>]…
```

- `fact` when `N == 1`, `facts` otherwise (including `0`).
- The three identity trailers are always present, in that order.
- Optional trailers come after the identity block, `stripped` before `evicted`, in the order given.
- **One trailer line per path or id.** A reader looks up a single path or id and must match it without parsing
  a comma-separated field; combining them would make every lookup a substring search that cannot distinguish
  `a.md` from `sub/a.md`.
- No trailer is emitted for an empty list, and the message never ends with a newline.

## Why these trailers

- `Omo-Facts-Batch` ties the commit back to the recovery envelope, so an interrupted mutation can be matched
  to the commit that finished it.
- `Omo-Provenance-Stripped` records that the writer removed a legacy user header from a file. That is a
  destructive edit to someone else's content, and history is where it stays auditable.
- `Omo-Agreement-Evicted` records an agreement that disappeared during the mutation, so a vanished rule can be
  traced to the commit that dropped it.
