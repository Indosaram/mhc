# Memory prompt section — clean-room spec

Behaviour reference: `crates/omo/memory-core/src/` prompt projection and the live projection in
this session's own `<self>` block. Implemented from scratch in `crates/maho-memory/src/prompt.rs`.

The memory section is what the agent sees of its store at the start of a turn. It is assembled from
four parts, in this priority order:

1. **Slots** — the five `system/` blocks whose bodies are injected whole, in the fixed order
   `persona`, `identity`, `human`, `boundaries`, `self-aware`. A slot missing from that list sorts
   after all five, by name, so output is stable.
2. **Index** — an `<external_projection>` block listing the paths of blocks that are only named,
   never injected. Directory groups are printed as their name with a file count.
3. **Facts** — the newest facts, as `- [date] text` bullets, without their provenance.
4. **People** — one line per person: the card's name plus its aliases, or the directory slug when
   the card names nobody.

## Budget

The section has a character budget (8,000 by default), a fact cap (12) and a people cap (8). Parts
are appended while they fit. A part that would exceed the budget is **dropped whole** — a slot, a
fact list, or a person list is never cut mid-entry, because a half-written memory reads as a false
statement rather than a missing one. Dropping one part does not stop later, cheaper parts from
fitting.

`assemble` reports which parts were included and which were dropped with a reason, so a caller can
warn or adjust the budget instead of silently losing material.

## Determinism

The same store renders the same section: slot order is fixed, and facts and people are taken in the
order they were given.
