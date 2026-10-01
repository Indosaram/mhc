use std::{fs::{self, OpenOptions}, io::{self, Write}, path::Path};
use std::os::unix::fs::OpenOptionsExt;
use serde_json::json;

pub const COOLDOWN_DAYS: u64 = 7;
const ONBOARDING_MARKER: &str = "onboarding-completed";

pub fn claim_onboarding(state_dir: &Path) -> bool {
    if fs::create_dir_all(state_dir).is_err() { return false; }
    let Ok(mut file) = OpenOptions::new().write(true).create_new(true).mode(0o600).open(state_dir.join(ONBOARDING_MARKER)) else { return false; };
    file.write_all(json!({"completedAt":jiff::Timestamp::now().to_string(),"version":1}).to_string().as_bytes()).is_ok()
}
pub fn is_onboarding_complete(state_dir: &Path) -> bool { state_dir.join(ONBOARDING_MARKER).exists() }
pub fn get_onboarding_marker_mtime(state_dir: &Path) -> Option<f64> {
    fs::metadata(state_dir.join(ONBOARDING_MARKER)).ok()?.modified().ok()?.duration_since(std::time::UNIX_EPOCH).ok().map(|d| d.as_secs_f64()*1000.0)
}
fn write_decline(dest: &Path) -> io::Result<()> {
    if let Some(parent) = dest.parent() { fs::create_dir_all(parent)?; }
    let tmp = dest.with_file_name(format!("{}.{}.{}.tmp",dest.file_name().unwrap_or_default().to_string_lossy(),std::process::id(),uuid::Uuid::new_v4()));
    let result = (|| {
        let mut file=OpenOptions::new().write(true).create_new(true).mode(0o600).open(&tmp)?;
        file.write_all(json!({"declinedAt":jiff::Timestamp::now().as_millisecond()}).to_string().as_bytes())?;
        fs::rename(&tmp,dest)
    })();
    if let Err(error) = fs::remove_file(&tmp) && error.kind() != io::ErrorKind::NotFound { eprintln!("onboarding temp cleanup failed: {error}"); }
    result
}
pub fn write_global_decline(state_dir: &Path) -> io::Result<()> { write_decline(&state_dir.join("onboarding-declined-global")) }
pub fn is_globally_declined(state_dir: &Path) -> bool { state_dir.join("onboarding-declined-global").exists() }
pub fn write_project_decline(state_dir: &Path, repo_hash: &str) -> io::Result<()> { write_decline(&state_dir.join("onboarding-declined-projects").join(repo_hash)) }
pub fn is_project_declined(state_dir: &Path, repo_hash: &str) -> bool { state_dir.join("onboarding-declined-projects").join(repo_hash).exists() }
pub fn write_cooldown(state_dir: &Path, repo_hash: &str, at: f64) -> io::Result<()> {
    let dir=state_dir.join("onboarding-cooldowns"); fs::create_dir_all(&dir)?;
    let mut file=OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(dir.join(repo_hash))?;
    file.write_all(json!({"until":at + COOLDOWN_DAYS as f64 * 86_400_000.0}).to_string().as_bytes())
}
pub fn is_cooling_down(state_dir: &Path, repo_hash: &str, now: f64) -> bool {
    let Ok(raw)=fs::read_to_string(state_dir.join("onboarding-cooldowns").join(repo_hash)) else { return false; };
    let Ok(value)=serde_json::from_str::<serde_json::Value>(&raw) else { return false; };
    value["until"].as_f64().is_some_and(|until| until.is_finite() && until>=0.0 && now<until)
}
