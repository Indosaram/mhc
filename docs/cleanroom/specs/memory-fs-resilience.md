# Filesystem resilience — clean-room spec

Behaviour reference: `crates/omo/memory-core/src/fs/`. Implemented in `crates/maho-memory/src/fs_resilience.rs`.

Memory writes are the one thing in this crate that must not be lost to a spurious failure. Two failure modes
are worth handling, and both are easy to get subtly wrong.

## `EINTR`

A syscall interrupted by a signal returns `Interrupted` having done nothing. Retrying is correct — **for
operations that are safe to repeat**.

The cap is 128 attempts, then the error is surfaced. A signal storm must produce an error the caller can act
on, not a hang.

The trap: a *whole-call* retry is not safe for a multi-syscall write. Buffer `[a b]` written as two writes may
have written `a` before the interruption; repeating the call writes `a` again. That is why
`write_all_to_handle` retries the inner `write_all` (which resumes from the file offset) rather than the outer
call.

## Exclusive create

`is_exclusive_flag` reports `O_EXCL` from an `open(2)`-style flag string.

An exclusive create gets **exactly one attempt**, and the reason is worth stating because it looks like an
oversight: the first attempt may have *succeeded in creating the file* before being interrupted. Retrying would
then fail with `AlreadyExists` and report contention where the file is in fact ours. A test pins this: two
`wx` writes to the same path fail the second time with `AlreadyExists` and the first content survives.

## Rename contention (Windows)

Windows refuses a rename while another handle holds the file. The holder usually releases within milliseconds,
so a bounded ladder — 10, 25, 50, 100, 200, 400 ms — turns a spurious failure into a success.

- The ladder is **empty off Windows**: a POSIX rename is atomic and does not fail this way.
- Only contention errors retry. Anything else is surfaced on the first attempt.
- The sleep is **injected** as a callback, so the ladder is tested without waiting and the caller owns the
  clock. This is also what keeps the module honest under the project's no-fixed-sleep rule: the policy is
  data, the delay is the caller's business.

Error classification differs per platform. On Windows, `PermissionDenied` and raw codes 1/5/13/16/32/33 cover
`ERROR_ACCESS_DENIED` (5), `ERROR_SHARING_VIOLATION` (32) and `ERROR_LOCK_VIOLATION` (33). On POSIX only 1
(`EPERM`), 13 (`EACCES`) and 16 (`EBUSY`) apply — code 5 is `EIO` there, and treating a real I/O failure as
contention would retry it six times before reporting it.
