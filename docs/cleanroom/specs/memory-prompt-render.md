# Prompt renderer — clean-room spec

Behaviour reference: `crates/omo/memory-core/src/compile/render.rs`. Implemented in
`crates/maho-memory/src/prompt_render.rs`.

## What this owns

Turning memory files into the text an agent's system prompt carries, and the sentinel markers that let a later
run swap that text in place. Four independent pieces, all pure string work.

## 1. `render_system_tree` — files into nested tags

A `CompiledSystemFile` names a path relative to the memory root. The renderer strips a leading `system/` and a
trailing `.md`, splits the rest on `/`, and builds a tree: `system/human/prefs/coding.md` nests `human` > `prefs` >
`coding`. Children emit in byte order of their labels.

Each node emits, at two-space indent per depth:

```
<label>
  <projection>$MEMORY_DIR/system/<path>.md</projection>
  <description>...</description>
  <body>
</label>
```

`<description>` is omitted when blank after trimming; the body is omitted when blank after trimming trailing
whitespace. A node with both a file and children emits its own file first, then descends.

**Redaction applies to labels and paths, never to bodies.** A file whose name contains an API-key shape renders
its label and its projection path with the secret replaced by `***`, but the body is emitted verbatim. This is
deliberate: a memory body is the content the agent is supposed to read, while a path is metadata that ends up in
logs and transcripts. Tests pin both halves — `sk-live-ABC123` in a directory name becomes `notes-***`, the same
string in a body stays as written.

## 2. `render_external_projection` — a tree listing for paths outside `system/`

Wraps a directory tree in `<external_projection>`, with `$MEMORY_DIR/` on the second line. Entries are drawn with
box glyphs: `├── ` and `└── ` for branches, `│   ` and four spaces for continuation. **Directories sort before
files**, then by name. A directory gets a trailing `/`; only paths with children count as directories, so a single
path `reference/` is a leaf.

## 3. Sentinel blocks

`mark_memory_block(identity, block)` produces exactly:

```
<!-- senpi-memory:{identity}:begin -->
{block}
<!-- senpi-memory:{identity}:end -->
```

## 4. `replace_memory_block` — swap one identity's block

The identity is taken from the *new* block's own begin sentinel; a block that does not start with
`<!-- senpi-memory:<id>:begin -->` is rejected with `MissingBeginSentinel`. An identity must be non-empty and must
not contain `:`, `\r`, or `\n`, so a malformed marker cannot swallow the rest of the prompt.

- No begin marker for that identity in the prompt → the block is appended after `prompt.trim_end()` with one blank
  line between.
- Otherwise every region from that identity's begin marker through its end marker is replaced. Other identities'
  blocks are untouched.
- A begin marker with **no** matching end marker is left alone and the remainder is copied verbatim. Never delete
  to end-of-document: a truncated prompt must not lose the text after it.

## 5. `strip_memory_block` — remove all blocks

Removes every well-formed block, for any identity, and trims the result. An unterminated marker survives as
literal text, and a prompt with no markers is merely trimmed. `strip(mark(prompt, block))` therefore returns the
prompt unchanged.

## Divergence note

The pin memoizes normalized haystacks in a `WeakMap`; that is a pure optimization and is not ported. Nothing here
caches.
