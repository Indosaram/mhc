# People index — clean-room spec (reading cards from a memory repository)

Behaviour reference: `crates/omo/memory-core/src/facts/person_index.rs`.
Implemented in `crates/maho-memory/src/facts_person_index.rs`.

## The gap this closes

`resolve_person_slug` routes a claim to a person by matching names against a `KnownPerson` slice the caller
supplies. Nothing in this port built that slice from a repository, so every caller would have had to pass an
empty one — and an empty index does not fail. It resolves every claim to `None`, and the fact is dropped with no
error anywhere. A silently-empty routing table is worse than a missing one.

## Layout read

| Path | Contents |
| --- | --- |
| `system/human.md` | the person the agent works with; slug `human` |
| `people/<slug>/card.md` | everyone else, one directory per slug |

Cards are markdown with YAML frontmatter. Two keys matter: `description` is the display name, `aliases` is an
inline JSON array of other spellings. Every other key is ignored — this reader feeds routing, and routing only
compares names.

## Rules

**The human comes first**, then the remaining people sorted by slug. Sorting makes the output independent of the
filesystem's directory order, so two runs over one repository agree and a tie resolves the same way both times.

**A `people/human/` directory is not read twice.** `system/human.md` already owns that slug; reading both would
put one person in the index twice and make an alias tie against themselves.

**`Person - ` is stripped from the display name.** The agent writes its own cards that way. Leaving the prefix on
would make the name list depend on the slug's capitalisation and stop `Person - Indo` matching a claim of `Indo`.

**A description that is only the prefix, or only whitespace, is not a name.** The slug is used instead, so the
person stays addressable by their directory rather than dropping out of the index entirely.

**A card that is missing, unreadable, or has no `description` is skipped.** Half a card cannot route anything.
Reporting it would only add a path to a diagnostic list that no caller acts on.

## Aliases are extra names, not replacements

The display name is always in the name list, so a card's `description` alone is enough to route to it; `aliases`
only adds. A card with no aliases is still addressable by name.

## Interaction with the routing rules

The longest matching name wins, ties go to the smallest slug, and a tie is reported through the callback. This
reader does not duplicate those rules — it produces the index and hands it to `resolve_person_slug`, so the two
cannot disagree. `known_people` is the one conversion between the on-disk shape and routing's type.

A name that appears on two cards is genuinely ambiguous: which card wins is decided by slug order, not by
anything about the person. That is why the tie is reported — the corpus has two people answering to one name,
and only the caller can decide whether to fix it.

## Scope boundary

Reads three kinds of file and returns a list. No git, no lock, no journal, no write.
