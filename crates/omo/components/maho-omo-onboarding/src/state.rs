use serde_json::json;
use std::{
    fs::{self, OpenOptions},
    io::{self, Write},
    path::Path,
};

#[cfg(unix)]
fn private_file(options: &mut OpenOptions) -> &mut OpenOptions {
    use std::os::unix::fs::OpenOptionsExt;
    options.mode(0o600)
}

#[cfg(not(unix))]
fn private_file(options: &mut OpenOptions) -> &mut OpenOptions {
    options
}

pub const COOLDOWN_DAYS: u64 = 7;
/// The claim keeps its historical name: a marker an older engine wrote (no `claimedAt`) means completed.
pub const ONBOARDING_MARKER: &str = "onboarding-completed";
pub const COMPLETION_MARKER: &str = "onboarding-completion";

pub fn claim_onboarding(state_dir: &Path) -> bool {
    if fs::create_dir_all(state_dir).is_err() {
        return false;
    }
    if state_dir.join(COMPLETION_MARKER).exists() {
        return false;
    }
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    let Ok(mut file) = private_file(&mut options).open(state_dir.join(ONBOARDING_MARKER)) else {
        return false;
    };
    file.write_all(
        json!({"claimedAt":jiff::Timestamp::now().to_string(),"version":2})
            .to_string()
            .as_bytes(),
    )
    .is_ok()
}

/// Records that onboarding finished; the first completion's time is kept (an existing record is left alone).
pub fn complete_onboarding(state_dir: &Path) -> io::Result<()> {
    fs::create_dir_all(state_dir)?;
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    match private_file(&mut options).open(state_dir.join(COMPLETION_MARKER)) {
        Ok(mut file) => file.write_all(
            json!({"completedAt":jiff::Timestamp::now().to_string(),"version":2})
                .to_string()
                .as_bytes(),
        ),
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => Ok(()),
        Err(error) => Err(error),
    }
}

/// A recorded completion exists; a held claim is judged by `claim_owner`.
pub fn is_onboarding_completion_recorded(state_dir: &Path) -> bool {
    state_dir.join(COMPLETION_MARKER).exists()
}

fn unlink_if_present(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

/// Drops this run's claim; a recorded completion stays.
pub fn release_onboarding(state_dir: &Path) -> io::Result<()> {
    unlink_if_present(&state_dir.join(ONBOARDING_MARKER))
}

/// Forgets onboarding entirely: the claim and the completion.
pub fn reset_onboarding(state_dir: &Path) -> io::Result<()> {
    unlink_if_present(&state_dir.join(COMPLETION_MARKER))?;
    unlink_if_present(&state_dir.join(ONBOARDING_MARKER))
}

/// Claimed or completed: either way no other process starts onboarding.
pub fn is_onboarding_complete(state_dir: &Path) -> bool {
    state_dir.join(ONBOARDING_MARKER).exists() || state_dir.join(COMPLETION_MARKER).exists()
}

fn mtime_iso(path: &Path) -> Option<String> {
    let modified = fs::metadata(path).ok()?.modified().ok()?;
    jiff::Timestamp::try_from(modified)
        .ok()
        .map(|t| t.to_string())
}

fn parse_record(raw: &str) -> Option<serde_json::Map<String, serde_json::Value>> {
    match serde_json::from_str::<serde_json::Value>(raw).ok()? {
        serde_json::Value::Object(map) => Some(map),
        _ => None,
    }
}

fn read_marker_time(state_dir: &Path, name: &str) -> Option<String> {
    let raw = fs::read_to_string(state_dir.join(name)).ok()?;
    let completed_at = parse_record(&raw).and_then(|map| {
        map.get("completedAt")
            .and_then(|v| v.as_str().map(str::to_owned))
    });
    match completed_at {
        Some(at) if at.parse::<jiff::Timestamp>().is_ok() => Some(at),
        _ => mtime_iso(&state_dir.join(name)),
    }
}

// Before claims and completion were split, the claim marker itself meant completed.
fn read_legacy_completion(state_dir: &Path) -> Option<String> {
    let raw = fs::read_to_string(state_dir.join(ONBOARDING_MARKER)).ok()?;
    if parse_record(&raw).is_some_and(|map| {
        map.get("claimedAt")
            .is_some_and(serde_json::Value::is_string)
    }) {
        return None;
    }
    read_marker_time(state_dir, ONBOARDING_MARKER)
}

/// When onboarding was completed, or `None` when it is not (a held claim is not completion).
pub fn read_onboarding_completed_at(state_dir: &Path) -> Option<String> {
    read_marker_time(state_dir, COMPLETION_MARKER).or_else(|| read_legacy_completion(state_dir))
}
pub fn get_onboarding_marker_mtime(state_dir: &Path) -> Option<f64> {
    fs::metadata(state_dir.join(ONBOARDING_MARKER))
        .ok()?
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .map(|d| d.as_secs_f64() * 1000.0)
}
fn write_decline(dest: &Path) -> io::Result<()> {
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp = dest.with_file_name(format!(
        "{}.{}.{}.tmp",
        dest.file_name().unwrap_or_default().to_string_lossy(),
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    let result = (|| {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        let mut file = private_file(&mut options).open(&tmp)?;
        file.write_all(
            json!({"declinedAt":jiff::Timestamp::now().as_millisecond()})
                .to_string()
                .as_bytes(),
        )?;
        fs::rename(&tmp, dest)
    })();
    if let Err(error) = fs::remove_file(&tmp)
        && error.kind() != io::ErrorKind::NotFound
    {
        maho_ext_api::diagnostic(&format!("onboarding temp cleanup failed: {error}"));
    }
    result
}
pub fn write_global_decline(state_dir: &Path) -> io::Result<()> {
    write_decline(&state_dir.join("onboarding-declined-global"))
}
pub fn is_globally_declined(state_dir: &Path) -> bool {
    state_dir.join("onboarding-declined-global").exists()
}
pub fn write_project_decline(state_dir: &Path, repo_hash: &str) -> io::Result<()> {
    write_decline(
        &state_dir
            .join("onboarding-declined-projects")
            .join(repo_hash),
    )
}
pub fn is_project_declined(state_dir: &Path, repo_hash: &str) -> bool {
    state_dir
        .join("onboarding-declined-projects")
        .join(repo_hash)
        .exists()
}
pub fn write_cooldown(state_dir: &Path, repo_hash: &str, at: f64) -> io::Result<()> {
    let dir = state_dir.join("onboarding-cooldowns");
    fs::create_dir_all(&dir)?;
    let mut options = OpenOptions::new();
    options.write(true).create(true).truncate(true);
    let mut file = private_file(&mut options).open(dir.join(repo_hash))?;
    file.write_all(
        json!({"until":at + COOLDOWN_DAYS as f64 * 86_400_000.0})
            .to_string()
            .as_bytes(),
    )
}
pub fn is_cooling_down(state_dir: &Path, repo_hash: &str, now: f64) -> bool {
    let Ok(raw) = fs::read_to_string(state_dir.join("onboarding-cooldowns").join(repo_hash)) else {
        return false;
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&raw) else {
        return false;
    };
    value["until"]
        .as_f64()
        .is_some_and(|until| until.is_finite() && until >= 0.0 && now < until)
}
