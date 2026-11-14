# Kibitzer recall-sidecar sweep and ownership (omo v5.1.35)

Selected source: `omo-ai@5.1.35/plugin/extensions/omo.js` (`sidecar-sweep.ts`, and the `Rte` owner
factory it is triggered from). The `~/code/oh-my-openagent` checkout is 5.0.0-beta.9 and predates this
code, so it is not the reference here.

| TS path | Rust path | status |
|---|---|---|
| sidecar-sweep.ts (dir + lock names, `lastActivity`, `isExpired`, `sweep`, `claimAndRemove`) | components/maho-omo-memory/src/sidecar_sweep.rs | done |
| `Rte` — owner map, `own`, owner lock, sweep kick, session-shutdown release | components/maho-omo-memory/src/sidecar_owner.rs | partial |
| `Rte` — per-session `wakes.ndjson` write chain | (none) | unported |
| `Rte` — gate/unavailable `appendEntry` notices | (none) | unported |

Contract that the port reproduces: `sidecars/<base64url(session)>`, one lock at
`locks/recall-sidecar.<encoded>.lock` acquired twice (owner for the session's life, sweep claim
non-blocking), 7-day horizon, `lstat`-based one-level `lastActivity`, `.prune-` tombstone rename before
recursive removal, and the warn-vs-silent split (listing/claim/removal failures warn; listing `ENOENT`,
claim contention and a lost rename race are silent).

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

The one-level `lastActivity` basis is safe because the sidecar's child transcript is a DIRECT child
appended on every wake; `sidecar_sweep_tests.rs` pins that shape. Upstream's `wakes.ndjson` is a second
such child and is not ported here.

No claim of full `Rte` parity is made: the `wakes.ndjson` write chain and the notice half remain open.
