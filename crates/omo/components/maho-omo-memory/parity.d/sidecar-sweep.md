# Kibitzer recall-sidecar sweep and ownership (omo v5.1.35)

Selected source: `omo-ai@5.1.35/plugin/extensions/omo.js` (`sidecar-sweep.ts`, and the `Rte` owner
factory it is triggered from). The `~/code/oh-my-openagent` checkout is 5.0.0-beta.9 and predates this
code, so it is not the reference here.

| TS path | Rust path | status |
|---|---|---|
| sidecar-sweep.ts (dir + lock names, `lastActivity`, `isExpired`, `sweep`, `claimAndRemove`) | components/maho-omo-memory/src/sidecar_sweep.rs | done |
| `Rte` — owner map, `own`, owner lock, sweep kick, session-shutdown release | components/maho-omo-memory/src/sidecar_owner.rs | done |
| `Rte` — per-session `wakes.ndjson` write chain (`p`, `Mte`) | components/maho-omo-memory/src/sidecar_wake_log.rs | done |
| `Rte` — gate/unavailable `appendEntry` notices | components/maho-omo-memory/src/sidecar_wake_log.rs | done |
| `Rte` — `whenIdle` (drain the in-flight sweeps) | components/maho-omo-memory/src/sidecar_owner.rs (`SidecarOwner::when_idle`) | done |

Contract that the port reproduces: `sidecars/<base64url(session)>`, one lock at
`locks/recall-sidecar.<encoded>.lock` acquired twice (owner for the session's life, sweep claim
non-blocking), 7-day horizon, `lstat`-based one-level `lastActivity`, `.prune-` tombstone rename before
recursive removal, and the warn-vs-silent split (listing/claim/removal failures warn; listing `ENOENT`,
claim contention and a lost rename race are silent).

`wakes.ndjson` contract: one JSON line per settled wake at
`sidecars/<encoded>/wakes.ndjson`, mode `0o600`, directory created first. Fields are `version`, `at`
(injected clock, RFC 3339 millis), `sessionId`, `wake`, `generation`, `status`, and then only what the
wake has: `cause`, `reason`, `configuration`, `model`, `candidateCount`, `nudged` (paths capped at 256
UTF-16 units), `steered`, `toolCalls`, `durationMs`, `slotWaitMs`, `cursors`, `contextTokens`, `usage`,
`diagnostic`. A record that would reach 4096 characters drops `reason` and `nudged` and adds
`truncated: true`; model/category text is redacted (not truncated) when it looks secret-bearing.

Notice contract: `omo-kibitzer:gate` fires on the THIRD consecutive diagnostic failure and once per
streak, carrying `consecutiveFailures` and the wake number; `omo-kibitzer:unavailable` fires once per
session for a configuration refusal (`category_unavailable` / `beyond_category`) with at most 16
`missingProviders`. A non-diagnostic wake clears the streak, so budget/deadline/shutdown ends never
accumulate toward the gate.

## Deliberate divergences

1. **Owned set read per entry, not snapshotted at kick time.** Upstream captures the owned set when the
   sweep is scheduled, so a session that takes ownership while a sweep is already running is invisible
   to it. This port reads the live owner map, because reclaiming a live session's directory is the one
   unrecoverable mistake the sweep can make.
2. **Sweep and owner acquire run on the blocking pool.** Upstream fires the sweep through a
   fire-and-forget promise and acquires the owner lock inside the same async chain. Both are blocking
   filesystem work here (`acquire_lock` sleeps and fsyncs its record; the sweep walks and removes
   directories), so both are handed to `offload_blocking` and never run on the thread that paints and
   reads input.
3. **Release and sweep are one task.** Upstream's shutdown releases the lock and then kicks an
   independent sweep. As two tasks on a real thread pool the order is not guaranteed, and a sweep that
   ran first would find its own session's lock still held. They are one unit here: release, then sweep.
4. **Recall is gated in the sidecar getter.** `sidecar_for` returns `None` when
   `memory.recall.enabled` is false, matching upstream's getter returning before it builds anything, so
   ownership and the sweep inherit the condition instead of carrying a second gate.
5. **No promise chain for the wake records.** Upstream keeps a `Map<session, Promise>` tail and appends
   each record on the previous one's `then`. The mhc append seam is a synchronous `write_all` on an
   `O_APPEND` file, so the chain collapses to the one call in `WakeReporter::report`; ordering within a
   session is guaranteed by the write being synchronous, not by queueing.

The one-level `lastActivity` basis is safe because the sidecar's child transcript is a DIRECT child
appended on every wake, and `wakes.ndjson` is a second such child written on every settled wake;
`sidecar_sweep_tests.rs` pins the transcript shape and `sidecar_wake_log_tests.rs` pins the record one.

`Rte` is now fully accounted for: `own`, `onWake` (record chain plus both notices) and `whenIdle` are
ported; no upstream `Rte` member remains unported.

`whenIdle` is the owner half only: upstream also awaits every live sidecar's `whenIdle` and the
composition's own idle, both of which already exist as `KibitzerSidecar::when_idle` and are driven by
the composition, so the owner exposes the sweep drain as an awaitable with no timeout and no polling
(a `tokio::sync::Notify` registered before the emptiness check, so a sweep finishing between the two
cannot be missed).
