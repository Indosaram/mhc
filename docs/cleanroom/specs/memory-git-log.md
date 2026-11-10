# Git log parsing — clean-room spec

Behaviour reference: `crates/omo/memory-core/src/git/repo_log.rs`. Implemented from scratch in
`crates/maho-memory/src/git_log.rs`.

Memory history is what the store shows when the user asks what changed. A commit parsed wrong is a
false statement about the past, so this parser fails closed: a truncated record is dropped rather
than reconstructed with empty fields.

## Format

The caller emits one record per commit with `--format=%x1e%H%x1f%s%x1f%b%x1f%an%x1f%ae%x1f%cI`:

| field | meaning |
|-------|---------|
| 0 | full hash |
| 1 | subject |
| 2 | body |
| 3 | author name |
| 4 | author email |
| 5 | committer date (RFC 3339) |

Records are split on `\x1e`; the leading empty chunk before the first record is dropped. A record
with fewer than six fields is a truncated record and yields no commit.

## Body normalization

`\r\n` becomes `\n` and trailing newlines are removed. Internal blank lines are preserved — they
separate prose from the trailer block.

## Trailers

Only a fixed set of keys is recognized:

`Agent-Id`, `Session-Id`, `Memory-Id`, `Operation`, `Reason`, `Generation`

Matched case-insensitively, stored as written. The whitelist is the point: treating every
`Word: value` line as a trailer invents metadata out of ordinary prose, so a body containing
`Note: this moved` would claim a `Note` trailer.

Git's own rule narrows it further: a trailer block is the **final** run of consecutive `Key: value`
lines. Parsing walks the body from the end and stops at the first line that is not a recognized
trailer, so a key/value line in the middle of prose is not a trailer. A key with no value is still
recorded (empty string).

## Paths

With `--name-only`, paths follow the date field, NUL-separated, with a leading `\n` on each from
git's output. A commit that touched no paths reports `Some([])`, distinct from `None`, which means
"paths were not requested".

## Known limitation

A subject or body containing a literal `\x1f` shifts the fields. This is inherent to a
separator-based format and is not defended against: git's own `%s` sanitization cannot emit that byte
from a real commit message.
