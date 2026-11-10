# Secret scan records — clean-room spec

Behaviour reference: `crates/omo/memory-core/src/reflection/completion_secret_scan.rs`. Implemented in
`crates/maho-memory/src/secret_scan_records.rs`.

The last gate before a reflection branch merges `--no-ff` into the memory repo screens every file
version the run introduces, not only the tip, because the merge carries the whole branch history. These
parsers are the only way that gate sees a file.

## `git diff-tree --raw -z`

Record: `:<src_mode> <dst_mode> <src_sha> <dst_sha> <status>\0<path>\0`.

The NUL terminator is the contract: a path may contain spaces **and newlines**, so splitting on
whitespace or lines would attribute one file's content to another. A path containing a newline is
tested directly.

Records with an empty path or fewer than five header fields are skipped, not errored: `--diff-filter=AMT`
can still leave framing for a pair the filter removed.

## Which versions are screened

- `100644`, `100755`, `120000`: content is read. A symlink target is a blob and can carry an evaded
  credential, so `120000` is included.
- `160000` (gitlink): refused on its file type alone, without reading anything — a submodule's content is
  not in this repository and cannot be screened.
- A zero destination sha is a deletion, not an introduction.

## `git cat-file --batch`

Record: `<oid> <type> <size>\n`, then exactly `size` bytes, then one newline.

The walk stays on **bytes**. Decoding to text first would let a multi-byte sequence inside a blob shift
every following record, so a credential in the second file could be missed — the test builds exactly
that case with a Korean body followed by another record. A non-UTF-8 body is still returned; the caller
decides how to screen it (the upstream screens latin1 so a binary cannot smuggle an ASCII credential).

Errors are returned rather than truncated: a body shorter than its declared size, or a header with no
size field, means the framing is broken and the scan cannot claim to have seen the file.

An `missing <type>` header (a pruned object) is skipped and the walk continues with the next record.
