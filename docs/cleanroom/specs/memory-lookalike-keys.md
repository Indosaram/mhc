# Look-alike frontmatter keys — clean-room spec

Behaviour reference: `crates/omo/memory-core/src/memfs/lookalike_text.rs` and
`memfs/frontmatter_entries.rs`. Implemented in `crates/maho-memory/src/lookalike_keys.rs`.

## The problem

A frontmatter key is compared byte-for-byte by every parser. `рrovenance`, with a Cyrillic `р` (U+0440), is a
different key from `provenance`. Nothing warns: the writer treats it as an ordinary extra key and appends it.

The result is a file with two fields the reader sees as one. A person reviewing the diff sees
`provenance:` twice and either misses it or assumes the renderer is at fault. The agent sees the real protected
key absent, and an unprotected extra key present.

The same trick with a zero-width space (`provenance\u{200b}`) or a combining grapheme joiner
(`provenance\u{034f}`) is invisible in a diff entirely.

## Skeleton

`look_alike_skeleton` reduces a key to how a person would read it:

1. **NFKC.** Compatibility forms fold: `ｐrovenance` (fullwidth `ｐ`) becomes `provenance`.
2. **Invisible characters removed.** Nonspacing marks, format characters, and the zero-width fillers
   U+115F/U+1160/U+180E/U+3164/U+FFA0/U+FFF0–FFF8/U+E0000–E0FFF. These render as nothing.
3. **Confusable letters mapped.** Cyrillic and Greek letters that are pixel-identical to Latin ones map to the
   Latin letter: `а`→`a`, `е`→`e`, `о`→`o`, `р`→`p`, `с`→`c`, `у`→`y`, `х`→`x`, `і`→`i`, and the uppercase
   forms. The table is fixed rather than computed from Unicode confusables data: it covers the letters that
   actually appear in Latin text and keeps the dependency surface at two crates.
4. **Case folded.**

Deliberately **not** done: stripping all non-ASCII. A key legitimately written in another script must not be
reported, and only the specific confusable letters are impersonation.

## Detection

- `normalized_key` = skeleton, with surrounding quotes and space trimmed. Quotes matter because YAML accepts
  `"provenance": value`; the quotes are syntax, not part of the key.
- `look_alike_keys(keys, canonical)` reports keys whose skeleton equals `canonical` while the bytes differ. An
  exact match is never reported — that is the real key.
- `impersonated_protected_key` returns the protected key a spelling impersonates, if any.

## Protected keys

`description`, `read_only`, `kind`, `aliases`, `limit`, `provenance`, `expires`.

These are the keys whose meaning the store enforces: `read_only` gates edits, `provenance` carries the claim
receipt, `expires` controls projection. A look-alike of an *ordinary* extra key is not an attack — it changes
nothing — so the protected set is what gets checked, not every key.
