//! One occupancy sweep's decisions (senpi `rpc/session-sweep.ts`).
//!
//! The registry walk is expressed over [`SweepCandidate`] views the host builds from its entries, so the
//! decision table stays a pure function.
use crate::session_path_key::session_directory_removed;
use maho_ext_api::SessionKind;
use std::path::Path;

/// How long a session must stand detached before the early-retirement branch may park it: a
/// disconnect that overlaps a sweep is a client about to come back.
pub const DETACHED_RETIREMENT_GRACE_MS: f64 = 5_000.0;

#[derive(Debug, Default, PartialEq, Eq)]
pub struct SweepVerdicts {
    /// Open sessions idle past the window: a retained one parks, any other closes as `idle_evicted`.
    pub idle: Vec<String>,
    /// Sessions no client holds whose transcript directory is gone; they close as
    /// `session_dir_removed` (senpi#2206).
    pub orphaned: Vec<String>,
}

/// What a sweep reads from one open registry entry.
pub struct SweepCandidate<'a> {
    pub session_id: &'a str,
    pub session_path: Option<&'a Path>,
    pub kind: SessionKind,
    pub attachments: i64,
    pub retain_on_disconnect: bool,
    /// The worker reports busy, or the session's complete activity contract (`isSessionBusy`) holds.
    pub busy: bool,
    /// `Some(flushed)` when the entry has a live session: whether its transcript is flushed.
    pub transcript_flushed: Option<bool>,
    /// Pending messages plus pending external admissions still owed a turn.
    pub delivery_debt: usize,
    pub detached_at: Option<f64>,
    /// Restarted to `now` when the session is busy, as senpi does.
    pub last_command_at: &'a mut f64,
}

/// A busy session restarts its idle clock instead of being evicted. A non-finite window evicts nothing
/// idle.
pub fn select_sweep_evictions<'a>(
    candidates: impl IntoIterator<Item = SweepCandidate<'a>>,
    now: f64,
    idle_eviction_ms: f64,
    has_pending_request: impl Fn(&str) -> bool,
) -> SweepVerdicts {
    let mut verdicts = SweepVerdicts::default();
    for candidate in candidates {
        let id = candidate.session_id.to_owned();
        if candidate.attachments == 0
            && candidate
                .session_path
                .is_some_and(session_directory_removed)
        {
            verdicts.orphaned.push(id);
            continue;
        }
        if !idle_eviction_ms.is_finite() || has_pending_request(candidate.session_id) {
            continue;
        }
        if candidate.busy {
            *candidate.last_command_at = now;
            continue;
        }
        if candidate.attachments > 0 {
            // An attached client holds the live routing handle; only the ordinary deadline applies.
            if now - *candidate.last_command_at >= idle_eviction_ms {
                verdicts.idle.push(id);
            }
            continue;
        }
        if candidate.kind == SessionKind::Worker
            && candidate.retain_on_disconnect
            && candidate.transcript_flushed == Some(true)
        {
            // Delivery debt is still owed a turn, even after the ordinary idle deadline.
            if candidate.delivery_debt > 0 {
                continue;
            }
            if let Some(detached_at) = candidate.detached_at {
                if now - detached_at >= DETACHED_RETIREMENT_GRACE_MS {
                    verdicts.idle.push(id);
                }
                continue;
            }
        }
        if now - *candidate.last_command_at >= idle_eviction_ms {
            verdicts.idle.push(id);
        }
    }
    verdicts
}

#[cfg(test)]
mod tests {
    use super::*;

    fn worker<'a>(id: &'a str, clock: &'a mut f64, detached_at: Option<f64>) -> SweepCandidate<'a> {
        SweepCandidate {
            session_id: id,
            session_path: None,
            kind: SessionKind::Worker,
            attachments: 0,
            retain_on_disconnect: true,
            busy: false,
            transcript_flushed: Some(true),
            delivery_debt: 0,
            detached_at,
            last_command_at: clock,
        }
    }

    #[test]
    fn detached_workers_retire_only_after_the_grace() {
        let (mut a, mut b) = (0.0, 0.0);
        let verdicts = select_sweep_evictions(
            [
                worker("fresh", &mut a, Some(9_000.0)),
                worker("stale", &mut b, Some(1_000.0)),
            ],
            10_000.0,
            60_000.0,
            |_| false,
        );
        assert_eq!(verdicts.idle, vec!["stale".to_owned()]);
    }

    #[test]
    fn busy_sessions_restart_their_clock_and_orphans_close() {
        let dir = tempfile::tempdir().expect("tempdir");
        let gone = dir.path().join("gone").join("s.jsonl");
        let (mut busy_clock, mut orphan_clock) = (0.0, 0.0);
        let mut busy = worker("busy", &mut busy_clock, None);
        busy.busy = true;
        let mut orphan = worker("orphan", &mut orphan_clock, None);
        orphan.session_path = Some(gone.as_path());
        let verdicts = select_sweep_evictions([busy, orphan], 100_000.0, 1_000.0, |_| false);
        assert_eq!(verdicts.orphaned, vec!["orphan".to_owned()]);
        assert!(verdicts.idle.is_empty());
        assert_eq!(busy_clock, 100_000.0);
    }

    #[test]
    fn non_finite_window_evicts_nothing_idle() {
        let mut clock = 0.0;
        let verdicts =
            select_sweep_evictions([worker("w", &mut clock, None)], 1e12, f64::INFINITY, |_| {
                false
            });
        assert_eq!(verdicts, SweepVerdicts::default());
    }
}
