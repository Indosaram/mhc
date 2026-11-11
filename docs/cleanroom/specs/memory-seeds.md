# Memory seeds — clean-room spec

Behaviour reference: `crates/omo/memory-core/src/seeds/`. Implemented in `crates/maho-memory/src/seeds.rs`.

## What a fresh store contains

| Path | Purpose |
|---|---|
| `system/persona.md` | The agent's self-model; explains what every other file is for |
| `system/human.md` | The primary person's card (`kind: person`) |
| `system/boundaries.md` | What the person said not to do, in their words |
| `system/self-aware.md` | How the agent looks from outside, maintained by reflection |
| `reference/self/observations.md` | Append-only journal of external reactions |
| `skills/memory-discipline/SKILL.md` | Routes new knowledge to the right home |

`system/persona.md` is written first. It is the file that tells a future reader what the rest are for, so a
store whose persona is missing or unreadable is one where nothing else makes sense.

## Prose is verbatim

The seed bodies are upstream's text, not a rewrite. They are behavioural specification: the persona states the
commit-before-effect rule, `observations.md` states the entry format, and the skill states the routing table.
Paraphrasing any of them changes what a fresh store teaches the model, which is not a stylistic choice.

`DEFAULT_MEMORY_BLOCK_LABELS` (`persona`, `human`, `boundaries`, `self-aware`) names the blocks projected into
the system prompt, and a test ties each label to a seeded `system/<label>.md`.

## Rendering

Memory seeds are rendered through `render_memory_file`, so they get real frontmatter and parse back cleanly.
The skill is the exception: it carries its own hand-written header because the skill loader reads `SKILL.md` as
strict YAML with keys the memory renderer does not know about.

`render_memory_file` failure on a seed is a bug, not user input — the body is compiled in. It renders to an
empty string rather than panicking, so a broken seed cannot take down startup, and the "no seed body is empty"
test catches it in CI instead.
