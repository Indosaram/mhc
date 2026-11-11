# Support: errno codes — clean-room spec

Behaviour reference: `crates/omo/memory-core/src/support/errno.rs`. Implemented in
`crates/maho-memory/src/errno_code.rs`.

A memory write can fail for reasons whose remedies differ, and `std::io::ErrorKind` erases the difference:
`ENOSPC` (disk full) and `EDQUOT` (quota exceeded) both surface as `Other`. A lock record, a retry decision
and a user-facing message all need the distinction, so the POSIX errno name is what has to survive.

## Mapping

`io_error_code` returns the errno name when `raw_os_error()` names one it knows, else the `ErrorKind` text.

The set matches upstream exactly — `EIO`, `EACCES`, `EPERM`, `ENOENT`, `ENOTDIR`, `EISDIR`, `EEXIST`,
`EBUSY`, `EBADF`, `EINVAL`, `EROFS`, `ENOSPC`, `EDQUOT`, `EMFILE`, `ENFILE`, `EINTR`, `EAGAIN`, `ELOOP`,
`ENAMETOOLONG`. `ENOMEM` and `EFAULT` are deliberately **not** included: upstream does not map them, and a
wider table would report names for failures no caller distinguishes.

An unmapped number returns `None` and degrades to the `ErrorKind` text. Reporting an unknown number under a
name borrowed from another platform would be a confidently wrong diagnosis.

## No `libc`

Upstream reads the constants from `libc`. This crate is built with the workspace's
`unsafe_code = "forbid"`, and the constants are the only thing it would take `libc` for, so the numbers are
written out per target family.

The numbers are **not** portable, and that is the whole reason the module is grouped by target:

| errno | Linux | macOS |
| --- | --- | --- |
| `EAGAIN` | 11 | 35 |
| `ENAMETOOLONG` | 36 | 63 |
| `EDQUOT` | 122 | 69 |
| `ELOOP` | 40 | 62 |

A single table would silently mislabel every one of these on one of the two shipping platforms — `ENAMETOOLONG`
on macOS would be reported as Linux's `ENOSPC`. Windows and others use the Linux numbering here; Windows has
no POSIX errno, so `raw_os_error()` values there do not reach the table in practice.

## Tests that pin the distinction

The suite asserts the property the module exists for rather than the table: disk-full and quota produce
different codes, permission failures keep their own names, an unmapped number falls back, and a
kind-only error falls back. If the table were collapsed to one platform's numbers, the first of those
would still pass — the table tests would not — so the platform grouping is what the spec pins.
