# Stale reflection holder — clean-room spec

Behaviour reference: `crates/omo/memory-core/src/reflection/stale_holder.rs`. Implemented in
`crates/maho-memory/src/stale_holder.rs`.

One decision drives both stale-lock recovery and the `doctor` report. Being wrong is expensive in both
directions: reclaiming a live run's lock makes two reflections write at once, and refusing a dead one's
lock blocks reflection permanently.

**Every uncertainty resolves to `Stuck`, never `Dead`.** `Dead` requires proving *all* of:

1. the launcher is on this host,
2. its pid is gone — or alive but provably reused,
3. any publish claimant is gone,
4. the run recorded a complete, valid outcome,
5. an outcome that matches this run id.

## Decision order, with the rule each failure pins

| Step | Condition | Verdict |
|---|---|---|
| lock file | absent | `None` |
| lock file | unreadable (non-ENOENT) | `Stuck` |
| lock file | not JSON | `Stuck` |
| launcher | pid or hostname missing | `Stuck` |
| launcher | hostname differs from this host | `Stuck` — cannot be probed from here |
| launcher | probe says alive | `Live` |
| launcher | probe says unknown | `Stuck` |
| launcher | dead, or alive-but-reused | continue |
| outcome | missing | `Stuck` — a dead launcher with no outcome is not settled |
| outcome | unreadable / not JSON / wrong shape / other run id | `Stuck` |
| claim | unreadable / not JSON | `Stuck` |
| claim | present but no `claimantPid` | `Stuck` |
| claimant | alive | `Live` |
| claimant | unknown (including pid 0) | `Stuck` |
| claimant | dead, or no claim file | `Dead` |

## PID reuse

A live pid may be overridden to `Dead` only when a recorded start identity and a freshly probed one are
**comparable and differ**. Comparable means the same scheme prefix (`ps-lstart:`,
`windows-process-creation-filetime:`): different schemes measure different quantities and are never
evidence about each other. An unreadable start probe, an absent start probe, or a mismatched scheme keeps
the pid `Live`.

Pid `0` is `Unknown` at the probe, never `Dead` — it means "not recorded", not "not running".

## Recorded outcome shape

All of: `version == 1`, `runId` equal to the lock's run id, `finishedAt` a string, `childExit` an object
with `code` null-or-number and `signal` null-or-string, `timedOut` a boolean, and `attempt` absent or a
positive integer.

A file that fails any part, **or names another run**, is not an outcome: finalising on it would settle the
wrong run. This is why the `runId` check is part of the shape rather than a separate step.
