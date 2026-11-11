# Facts abort signal — clean-room spec

Behaviour reference: `crates/omo/memory-core/src/facts/queue.rs` (`FactsAbortSignal`,
`FactsAbortedFuture`). Implemented in `crates/maho-memory/src/facts_abort.rs`.

A cancelled facts run must stop promptly. The failure this prevents is specific: cancelling during startup,
before a step has subscribed, must not leave that step waiting for a signal that already fired.

## Contract

- `abort()` sets the flag and runs every registered callback. It is **idempotent**: a second abort does not
  re-run callbacks, so a callback that deletes or releases something cannot run twice.
- `on_abort(cb)` runs `cb` on abort, or **immediately** when the signal is already aborted.
- `aborted()` is a future that resolves on abort, or at once when already aborted.
- `flag()` exposes the shared `Arc<AtomicBool>` for a transport that must share one cancellation flag with its
  own reactor.
- Clones share one state, so any holder can abort the run.

## The race that matters

`on_abort` and the future's `poll` both re-check the flag **while holding the subscriber lock**. With one
check outside the lock, an abort landing between that check and the lock would be invisible: the callback is
registered after the abort already drained the list, and nothing ever wakes it. Re-checking under the lock
leaves only two orderings — "the subscriber sees the flag" or "the abort sees the subscriber" — and never
neither.

## No async runtime dependency

The wakeup is a one-shot `Waker` slot rather than a `tokio`/`futures` channel. A facts run must be cancellable
from whichever executor it is embedded in; taking a runtime as a dependency to implement one flag would force
that choice on every caller.

Because there is exactly one waiter per `aborted()` future and the sender is dropped with it, a wake that
arrives between the flag load and the waker store is the only thing that must be guarded — which the
under-lock re-check does.
