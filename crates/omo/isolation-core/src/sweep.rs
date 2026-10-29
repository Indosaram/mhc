use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::backend::{BackendKind, IsolationBackend, Result, BACKEND_FILE};
use crate::owner::{read_owner_liveness, OwnerLiveness, OwnerProbe};
use crate::process_identity::process_owner_probe;
use crate::util::now_ms;

pub type BackendRef = Arc<dyn IsolationBackend>;

#[derive(Default)]
pub struct SweepOptions {
    pub backends: Vec<BackendRef>,
    pub probe: Option<Arc<dyn OwnerProbe>>,
    pub now: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkippedEntry {
    pub path: PathBuf,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SweepResult {
    pub reclaimed: Vec<PathBuf>,
    pub kept: Vec<PathBuf>,
    pub skipped: Vec<SkippedEntry>,
}

fn is_isolation_dir_name(name: &str) -> bool {
    let Some(rest) = name.strip_prefix('t') else {
        return false;
    };
    if rest.len() < 10 {
        return false;
    }
    let (head, tail) = rest.split_at(10);
    if !head
        .bytes()
        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return false;
    }
    if tail.is_empty() {
        return true;
    }
    if let Some(suffix) = tail.strip_prefix(".creating-") {
        return !suffix.is_empty() && suffix.bytes().all(|byte| byte.is_ascii_digit());
    }
    if let Some(suffix) = tail.strip_prefix(".retained-") {
        return !suffix.is_empty()
            && suffix
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-');
    }
    false
}

fn sweep_entry(
    path: &Path,
    probe: &dyn OwnerProbe,
    now: u64,
    backends: &[BackendRef],
    result: &mut SweepResult,
) -> Result<()> {
    let liveness = read_owner_liveness(path, probe, now)?;
    if liveness != OwnerLiveness::Dead && liveness != OwnerLiveness::Reclaimable {
        result.kept.push(path.to_path_buf());
        return Ok(());
    }
    let text = std::fs::read_to_string(path.join(BACKEND_FILE))?;
    let record: serde_json::Value = serde_json::from_str(&text)
        .map_err(|error| crate::backend::IsolationError::other(error.to_string()))?;
    let Some(kind) = record
        .get("backend")
        .and_then(|backend| backend.as_str())
        .and_then(BackendKind::parse)
    else {
        result.skipped.push(SkippedEntry {
            path: path.to_path_buf(),
            reason: "invalid backend marker".to_string(),
        });
        return Ok(());
    };
    let Some(backend) = backends.iter().find(|backend| backend.kind() == kind) else {
        result.skipped.push(SkippedEntry {
            path: path.to_path_buf(),
            reason: format!("No implementation for {}", kind.as_str()),
        });
        return Ok(());
    };
    backend.stop(&path.join("m"))?;
    std::fs::remove_dir_all(path)?;
    result.reclaimed.push(path.to_path_buf());
    Ok(())
}

pub fn sweep_stale_isolations(root_dirs: &[PathBuf], options: &SweepOptions) -> Result<SweepResult> {
    let mut result = SweepResult::default();
    let mut seen: HashSet<PathBuf> = HashSet::new();
    let default_probe = process_owner_probe();
    let probe: &dyn OwnerProbe = match &options.probe {
        Some(probe) => probe.as_ref(),
        None => &default_probe,
    };
    let now = options.now.unwrap_or_else(now_ms);
    for root in root_dirs {
        if !seen.insert(root.clone()) {
            continue;
        }
        let entries = match std::fs::read_dir(root) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                result.skipped.push(SkippedEntry {
                    path: root.clone(),
                    reason: error.to_string(),
                });
                continue;
            }
        };
        for entry in entries {
            let Ok(entry) = entry else { continue };
            let name = entry.file_name().to_string_lossy().into_owned();
            if !is_isolation_dir_name(&name) {
                continue;
            }
            let path = root.join(&name);
            let is_dir = entry.file_type().map(|kind| kind.is_dir()).unwrap_or(false);
            if !is_dir {
                result.skipped.push(SkippedEntry {
                    path,
                    reason: "not a directory".to_string(),
                });
                continue;
            }
            if let Err(error) = sweep_entry(&path, probe, now, &options.backends, &mut result) {
                result.skipped.push(SkippedEntry {
                    path,
                    reason: error.to_string(),
                });
            }
        }
    }
    Ok(result)
}
