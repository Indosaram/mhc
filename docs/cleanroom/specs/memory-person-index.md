# Person index — clean-room spec

Behaviour reference: `crates/omo/memory-core/src/facts/person_index.rs`. Implemented in
`crates/maho-memory/src/person_index.rs`; the entries it produces are consumed by
`memory-facts-person-routing.md`.

The index is what routing matches against, so it must contain exactly the names people are actually called.

## Layout

- The primary human: `system/human.md`, slug `human`.
- Everyone else: `people/<slug>/card.md`, one directory per person.

`people/human/` is **never** indexed. Two entries for one person would make every one of their names a tie
and hand the tie-break a coin flip between two entries for the same human.

## Read on every run

No caching. A card edited between runs must be honoured; a stale index silently files new facts under the
wrong person, and nothing in the store would reveal it.

## Display names

The description is `Person - <name>`; the prefix is generator boilerplate, not anyone's name. Leaving it in
would enter the index as a name nobody answers to. Only the exact prefix is stripped:
`Personable` and `Persona` keep their names.

When the stripped name is empty, the fallback is the slug (`human` for the primary).

## Names

Display name first, then every alias, de-duplicated in order. This is the list routing weights; order is
observable only through de-duplication, since routing compares lengths.

## Skipped people

A person whose card is **missing, unparseable, or has no `description`** is skipped, never indexed under a
guessed name. Routing then reports "no match" and the caller creates a new person, which is honest; indexing
a guessed name would silently capture someone else's facts.

## Tolerance

Card parsing is deliberately more tolerant than the strict memory-file parser:

- unknown keys are ignored (hand-written cards carry them);
- `aliases` accepts a JSON array **or** a plain `[a, b]` list, because hand-written cards use the latter and
  dropping those aliases would lose the person's other names;
- an aliases value that parses as neither drops the aliases but keeps the person.

A quoted array string (`aliases: "[\"A\"]"`) is **not** aliases: the owner wrote a string where a list
belongs, and guessing at its contents would invent names.

## Ordering

People are indexed in **slug order**, not directory order, so the index — and therefore the tie-break among
equal-length names in routing — does not depend on filesystem iteration order.
