# Support: host identity — clean-room spec

Behaviour reference: `crates/omo/memory-core/src/support/host.rs`. Implemented in
`crates/maho-memory/src/host_identity.rs`.

A lock record carries the host name that wrote it. A contender uses that name to answer one question: *may
I reclaim this dead owner's lock?* Wrong answers in either direction are worse than an imprecise name.

- Name is **empty** → record validation rejects it, so no lock can be written at all.
- Name is **foreign when it should match** → a dead owner on this machine is never reclaimed; the memory
  store wedges until a human intervenes.
- Name is **matching when it should be foreign** → a live owner on another machine is reclaimed. This is the
  only case with data-loss potential, so it is guarded separately by a liveness check before any reclaim.

## Source order

1. `/proc/sys/kernel/hostname` — the kernel's own answer, so two processes cannot disagree through their
   environments.
2. `/etc/hostname` — the configured name, still machine-wide rather than per-process.
3. `HOSTNAME`, then `COMPUTERNAME` — the Node `os.hostname()` fallback, needed on hosts without procfs.
4. `unknown` — never an empty string.

The upstream implementation called `gethostname(2)` first. This crate forbids `unsafe`, and the FFI call
needs it, so the kernel's procfs entry is the primary source instead: it is the same value the syscall
returns on Linux, read without unsafe. The fallback order below it is unchanged.

## `unknown` is a wildcard, deliberately

`same_machine_for(recorded, current)` returns true when `recorded == current` **or** `recorded == "unknown"`.

`unknown` is what builds wrote before the OS name was consulted. A record carrying it *might* be ours.
Treating it as foreign would make every such owner permanently unreclaimable — a store no one can ever lock
again. The wildcard under-claims safety, and the liveness check that runs before reclaim covers that gap.

## Process start tokens

`process_start(pid)` returns `proc-start:<field 22 of /proc/<pid>/stat>`, i.e. clock ticks since boot.

This distinguishes a live process from a later process that reused its pid. `None` means *cannot prove*, and
a record stores `unavailable` — which a caller must read as "do not reclaim", never as "gone".

Field 2 of `stat` is the executable name in parentheses and may contain spaces **and** parentheses, so the
line is split after its last `) ` rather than tokenized from the start. Getting this wrong shifts every
field after it and yields a start token for the wrong position in the file.

## `SystemLockHost`

The trait exists so lock-record creation is testable without touching the real clock, process table or
hostname. Before this module the MIT crate had the trait and test fakes only — no production implementation,
so nothing could create a real lock record. `SystemLockHost` is that implementation:

- `process_id` — `std::process::id()`
- `process_start` — the procfs token above
- `hostname` — [`hostname`]
- `nonce` — `random_ids::random_uuid()`
- `now` — UTC ISO-8601 with milliseconds and `Z`
- `os_file_lock_available` — `true`: `flock` is available on every platform this crate builds for
