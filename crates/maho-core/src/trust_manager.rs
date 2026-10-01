//! Port of senpi packages/coding-agent/src/core/trust-manager.ts.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::config::config_dir_name;
use crate::lockfile_policy::{FILE_STORAGE_SYNC_LOCK_BUDGET_MS, acquire_lock_sync};
use crate::paths::{canonicalize_path, canonicalize_path_strict, resolve_path, PathInputOptions};
use crate::text::strip_bom;

pub type ProjectTrustDecision = Option<bool>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectTrustStoreEntry {
    pub path: String,
    pub decision: bool,
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

pub fn find_nearest_trust_entry(data: &BTreeMap<String, Option<bool>>, cwd: &str) -> Option<ProjectTrustStoreEntry> {
    let mut current_dir = normalize_cwd(cwd);
    loop {
        if let Some(Some(decision)) = data.get(&current_dir) {
            return Some(ProjectTrustStoreEntry { path: current_dir, decision: *decision });
        }
        match parent_dir(&current_dir) {
            Some(parent) => current_dir = parent,
            None => return None,
        }
    }
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
        updates: vec![ProjectTrustUpdate { path: trust_path.clone(), decision: Some(true) }],
        saved_path: Some(trust_path.clone()),
    }];
    if let Some(parent_path) = get_project_trust_parent_path(cwd) {
        options.push(ProjectTrustOption {
            label: format!("Trust parent folder ({parent_path})"),
            trusted: true,
            updates: vec![
                ProjectTrustUpdate { path: parent_path.clone(), decision: Some(true) },
                ProjectTrustUpdate { path: trust_path.clone(), decision: None },
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
        updates: vec![ProjectTrustUpdate { path: trust_path.clone(), decision: Some(false) }],
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
    let content = std::fs::read_to_string(path).map_err(|error| format!("Failed to read trust store {path}: {error}"))?;
    let parsed: Value = serde_json::from_str(strip_bom(&content)).map_err(|error| format!("Failed to read trust store {path}: {error}"))?;
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
        std::fs::create_dir_all(parent).map_err(|error| format!("Failed to write trust store {path}: {error}"))?;
    }
    let mut serialized = serde_json::to_string_pretty(&Value::Object(sorted)).map_err(|error| format!("Failed to write trust store {path}: {error}"))?;
    serialized.push('\n');
    std::fs::write(path, serialized).map_err(|error| format!("Failed to write trust store {path}: {error}"))
}

pub fn with_trust_file_lock<T>(path: &str, action: impl FnOnce() -> T) -> Result<T, String> {
    if let Some(parent) = Path::new(path).parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _guard = acquire_lock_sync(path, FILE_STORAGE_SYNC_LOCK_BUDGET_MS).map_err(|error| error.message())?;
    Ok(action())
}

/// True when cwd has project-local resources that must be gated by project trust.
pub fn has_trust_requiring_project_resources(cwd: &str, home_dir: Option<&str>) -> bool {
    let home = home_dir.map(str::to_owned).unwrap_or_else(crate::config::home_dir);
    let home_dir = canonicalize_path(&resolve(&home));
    let user_agents_skills_dir = PathBuf::from(&home_dir).join(".agents").join("skills").to_string_lossy().into_owned();
    let mut current_dir = canonicalize_path(&resolve(cwd));

    let config_dir = PathBuf::from(&current_dir).join(config_dir_name()).to_string_lossy().into_owned();
    if TRUST_REQUIRING_PROJECT_CONFIG_RESOURCES.iter().any(|entry| Path::new(&config_dir).join(entry).exists()) {
        return true;
    }

    loop {
        let agents_skills_dir = PathBuf::from(&current_dir).join(".agents").join("skills").to_string_lossy().into_owned();
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
}

impl ProjectTrustStore {
    pub fn new(agent_dir: &str) -> Self {
        Self { trust_path: Path::new(&resolve(agent_dir)).join("trust.json").to_string_lossy().into_owned() }
    }

    pub fn trust_path(&self) -> &str {
        &self.trust_path
    }

    pub fn get(&self, cwd: &str) -> Result<ProjectTrustDecision, String> {
        Ok(self.get_entry(cwd)?.map(|entry| entry.decision))
    }

    pub fn get_entry(&self, cwd: &str) -> Result<Option<ProjectTrustStoreEntry>, String> {
        let lock_path = self.trust_path.clone();
        let trust_path = self.trust_path.clone();
        let cwd = cwd.to_owned();
        with_trust_file_lock(&lock_path, move || {
            let data = read_trust_file(&trust_path)?;
            Ok(find_nearest_trust_entry(&data, &cwd))
        })?
    }

    pub fn set(&self, cwd: &str, decision: ProjectTrustDecision) -> Result<(), String> {
        self.set_many(&[ProjectTrustUpdate { path: cwd.to_owned(), decision }])
    }

    pub fn set_many(&self, decisions: &[ProjectTrustUpdate]) -> Result<(), String> {
        let lock_path = self.trust_path.clone();
        let trust_path = self.trust_path.clone();
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
            write_trust_file(&trust_path, &data)
        })?
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert_eq!(read_trust_file(&path_string).expect_err("error"), format!("Invalid trust store {path_string}: expected an object"));
        std::fs::write(&path, "{\"/x\": 1}").expect("write");
        assert_eq!(
            read_trust_file(&path_string).expect_err("error"),
            format!("Invalid trust store {path_string}: value for \"/x\" must be true, false, or null")
        );
        std::fs::write(&path, "{oops").expect("write");
        assert!(read_trust_file(&path_string).expect_err("error").starts_with(&format!("Failed to read trust store {path_string}: ")));
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
        let tmp = tempfile::tempdir().expect("tempdir");
        let home = tmp.path().to_string_lossy().into_owned();
        let cwd = tmp.path().join("project");
        std::fs::create_dir_all(&cwd).expect("mkdir");
        let cwd = cwd.to_string_lossy().into_owned();
        std::fs::create_dir_all(tmp.path().join(".maho/agent")).expect("mkdir");
        std::fs::create_dir_all(tmp.path().join(".agents/skills")).expect("mkdir");
        assert!(!has_trust_requiring_project_resources(&home, Some(&home)));
        assert!(!has_trust_requiring_project_resources(&cwd, Some(&home)));

        std::fs::write(tmp.path().join(".maho/settings.json"), "{}").expect("write");
        assert!(has_trust_requiring_project_resources(&home, Some(&home)));
        std::fs::remove_file(tmp.path().join(".maho/settings.json")).expect("remove");

        std::fs::create_dir_all(std::path::Path::new(&cwd).join(".maho")).expect("mkdir");
        assert!(!has_trust_requiring_project_resources(&cwd, Some(&home)));
        std::fs::write(std::path::Path::new(&cwd).join(".maho/settings.json"), "{}").expect("write");
        assert!(has_trust_requiring_project_resources(&cwd, Some(&home)));
        std::fs::remove_dir_all(std::path::Path::new(&cwd).join(".maho")).expect("remove");

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
        let missing = tmp.path().join("does-not-exist").to_string_lossy().into_owned();
        store.set(&missing, Some(true)).expect("set");
        assert_eq!(store.get(&missing).expect("get"), Some(true));
        let child = tmp.path().join("does-not-exist/child").to_string_lossy().into_owned();
        assert_eq!(store.get(&child).expect("get"), Some(true));
    }
}
