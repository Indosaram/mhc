//! Port of senpi packages/coding-agent/src/core/trust-manager.ts.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::config::config_dir_name;
use crate::linked_worktree_checkout::linked_worktree_main_checkout_dir;
use crate::lockfile_policy::{
    FILE_STORAGE_SYNC_LOCK_BUDGET_MS, acquire_lock_async, acquire_lock_sync,
};
use crate::paths::{PathInputOptions, canonicalize_path, canonicalize_path_strict, resolve_path};
use crate::text::strip_bom;
use crate::trust_store_writers::{
    in_trust_store_lock_scope, in_trust_store_write_turn, trust_store_lock_held_in_realm,
};

pub type ProjectTrustDecision = Option<bool>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectTrustStoreEntry {
    pub path: String,
    pub decision: bool,
}

/// The nearest entry at or above a folder; `decision: None` is an explicit
/// "no decision, none inherited" entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectTrustNearestEntry {
    pub path: String,
    pub decision: ProjectTrustDecision,
}

/// What is saved for a folder: a decision, `NoInherit` for the explicit no-decision
/// entry (`set_project_trust`'s `no_inherit`), or `None` when nothing is saved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SavedProjectTrust {
    Trusted,
    Untrusted,
    NoInherit,
    None,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectTrustUpdate {
    pub path: String,
    pub decision: ProjectTrustDecision,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectTrustOption {
    pub label: String,
    pub trusted: bool,
    pub updates: Vec<ProjectTrustUpdate>,
    pub saved_path: Option<String>,
}

pub const TRUST_REQUIRING_PROJECT_CONFIG_RESOURCES: [&str; 7] = [
    "settings.json",
    "extensions",
    "skills",
    "prompts",
    "themes",
    "SYSTEM.md",
    "APPEND_SYSTEM.md",
];

fn resolve(input: &str) -> String {
    resolve_path(input, input, &PathInputOptions::default())
}

/// A workspace path the filesystem will not confirm must not be keyed by its raw spelling.
pub fn normalize_cwd(cwd: &str) -> String {
    let resolved = resolve(cwd);
    canonicalize_path_strict(&resolved).unwrap_or(resolved)
}

fn parent_dir(path: &str) -> Option<String> {
    let parent = Path::new(path).parent()?.to_string_lossy().into_owned();
    if parent == path { None } else { Some(parent) }
}

/// The nearest entry at or above `cwd`. An explicit `null` saved for a folder
/// (`set_project_trust`'s `no_inherit`) is an entry as well: it holds no decision,
/// and the walk stops there, so neither a folder above it nor, for a linked worktree,
/// the main checkout decides for it.
pub fn find_nearest_trust_entry_or_no_inherit(
    data: &BTreeMap<String, Option<bool>>,
    cwd: &str,
) -> Option<ProjectTrustNearestEntry> {
    let mut current_dir = normalize_cwd(cwd);
    loop {
        if let Some(value) = data.get(&current_dir) {
            return Some(ProjectTrustNearestEntry {
                path: current_dir,
                decision: *value,
            });
        }
        match parent_dir(&current_dir) {
            Some(parent) => current_dir = parent,
            None => return None,
        }
    }
}

pub fn find_nearest_trust_entry(
    data: &BTreeMap<String, Option<bool>>,
    cwd: &str,
) -> Option<ProjectTrustStoreEntry> {
    let entry = find_nearest_trust_entry_or_no_inherit(data, cwd)?;
    entry.decision.map(|decision| ProjectTrustStoreEntry {
        path: entry.path,
        decision,
    })
}

pub fn get_project_trust_parent_path(cwd: &str) -> Option<String> {
    let trust_path = normalize_cwd(cwd);
    parent_dir(&trust_path)
}

pub fn get_project_trust_options(cwd: &str, include_session_only: bool) -> Vec<ProjectTrustOption> {
    let trust_path = normalize_cwd(cwd);
    let mut options = vec![ProjectTrustOption {
        label: "Trust".to_owned(),
        trusted: true,
        updates: vec![ProjectTrustUpdate {
            path: trust_path.clone(),
            decision: Some(true),
        }],
        saved_path: Some(trust_path.clone()),
    }];
    if let Some(parent_path) = get_project_trust_parent_path(cwd) {
        options.push(ProjectTrustOption {
            label: format!("Trust parent folder ({parent_path})"),
            trusted: true,
            updates: vec![
                ProjectTrustUpdate {
                    path: parent_path.clone(),
                    decision: Some(true),
                },
                ProjectTrustUpdate {
                    path: trust_path.clone(),
                    decision: None,
                },
            ],
            saved_path: Some(parent_path),
        });
    }
    if include_session_only {
        options.push(ProjectTrustOption {
            label: "Trust (this session only)".to_owned(),
            trusted: true,
            updates: Vec::new(),
            saved_path: None,
        });
    }
    options.push(ProjectTrustOption {
        label: "Do not trust".to_owned(),
        trusted: false,
        updates: vec![ProjectTrustUpdate {
            path: trust_path.clone(),
            decision: Some(false),
        }],
        saved_path: Some(trust_path),
    });
    if include_session_only {
        options.push(ProjectTrustOption {
            label: "Do not trust (this session only)".to_owned(),
            trusted: false,
            updates: Vec::new(),
            saved_path: None,
        });
    }
    options
}

pub type TrustFile = BTreeMap<String, Option<bool>>;

pub fn read_trust_file(path: &str) -> Result<TrustFile, String> {
    if !Path::new(path).exists() {
        return Ok(TrustFile::new());
    }
    let content = std::fs::read_to_string(path)
        .map_err(|error| format!("Failed to read trust store {path}: {error}"))?;
    let parsed: Value = serde_json::from_str(strip_bom(&content))
        .map_err(|error| format!("Failed to read trust store {path}: {error}"))?;
    let Some(object) = parsed.as_object() else {
        return Err(format!("Invalid trust store {path}: expected an object"));
    };
    let mut data = TrustFile::new();
    for (key, value) in object {
        let decision = match value {
            Value::Bool(decision) => Some(*decision),
            Value::Null => None,
            _ => {
                return Err(format!(
                    "Invalid trust store {path}: value for {} must be true, false, or null",
                    serde_json::to_string(key).unwrap_or_else(|_| format!("\"{key}\""))
                ));
            }
        };
        data.insert(key.clone(), decision);
    }
    Ok(data)
}

pub fn write_trust_file(path: &str, data: &TrustFile) -> Result<(), String> {
    let mut sorted = serde_json::Map::new();
    for (key, value) in data {
        sorted.insert(key.clone(), value.map(Value::Bool).unwrap_or(Value::Null));
    }
    if let Some(parent) = Path::new(path).parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("Failed to write trust store {path}: {error}"))?;
    }
    let mut serialized = serde_json::to_string_pretty(&Value::Object(sorted))
        .map_err(|error| format!("Failed to write trust store {path}: {error}"))?;
    serialized.push('\n');
    std::fs::write(path, serialized)
        .map_err(|error| format!("Failed to write trust store {path}: {error}"))
}

