//! Owner-recording onboarding claim (upstream `claim-owner.ts`).
//!
//! The claim marker records its owner (pid + process start identity). A held claim whose owner is gone
//! (exited, or its pid reused) is taken over under a takeover lock for exactly that claim, so concurrent
//! claimants yield one winner and a live claim is never moved or removed.

use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::Arc,
};

use memory_core::locks::{
    ProcessLiveness, get_pid_liveness, get_process_start_identity, start_identities_conflict,
};
use memory_core::support::sha256::sha256_hex;
use serde_json::{Value, json};

use crate::state::{
    ONBOARDING_MARKER, is_onboarding_completion_recorded, read_onboarding_completed_at,
};

/// A claim written before claims recorded their owner cannot be probed; past this age it is abandoned.
pub const OWNERLESS_CLAIM_STALE_MS: i64 = 24 * 3_600_000;
/// Takeover locks whose holders died form a chain; past this length the takeover yields.
pub const MAX_TAKEOVER_CHAIN: usize = 16;

pub type PidLiveness = Arc<dyn Fn(u32) -> ProcessLiveness + Send + Sync>;
pub type StartIdentity = Arc<dyn Fn(u32) -> Option<String> + Send + Sync>;
pub type NowMs = Arc<dyn Fn() -> i64 + Send + Sync>;

/// Identity readers for judging a claim's owner; every field defaults to the real process table.
#[derive(Clone, Default)]
pub struct ClaimOwnerSeams {
    pub pid: Option<u32>,
    pub get_pid_liveness: Option<PidLiveness>,
    pub get_process_start_identity: Option<StartIdentity>,
    pub now: Option<NowMs>,
}

