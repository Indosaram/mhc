# Frontmatter entries — clean-room spec

Behaviour reference: `crates/omo/memory-core/src/memfs/frontmatter_entries.rs`. Implemented in
`crates/maho-memory/src/frontmatter_entries.rs`.

## Entry reading

A frontmatter block is one reading of its key lines, shared by every caller that needs to inspect keys:

- Split the file with `split_frontmatter`; the header is the text between the `---` fences.
- A line is an entry when it contains `:` at any index except the first.
- The key is the text before the first `:`, trimmed; the value is everything after it, trimmed.
- The original line is kept verbatim (minus a trailing `\r`), so callers can rewrite a line in place.

Lines without a colon, list items, and continuation lines produce no entry. A leading colon (`: value`) has no
key and is skipped rather than producing an empty key.

Values are kept as raw source text, not decoded — decoding belongs to `frontmatter::decode_scalar_source`.

## Duplicate keys

`duplicate_frontmatter_keys` reports each repeated key once, in the order its **second** occurrence appears, and
compares keys case-sensitively. `Description` and `description` are not duplicates by this measure; that is what
the look-alike check is for.

## Look-alike keys

`normalized_frontmatter_key` folds a key through `look_alike_skeleton` (NFKC, invisible marks removed, Cyrillic
and Greek confusables mapped to Latin, lowercased) and then strips surrounding quotes and whitespace.

`look_alike_frontmatter_keys(content, canonical)` returns every key that reads as `canonical` but is spelled
differently. Exact spelling is excluded. This is how a note that writes `descriptіon` with a Cyrillic `і` is
caught before it silently escapes a frontmatter contract check.

## Note on the confusable table

The skeleton mapping is the one in `lookalike_keys::look_alike_skeleton`. Not every Cyrillic letter is mapped —
only those that render indistinguishably from Latin in common fonts. `д` (U+0434) is not; `ԁ` (U+0501) is.