pub fn with_trust_file_lock<T>(path: &str, action: impl FnOnce() -> T) -> Result<T, String> {
    if trust_store_lock_held_in_realm(path) {
        return Ok(action());
    }
    if let Some(parent) = Path::new(path).parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _guard = acquire_lock_sync(path, FILE_STORAGE_SYNC_LOCK_BUDGET_MS)
        .map_err(|error| error.message())?;
    Ok(action())
}

pub async fn with_trust_file_write_lock<T, F, Fut>(path: &str, write: F) -> Result<T, String>
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = Result<T, String>>,
{
    in_trust_store_write_turn(path, move || async move {
        if let Some(parent) = Path::new(path).parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _guard = acquire_lock_async(path, FILE_STORAGE_SYNC_LOCK_BUDGET_MS)
            .await
            .map_err(|error| error.message())?;
        in_trust_store_lock_scope(path, async move { write().await }).await
    })
    .await
}

fn is_home_or_above_dir(dir: &str, home_dir: &str) -> bool {
    if dir == home_dir {
        return true;
    }
    let prefix = if dir.ends_with(std::path::MAIN_SEPARATOR) {
        dir.to_owned()
    } else {
        format!("{dir}{}", std::path::MAIN_SEPARATOR)
    };
    home_dir.starts_with(&prefix)
}