impl ClaimOwnerSeams {
    fn liveness(&self, pid: u32) -> ProcessLiveness {
        self.get_pid_liveness
            .as_ref()
            .map_or_else(|| get_pid_liveness(pid), |probe| probe(pid))
    }
    fn start_identity(&self, pid: u32) -> Option<String> {
        self.get_process_start_identity
            .as_ref()
            .map_or_else(|| get_process_start_identity(pid), |probe| probe(pid))
    }
    fn now_ms(&self) -> i64 {
        self.now
            .as_ref()
            .map_or_else(|| jiff::Timestamp::now().as_millisecond(), |now| now())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OnboardingStateSnapshot {
    /// ISO time onboarding finished; `None` while it has not.
    pub completed_at: Option<String>,
    /// A claim is held by a running owner and onboarding has not finished.
    pub in_progress: bool,
}

pub fn read_onboarding_state(state_dir: &Path, seams: &ClaimOwnerSeams) -> OnboardingStateSnapshot {
    if let Some(completed_at) = read_onboarding_completed_at(state_dir) {
        return OnboardingStateSnapshot {
            completed_at: Some(completed_at),
            in_progress: false,
        };
    }
    let in_progress = read_raw(&state_dir.join(ONBOARDING_MARKER))
        .is_some_and(|held| claim_holder_live(&held, seams));
    OnboardingStateSnapshot {
        completed_at: None,
        in_progress,
    }
}

/// Takes the install-wide claim, recording its owner. A held claim whose owner is gone is taken over; a
/// live or unprovable owner blocks.
pub fn claim_onboarding_owned(state_dir: &Path, seams: &ClaimOwnerSeams) -> bool {
    if fs::create_dir_all(state_dir).is_err() || is_onboarding_completion_recorded(state_dir) {
        return false;
    }
    let marker = state_dir.join(ONBOARDING_MARKER);
    let pid = seams.pid.unwrap_or_else(std::process::id);
    let process_start = seams.start_identity(pid);
    let claimed_at = jiff::Timestamp::from_millisecond(seams.now_ms())
        .map_or_else(|_| String::new(), |t| t.to_string());
    let record = json!({
        "claimedAt": claimed_at,
        "version": 2,
        "owner": {"pid": pid, "processStart": process_start},
        "nonce": uuid::Uuid::new_v4().to_string(),
    })
    .to_string();
    if write_exclusive(&marker, &record) {
        return true;
    }
    let Some(held) = read_raw(&marker) else {
        return write_exclusive(&marker, &record);
    };
    if claim_holder_live(&held, seams) {
        return false;
    }
    take_over_claim(state_dir, &marker, &held, &record, seams)
}

fn write_exclusive(path: &Path, content: &str) -> bool {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options
        .open(path)
        .and_then(|mut file| file.write_all(content.as_bytes()))
        .is_ok()
}

fn sibling(marker: &Path, suffix: &str) -> PathBuf {
    let mut name = marker
        .file_name()
        .map(std::ffi::OsStr::to_os_string)
        .unwrap_or_default();
    name.push(suffix);
    marker.with_file_name(name)
}

/// Replaces the judged-dead claim only while holding the takeover lock for exactly that claim: an
/// exclusively created file named by a hash of the judged bytes, holding our own claim record. A lock whose
/// holder died is passed by locking the dead holder's record in turn, so exactly one live contender holds
/// the end of the chain.
fn take_over_claim(
    state_dir: &Path,
    marker: &Path,
    judged: &str,
    mine: &str,
    seams: &ClaimOwnerSeams,
) -> bool {
    let mut target = judged.to_owned();
    for _ in 0..MAX_TAKEOVER_CHAIN {
        let digest = sha256_hex(target.as_bytes());
        let lock = sibling(marker, &format!(".takeover.{}", &digest[..32]));
        if write_exclusive(&lock, mine) {
            return replace_judged_claim(state_dir, marker, judged, mine, &lock);
        }
        let Some(holder) = read_raw(&lock) else {
            continue;
        };
        if claim_holder_live(&holder, seams) {
            return false;
        }
        target = holder;
    }
    false
}

fn replace_judged_claim(
    state_dir: &Path,
    marker: &Path,
    judged: &str,
    mine: &str,
    lock: &Path,
) -> bool {
    let staged = sibling(marker, &format!(".claim.{}", uuid::Uuid::new_v4()));
    let won = read_raw(marker).as_deref() == Some(judged)
        && !is_onboarding_completion_recorded(state_dir)
        && write_exclusive(&staged, mine)
        && fs::rename(&staged, marker).is_ok();
    let _ = fs::remove_file(&staged);
    let _ = fs::remove_file(lock);
    won
}

fn read_raw(path: &Path) -> Option<String> {
    fs::read_to_string(path).ok()
}

// Unparseable content or a marker without `claimedAt` is a pre-split marker, which means completed, so it
// is never taken over.
fn claim_holder_live(raw: &str, seams: &ClaimOwnerSeams) -> bool {
    let Ok(Value::Object(record)) = serde_json::from_str::<Value>(raw) else {
        return true;
    };
    let Some(claimed_at) = record.get("claimedAt").and_then(Value::as_str) else {
        return true;
    };
    if let Some(owner) = record.get("owner").and_then(Value::as_object)
        && let Some(pid) = owner
            .get("pid")
            .and_then(Value::as_u64)
            .and_then(|pid| u32::try_from(pid).ok())
            .filter(|pid| *pid > 0)
    {
        return owner_alive(
            pid,
            owner.get("processStart").and_then(Value::as_str),
            seams,
        );
    }
    match claimed_at.parse::<jiff::Timestamp>() {
        Ok(at) => seams.now_ms() - at.as_millisecond() < OWNERLESS_CLAIM_STALE_MS,
        Err(_) => true,
    }
}

/// An owner is gone when its pid no longer runs, or runs with a start identity that provably belongs to
/// another process (pid reuse). Anything unprovable keeps the owner alive.
fn owner_alive(pid: u32, recorded_start: Option<&str>, seams: &ClaimOwnerSeams) -> bool {
    match seams.liveness(pid) {
        ProcessLiveness::Dead => false,
        ProcessLiveness::Unknown => true,
        ProcessLiveness::Alive => {
            let Some(recorded) = recorded_start else {
                return true;
            };
            seams
                .start_identity(pid)
                .is_none_or(|actual| !start_identities_conflict(recorded, &actual))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_792_368_000_000;

    fn seams(pid: u32, alive: &[u32]) -> ClaimOwnerSeams {
        let alive_liveness = alive.to_vec();
        let alive_identity = alive.to_vec();
        ClaimOwnerSeams {
            pid: Some(pid),
            get_pid_liveness: Some(Arc::new(move |p| {
                if alive_liveness.contains(&p) {
                    ProcessLiveness::Alive
                } else {
                    ProcessLiveness::Dead
                }
            })),
            get_process_start_identity: Some(Arc::new(move |p| {
                alive_identity
                    .contains(&p)
                    .then(|| format!("proc-start-epoch:{p}"))
            })),
            now: Some(Arc::new(|| NOW)),
        }
    }

    #[test]
    fn fresh_claim_records_owner_and_reads_in_progress() {
        let dir = tempfile::tempdir().expect("tempdir");
        let seams = seams(10, &[10]);
        assert!(claim_onboarding_owned(dir.path(), &seams));
        assert!(!claim_onboarding_owned(
            dir.path(),
            &self::seams(11, &[10, 11])
        ));
        let state = read_onboarding_state(dir.path(), &seams);
        assert_eq!(
            state,
            OnboardingStateSnapshot {
                completed_at: None,
                in_progress: true
            }
        );
    }

    #[test]
    fn dead_owner_claim_is_taken_over() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert!(claim_onboarding_owned(dir.path(), &seams(10, &[10])));
        assert!(claim_onboarding_owned(dir.path(), &seams(11, &[11])));
        let raw = fs::read_to_string(dir.path().join(ONBOARDING_MARKER)).expect("marker");
        assert!(raw.contains("\"pid\":11"));
        let leftovers = fs::read_dir(dir.path()).expect("dir").flatten().count();
        assert_eq!(leftovers, 1);
    }

    #[test]
    fn pid_reuse_is_taken_over() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert!(claim_onboarding_owned(dir.path(), &seams(10, &[10])));
        // Pid 10 is alive again but with another start identity.
        let mut reused = seams(11, &[10, 11]);
        reused.get_process_start_identity =
            Some(Arc::new(|p| Some(format!("proc-start-epoch:other-{p}"))));
        assert!(claim_onboarding_owned(dir.path(), &reused));
    }

    #[test]
    fn completion_wins_over_claims() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert!(claim_onboarding_owned(dir.path(), &seams(10, &[10])));
        crate::state::complete_onboarding(dir.path()).expect("complete");
        let state = read_onboarding_state(dir.path(), &seams(10, &[10]));
        assert!(state.completed_at.is_some() && !state.in_progress);
        assert!(!claim_onboarding_owned(dir.path(), &seams(12, &[12])));
    }

    #[test]
    fn ownerless_claim_goes_stale_after_a_day() {
        let dir = tempfile::tempdir().expect("tempdir");
        let claimed = jiff::Timestamp::from_millisecond(NOW - OWNERLESS_CLAIM_STALE_MS + 1_000)
            .expect("ts")
            .to_string();
        fs::write(
            dir.path().join(ONBOARDING_MARKER),
            json!({"claimedAt": claimed, "version": 2}).to_string(),
        )
        .expect("write");
        assert!(read_onboarding_state(dir.path(), &seams(1, &[1])).in_progress);
        let old = jiff::Timestamp::from_millisecond(NOW - OWNERLESS_CLAIM_STALE_MS - 1_000)
            .expect("ts")
            .to_string();
        fs::write(
            dir.path().join(ONBOARDING_MARKER),
            json!({"claimedAt": old, "version": 2}).to_string(),
        )
        .expect("write");
        assert!(!read_onboarding_state(dir.path(), &seams(1, &[1])).in_progress);
    }

    #[test]
    fn legacy_marker_means_completed_and_is_never_taken_over() {
        let dir = tempfile::tempdir().expect("tempdir");
        fs::write(
            dir.path().join(ONBOARDING_MARKER),
            json!({"completedAt": "2026-01-01T00:00:00.000Z", "version": 1}).to_string(),
        )
        .expect("write");
        let state = read_onboarding_state(dir.path(), &seams(1, &[1]));
        assert_eq!(
            state.completed_at.as_deref(),
            Some("2026-01-01T00:00:00.000Z")
        );
        assert!(!claim_onboarding_owned(dir.path(), &seams(1, &[1])));
    }
}
