use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::{
    collections::BTreeMap,
    fs::{File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::Path,
};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunAttempt {
    pub attempt: u32,
    pub model: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking: Option<String>,
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RunKind {
    Reflection,
    Dream,
    Facts,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunLaunchManifest {
    pub version: u32,
    pub run_id: String,
    pub attempt: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_attempt: Option<RunAttempt>,
    pub kind: RunKind,
    pub command: String,
    pub args: Vec<String>,
    pub cwd: String,
    pub env: BTreeMap<String, String>,
    pub hard_deadline_at: f64,
    /// When the launcher queued the run; the supervisor rebases the deadlines by the queue duration.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub queued_at: Option<f64>,
    /// The machine-wide memory root the supervisor takes its admission lease under.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub memory_root: Option<String>,
    /// The configured machine-wide reflection capacity (`memory.reflection.max_concurrent_runs`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_concurrent_runs: Option<usize>,
    pub termination_grace_ms: f64,
    pub max_output_bytes: usize,
    pub stdout_path: String,
    pub stderr_path: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunPrelaunchArtifact {
    pub version: u32,
    pub run_id: String,
    pub worktree_dir: String,
    pub worktree_branch: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ChildExit {
    pub code: Option<i32>,
    pub signal: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunOutcome {
    pub version: u32,
    pub run_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attempt: Option<u32>,
    pub finished_at: String,
    pub child_exit: ChildExit,
    pub timed_out: bool,
    /// Published by recovery from a validated worktree tip, not by a supervisor that saw the child exit.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recovered_from_worktree: Option<bool>,
}
pub const CHILD_EXIT_FILENAME: &str = "child-exit.json";
/// Written by the child bootstrap the moment the model child exits, before it reports to the supervisor.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunChildExit {
    pub version: u32,
    pub run_id: String,
    pub attempt: u32,
    pub code: Option<i32>,
    pub signal: Option<String>,
    pub finished_at: String,
    /// The bootstrap's clock had reached the hard deadline, so its own enforcement may have ended the child.
    pub timed_out: bool,
}
pub fn run_outcome_matches_ledger(attempt: Option<u32>, outcome: &RunOutcome) -> bool {
    attempt == outcome.attempt
}
#[derive(Debug)]
pub enum ArtifactError {
    Io(std::io::Error),
    Json(serde_json::Error),
    InvalidPrelaunch,
    InvalidOutputLimit,
}
impl std::fmt::Display for ArtifactError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => error.fmt(f),
            Self::Json(error) => error.fmt(f),
            Self::InvalidPrelaunch => f.write_str("Invalid reflection prelaunch artifact"),
            Self::InvalidOutputLimit => f.write_str("run output limit must be positive"),
        }
    }
}
impl std::error::Error for ArtifactError {}
impl From<std::io::Error> for ArtifactError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}
impl From<serde_json::Error> for ArtifactError {
    fn from(error: serde_json::Error) -> Self {
        Self::Json(error)
    }
}
pub fn read_run_json<T: DeserializeOwned>(path: &Path) -> Result<T, ArtifactError> {
    Ok(serde_json::from_slice(&std::fs::read(path)?)?)
}
fn write_and_sync<T: Serialize>(file: &mut File, value: &T) -> Result<(), ArtifactError> {
    file.write_all(serde_json::to_string_pretty(value)?.as_bytes())?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    Ok(())
}
pub fn write_run_json_atomic<T: Serialize>(
    path: &Path,
    value: &T,
    mode: u32,
) -> Result<(), ArtifactError> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(mode);
    }
    #[cfg(not(unix))]
    let _ = mode;
    let temporary = path.with_file_name(format!(
        "{}.tmp-{}-{}",
        path.file_name().unwrap_or_default().to_string_lossy(),
        std::process::id(),
        memory_core::support::random::random_uuid()
    ));
    let mut file = options.open(&temporary)?;
    let written = write_and_sync(&mut file, value);
    drop(file);
    let renamed =
        written.and_then(|()| std::fs::rename(&temporary, path).map_err(ArtifactError::from));
    if renamed.is_err() {
        let _ = unlink_run_artifact(&temporary);
    }
    renamed?;
    match File::open(path.parent().unwrap_or(Path::new(".")))
        .and_then(|directory| directory.sync_all())
    {
        Ok(()) => {}
        #[cfg(windows)]
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::PermissionDenied | std::io::ErrorKind::IsADirectory
            ) => {}
        Err(error) => return Err(error.into()),
    }
    Ok(())
}
pub fn read_run_text_tail(path: &Path, max_bytes: usize) -> Result<String, ArtifactError> {
    if max_bytes == 0 {
        return Err(ArtifactError::InvalidOutputLimit);
    }
    let mut file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(String::new()),
        Err(error) => return Err(error.into()),
    };
    let size = file.metadata()?.len();
    let limit = u64::try_from(max_bytes).map_err(|_| ArtifactError::InvalidOutputLimit)?;
    file.seek(SeekFrom::Start(size.saturating_sub(limit)))?;
    let mut buffer = Vec::new();
    file.take(size.min(limit)).read_to_end(&mut buffer)?;
    let text = String::from_utf8_lossy(&buffer);
    Ok(if size > limit {
        format!("[truncated to last {max_bytes} bytes]\n{text}")
    } else {
        text.into_owned()
    })
}
pub fn update_run_ledger(
    path: &Path,
    fields: &serde_json::Map<String, serde_json::Value>,
) -> Result<(), ArtifactError> {
    let mut current: serde_json::Map<String, serde_json::Value> = read_run_json(path)?;
    current.extend(fields.clone());
    write_run_json_atomic(path, &current, 0o600)
}
pub fn unlink_run_artifact(path: &Path) -> Result<(), ArtifactError> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}
pub fn parse_run_prelaunch_artifact(
    value: serde_json::Value,
) -> Result<RunPrelaunchArtifact, ArtifactError> {
    let artifact: RunPrelaunchArtifact =
        serde_json::from_value(value).map_err(|_| ArtifactError::InvalidPrelaunch)?;
    if artifact.version != 1 {
        return Err(ArtifactError::InvalidPrelaunch);
    }
    Ok(artifact)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn durable_json_round_trip_and_update() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("ledger.json");
        write_run_json_atomic(&path, &serde_json::json!({"attempt": 1}), 0o600).unwrap();
        update_run_ledger(
            &path,
            serde_json::json!({"attempt": 2, "runId": "run-1"})
                .as_object()
                .unwrap(),
        )
        .unwrap();
        let value: serde_json::Value = read_run_json(&path).unwrap();
        assert_eq!(value["attempt"], 2);
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 1);
    }
    #[test]
    fn tail_is_bounded_and_missing_is_empty() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("stdout.log");
        assert_eq!(read_run_text_tail(&path, 3).unwrap(), "");
        std::fs::write(&path, "abcdef").unwrap();
        assert_eq!(
            read_run_text_tail(&path, 3).unwrap(),
            "[truncated to last 3 bytes]\ndef"
        );
        assert!(read_run_text_tail(&path, 0).is_err());
    }
    #[test]
    fn outcome_recovery_marker_is_optional_and_round_trips() {
        let outcome = RunOutcome {
            version: 1,
            run_id: "run".into(),
            attempt: Some(1),
            finished_at: "now".into(),
            child_exit: ChildExit {
                code: Some(0),
                signal: None,
            },
            timed_out: false,
            recovered_from_worktree: Some(true),
        };
        let value = serde_json::to_value(&outcome).unwrap();
        assert_eq!(value["recoveredFromWorktree"], true);
        assert_eq!(
            serde_json::from_value::<RunOutcome>(value)
                .unwrap()
                .recovered_from_worktree,
            Some(true)
        );
        let plain = RunOutcome {
            recovered_from_worktree: None,
            ..outcome
        };
        assert!(
            serde_json::to_value(&plain)
                .unwrap()
                .get("recoveredFromWorktree")
                .is_none()
        );
    }
    #[test]
    fn prelaunch_rejects_corruption() {
        assert!(parse_run_prelaunch_artifact(serde_json::json!({"version": 2})).is_err());
    }
    #[test]
    fn launch_manifest_carries_the_admission_protocol() {
        let manifest = RunLaunchManifest {
            version: 1,
            run_id: "run".into(),
            attempt: 1,
            next_attempt: None,
            kind: RunKind::Reflection,
            command: "child".into(),
            args: vec![],
            cwd: "/w".into(),
            env: BTreeMap::new(),
            hard_deadline_at: 100.0,
            queued_at: Some(90.0),
            memory_root: Some("/memory".into()),
            max_concurrent_runs: Some(2),
            termination_grace_ms: 10.0,
            max_output_bytes: 100,
            stdout_path: "stdout".into(),
            stderr_path: "stderr".into(),
        };
        let value = serde_json::to_value(&manifest).unwrap();
        assert_eq!(value["queuedAt"], 90.0);
        assert_eq!(value["memoryRoot"], "/memory");
        assert_eq!(value["maxConcurrentRuns"], 2);
        let round: RunLaunchManifest = serde_json::from_value(value).unwrap();
        assert_eq!(round.queued_at, Some(90.0));
        assert_eq!(round.memory_root.as_deref(), Some("/memory"));
        assert_eq!(round.max_concurrent_runs, Some(2));
    }
    #[test]
    fn a_manifest_without_admission_metadata_omits_it() {
        let manifest = RunLaunchManifest {
            version: 1,
            run_id: "run".into(),
            attempt: 1,
            next_attempt: None,
            kind: RunKind::Facts,
            command: "child".into(),
            args: vec![],
            cwd: "/w".into(),
            env: BTreeMap::new(),
            hard_deadline_at: 100.0,
            queued_at: None,
            memory_root: None,
            max_concurrent_runs: None,
            termination_grace_ms: 10.0,
            max_output_bytes: 100,
            stdout_path: "stdout".into(),
            stderr_path: "stderr".into(),
        };
        let value = serde_json::to_value(&manifest).unwrap();
        for key in ["queuedAt", "memoryRoot", "maxConcurrentRuns"] {
            assert!(value.get(key).is_none(), "{key}");
        }
    }
}
