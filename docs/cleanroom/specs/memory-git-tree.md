# Git tree and blob parsing — clean-room spec

Behaviour reference: `crates/omo/memory-core/src/git/repo_tree.rs`. Implemented from scratch in
`crates/maho-memory/src/git_tree.rs`.

These parsers answer two questions the store cannot answer any other way: which files exist in a
revision, and what each blob contains. A path dropped here is a memory reported as absent. An
over-read in `cat-file` splices one blob's bytes into another.

## `git ls-tree -r -l -z` → sized entries

Records are NUL-separated. Each record is `<mode> <type> <oid> <size>\t<path>`, split at the first
tab so a path containing spaces stays whole.

- Fewer than four metadata fields → skipped.
- Size `-`, or unparsable → skipped. Git prints `-` for submodule entries; there is no blob to size.
- Empty path → skipped.

## `git ls-tree -r -z` → blob entries

Same record shape with three metadata fields. Only `blob` records are kept; `tree` and `commit`
entries have no readable content for the store.

## `git cat-file --batch` → contents by oid

Framed as `<oid> <type> <size>\n<content>\n`. **Bodies are read by declared size, not by line or
NUL**, because memory Markdown contains newlines and may contain any byte.

Three error cases, each of which must fail rather than guess:

| case | outcome |
|------|---------|
| output ends inside a header | `TruncatedHeader` |
| header lacks a usable size | `MalformedHeader` |
| declared size runs past the output | `TruncatedBody` |

`<oid> missing` carries **no size field** and no body. It is consumed as a zero-byte record and
produces no entry — returning an error there would make one pruned object abort an entire history
read.

Non-blob objects are consumed (offset advances by their size) and skipped, so a `tree` record in the
stream does not corrupt the record after it.

`size` is parsed as `usize` and added with `checked_add`, so a hostile or corrupt size cannot wrap
into a small offset and read the wrong bytes.
