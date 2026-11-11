# Reflection completion validation — clean-room spec

Behaviour reference: `crates/omo/memory-core/src/reflection/completion_validation.rs` (the pure parts).
Implemented in `crates/maho-memory/src/reflection_completion.rs`.

The full upstream module validates a whole worktree against git — administrative-file snapshots, a clean tree,
secret scanning, frontmatter, and a repair loop. Only the two genuinely pure decisions are ported here; the git
half needs a worktree handle this crate does not have yet.

## `validate_dream_token_budget` — did a pressure dream relieve pressure?

Takes the run's `DreamOrigin`, the estimated `total_tokens` of the committed `system/` tree, and the
`target_tokens` the run was asked to get below.

- **Only `Pressure` is measured.** `Manual`, `Idle`, and `Shutdown` runs are `Valid` no matter how large the
  corpus is, because nobody asked them to shrink it. Reporting them as failures would punish a run for doing what
  it was launched to do.
- A pressure run is `Valid` only when `total_tokens < target_tokens` — strictly below. Landing exactly on the
  target is `BudgetNotMet`.
- The failure `detail` names both numbers, because the receipt is read by a human deciding whether to re-run.

## `frontmatter_block` — the header text, CRLF-normalized

Returns the frontmatter header of a document, or `None` when it has none.

Normalization (`\r\n` → `\n`, then any lone `\r` → `\n`) happens **before** splitting. This is the whole point of
the function: it is compared against the same path at a base revision, and a file whose only change is a line-ending
conversion must not look like a frontmatter change. Without normalizing first, a CRLF rewrite of an untouched header
would read as a violation and force a needless repair round.

The returned header excludes its trailing newline and the `---` fences. A body-only edit yields an identical block,
which is how the gate lets a legacy file's body be edited without re-litigating its (possibly invalid) frontmatter —
that is the normalizer's job, not the reflection's. An unterminated block is `None`.
