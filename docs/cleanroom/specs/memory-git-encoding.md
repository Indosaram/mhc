# Dirty markdown encoding diagnostics — clean-room spec

Behaviour reference: `crates/omo/memory-core/src/git/porcelain.rs` and `git/repo_status.rs`.
Implemented in `crates/maho-memory/src/git_encoding.rs`.

When a commit is refused because the repository is dirty, the useful error is not "dirty" — it is *which file
is unreadable and why*. A memory that has been saved as UTF-16 is still a valid file; it is just not a memory
any reader can parse, so rewriting it would silently destroy what the author meant.

## Only markdown is inspected

A NUL byte in a binary memory artifact is the format, not a defect. Inspecting every dirty path would report
every legitimate binary file as broken.

## BOM before NUL

UTF-16 carries a NUL in the second byte of most code units, so a UTF-16 BOM test must come **before** the
NUL test. Reversed, every UTF-16 file would be reported as "possibly UTF-16" without ever naming the encoding
that actually decides how to repair it.

## A deleted file is not read

A `D` entry has no content to inspect. Reading the path anyway would either fail or, worse, read a same-named
file that is no longer the memory that entry describes. Deletion is `Change::Deleted` on either status column,
and `git_status` already models this — `is_readable_content()` is the gate.

## A renamed file is inspected at its new path

Porcelain renders a rename as `old -> new`. The old path no longer exists; the new one holds the bytes. The
parser already resolves this (`StatusEntry::path` is the destination, `from` the source).

## An unreadable file produces no diagnosis

If the file cannot be read, nothing is reported for it. A missing file is a status problem, not an encoding
one, and inventing a diagnosis would point the reader at the wrong repair.

## Unrelated changes are refused, and an unparseable status counts as unrelated

`assert_no_unrelated_changes` fails when the status names any path outside the expected set. The failure copies
the porcelain text verbatim so the caller can see what actually changed, together with the encoding
diagnostics for the dirty markdown among those paths.

A status that cannot be parsed is treated as *unrelated*, not as clean. The alternative — assuming cleanliness
when the parser balked — would let a commit proceed over changes nobody could read.

## Scope boundary

Parsing porcelain is `git_status`; this module only decides what to inspect and when to refuse. It runs no git
command (`get_status` is injected) and writes nothing.
