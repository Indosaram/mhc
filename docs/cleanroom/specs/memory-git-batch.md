# Git batch blob framing — clean-room spec

Behaviour reference: `crates/omo/memory-core/src/reflection/completion_secret_scan.rs`. Implemented in
`crates/maho-memory/src/git_batch.rs`.

## Why this exists

Before a reflection branch is merged `--no-ff` into the memory repository, every file version the branch
introduces is screened for credentials. Blob bodies are fetched in one `git cat-file --batch` call, and that
output has to be framed exactly. Get the framing wrong and every record after the first misread one shifts: the
gate then reports "no secrets" for content it never actually looked at.

## Wire format

For each requested oid, git writes:

```
<oid> SP <type> SP <size> LF <size bytes> LF
```

`<size>` is the length in bytes of the body, and it is authoritative. A body may contain newlines, NULs, or text
that looks like a header, so scanning for `\n` is not a substitute.

`git cat-file --batch` also answers missing oids with `<oid> missing` and has no size. That record carries no
body; the parser steps past it. Anything that cannot be framed — a header with no trailing newline, a
non-numeric size, an empty oid, or a body running past the end of output — is an error. A partial map is never
returned, because callers treat "absent from the map" as "clean".

## Decoding

Bodies decode as UTF-8 when valid, otherwise Latin-1 (one character per byte). Latin-1 is chosen because it keeps
every byte distinct: a binary that fails UTF-8 decoding still exposes any embedded ASCII credential to the
scanner instead of being dropped or replaced with `U+FFFD`.

## Blob modes

`100644`, `100755`, and `120000` carry content and are scanned. `160000` is a gitlink: it has no blob in this
repository and is refused on its file type alone. `040000` is a tree and is never scanned.

## Helpers

- `is_zero_sha` — a non-empty all-`0` oid means "deleted on this side" in a raw diff record.
- `short_sha` — first seven characters, used only for refusal text.
