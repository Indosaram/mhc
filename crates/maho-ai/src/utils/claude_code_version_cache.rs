//! Port of senpi packages/ai/src/utils/claude-code-version-cache.ts.
//!
//! Node entry point for the Claude Code version cache. `claude_code_version.rs` keeps no state on
//! disk; hosts with a filesystem call [`install_claude_code_version_file_store`] once at startup so
//! the resolved version survives process restarts and one lookup per six hours serves every session.
//!
//! The agent-dir env override and default carry the maho brand like `cursor_context_limit.rs`:
//! `MAHO_CODING_AGENT_DIR`, then `CODING_AGENT_DIR`, then `~/.omo/agent`.

use super::claude_code_version::{
    CachedClaudeCodeVersion, ClaudeCodeVersionStore, install_claude_code_version_store,
};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

const CACHE_FILE: &str = "claude-code-version.json";

fn trim_trailing_slash(path: &str) -> &str {
    path.strip_suffix('/').unwrap_or(path)
}

/// Same resolution as the other maho-ai stores: the configured agent directory, then the default.
pub fn resolve_claude_code_version_cache_path(env: &HashMap<String, String>) -> PathBuf {
    let agent_dir = env
        .get("MAHO_CODING_AGENT_DIR")
        .or_else(|| env.get("CODING_AGENT_DIR"))
        .cloned()
        .unwrap_or_else(|| {
            let home = env.get("HOME").map_or(".", String::as_str);
            format!("{}/.omo/agent", trim_trailing_slash(home))
        });
    PathBuf::from(format!("{}/{CACHE_FILE}", trim_trailing_slash(&agent_dir)))
}

struct FileStore {
    path: PathBuf,
    saving_enabled: AtomicBool,
}

fn parse_cached(value: &Value) -> Option<CachedClaudeCodeVersion> {
    Some(CachedClaudeCodeVersion {
        version: value.get("version")?.as_str()?.to_owned(),
        checked_at: value.get("checkedAt")?.as_f64()? as i64,
    })
}

fn save(path: &Path, record: &CachedClaudeCodeVersion) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut temporary = path.as_os_str().to_owned();
    temporary.push(format!(".{}.tmp", std::process::id()));
    let body = json!({"version": record.version, "checkedAt": record.checked_at});
    std::fs::write(&temporary, format!("{body}\n"))?;
    std::fs::rename(&temporary, path)
}

impl ClaudeCodeVersionStore for FileStore {
    fn load(&self) -> Option<CachedClaudeCodeVersion> {
        // No file on first run, and a truncated or hand-edited one is the same situation for a
        // cache: the bundled floor is the documented fallback.
        let text = std::fs::read_to_string(&self.path).ok()?;
        parse_cached(&serde_json::from_str(&text).ok()?)
    }

    fn save(&self, record: CachedClaudeCodeVersion) {
        if !self.saving_enabled.load(Ordering::Relaxed) {
            return;
        }
        // A read-only agent directory must not fail the live request, and one failure disables
        // saving so a broken path cannot cost a write per refresh.
        if save(&self.path, &record).is_err() {
            self.saving_enabled.store(false, Ordering::Relaxed);
        }
    }
}

pub fn create_claude_code_version_file_store(
    path: impl Into<PathBuf>,
) -> Arc<dyn ClaudeCodeVersionStore> {
    Arc::new(FileStore {
        path: path.into(),
        saving_enabled: AtomicBool::new(true),
    })
}

#[derive(Debug, Clone, Default)]
pub struct InstallClaudeCodeVersionFileStoreOptions {
    /// Cache file; defaults to `<agent dir>/claude-code-version.json`.
    pub path: Option<PathBuf>,
    /// Serve the cache but never fetch (host offline mode); defaults to `PI_OFFLINE`.
    pub offline: Option<bool>,
}

pub fn install_claude_code_version_file_store(options: InstallClaudeCodeVersionFileStoreOptions) {
    let env: HashMap<String, String> = std::env::vars().collect();
    let offline = options.offline.unwrap_or_else(|| {
        env.get("PI_OFFLINE")
            .is_some_and(|value| !value.is_empty() && value != "0")
    });
    let path = options
        .path
        .unwrap_or_else(|| resolve_claude_code_version_cache_path(&env));
    install_claude_code_version_store(Some(create_claude_code_version_file_store(path)), offline);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect()
    }

    #[test]
    fn resolves_the_cache_path_from_the_agent_dir() {
        assert_eq!(
            resolve_claude_code_version_cache_path(&env(&[
                ("MAHO_CODING_AGENT_DIR", "/a/"),
                ("CODING_AGENT_DIR", "/b")
            ])),
            PathBuf::from("/a/claude-code-version.json")
        );
        assert_eq!(
            resolve_claude_code_version_cache_path(&env(&[("CODING_AGENT_DIR", "/b")])),
            PathBuf::from("/b/claude-code-version.json")
        );
        assert_eq!(
            resolve_claude_code_version_cache_path(&env(&[("HOME", "/home/u/")])),
            PathBuf::from("/home/u/.omo/agent/claude-code-version.json")
        );
    }

    #[test]
    fn round_trips_records_and_treats_corrupt_files_as_absent() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("nested/claude-code-version.json");
        let store = create_claude_code_version_file_store(&path);
        assert!(store.load().is_none());
        store.save(CachedClaudeCodeVersion {
            version: "2.1.3".into(),
            checked_at: 42,
        });
        let loaded = store.load().expect("saved record");
        assert_eq!((loaded.version.as_str(), loaded.checked_at), ("2.1.3", 42));
        std::fs::write(&path, "{\"version\":").unwrap();
        assert!(store.load().is_none());
        std::fs::write(&path, "{\"version\":1,\"checkedAt\":2}").unwrap();
        assert!(store.load().is_none());
    }

    #[test]
    fn disables_saving_after_the_first_write_failure() {
        let dir = tempfile::tempdir().expect("tempdir");
        let blocker = dir.path().join("file");
        std::fs::write(&blocker, "x").unwrap();
        let path = blocker.join("claude-code-version.json");
        let store = create_claude_code_version_file_store(&path);
        store.save(CachedClaudeCodeVersion {
            version: "2.0.0".into(),
            checked_at: 1,
        });
        std::fs::remove_file(&blocker).unwrap();
        store.save(CachedClaudeCodeVersion {
            version: "2.0.1".into(),
            checked_at: 2,
        });
        assert!(!path.exists());
    }
}
