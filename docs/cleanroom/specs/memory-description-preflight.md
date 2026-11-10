# Description preflight — clean-room spec

Behaviour reference: `crates/omo/memory-core/src/reflection/description_preflight.rs`. Implemented in
`crates/maho-memory/src/description_preflight.rs`.

Only run on a **clean, finished** reflection branch, before the pure completion gate. Its single job is
the length defect: a reflection that is otherwise correct but has a 1,500-character description would
otherwise be discarded along with the whole run.

## Two different measures, on purpose

| Stage | Measure | Why |
|---|---|---|
| Gate (`frontmatter`) | `chars().count() > 1024` | matches the message `'description' exceeds 1024 characters (<n>)` |
| Trimmer budget | UTF-16 units ≤ 1023 | the original contract was a JS string cap, i.e. UTF-16 units |

They agree for ASCII and for every BMP script including Korean, and diverge only on astral characters
(emoji: one `char`, two units). Budgeting in units is the safe direction — every character is at least
one unit, so ≤1023 units is ≤1023 characters, plus the ellipsis, which keeps the result under the
character cap the gate enforces. A naive unit-for-character swap in the other direction would not.

## Shortening rule

1. Walk `char`s, stop before adding one that would exceed 1023 units. Never slice at a byte offset:
   that panics inside a multi-byte character.
2. If the character right after the prefix is a space, cut there. Otherwise cut at the last space in the
   prefix, so the text does not end mid-word.
3. Trim trailing spaces and append `…`.

No space anywhere (one long token) falls back to cutting at the prefix itself.

## Rewriting the header

`description: ` value replaced in the **first** `description:` line only.

- Every other byte survives: other keys, their order, and CRLF line endings. Re-rendering the header
  would also normalize values the gate never complained about, which turns a length repair into an
  unrelated rewrite of someone's memory file.
- A tab after the key counts as spacing and is preserved.
- `description:value` with no spacing is **not** a description line (it is not a header line under the
  strict grammar), so it is never rewritten.
- A header with no description line comes back unchanged.

## Failures are diagnostics, not failures

If the repo cannot be opened, the branch is dirty/tampered, or a rewrite fails part-way, the preflight
reports through the warn sink with both the error and any rollback error (both redacted), and returns an
empty list. The completion gate then decides the run's outcome. The warn sink itself is wrapped so a
panicking sink cannot prevent the gate from settling the run — the same reason upstream wraps the call in
`try`/`catch`.

A partial rewrite is rolled back to exactly the state the agent left the branch in, so a refused commit
never leaves half-shortened descriptions behind.
