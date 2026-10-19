//! Port of `__adversarial__/chaos-bench.test.ts`. Seeded adversarial interleaving bench. Each
//! iteration scripts fake runners through a randomized mix of start/queue, completion, steer,
//! interrupt, running/pending cancel, abortable parent waits, revive, eviction, reconciliation,
//! shutdown, rpc clean/signal exit and notifier retry, then asserts:
//!   (1) exactly-once notification per (task_id, run_epoch)
//!   (2) terminal idempotence (no terminal overwrite; late transitions logged not applied)
//!   (3) no concurrency slot leak (all slots released + queue drained when every task is terminal)
//!   (4) no unhandled panic for the whole run (this port is synchronous, so there is no
//!       async-rejection channel distinct from a propagated `Result`/panic; a run that completes
//!       without panicking already proves this, so there is nothing further to assert here)
//!   (5) every waitFor settles and a cancelled-pending task never launches
//!   (6-13) suspend/revive ownership, recovery, epoch, pid, deliberate-stop, cap, reclamation,
//!          and no-terminal-rerun lifecycle laws
#![cfg(test)]

use super::chaos_drive::run_iteration;
use super::chaos_harness::{ChaosHarnessOptions, build_harness};
use super::prng::{derive_seed, hash_seed};

const DEFAULT_SEED: &str = "senpi-task-w1-chaos";
const ITERATIONS: u32 = 200;
const INVARIANT_IDS: [u8; 13] = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13];

#[test]
fn given_200_randomized_event_interleavings_when_each_is_driven_to_quiescence_then_all_thirteen_invariants_hold() {
    let mut failures: Vec<String> = Vec::new();
    assert_eq!(INVARIANT_IDS.len(), 13);

    let seam_harness = build_harness(ChaosHarnessOptions {
        concurrency: 1,
        residency_max: 1,
        max_depth: 1,
    });
    assert_eq!(seam_harness.retry_scheduler.pending_count(), 0);
    assert_eq!(seam_harness.waiters.counts(), (0, 0));
    drop(seam_harness);

    let seed_label = std::env::var("SEED").unwrap_or_else(|_| DEFAULT_SEED.to_string());
    let base_seed = hash_seed(&seed_label);
    eprintln!("[chaos] seed={seed_label} base={base_seed} iterations={ITERATIONS}");

    for iteration in 0..ITERATIONS {
        let seed = derive_seed(base_seed, iteration);
        let report = run_iteration(seed);
        for violation in report.violations {
            failures.push(format!(
                "iter {iteration} seed={seed}: inv{} {}",
                violation.invariant, violation.detail
            ));
        }
    }

    if !failures.is_empty() {
        eprintln!("[chaos] {} violation(s):\n{}", failures.len(), failures.join("\n"));
    }
    assert_eq!(failures, Vec::<String>::new());
}
