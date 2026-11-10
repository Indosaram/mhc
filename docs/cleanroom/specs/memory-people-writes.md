# People writes — clean-room spec

Behaviour reference: `crates/omo/memory-core/src/people/` and every `people/<slug>/` directory in a
live store. Implemented from scratch in `crates/maho-memory/src/people_store.rs`; the parse and
render halves live in `records.rs`.

A person directory holds exactly two files: `card.md` with `description`, `kind` and `aliases`
frontmatter, and `observations.md` with an optional `description` and three sections — `Explicit`,
`Inductive`, `Deductive`. Observed stores use that order and spell the section names exactly; the
writer accepts any casing and always writes the canonical spelling.

## Writes are append-only

A write adds one entry to one section of one person. Nothing existing is rewritten, reordered or
re-rendered beyond the entry itself, so a store that is read and written back unchanged comes out
byte for byte identical.

An entry that is already recorded for that person — in any section — is **not** appended a second
time. The write reports `Unchanged` and names the section where the existing entry lives, because a
duplicate entry would read as two separate observations of the same thing, which is a stronger
claim than the store supports.

## Provenance is never silently upgraded

Each entry keeps the provenance comment it was written with:
`<!-- src: <id>; pattern: <type>; confidence: low|medium|high -->`. Re-reading and re-writing never
drops a field and never raises `confidence`. An entry recorded as an inference stays an inference.

## Refusals

A write is refused, and nothing is written, when the person is unknown, the section is not one of
the three, the text is empty, or the dream policy does not admit it. The policy drops an over-long
entry **whole** rather than truncating it — a truncated observation would silently change what was
observed.

## Loading

`book_from_files` walks `people/<slug>/` only. `card.md` and `observations.md` are read; anything
else in the tree is ignored, and a person missing one of the two files loads with that half empty
rather than being dropped.
