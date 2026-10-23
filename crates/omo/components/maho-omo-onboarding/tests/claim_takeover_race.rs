//! A dead claim taken over by N claimants at once (upstream `claim-takeover-race.test.ts`, contender thread
//! port of `claim-takeover-contender.ts`): one claimant parks inside its first own-pid identity read until
//! released, the others run to completion meanwhile. Exactly one wins and its claim is never removed.

use std::{
    sync::{Arc, Barrier, mpsc},
    time::Duration,
};

use maho_omo_onboarding::{
    claim_owner::{ClaimOwnerSeams, claim_onboarding_owned, read_onboarding_state},
    state::ONBOARDING_MARKER,
};
use memory_core::locks::ProcessLiveness;

const NOW: i64 = 1_792_368_000_000;
const DEAD_OWNER: u32 = 4242;
const BOUND: Duration = Duration::from_secs(10);

fn seams(pid: u32, alive: Vec<u32>) -> ClaimOwnerSeams {
    let for_liveness = alive.clone();
    ClaimOwnerSeams {
        pid: Some(pid),
        get_pid_liveness: Some(Arc::new(move |p| {
            if for_liveness.contains(&p) {
                ProcessLiveness::Alive
            } else {
                ProcessLiveness::Dead
            }
        })),
        get_process_start_identity: Some(Arc::new(move |p| {
            alive.contains(&p).then(|| format!("proc-start-epoch:{p}"))
        })),
        now: Some(Arc::new(|| NOW)),
    }
}

fn seed_dead_claim(dir: &std::path::Path) -> std::io::Result<()> {
    let claimed = jiff::Timestamp::from_millisecond(NOW)
        .map_err(std::io::Error::other)?
        .to_string();
    std::fs::write(
        dir.join(ONBOARDING_MARKER),
        serde_json::json!({"claimedAt": claimed, "version": 2, "owner": {"pid": DEAD_OWNER, "processStart": "proc-start-epoch:1"}, "nonce": "dead"}).to_string(),
    )
}

#[test]
fn many_claimants_of_a_dead_claim_yield_exactly_one_winner()
-> Result<(), Box<dyn std::error::Error>> {
    for claimants in [3_u32, 5] {
        let dir = tempfile::tempdir()?;
        seed_dead_claim(dir.path())?;
        let pids: Vec<u32> = (0..claimants).map(|index| 5100 + index).collect();
        let gate = Arc::new(Barrier::new(pids.len()));
        let (parked_tx, parked_rx) = mpsc::channel::<()>();
        let (release_tx, release_rx) = mpsc::channel::<()>();
        let release_rx = Arc::new(std::sync::Mutex::new(release_rx));
        let mut threads = Vec::new();
        for (index, pid) in pids.iter().copied().enumerate() {
            let path = dir.path().to_owned();
            let gate = Arc::clone(&gate);
            let mut own = seams(pid, pids.clone());
            if index == 0 {
                // The first claimant parks inside its own-pid identity read until released.
                let parked_tx = parked_tx.clone();
                let release_rx = Arc::clone(&release_rx);
                let parked = std::sync::atomic::AtomicBool::new(false);
                let alive = pids.clone();
                own.get_process_start_identity = Some(Arc::new(move |p| {
                    if p == pid && !parked.swap(true, std::sync::atomic::Ordering::SeqCst) {
                        let _ = parked_tx.send(());
                        if let Ok(rx) = release_rx.lock() {
                            let _ = rx.recv_timeout(BOUND);
                        }
                    }
                    alive.contains(&p).then(|| format!("proc-start-epoch:{p}"))
                }));
            }
            threads.push(std::thread::spawn(move || {
                gate.wait();
                claim_onboarding_owned(&path, &own)
            }));
        }
        parked_rx.recv_timeout(BOUND)?;
        let _ = release_tx.send(());
        let winners: Vec<bool> = threads
            .into_iter()
            .map(|t| t.join().map_err(|_| "claimant panicked"))
            .collect::<Result<_, _>>()?;
        assert_eq!(
            winners.iter().filter(|won| **won).count(),
            1,
            "{claimants} claimants"
        );
        let winner_pid = pids[winners.iter().position(|won| *won).ok_or("no winner")?];
        let raw = std::fs::read_to_string(dir.path().join(ONBOARDING_MARKER))?;
        let owner = serde_json::from_str::<serde_json::Value>(&raw)?["owner"]["pid"].as_u64();
        assert_eq!(owner, Some(u64::from(winner_pid)));
        assert!(read_onboarding_state(dir.path(), &seams(1, pids.clone())).in_progress);
        assert_eq!(
            std::fs::read_dir(dir.path())?.count(),
            1,
            "staged and lock files are always unlinked"
        );
    }
    Ok(())
}
