# Reflection lock liveness probe — clean-room spec

Behaviour reference: `crates/omo/memory-core/src/locks/process_identity.rs`.
Implemented in `crates/maho-memory/src/stale_holder.rs` (`SystemReflectionLockProbe`) over
`crates/maho-memory/src/host_identity.rs`.

A reflection lock records who holds it: pid, hostname, and a start-time identity. Recovering a lock left by a
crashed run means proving that owner is gone — and pid alone cannot prove that, because the OS reuses pids.

## The start identity is what separates "still running" from "pid reused"

A live pid whose recorded start identity **differs** from the one now observed is `Dead`: the number was
reused by a different process. A live pid with a matching identity is `Alive`.

`launcher_liveness` only downgrades to `Dead` on a *proven* mismatch. An unreadable or incomparable start
identity keeps the holder `Alive`. Over-claiming a dead owner merely delays recovery; under-claiming kills a
lock that a running session still needs, so every uncertainty resolves toward `Alive`.

## Scheme comparison

Identities are `<scheme>:<value>` and are compared only when the schemes match. `ps-lstart:…`,
`proc-start:…` and `windows-process-creation-filetime:…` measure different quantities and are never evidence
about each other — a macOS record compared against a Linux probe would otherwise "prove" reuse on every check.

An identity with no scheme (no `:`, or `:` at index 0) never conflicts.

## Defect this slice fixes

`SystemReflectionLockProbe::process_start_identity` returned `Ok(None)` unconditionally, so the reuse check in
`launcher_liveness` could never fire: every live pid read as `Alive`, and a lock whose owner crashed and whose
pid was later reused was treated as held forever. Nothing caught it because the existing tests all used a fake
probe — the real implementation had no coverage of the branch it existed to serve.

`hostname()` had the same shape of defect: it read only `$HOSTNAME`, while `host_identity::hostname()` reads
the kernel name first with `/proc/sys/kernel/hostname` → `/etc/hostname` → env fallbacks. Two probes of "this
machine" that disagree make records unreclaimable, because a holder written by one is judged foreign by the
other. Both now delegate to `host_identity`.

## Platform coverage

- Linux: start identity is `/proc/<pid>/stat` field 22, field 2 (comm) may contain spaces and parentheses, so
  the split starts after the **last** `") "`. Liveness is `/proc/<pid>` existence, which agrees with `kill -0`
  for zombies.
- Other targets: start identity is `None`, and `launcher_liveness` therefore keeps a live pid `Alive`. That is
  the conservative answer, not a silent success — the record stores nothing, and nothing is proven gone.

No FFI: this crate forbids `unsafe_code`, so the upstream `libc::kill`/`libproc`/`GetProcessTimes` probes are
replaced by `/proc` reads. The behaviour differs from upstream only in returning `None` rather than a
macOS/Windows identity, which downgrades to `Alive` instead of proving reuse.