fn find_git_repo_root(dir: &str) -> Option<String> {
    let mut current_dir = dir.to_owned();
    while !Path::new(&current_dir).join(".git").exists() {
        current_dir = parent_dir(&current_dir)?;
    }
    Some(current_dir)
}

/// The project `.agents/skills` directories for `cwd`, nearest first: one per folder from `cwd` up
/// to the git root (or filesystem root), stopping below the user's home.
pub fn project_agents_skill_dirs(cwd: &str, home_dir: Option<&str>) -> Vec<String> {
    let home = home_dir
        .map(str::to_owned)
        .unwrap_or_else(crate::config::home_dir);
    let home_dir = canonicalize_path(&resolve(&home));
    let start_dir = resolve(cwd);
    let git_repo_root = find_git_repo_root(&start_dir);
    let mut dirs = Vec::new();
    let mut current_dir = start_dir;
    while !is_home_or_above_dir(&canonicalize_path(&current_dir), &home_dir) {
        dirs.push(
            PathBuf::from(&current_dir)
                .join(".agents")
                .join("skills")
                .to_string_lossy()
                .into_owned(),
        );
        if git_repo_root.as_deref() == Some(current_dir.as_str()) {
            break;
        }
        match parent_dir(&current_dir) {
            Some(parent) => current_dir = parent,
            None => break,
        }
    }
    dirs
}

/// True when cwd has project-local resources that must be gated by project trust.
pub fn has_trust_requiring_project_resources(cwd: &str, home_dir: Option<&str>) -> bool {
    let home = home_dir
        .map(str::to_owned)
        .unwrap_or_else(crate::config::home_dir);
    let home_dir = canonicalize_path(&resolve(&home));
    let user_agents_skills_dir = PathBuf::from(&home_dir)
        .join(".agents")
        .join("skills")
        .to_string_lossy()
        .into_owned();
    let mut current_dir = canonicalize_path(&resolve(cwd));

    let config_dir = PathBuf::from(&current_dir)
        .join(config_dir_name())
        .to_string_lossy()
        .into_owned();
    if TRUST_REQUIRING_PROJECT_CONFIG_RESOURCES
        .iter()
        .any(|entry| Path::new(&config_dir).join(entry).exists())
    {
        return true;
    }

    loop {
        let agents_skills_dir = PathBuf::from(&current_dir)
            .join(".agents")
            .join("skills")
            .to_string_lossy()
            .into_owned();
        if agents_skills_dir != user_agents_skills_dir && Path::new(&agents_skills_dir).exists() {
            return true;
        }
        match parent_dir(&current_dir) {
            Some(parent) => current_dir = parent,
            None => return false,
        }
    }
}

pub struct ProjectTrustStore {
    trust_path: String,
    generation_path: String,
}

impl ProjectTrustStore {
    pub fn new(agent_dir: &str) -> Self {
        let trust_path = Path::new(&resolve(agent_dir))
            .join("trust.json")
            .to_string_lossy()
            .into_owned();
        let generation_path = format!("{trust_path}.generation");
        Self {
            trust_path,
            generation_path,
        }
    }

    pub fn trust_path(&self) -> &str {
        &self.trust_path
    }

