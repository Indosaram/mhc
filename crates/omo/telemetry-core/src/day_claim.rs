use std::fs::{self, OpenOptions};
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

const CLAIM_PREFIX: &str = "daily-active.";
const CLAIM_SUFFIX: &str = ".claim";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DayClaimOutcome {
    Claimed,
    AlreadyClaimed,
    Unavailable,
}

pub fn get_telemetry_day_claim_file_path(state_dir: &Path, day_utc: &str) -> PathBuf {
    state_dir.join(format!("{CLAIM_PREFIX}{day_utc}{CLAIM_SUFFIX}"))
}

/// Exclusive-create is what makes the daily-active decision safe: creating the file with
/// create_new(true) fails with AlreadyExists when another process already owns the day, so
/// exactly one caller of a fresh day is told to capture no matter how many start at once.
/// Anything else - a read-then-write, or a write whose failure is ignored - degrades to one
/// event per launch on the machines where it matters.
pub fn claim_utc_day(state_dir: &Path, day_utc: &str) -> DayClaimOutcome {
    if fs::create_dir_all(state_dir).is_err() {
        return DayClaimOutcome::Unavailable;
    }

    let claim_path = get_telemetry_day_claim_file_path(state_dir, day_utc);
    match OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&claim_path)
    {
        Ok(_) => {
            prune_superseded_claims(state_dir, day_utc);
            DayClaimOutcome::Claimed
        }
        Err(err) if err.kind() == ErrorKind::AlreadyExists => DayClaimOutcome::AlreadyClaimed,
        Err(_) => DayClaimOutcome::Unavailable,
    }
}

fn prune_superseded_claims(state_dir: &Path, day_utc: &str) {
    let current_claim = format!("{CLAIM_PREFIX}{day_utc}{CLAIM_SUFFIX}");
    let entries = match fs::read_dir(state_dir) {
        Ok(entries) => entries,
        Err(_) => return, // Housekeeping only: the day is already claimed, so a failure here must not change the decision.
    };

    for entry in entries.flatten() {
        let file_name = entry.file_name();
        let Some(name_str) = file_name.to_str() else {
            continue;
        };
        if name_str == current_claim {
            continue;
        }
        if !name_str.starts_with(CLAIM_PREFIX) || !name_str.ends_with(CLAIM_SUFFIX) {
            continue;
        }
        // A claim left behind costs one stale file, never a duplicate event.
        let _ = fs::remove_file(entry.path());
    }
}
