# Memory paths — clean-room spec

Behaviour reference: `crates/omo/memory-core/src/memfs/paths.rs`. Implemented in
`crates/maho-memory/src/paths.rs`.

Every memory write resolves to a path under the memory root, or it fails. The check is not lexical
alone: the first **existing** ancestor is canonicalized, because `..` inside a symlink target is
invisible to string comparison, and a symlinked ancestor redirects a write outside the store while the
relative path still looks confined.

## Two modes

| mode | caller | accepts |
|------|--------|---------|
| tool path (`tool_path: true`) | memory tool block label | `.md` only, extension stripped, normalized back to `.md` |
| repository path (`false`) | anything git tracks | any extension, directories |

`system/contacts`, `system/contacts.md`, and `memory/system/contacts` all resolve to
`<root>/system/contacts.md`. Repeated separators collapse; backslashes read as separators.

## Refused

- empty or whitespace-only input; any NUL byte
- `~/…` and `$HOME/…`: a memory path is never home-relative
- an absolute path outside the root (POSIX or `C:\…`), reported with the "can only be used to modify
  files in {root}" message
- a leading `/` after trimming — `system/x` is a label, not a filesystem root path
- `.` and `..` segments, in any position
- `.git` as a segment, case-insensitively: the store is a git repository and its administration
  directory is not content
- a non-`.md` extension in tool mode, including `.MD` — the suffix check is exact, so a case-preserving
  filesystem cannot produce two blocks that differ only in extension case
- a symlinked ancestor whose real location is outside the root

A symlink that stays inside the root is allowed; a missing leaf under a confined ancestor resolves
normally (creating new blocks is the common case).

## Content projection

`is_memory_content_path` decides what the prompt index may project: `system/`, `reference/`, and
`people/` prefixes ending in `.md`, plus `skills/<name>/SKILL.md` with a non-empty name. Line
separators (`\n`, `\r`, U+2028, U+2029) disqualify the path outright — a newline in a label is how a
rendered index line becomes two.
