# Git status parsing — clean-room spec

Behaviour reference: `crates/omo/memory-core/src/git/porcelain.rs` and
`crates/omo/memory-core/src/facts/recovery_ownership.rs::parse_porcelain_path`.
Implemented from scratch in `crates/maho-memory/src/git_status.rs`.

The memory store learns which Markdown files changed by reading `git status --porcelain`. Every path
this parser fails to decode is a file the store silently stops tracking, so decoding correctness is
the point of the module.

## Line shape

```
XY <path>
XY <from> -> <to>
```

Column 1 is the index state, column 2 the working-tree state, column 3 a space. `X` and `Y` are
`A` added, `M` modified, `D` deleted, `R` renamed, `C` copied, `?` untracked (both columns), `U`
unmerged; anything else is carried through as `Other` rather than rejected.

A blank line yields no entry. A trailing `\r` is stripped, so output produced on Windows parses.

## Quoted paths

Git quotes a path when it contains bytes outside printable ASCII, and escapes the bytes the way C
does: `\t`, `\n`, `\r`, `\a`, `\b`, `\f`, `\v`, `\\`, `\"`, and `\NNN` octal.

The octal escapes are **bytes**, not characters. They are collected into a byte buffer and only then
decoded as UTF-8. This is why a Korean file name — which arrives as three octal escapes per
character — must come back as one character per syllable. The upstream helper instead only strips
the surrounding quotes, so every non-ASCII memory path is left as a literal `"\355\225..."` string
and never matches a file on disk. That is the bug this module exists to not have.

Escapes this build does not implement (`\q`, a truncated `\35`) and an unterminated quote are hard
errors, not best-effort guesses: a wrong path is worse than a failed parse, because a wrong path
either misses the file or, if it happens to exist, attributes one memory's content to another.

## Renames and copies

Git writes `from -> to`. The destination is the reported `path`; the original is `from`. Only the
destination is read, because the original no longer exists and reading it would resurrect content
the user moved away.

The separator is only honoured **outside** quotes, so a path that legitimately contains ` -> ` is not
split.

## Failures

An unparsable line fails the whole parse. Skipping it would silently drop a changed path, and the
store would keep serving stale content for a memory the user just edited.
