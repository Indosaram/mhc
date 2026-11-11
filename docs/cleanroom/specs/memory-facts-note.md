# Facts note rendering — clean-room spec

Behaviour reference: `crates/omo/memory-core/src/facts/mutation_plan.rs` (`render_facts_note`,
`facts_records_hash`). Implemented in `crates/maho-memory/src/facts_note.rs`.

A monthly note (`notes/facts/YYYY-MM.md`) is the durable destination for extracted facts. Many runs write to
the same file, so rendering is a **pure function** of (existing file contents, new records). The applier
writes the bytes it planned and records their hashes in a recovery envelope; if a re-run produced different
bytes for the same input, those hashes would no longer describe the file and recovery would corrupt it.

## Path

`notes/facts/<first seven characters of the date>.md`. A malformed date still yields a path (`""` →
`notes/facts/.md`) rather than failing the batch, because one bad record must not block every good one.

## Appending

Existing facts are **never rewritten**. They may have been written by another session, and the note is
append-only by construction: the parsed body is carried through and new bullets follow it.

A separator newline is inserted only when the existing body does not already end at a line boundary.
Otherwise the first new bullet would join the last existing line and stop parsing as a fact.

## Frontmatter

- An existing file's frontmatter is authoritative, including `read_only` and any extra keys. Its description
  is never replaced with the generated one; repairing a description is a separate operation (see
  `memory-description-preflight.md`).
- A brand-new note gets `Explicit facts for <stem>` where `<stem>` is the file name without `.md`. It is
  derived from the path, not the date, so the two can never disagree.
- An empty description is refused at render time rather than written, because the description is what the
  memory index shows and an empty one hides the block.

## Fact lines

`- [<date>] <text>` — the text is carried verbatim, including any `(source: …)` provenance parenthetical.
Stripping or re-rendering provenance is the display layer's job (`fact_display_text`), not the note's: the
stored line must remain an honest record of what was written.

## Idempotence

Rendering the same input twice is byte-identical. This is the property the recovery envelope depends on, and
it is tested directly rather than inferred from the pieces.