    fn read_generation_sync(&self) -> Result<u64, String> {
        read_generation_file(&self.generation_path)
    }

    /// Changes on every write made through `set_many` / `set_no_inherit` in any process or realm.
    pub fn revision(&self) -> Result<String, String> {
        let generation = self.read_generation_sync()?;
        let stats = match std::fs::metadata(&self.trust_path) {
            Ok(meta) => Some(meta),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
            Err(err) => {
                return Err(format!(
                    "Failed to stat trust store {}: {err}",
                    self.trust_path
                ));
            }
        };
        Ok(format_revision(generation, stats.as_ref()))
    }

    pub async fn revision_async(&self) -> Result<String, String> {
        let generation = match tokio::fs::read_to_string(&self.generation_path).await {
            Ok(text) => parse_generation(&text),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => 0,
            Err(err) => {
                return Err(format!(
                    "Failed to read trust generation {}: {err}",
                    self.generation_path
                ));
            }
        };
        let stats = match tokio::fs::metadata(&self.trust_path).await {
            Ok(meta) => Some(meta),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
            Err(err) => {
                return Err(format!(
                    "Failed to stat trust store {}: {err}",
                    self.trust_path
                ));
            }
        };
        Ok(format_revision(generation, stats.as_ref()))
    }

    pub fn get(&self, cwd: &str) -> Result<ProjectTrustDecision, String> {
        Ok(self
            .get_nearest_entry(cwd)?
            .and_then(|entry| entry.decision))
    }

    /// The nearest saved decision at or above `cwd`; `None` also when the nearest entry holds `null`.
    pub fn get_entry(&self, cwd: &str) -> Result<Option<ProjectTrustStoreEntry>, String> {
        Ok(self.get_nearest_entry(cwd)?.and_then(|entry| {
            entry.decision.map(|decision| ProjectTrustStoreEntry {
                path: entry.path,
                decision,
            })
        }))
    }

    pub fn get_nearest_entry(&self, cwd: &str) -> Result<Option<ProjectTrustNearestEntry>, String> {
        let lock_path = self.trust_path.clone();
        let trust_path = self.trust_path.clone();
        let cwd = cwd.to_owned();
        with_trust_file_lock(&lock_path, move || {
            let data = read_trust_file(&trust_path)?;
            Ok(find_nearest_trust_entry_or_no_inherit(&data, &cwd))
        })?
    }

    pub fn set(&self, cwd: &str, decision: ProjectTrustDecision) -> Result<(), String> {
        self.set_many(&[ProjectTrustUpdate {
            path: cwd.to_owned(),
            decision,
        }])
    }

    pub fn set_no_inherit(&self, cwd: &str) -> Result<(), String> {
        let lock_path = self.trust_path.clone();
        let trust_path = self.trust_path.clone();
        let generation_path = self.generation_path.clone();
        let key = normalize_cwd(cwd);
        with_trust_file_lock(&lock_path, move || {
            let mut data = read_trust_file(&trust_path)?;
            data.insert(key, None);
            bump_generation_file(&generation_path)?;
            write_trust_file(&trust_path, &data)
        })?
    }

    pub async fn set_async(&self, cwd: &str, decision: ProjectTrustDecision) -> Result<(), String> {
        self.set_many_async(&[ProjectTrustUpdate {
            path: cwd.to_owned(),
            decision,
        }])
        .await
    }

    pub async fn set_no_inherit_async(&self, cwd: &str) -> Result<(), String> {
        let lock_path = self.trust_path.clone();
        let trust_path = self.trust_path.clone();
        let generation_path = self.generation_path.clone();
        let key = normalize_cwd(cwd);
        with_trust_file_write_lock(&lock_path, move || async move {
            let mut data = read_trust_file(&trust_path)?;
            data.insert(key, None);
            bump_generation_file(&generation_path)?;
            write_trust_file(&trust_path, &data)
        })
        .await
    }

