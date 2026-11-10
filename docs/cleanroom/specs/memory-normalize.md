# Frontmatter normalization — clean-room spec

Behaviour reference: `crates/omo/memory-core/src/memfs/normalize.rs`. Implemented in
`crates/maho-memory/src/normalize.rs`.

A one-time, idempotent pass that repairs legacy frontmatter into the strict single-line YAML form. It
must never lose a memory, so a rewrite is only written after it re-parses to exactly the same memory.

## Decision order

`normalize_contents` decides in this order, and the order is the contract:

1. **Already strict** → `Unchanged`, without parsing. Most files take this path on every pass; parsing
   them would be wasted work.
2. **Unparseable** → `Skip("unparseable")`. A file with no header, a missing `description`, or a
   non-`key: value` line is not guessed at.
3. **`read_only: "true"`** → `Skip("read_only")`, before any rewrite is attempted. The owner forbade
   changes; normalization does not get an exception.
4. **Render fails** → `Skip("unparseable")`.
5. **Rendered text is still not strict, or re-parses differently** → `Skip("not_reproducible")`.
6. Otherwise → `Rewritten`.

Step 5 is what makes a normalization bug harmless: a renderer that drops an extra key or mangles a
description produces a mismatch and the file is left alone.

## Canonical form

Fixed key order: `description`, `read_only`, `kind`, `aliases`, then extra keys sorted. A fixed order is
what makes a rewrite a no-op for an already-normal file — otherwise two renders could differ by key order
alone and the pass would never converge.

Extras must be alphanumeric, `_`, or `-`, and may not be a contract key (`description`, `read_only`,
`kind`, `aliases`, `limit`). A file that tries to smuggle `kind` in as an extra is refused rather than
rendered twice.

## Values

`sanitize_frontmatter_value` flattens a run of line breaks to one space and trims. A frontmatter value is
one line by contract; a raw newline would either create a continuation line (which the strict grammar
rejects) or be silently truncated. Flattening keeps the text and drops only the break.

`read_only` is rendered verbatim as a raw scalar — it is a contract word (`true`), not a description —
while `description` and `kind` go through `render_string_scalar`.

## Body

The body is preserved byte for byte. An empty body renders exactly one trailing newline.
