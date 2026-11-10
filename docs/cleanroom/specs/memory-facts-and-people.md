# Memory facts and people records — behaviour spec (MIT clean room)

Phase 1, item 7, second slice of the memory clean room. Companion specs:
`memory-format.md` (store layout, importer input) and `memory-agreements.md` (agreements and
prompt projection). This one covers the two remaining Markdown record kinds the legacy store keeps
on disk: dated **facts** and **people** records.

Written from observed data (`~/.omo/memory/agents/*/repo`, 269 identity repos) and from the
runtime's own injected memory preamble. No SUL source text is reproduced.

## 1. Fact entries

Facts live in `notes/facts/<YYYY-MM>.md`, one file per month, in the form:

```
---
description: Explicit facts for 2026-10
---
- [2026-10-18] <fact> (source: <provenance>)
- [2026-10-06] <fact> (source: <provenance>)
```

| Part | Shape | Meaning |
| --- | --- | --- |
| frontmatter | `description:` only | the block description; required for projection |
| date | `[YYYY-MM-DD]` at line start | when the fact was learned, not when it was written |
| text | free text, one line | the fact itself |
| provenance | trailing `(source: …)` | who asserted it; free text, may carry a caveat |

Rules:

- **Month buckets, not one file.** A new fact goes into the file for its own month. A file whose
  name no longer matches its entries is not rewritten retroactively.
- **Newest first.** Observed files list the most recent date first; a writer inserts at the top of
  the list, under the frontmatter.
- **Provenance is part of the record.** `(source: agent suggestion, not a user decision)` is not
  noise: it marks a fact the person never confirmed. A reader must not upgrade such a fact to a
  user decision, and a writer must never drop the trailing parenthetical when re-rendering.
- **Dates are unique per entry but repeat across entries**; ordering is by date descending, not by
  insertion time.
- A malformed line (no leading `- [date]`) is kept verbatim in a trailing bucket, never discarded.

## 2. People records

Each person gets a directory `people/<slug>/` with exactly two files:

### `card.md`

```
---
description: Person - <Name>
kind: person
aliases: ["<Alias>", ...]
---
```

- `kind: person` marks the directory as a person rather than a topic.
- `aliases` is a JSON array of alternate names; `[]` when there are none.
- The card body is free text and may be empty (all observed cards are frontmatter-only).

### `observations.md`

```
---
description: Observations - <Name>
---
## Explicit

- [YYYY-MM-DD] <observation> <!-- src: <session-or-id>; pattern: <type>; confidence: <level> -->
```

| Part | Shape | Meaning |
| --- | --- | --- |
| section heading | `## Explicit` / `## Inductive` / `## Deductive` | how the observation was obtained |
| date | `[YYYY-MM-DD]` | when observed |
| text | free text | the observation |
| `src` | session id or short hash | where it came from; may be truncated (`01a0c327`) |
| `pattern` | `preference`, `build-placement`, … | open vocabulary; unknown values are preserved |
| `confidence` | `low` / `medium` / `high` | open vocabulary; preserved |

Rules:

- **Three sections, fixed order**: `Explicit` (stated by the person), `Inductive` (generalised from
  behaviour), `Deductive` (derived by reasoning). A section may be absent.
- An entry belongs to exactly one section and never moves between them on rewrite.
- The provenance comment is optional on old entries; when present it is preserved byte-for-byte.
- The slug is a normalised name (`indosaram`, `indo`, `윤인도` all occur). Two slugs may point at the
  same human; nothing merges them automatically.

## 3. What the MIT memory crate must provide

1. **Parse** `notes/facts/<YYYY-MM>.md` into dated entries with provenance retained, preserving
   unknown provenance text exactly.
2. **Parse** `people/<slug>/card.md` and `observations.md`, keeping `kind`, `aliases`, the three
   sections, and each entry's `src`/`pattern`/`confidence`.
3. **Round-trip**: re-rendering an untouched block reproduces it byte-for-byte, including the
   trailing `(source: …)` and `<!-- src: … -->` comments.
4. **Never upgrade provenance**: an entry whose provenance says it is not a user decision must stay
   labelled that way through any import or re-render.

Out of scope for this slice: reflection/dream runs (they *write* these records), kibitzer, recall
ranking (already in `recall.rs`).

## 4. Divergence notes

- `people/<slug>/observations.md` uses `<!-- src: … -->`; `agreements.md` uses
  `<!-- agreement-event: … -->`. Both are HTML comments after the entry, so one line-oriented
  parser shape serves both, but the metadata is not interchangeable.
- One store used the slug `human` alongside `indosaram`; the clean room must not assume the primary
  human has a fixed slug.
