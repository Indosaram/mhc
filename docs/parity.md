# Parity tracking

mhc proves it matches upstream in two ways: a per-crate ledger that maps every upstream TypeScript
file to its Rust port, and golden fixtures rendered by the pinned upstream itself.

## Ledgers (`parity.d/`)

Each crate keeps its ledger as fragments under `crates/<crate>/parity.d/<todo>.md`, one fragment per
work item. Tables headed `| TS path | Rust path | TS tests | Rust tests | status |` are audited;
`status` is `done`, `partial`, `todo` or `n/a: <reason>`. The full format, the ownership rules and
how a later fragment closes an earlier row are in [tools/PARITY-FORMAT.md](../tools/PARITY-FORMAT.md).

```bash
SENPI_SRC=/path/to/senpi bun tools/parity-audit.mjs --crate maho-core
SENPI_SRC=/path/to/senpi bun tools/parity-audit.mjs --all
bun tools/parity-audit.mjs --self-test
```

The audit exits 0 on pass, 1 on an audit failure, 2 on a usage error and 3 when `SENPI_SRC` is not
at the commit pinned in [PINS.md](../PINS.md). `tools/parity-owners.json` assigns each todo its file
list (`--only`).

## Golden fixtures (`tools/golden/`)

```bash
bun tools/golden/run.mjs --case <name>     # or --all
```

renders `tools/golden/cases/*.json` with pinned senpi and writes `crates/<crate>/tests/golden/`. The
Rust tests compare against those files and have no update mode. `tools/golden/faux-harness.mjs` and
`faux-tui.mjs` run senpi (optionally with OmO via `--omo` and `$OMO_SRC`) on the faux provider in an
isolated temporary `HOME`, as the reference side for event and pty comparisons.

## The notification delivery contract

A notification is a message the agent sends on its own initiative: a monitor wake, a DAG or
host-liveness wake, a `.ulw` continuation, a team update, or a terminal exit notice. These rules
are what the delivery work established, and each one is enforced at the cited line.

1. **Delivered means the receipt settled Ok.** Admission alone is not delivery. The production
   monitor closure admits a wake with `send_message_with_receipt`
   (`crates/builtins/maho-ext-terminal/src/extension.rs:1311`) and hands the receipt to the queue;
   `settle` decides the wake's fate from the settle outcome
   (`crates/builtins/maho-ext-terminal/src/monitor_notify.rs:248`). Terminal exit notices behave
   the same way: `DeliveryOutcome::Delivered` inserts the id into `notified`, while `Indeterminate`
   is kept as delivered and never resent, because a duplicate announcement is worse than none
   (`crates/builtins/maho-ext-terminal/src/notify.rs:200-235`).
2. **A failed settle rolls back and retries; a spent wake budget is not refunded.** `rollback`
   re-queues the lines and gives back the pacing token and rate-limit stamp
   (`crates/builtins/maho-ext-terminal/src/monitor_notify.rs:718`), so the next pass retries the same
   wake. The wake-budget pause is applied by the commit hook, which only runs for a committed wake
   (`crates/builtins/maho-ext-terminal/src/monitor_notify.rs:301`), so a refused or dropped wake
   never costs a monitored session its budget.
3. **Producers must handle a refused enqueue.** A coordinator that refuses an enqueue returns it to
   the caller, and each producer owns the refusal: `dag_wake` keeps the injection buffered and
   replays it on the next session start (`crates/omo/components/maho-omo-task/src/dag_wake.rs:267`),
   `host_liveness_notice` reports the returned error once
   (`crates/omo/components/maho-omo-task/src/host_liveness_notice.rs:64`), and the `.ulw`
   continuation records a post-enqueue loss through `on_delivery_failed`
   (`crates/omo/components/maho-omo-ulw-loop/src/index.rs:562`) instead of a log line that no test
   could observe.
4. **A retried notice is dropped when its run was already collected.** A team update snapshots the
   run it describes (`crates/maho-team/src/team.rs:144`), and a retry is discarded with a diagnostic
   when `team_wait` has since collected those results or a member moved on to a newer run
   (`crates/maho-team/src/team.rs:166`). A notice carrying no run (a delivery or start failure, the
   wall-clock stop) is never dropped, since suppressing it would hide the loss it reports.

**Divergence: a rolled-back wake can duplicate one overflow spill line.** A wake that is rolled
back writes its overflow lines to their diagnostic file again on the retry, so that file can hold
the same chunk twice (`crates/builtins/maho-ext-terminal/src/monitor_notify.rs:715-720`). Losing
the wake is worse than a duplicated spill line, and the file is a diagnostic, not a delivery
record.

**Test-wait exceptions.** `crates/maho-team/tests/team.rs` observes a real tmux server and a fake
tmux process, neither of which can raise an in-process signal; `await_tmux`
(`crates/maho-team/tests/team.rs:552`) and `await_log_line` (`crates/maho-team/tests/team.rs:534`)
are the only bounded waits there, one per external-process site, each with a 10 s timeout naming the
await. Every other team wait awaits its exact event: a notifier signal (`:153`) or the observer
hook's cancellation record (`:144`).

## Other checks

- `bun tools/check-omo-sites.mjs` verifies the `.omo` / `.maho` home-directory literals against
  `tools/omo-sites.json`.
- `tools/run-session2-residual-gate.mjs` runs a declared gate command set and
  `tools/verify-session2-residual.mjs` re-verifies its evidence.

## Further reading

- [docs/divergences.md](divergences.md) — Intentional safety and privacy divergences from upstream
- [docs/perf-divergences.md](perf-divergences.md) — Intentional performance divergences from upstream

