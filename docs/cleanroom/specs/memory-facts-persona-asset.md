# Persona prompt assets — clean-room spec

Behaviour reference: `crates/omo/memory-core/src/personas/manifest.rs` and `facts/assets.rs`
(`load_facts_persona_from_dir`). Implemented in `crates/maho-memory/src/persona_assets.rs`.

Each background lane (reflection, dream, facts, kibitzer) is driven by a markdown persona prompt. A build must
work with no assets directory, and an operator must be able to replace one prompt without rebuilding.

## Built-in prompts

Compiled in with `include_str!`, so a missing or unreadable assets directory cannot leave a lane with an empty
prompt. The content is the upstream persona file itself, not a paraphrase: the prompt is behavioural
specification for the model, and a rewritten one would silently change what gets extracted.

## Override lookup

`load_persona(kind, assets_dir)`:

- No assets directory: the built-in prompt.
- A file named by `kind` absent from that directory: the built-in prompt. This is the normal case, and it is
  not an error.
- The file present: its contents, verbatim.
- The file present but unreadable (a directory, a permission error): **an error**. Falling back to the built-in
  would run the lane on a prompt the operator believed they had replaced.

## Cache

Content is cached per **full path** in a process-wide map.

- Path-keyed, not kind-keyed: two identities pointed at different assets directories must not serve each
  other's prompts.
- Content is inserted on first read and never replaced, because the file does not change while a process runs.
  That also keeps the common case — many runs, one assets directory — to a single directory read.
- A missing file is never cached as an empty string; the built-in is returned each time. Caching the absence
  would turn a later file write into a silent no-op for the rest of the process.

## Filenames

`reflection-persona.md`, `dream-persona.md`, `facts-persona.md`, `kibitzer-persona.md` — the names upstream
uses, pinned by a test so a rename cannot make every override silently miss.