    pub fn set_many(&self, decisions: &[ProjectTrustUpdate]) -> Result<(), String> {
        let lock_path = self.trust_path.clone();
        let trust_path = self.trust_path.clone();
        let generation_path = self.generation_path.clone();
        let decisions = decisions.to_vec();
        with_trust_file_lock(&lock_path, move || {
            let mut data = read_trust_file(&trust_path)?;
            for update in &decisions {
                let key = normalize_cwd(&update.path);
                match update.decision {
                    None => {
                        data.remove(&key);
                    }
                    Some(decision) => {
                        data.insert(key, Some(decision));
                    }
                }
            }
            bump_generation_file(&generation_path)?;
            write_trust_file(&trust_path, &data)
        })?
    }

    pub async fn set_many_async(&self, decisions: &[ProjectTrustUpdate]) -> Result<(), String> {
        let lock_path = self.trust_path.clone();
        let trust_path = self.trust_path.clone();
        let generation_path = self.generation_path.clone();
        let decisions = decisions.to_vec();
        with_trust_file_write_lock(&lock_path, move || async move {
            let mut data = read_trust_file(&trust_path)?;
            for update in &decisions {
                let key = normalize_cwd(&update.path);
                match update.decision {
                    None => {
                        data.remove(&key);
                    }
                    Some(decision) => {
                        data.insert(key, Some(decision));
                    }
                }
            }
            bump_generation_file(&generation_path)?;
            write_trust_file(&trust_path, &data)
        })
        .await
    }
}

fn parse_generation(text: &str) -> u64 {
    text.trim().parse::<u64>().unwrap_or(0)
}

fn read_generation_file(path: &str) -> Result<u64, String> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(parse_generation(&text)),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(0),
        Err(err) => Err(format!("Failed to read trust generation {path}: {err}")),
    }
}

fn bump_generation_file(path: &str) -> Result<(), String> {
    if let Some(parent) = Path::new(path).parent() {
        std::fs::create_dir_all(parent)
            .map_err(|err| format!("Failed to create trust generation dir {path}: {err}"))?;
    }
    let next = read_generation_file(path)? + 1;
    let tmp = format!("{path}.next");
    std::fs::write(&tmp, format!("{next}\n"))
        .map_err(|err| format!("Failed to write trust generation {tmp}: {err}"))?;
    std::fs::rename(&tmp, path)
        .map_err(|err| format!("Failed to rename trust generation {tmp} -> {path}: {err}"))
}

fn format_revision(generation: u64, meta: Option<&std::fs::Metadata>) -> String {
    match meta {
        None => format!("{generation}:absent"),
        Some(meta) => {
            let mtime_ns = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map_or(0, |d| d.as_nanos());
            format!("{generation}:{mtime_ns}:{}", meta.len())
        }
    }
}

fn map_nearest_to_saved(entry: Option<ProjectTrustNearestEntry>) -> SavedProjectTrust {
    match entry {
        None => SavedProjectTrust::None,
        Some(ProjectTrustNearestEntry {
            decision: Some(true),
            ..
        }) => SavedProjectTrust::Trusted,
        Some(ProjectTrustNearestEntry {
            decision: Some(false),
            ..
        }) => SavedProjectTrust::Untrusted,
        Some(ProjectTrustNearestEntry { decision: None, .. }) => SavedProjectTrust::NoInherit,
    }
}

/// What is saved for `cwd`: the store's own entry, or for a linked git worktree, its main checkout's entry.
pub async fn get_saved_project_trust_state(
    store: &ProjectTrustStore,
    cwd: &str,
) -> Result<SavedProjectTrust, String> {
    let own = store.get_nearest_entry(cwd)?;
    if own.is_some() {
        return Ok(map_nearest_to_saved(own));
    }
    let Some(main_checkout_dir) = linked_worktree_main_checkout_dir(cwd).await else {
        return Ok(SavedProjectTrust::None);
    };
    Ok(map_nearest_to_saved(
        store.get_nearest_entry(&main_checkout_dir)?,
    ))
}

