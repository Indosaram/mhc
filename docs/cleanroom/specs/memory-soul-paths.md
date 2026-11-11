# Soul paths — clean-room spec

Behaviour reference: `crates/omo/memory-core/src/soul/paths.rs`. Implemented in
`crates/maho-memory/src/soul_paths.rs`.

## What a soul file is

| Path | Why it counts |
|---|---|
| `system/persona.md` | Who the agent is |
| `system/identity.md` | Its name, creature, vibe, sigil |
| `system/boundaries.md` | What the person said not to do |

An edit to one of these is not an ordinary memory write. It changes the agent's identity, so it carries the
`soul edit` token and the reply must say so. The person should hear it from the agent, not discover it in a
diff.

`system/human.md` and `system/self-aware.md` are deliberately **not** here: they record what was learned about
the person and about the agent's own behaviour, which is memory work, not a change of self.

## Exact match, not suffix

`touches_soul_path` compares against the canonical paths exactly.

A suffix or basename comparison would treat `notes/system/persona.md` — a note that happens to be named
persona — as the persona itself, and then require an announcement for writing an ordinary note. It would also
let a path like `people/x/system/boundaries.md` silently qualify. The three names are only meaningful at the
top level of a memory store, so that is where they are matched.

## Shipped copy

`MEMORY_SOUL_EDIT_RESULT_TOKEN` (`soul edit`) is machine-consumed and must stay stable. `SOUL_EDIT_RESULT_LINE`
is the model-facing instruction appended to the tool result.
