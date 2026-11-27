//! Port of senpi packages/coding-agent/src/core/shared-session-stores.ts.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::config::get_agent_dir;
use crate::paths::{PathInputOptions, normalize_path};
use crate::session_discovery::{SessionInfo, list_sessions_from_dir_with_index_dir};

const SHARED_SESSION_STORES_DIR_NAME: &str = "session-stores";
const SHARED_SESSION_STORES_VERSION: u64 = 1;

fn get_shared_session_stores_dir(agent_dir: Option<&str>) -> PathBuf {
    let base = agent_dir.map_or_else(|| PathBuf::from(get_agent_dir()), PathBuf::from);
    base.join(SHARED_SESSION_STORES_DIR_NAME)
}

fn registered_root(file: &Path) -> Option<String> {
    let raw = std::fs::read_to_string(file).ok()?;
    let entry: Value = serde_json::from_str(&raw).ok()?;
    if entry.get("version").and_then(Value::as_u64) != Some(SHARED_SESSION_STORES_VERSION) {
        return None;
    }
    let root = entry.get("root").and_then(Value::as_str)?;
    if !Path::new(root).is_absolute() {
        return None;
    }
    Some(normalize_path(root, &PathInputOptions::default()))
}

fn registered_roots(agent_dir: Option<&str>) -> HashSet<String> {
    let stores_dir = get_shared_session_stores_dir(agent_dir);
    let mut roots = HashSet::new();
    let Ok(entries) = std::fs::read_dir(stores_dir) else {
        return roots;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if entry.file_type().is_ok_and(|ft| ft.is_file())
            && path.extension().and_then(|e| e.to_str()) == Some("json")
            && let Some(root) = registered_root(&path)
        {
            roots.insert(root);
        }
    }
    roots
}

pub fn is_shared_session_store_dir(dir: &str, agent_dir: Option<&str>) -> bool {
    let Some(parent) = Path::new(dir).parent() else {
        return false;
    };
    let normalized = normalize_path(&parent.to_string_lossy(), &PathInputOptions::default());
    registered_roots(agent_dir).contains(&normalized)
}

pub fn shared_session_store_dirs(agent_dir: Option<&str>) -> Vec<String> {
    let mut dirs = Vec::new();
    let mut seen = HashSet::new();
    for root in registered_roots(agent_dir) {
        let Ok(entries) = std::fs::read_dir(&root) else {
            continue;
        };
        for entry in entries.flatten() {
            if entry.file_type().is_ok_and(|ft| ft.is_dir()) {
                let p = entry.path().to_string_lossy().into_owned();
                if seen.insert(p.clone()) {
                    dirs.push(p);
                }
            }
        }
    }
    dirs
}

pub fn shared_store_summary_dir(dir: &str, agent_dir: Option<&str>) -> String {
    let normalized = normalize_path(dir, &PathInputOptions::default());
    let key = &hex::encode(Sha256::digest(normalized.as_bytes()))[..32];
    let base = agent_dir.map_or_else(|| PathBuf::from(get_agent_dir()), PathBuf::from);
    base.join("cache")
        .join("session-store-summaries")
        .join(key)
        .to_string_lossy()
        .into_owned()
}

pub fn list_shared_store_sessions(
    agent_dir: Option<&str>,
    include: impl Fn(&SessionInfo) -> bool,
) -> Vec<SessionInfo> {
    let mut sessions = Vec::new();
    for dir in shared_session_store_dirs(agent_dir) {
        let summary_dir = shared_store_summary_dir(&dir, agent_dir);
        let listed = list_sessions_from_dir_with_index_dir(&dir, None, 0, None, Some(&summary_dir));
        sessions.extend(listed.into_iter().filter(|s| include(s)));
    }
    sessions
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn discovers_registered_shared_session_store_and_isolates_summary_index() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let agent_dir = tmp.path().join("agent");
        let store_root = tmp.path().join("desktop-store");
        let store_project = store_root.join("--proj--");
        std::fs::create_dir_all(agent_dir.join("session-stores")).expect("mkdir");
        std::fs::create_dir_all(&store_project).expect("mkdir");

        std::fs::write(
            agent_dir.join("session-stores/desktop.json"),
            json!({ "version": 1, "root": store_root.to_string_lossy() }).to_string(),
        )
        .expect("write");

        let agent_str = agent_dir.to_string_lossy().into_owned();
        assert!(is_shared_session_store_dir(
            &store_project.to_string_lossy(),
            Some(&agent_str)
        ));
        let dirs = shared_session_store_dirs(Some(&agent_str));
        assert_eq!(dirs.len(), 1);

        let summary_dir = shared_store_summary_dir(&dirs[0], Some(&agent_str));
        assert!(summary_dir.contains("session-store-summaries"));
    }
}