/// Synchronous version of `get_saved_project_trust_state`.
pub fn get_saved_project_trust_state_sync(
    store: &ProjectTrustStore,
    cwd: &str,
) -> Result<SavedProjectTrust, String> {
    let own = store.get_nearest_entry(cwd)?;
    if own.is_some() {
        return Ok(map_nearest_to_saved(own));
    }
    let Some(main_checkout_dir) =
        crate::linked_worktree_checkout::linked_worktree_main_checkout_dir_sync(cwd)
    else {
        return Ok(SavedProjectTrust::None);
    };
    Ok(map_nearest_to_saved(
        store.get_nearest_entry(&main_checkout_dir)?,
    ))
}

/// The saved decision for `cwd`, with `NoInherit` and `None` both returned as `None`.
pub async fn get_saved_project_trust(
    store: &ProjectTrustStore,
    cwd: &str,
) -> Result<ProjectTrustDecision, String> {
    Ok(match get_saved_project_trust_state(store, cwd).await? {
        SavedProjectTrust::Trusted => Some(true),
        SavedProjectTrust::Untrusted => Some(false),
        SavedProjectTrust::NoInherit | SavedProjectTrust::None => None,
    })
}

pub fn get_saved_project_trust_sync(
    store: &ProjectTrustStore,
    cwd: &str,
) -> Result<ProjectTrustDecision, String> {
    Ok(match get_saved_project_trust_state_sync(store, cwd)? {
        SavedProjectTrust::Trusted => Some(true),
        SavedProjectTrust::Untrusted => Some(false),
        SavedProjectTrust::NoInherit | SavedProjectTrust::None => None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn trust_lock_ownership_is_limited_to_the_acquiring_task() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("trust.json").to_string_lossy().into_owned();
        let path_for_write = path.clone();
        with_trust_file_write_lock(&path, move || async move {
            assert!(trust_store_lock_held_in_realm(&path_for_write));
            assert_eq!(
                with_trust_file_lock(&path_for_write, || 7).expect("nested"),
                7
            );
            let path_for_other = path_for_write.clone();
            let other =
                tokio::spawn(async move { trust_store_lock_held_in_realm(&path_for_other) })
                    .await
                    .expect("other task");
            assert!(!other, "another task must acquire its own file lock");
            Ok(())
        })
        .await
        .expect("write lock");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn explicit_trust_guard_does_not_authorize_another_task() {
        let path = "task-ownership-probe";
        let _guard = crate::trust_store_writers::hold_trust_store_lock_in_realm(path);
        assert!(trust_store_lock_held_in_realm(path));
        assert!(
            !tokio::spawn(async move { trust_store_lock_held_in_realm(path) })
                .await
                .expect("other task")
        );
    }

    #[tokio::test]
    async fn trust_lock_is_owned_while_write_future_is_created() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("trust.json").to_string_lossy().into_owned();
        let path_for_write = path.clone();
        with_trust_file_write_lock(&path, move || {
            let nested = if trust_store_lock_held_in_realm(&path_for_write) {
                with_trust_file_lock(&path_for_write, || ())
            } else {
                Err("lock ownership missing while creating future".to_owned())
            };
            async move { nested }
        })
        .await
        .expect("nested write");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn contended_async_trust_lock_does_not_block_the_executor() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("trust.json").to_string_lossy().into_owned();
        let holder_path = path.clone();
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let holder = std::thread::spawn(move || {
            let _guard = acquire_lock_sync(&holder_path, FILE_STORAGE_SYNC_LOCK_BUDGET_MS)
                .expect("holder lock");
            ready_tx.send(()).expect("holder ready");
            release_rx
                .recv_timeout(std::time::Duration::from_secs(5))
                .is_ok()
        });
        ready_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("holder ready");
        let (attempt_tx, attempt_rx) = tokio::sync::oneshot::channel();
        let waiter = tokio::spawn(async move {
            attempt_tx.send(()).expect("notify attempt");
            with_trust_file_write_lock(&path, || async { Ok(()) }).await
        });
        attempt_rx.await.expect("attempt started");
        release_tx.send(()).expect("release holder");
        assert!(
            holder.join().expect("holder thread"),
            "executor stopped making progress"
        );
        waiter.await.expect("waiter task").expect("waiter lock");
    }

    #[test]
    fn stores_decisions_and_inherits_from_parent_directories() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let agent_dir = tmp.path().join("agent").to_string_lossy().into_owned();
        let parent_dir = tmp.path().join("trusted-parent");
        let child_dir = parent_dir.join("project");
        std::fs::create_dir_all(&child_dir).expect("mkdir");
        let store = ProjectTrustStore::new(&agent_dir);
        let parent = parent_dir.to_string_lossy().into_owned();
        let child = child_dir.to_string_lossy().into_owned();

        assert_eq!(store.get(&child).expect("get"), None);
        store.set(&parent, Some(true)).expect("set");
        assert_eq!(store.get(&child).expect("get"), Some(true));
        store.set(&child, Some(false)).expect("set");
        assert_eq!(store.get(&child).expect("get"), Some(false));
        store.set(&child, None).expect("set");
        assert_eq!(store.get(&child).expect("get"), Some(true));
    }

    #[test]
    fn trust_file_is_written_sorted_with_two_space_indent() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("trust.json");
        let path = path.to_string_lossy().into_owned();
        let mut data = TrustFile::new();
        data.insert("/b".to_owned(), Some(true));
        data.insert("/a".to_owned(), None);
        write_trust_file(&path, &data).expect("write");
        let content = std::fs::read_to_string(&path).expect("read");
        assert_eq!(content, "{\n  \"/a\": null,\n  \"/b\": true\n}\n");
        assert_eq!(read_trust_file(&path).expect("read"), data);
    }

    #[test]
    fn refuses_a_malformed_trust_store() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("trust.json");
        let path_string = path.to_string_lossy().into_owned();
        std::fs::write(&path, "[]").expect("write");
        assert_eq!(
            read_trust_file(&path_string).expect_err("error"),
            format!("Invalid trust store {path_string}: expected an object")
        );
        std::fs::write(&path, "{\"/x\": 1}").expect("write");
        assert_eq!(
            read_trust_file(&path_string).expect_err("error"),
            format!(
                "Invalid trust store {path_string}: value for \"/x\" must be true, false, or null"
            )
        );
        std::fs::write(&path, "{oops").expect("write");
        assert!(
            read_trust_file(&path_string)
                .expect_err("error")
                .starts_with(&format!("Failed to read trust store {path_string}: "))
        );
    }

    #[test]
    fn trust_options_include_the_parent_and_session_only_entries() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let project = tmp.path().join("project");
        std::fs::create_dir_all(&project).expect("mkdir");
        let project = project.to_string_lossy().into_owned();
        let options = get_project_trust_options(&project, false);
        assert_eq!(options.len(), 3);
        assert_eq!(options[0].label, "Trust");
        assert!(options[1].label.starts_with("Trust parent folder ("));
        assert_eq!(options[1].updates.len(), 2);
        assert_eq!(options[1].updates[1].decision, None);
        assert_eq!(options[2].label, "Do not trust");
        let with_session = get_project_trust_options(&project, true);
        assert_eq!(with_session.len(), 5);
        assert_eq!(with_session[2].label, "Trust (this session only)");
        assert!(with_session[2].updates.is_empty());
        assert_eq!(with_session[4].label, "Do not trust (this session only)");
    }

    #[test]
    fn detects_trust_requiring_project_resources() {
        let tmp = crate::test_support::isolated_tempdir();
        let home = tmp.path().to_string_lossy().into_owned();
        let cwd = tmp.path().join("project");
        std::fs::create_dir_all(&cwd).expect("mkdir");
        let cwd = cwd.to_string_lossy().into_owned();
        std::fs::create_dir_all(tmp.path().join(".omo/agent")).expect("mkdir");
        std::fs::create_dir_all(tmp.path().join(".agents/skills")).expect("mkdir");
        assert!(!has_trust_requiring_project_resources(&home, Some(&home)));
        assert!(!has_trust_requiring_project_resources(&cwd, Some(&home)));

        std::fs::write(tmp.path().join(".omo/settings.json"), "{}").expect("write");
        assert!(has_trust_requiring_project_resources(&home, Some(&home)));
        std::fs::remove_file(tmp.path().join(".omo/settings.json")).expect("remove");

        std::fs::create_dir_all(std::path::Path::new(&cwd).join(".omo")).expect("mkdir");
        assert!(!has_trust_requiring_project_resources(&cwd, Some(&home)));
        std::fs::write(std::path::Path::new(&cwd).join(".omo/settings.json"), "{}").expect("write");
        assert!(has_trust_requiring_project_resources(&cwd, Some(&home)));
        std::fs::remove_dir_all(std::path::Path::new(&cwd).join(".omo")).expect("remove");

        std::fs::write(std::path::Path::new(&cwd).join("AGENTS.md"), "x").expect("write");
        assert!(!has_trust_requiring_project_resources(&cwd, Some(&home)));
        std::fs::remove_file(std::path::Path::new(&cwd).join("AGENTS.md")).expect("remove");

        std::fs::create_dir_all(std::path::Path::new(&cwd).join(".agents/skills")).expect("mkdir");
        assert!(has_trust_requiring_project_resources(&cwd, Some(&home)));
    }

    #[test]
    fn an_unresolvable_workspace_path_is_not_trusted_by_a_raw_spelling() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let agent_dir = tmp.path().join("agent").to_string_lossy().into_owned();
        let store = ProjectTrustStore::new(&agent_dir);
        let missing = tmp
            .path()
            .join("does-not-exist")
            .to_string_lossy()
            .into_owned();
        store.set(&missing, Some(true)).expect("set");
        assert_eq!(store.get(&missing).expect("get"), Some(true));
        let child = tmp
            .path()
            .join("does-not-exist/child")
            .to_string_lossy()
            .into_owned();
        assert_eq!(store.get(&child).expect("get"), Some(true));
    }

    #[test]
    fn no_inherit_stops_parent_inheritance_and_bumps_revision() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let agent_dir = tmp.path().join("agent").to_string_lossy().into_owned();
        let parent_dir = tmp.path().join("parent");
        let child_dir = parent_dir.join("child");
        std::fs::create_dir_all(&child_dir).expect("mkdir");
        let parent = parent_dir.to_string_lossy().into_owned();
        let child = child_dir.to_string_lossy().into_owned();

        let store = ProjectTrustStore::new(&agent_dir);
        let rev0 = store.revision().expect("rev0");
        store.set(&parent, Some(true)).expect("set parent");
        let rev1 = store.revision().expect("rev1");
        assert_ne!(rev0, rev1);
        assert_eq!(
            get_saved_project_trust_state_sync(&store, &child).expect("state"),
            SavedProjectTrust::Trusted
        );

        store.set_no_inherit(&child).expect("no_inherit");
        let rev2 = store.revision().expect("rev2");
        assert_ne!(rev1, rev2);
        assert_eq!(
            get_saved_project_trust_state_sync(&store, &child).expect("state"),
            SavedProjectTrust::NoInherit
        );
        assert_eq!(
            get_saved_project_trust_sync(&store, &child).expect("decision"),
            None
        );
    }
}
