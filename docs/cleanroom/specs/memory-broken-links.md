# Broken links — clean-room spec

Behaviour reference: `crates/omo/memory-core/src/audit/audit.rs` (the `dangling_links` half). Implemented in
`crates/maho-memory/src/broken_links.rs`.

A memory store is a wiki: notes point at each other with `[[name]]`, and a rename that misses one referrer
leaves a link that resolves to nothing. Nothing fails at write time — the note just reads as if the target
still exists.

This module covers link **extraction and resolution**. Walking the corpus, reading the files, and assembling
the report need I/O and stay at the boundary.

## What is not a link

- **Fenced code blocks.** A ```` ``` ```` or `~~~` fence, opened by a run of three or more at indent ≤ 3,
  closes only on the same character with a run at least as long and nothing but space after it. A fence of
  four backticks therefore contains a three-backtick fence rather than ending at it. Notes that document
  markdown contain example links; reporting them would make the check unusable.
- **Inline code spans.** `` `[[name]]` `` is literal text. A span opened with *n* backticks closes on the next
  run of exactly *n*. An unmatched backtick is a literal character in markdown, so the rest of the line stays
  text and a link after it is still checked.
- **Images.** `![alt](path)` is an asset, not a note reference.
- **Empty targets and remote schemes.** `#anchor` alone, `https:`, `http:`, `mailto:`.

## Resolution

- A **wiki link** resolves against the corpus root, and also resolves when `<target>.md` exists: `[[name]]`
  addresses a note, which is stored as `name.md`.
- An **inline link** resolves against the directory of the file that contains it.
- `#anchor` is stripped before resolving.
- `.` and `..` segments are normalized. `..` at the root is preserved, not clamped.
- A link is satisfied by an exact file, or by a directory that contains anything.

## Escaping

A target that starts with `/`, or that normalizes to `..` or `../…`, is reported with `escapes: true` rather
than as an ordinary dangling link. It is a different mistake: not a rename that missed a referrer, but a note
pointing outside the store. Also, resolving it against an unbounded parent would silently read paths the store
does not own.
