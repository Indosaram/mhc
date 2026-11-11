# English stemmer — clean-room spec

Behaviour reference: `crates/omo/memory-core/src/recall/english_stem.rs`. Implemented in
`crates/maho-memory/src/english_stem.rs`.

## Scope

A deliberately light suffix folder for the recall BM25 index — not a full Porter stemmer. It exists so that
`memories` and `memory`, or `recording` and `record`, land on the same index term. Both indexing and querying must
apply it, or the fold helps nothing.

## Preconditions

A token is folded only when it is longer than 3 characters **and** every byte is a lowercase ASCII letter.
`Memory`, `memo-ry`, and Hangul pass through untouched. This keeps the folder from touching identifiers, paths,
or non-English text.

## Passes, in order

1. **Plural**
   - `sses` → `ss` (`classes` → `class`)
   - `ies` with more than 4 characters → `y` (`memories` → `memory`)
   - `es` after `ch`, `sh`, `x`, `z`, or `ss` → drop `es` (`batches` → `batch`)
   - trailing `s` unless the word ends in `ss`, `us`, or `is` (`notes` → `note`, `status` unchanged)
2. **Verb ending** — `ing`, else `ed`, stripped only if the remaining stem is at least 3 bytes and holds a vowel.
   `bring` and `being` keep their suffix.
3. **Doubled consonant** — a leftover doubled final consonant collapses (`running` → `run`), except `l`, `s`,
   `z` (`calling` → `call`, `passing` → `pass`).
4. **Derivational** — `ment` when the word is at least 8 bytes, else `ly`, else `tion`/`sion` when at least 6
   bytes.
5. **Trailing `e`** — dropped when the word is still longer than 3 characters.

## Consequences worth knowing

The passes compound, so the output is not always a real word: `notes` → `not`, `agreement` → `agre`,
`decision` → `decis`. That is acceptable and intended — the fold only has to be *consistent*, because both the
document and the query go through it. Do not "fix" a stem to look like English; that would break the pairing.

Folding is idempotent in practice for already-folded words, and a word and its plural always meet at the same
term.
