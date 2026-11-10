# Memory frontmatter — clean-room spec

Behaviour reference: `crates/omo/memory-core/src/memfs/frontmatter*.rs`. Implemented from scratch in
`crates/maho-memory/src/frontmatter.rs`.

Memory files are Markdown with a `---` header. The header is read through a **strict single-line YAML
subset**, not a general YAML parser. The subset exists so a description can never carry a second line,
a nested mapping, or a block scalar that the prompt index would render differently from what the
writer meant.

## Splitting

`split_frontmatter` accepts `---\n` or `---\r\n` at the start and closes on a line that is exactly
`---` (after stripping `\r`/`\n`). The header excludes that closing line. `None` means no well-formed
header, which is distinct from an empty header (`---\n---\n` yields `("", body)`).

A body that happens to contain `---` is not re-split: only the first closing marker counts.

## Keys

`^([A-Za-z0-9_-]+):(?:[ \t]+(.*?))?[ \t]*$` — alphanumeric, `_`, `-`. A colon with no following space
is not a key separator.

## Values

| form | rule |
|------|------|
| JSON double-quoted | decoded with JSON escapes; a raw control character or bad escape fails |
| YAML single-quoted | decoded, `''` unescapes to `'` |
| JSON string array | used by `aliases` |
| bare `>` or `|` | rejected: block scalars have no single-line meaning |
| plain | accepted only when `is_plain_safe_scalar` |

`is_plain_safe_scalar` rejects exactly the forms a YAML reader reinterprets:

- leading indicators `- ? : , [ ] { } # & * ! | > ' " % @ \``
- `: `, a trailing `:`, or ` #` inside the text
- surrounding whitespace, control characters, tabs
- core-schema words (`true`, `false`, `null`, `~` and their case variants)
- numeric forms (`1`, `-2`, `1.5`, `.5`, `1e3`, `0x10`, `0o7`, `.inf`, `.nan`)

`description` and `kind` are held to the plain-scalar rule even when another key would be allowed to
carry `true` or a number: they are written by the model and surface in the prompt index, so a
description that a YAML reader turns into a boolean is a lost memory.

## Header grammar

Rejected: indented continuation lines, `#` comment lines, lines that are not `key: value`, duplicate
keys, a missing `description`, an empty/whitespace-only `description`, and a multi-line description.

## Description contract

Applied on top of the grammar: non-empty, single line, at most 1024 characters **counted as
characters** (so a 1024-character Korean description passes), and free of tool-call scaffolding.

`find_tool_call_scaffolding` matches `<\/?(?:description|parameter)\b[^>]*>?` case-insensitively. It
catches a malformed tool call whose arguments split across description and body — the leftover tag is
not a memory, and letting it through writes `</parameter>` into a user's notes.

Two checks exist separately because they guard different things: the pure renderer/normalizer
guarantees the grammar, while model output additionally has to satisfy the description contract. A
test pins the distinction with scaffolding, the one case where grammar alone passes.

## Rendering

`render_string_scalar` writes plain when safe and JSON-quoted otherwise; `render_document` writes
`---`, one `key: rendered` per line, `---`, then the body. Every rendered value decodes back to
itself through `decode_scalar_source`, which is asserted directly.
