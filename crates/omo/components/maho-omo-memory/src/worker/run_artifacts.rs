use std::{collections::BTreeMap, fs::{File, OpenOptions}, io::{Read, Seek, SeekFrom, Write}, path::Path, sync::atomic::{AtomicU64, Ordering}};
use serde::{Serialize, Deserialize, de::DeserializeOwned};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunAttempt { pub attempt: u32, pub model: String, #[serde(skip_serializing_if = "Option::is_none")] pub thinking: Option<String> }
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RunKind { Reflection, Dream, Facts }
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunLaunchManifest {
    pub version: u32, pub run_id: String, pub attempt: u32,
    #[serde(skip_serializing_if = "Option::is_none")] pub next_attempt: Option<RunAttempt>,
    pub kind: RunKind, pub command: String, pub args: Vec<String>, pub cwd: String,
    pub env: BTreeMap<String, String>, pub hard_deadline_at: f64, pub termination_grace_ms: f64,
    pub max_output_bytes: usize, pub stdout_path: String, pub stderr_path: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunPrelaunchArtifact { pub version: u32, pub run_id: String, pub worktree_dir: String, pub worktree_branch: String }
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ChildExit { pub code: Option<i32>, pub signal: Option<String> }
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunOutcome {
    pub version: u32, pub run_id: String,
    #[serde(skip_serializing_if = "Option::is_none")] pub attempt: Option<u32>,
    pub finished_at: String, pub child_exit: ChildExit, pub timed_out: bool,
}
pub fn run_outcome_matches_ledger(attempt: Option<u32>, outcome: &RunOutcome) -> bool { attempt == outcome.attempt }
#[derive(Debug)]
pub enum ArtifactError { Io(std::io::Error), Json(serde_json::Error), InvalidPrelaunch, InvalidOutputLimit }
impl std::fmt::Display for ArtifactError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { match self { Self::Io(error) => error.fmt(f), Self::Json(error) => error.fmt(f), Self::InvalidPrelaunch => f.write_str("Invalid reflection prelaunch artifact"), Self::InvalidOutputLimit => f.write_str("run output limit must be positive") } }
}
impl std::error::Error for ArtifactError {}
impl From<std::io::Error> for ArtifactError { fn from(error: std::io::Error) -> Self { Self::Io(error) } }
impl From<serde_json::Error> for ArtifactError { fn from(error: serde_json::Error) -> Self { Self::Json(error) } }
pub fn read_run_json<T: DeserializeOwned>(path: &Path) -> Result<T, ArtifactError> { Ok(serde_json::from_slice(&std::fs::read(path)?)?) }
static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);
pub fn write_run_json_atomic<T: Serialize>(path: &Path, value: &T, mode: u32) -> Result<(), ArtifactError> {
    let mut options = OpenOptions::new(); options.write(true).create_new(true);
    #[cfg(unix)] { use std::os::unix::fs::OpenOptionsExt; options.mode(mode); }
    #[cfg(not(unix))] let _ = mode;
    let temporary = path.with_file_name(format!("{}.tmp-{}-{}", path.file_name().unwrap_or_default().to_string_lossy(), std::process::id(), NEXT_TEMP.fetch_add(1, Ordering::Relaxed)));
    let mut file = options.open(&temporary)?;
    file.write_all(serde_json::to_string_pretty(value)?.as_bytes())?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    drop(file);
    std::fs::rename(&temporary, path)?;
    #[cfg(unix)] File::open(path.parent().unwrap_or(Path::new(".")))?.sync_all()?;
    Ok(())
}
pub fn read_run_text_tail(path: &Path, max_bytes: usize) -> Result<String, ArtifactError> {
    if max_bytes == 0 { return Err(ArtifactError::InvalidOutputLimit); }
    let mut file = match File::open(path) { Ok(file) => file, Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(String::new()), Err(error) => return Err(error.into()) };
    let size = file.metadata()?.len();
    let limit = u64::try_from(max_bytes).map_err(|_| ArtifactError::InvalidOutputLimit)?;
    file.seek(SeekFrom::Start(size.saturating_sub(limit)))?;
    let mut buffer = Vec::new(); file.read_to_end(&mut buffer)?;
    let text = String::from_utf8_lossy(&buffer);
    Ok(if size > limit { format!("[truncated to last {max_bytes} bytes]\n{text}") } else { text.into_owned() })
}
pub fn update_run_ledger(path: &Path, fields: &serde_json::Map<String, serde_json::Value>) -> Result<(), ArtifactError> {
    let mut current: serde_json::Map<String, serde_json::Value> = read_run_json(path)?;
    current.extend(fields.clone()); write_run_json_atomic(path, &current, 0o600)
}
pub fn unlink_run_artifact(path: &Path) -> Result<(), ArtifactError> {
    match std::fs::remove_file(path) { Ok(()) => Ok(()), Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()), Err(error) => Err(error.into()) }
}
pub fn parse_run_prelaunch_artifact(value: serde_json::Value) -> Result<RunPrelaunchArtifact, ArtifactError> {
    let artifact: RunPrelaunchArtifact = serde_json::from_value(value).map_err(|_| ArtifactError::InvalidPrelaunch)?;
    if artifact.version != 1 { return Err(ArtifactError::InvalidPrelaunch); }
    Ok(artifact)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn durable_json_round_trip_and_update() {
        let root = tempfile::tempdir().unwrap(); let path = root.path().join("ledger.json");
        write_run_json_atomic(&path, &serde_json::json!({"attempt": 1}), 0o600).unwrap();
        update_run_ledger(&path, serde_json::json!({"attempt": 2, "runId": "run-1"}).as_object().unwrap()).unwrap();
        let value: serde_json::Value = read_run_json(&path).unwrap(); assert_eq!(value["attempt"], 2); assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 1);
    }
    #[test]
    fn tail_is_bounded_and_missing_is_empty() {
        let root = tempfile::tempdir().unwrap(); let path = root.path().join("stdout.log"); assert_eq!(read_run_text_tail(&path, 3).unwrap(), "");
        std::fs::write(&path, "abcdef").unwrap(); assert_eq!(read_run_text_tail(&path, 3).unwrap(), "[truncated to last 3 bytes]\ndef"); assert!(read_run_text_tail(&path, 0).is_err());
    }
    #[test]
    fn prelaunch_rejects_corruption() { assert!(parse_run_prelaunch_artifact(serde_json::json!({"version": 2})).is_err()); }
}
