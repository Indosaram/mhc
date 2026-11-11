# Facts person routing — clean-room spec

Behaviour reference: `crates/omo/memory-core/src/facts/person_routing.rs`. Implemented in
`crates/maho-memory/src/facts_person_routing.rs` (slug resolution and observation normalisation; file
rendering lives in the people store).

Two ids can name the same person, and one person answers to many names. Routing must collapse the first and
preserve the second.

## Name folding

Names are compared after **NFKC + lowercase + trim**.

NFKC comes first because the same name arrives composed or decomposed depending on the input method
(`Jose\u{0301}` vs `José`). Comparing raw bytes would file one person twice. Folding happens on both sides —
the index's names and the claim's name and aliases — so the comparison is symmetric.

## Resolution

A claim resolves to a slug when any of its names matches any name of a known person. Among all matches:

1. **The longest matching name wins.** A short alias is usually contained in a long one: `Indo` matches
   half the address book, `Indosaram` only its owner. Length is measured in **UTF-16 units**, not `chars()`,
   because the index being reproduced came from a JS string where an astral character costs two units.
2. **Ties go to the smallest slug** (byte order). This is what makes the decision deterministic: the same
   input always produces the same target, so re-running extraction never migrates a fact between two people.

The weighting is per **person**, and the person's longest matching name counts. A person with both `Indo`
and `Indosaram` is weighted by `Indosaram` when that is what matched.

When several slugs tie at the winning length, an optional callback receives the full winner list and the
chosen slug. The callback can only observe — the decision is already made — so a collision can be surfaced
for a human without making routing depend on how many listeners are attached.

No match means `None`: the caller creates a new person rather than guessing.

## Self-reference

`user`, `me`, `you`, `i`, `the user`, `human` always denote the primary human. Checked **after** folding,
so `User`, `YOU`, and `the user` are the same reference.

## Observation normalisation

Used to decide whether two observations are the same fact. Applies: NFKC, lowercase, collapse internal
whitespace, drop a **trailing** HTML comment, drop trailing punctuation.

- The trailing comment is dropped because captured observations frequently end with one, and the same fact
  must not become two entries depending on whether the capture kept it.
- Only *trailing* comments are dropped. A comment whose first `-->` is not at the end of the string is
  interior text and stays, matching the lazy-regex behaviour this replaces: `a <!-- b --> c -->` has no
  trailing comment.
- Punctuation is dropped because sentence-final style varies per capture. Inner punctuation stays, so
  `lives in Seoul, Korea` keeps its comma.

The result is idempotent: normalising an already-normalised string changes nothing.
