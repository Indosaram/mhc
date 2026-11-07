use crate::types::{Action, Rule, Ruleset};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
};

const USER_ANSWER_SOURCE: &str = "user-answer";
const LEGACY_FILE: &str = ".omo/permissions-approved.jsonl";

#[derive(Serialize, Deserialize)]
struct StoredRule {
    permission: String,
    pattern: String,
    action: Action,
    #[serde(default)]
    source: Option<String>,
}

impl From<&Rule> for StoredRule {
    fn from(rule: &Rule) -> Self {
        Self {
            permission: rule.permission.clone(),
            pattern: rule.pattern.clone(),
            action: rule.action,
            source: None,
        }
    }
}

fn stored_into_rule(stored: StoredRule) -> Option<Rule> {
    if stored.source.as_deref() != Some(USER_ANSWER_SOURCE)
        || !is_approved(&stored.permission, &stored.pattern, stored.action)
    {
        return None;
    }
    Some(Rule {
        permission: stored.permission,
        pattern: stored.pattern,
        action: stored.action,
    })
}

fn is_approved(permission: &str, pattern: &str, action: Action) -> bool {
    action == Action::Allow
        && !permission.is_empty()
        && permission != "*"
        && !permission.contains('\n')
        && !pattern.is_empty()
        && !pattern.contains('\n')
        && !pattern.contains('\r')
}

pub fn approved_store_path(agent_dir: &Path, project_dir: &Path) -> PathBuf {
    use sha2::Digest;
    let canonical = fs::canonicalize(project_dir).unwrap_or_else(|_| project_dir.to_path_buf());
    let digest = sha2::Sha256::digest(canonical.to_string_lossy().as_bytes());
    let hash = digest[..8]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    agent_dir
        .join("permissions")
        .join("approved")
        .join(format!("{hash}.jsonl"))
}

pub fn approved_store_real_path(agent_dir: &Path, project_dir: &Path) -> PathBuf {
    let path = approved_store_path(agent_dir, project_dir);
    fs::canonicalize(&path).unwrap_or(path)
}

pub fn approved_file_version(agent_dir: &Path, project_dir: &Path) -> Option<String> {
    let metadata = fs::metadata(approved_store_path(agent_dir, project_dir)).ok()?;
    Some(format!(
        "{:?}:{}",
        metadata.modified().ok()?,
        metadata.len()
    ))
}

fn is_symlink(path: &Path) -> bool {
    fs::symlink_metadata(path)
        .map(|metadata| metadata.file_type().is_symlink())
        .unwrap_or(false)
}

fn read_stored(path: &Path, require_source: bool) -> Ruleset {
    if is_symlink(path) {
        return Vec::new();
    }
    let Ok(content) = fs::read_to_string(path) else {
        return Vec::new();
    };
    content
        .lines()
        .filter_map(|line| {
            let stored = serde_json::from_str::<StoredRule>(line).ok()?;
            if require_source {
                stored_into_rule(stored)
            } else {
                if !is_approved(&stored.permission, &stored.pattern, stored.action) {
                    return None;
                }
                Some(Rule {
                    permission: stored.permission,
                    pattern: stored.pattern,
                    action: stored.action,
                })
            }
        })
        .collect()
}

pub fn load_approved(agent_dir: &Path, project_dir: &Path) -> Ruleset {
    let store = approved_store_path(agent_dir, project_dir);
    if is_symlink(&store) {
        return Vec::new();
    }
    let marker = PathBuf::from(format!("{}.migrated", store.display()));
    let legacy = project_dir.join(LEGACY_FILE);
    if !marker.exists() && !store.exists() && legacy.exists() {
        let legacy_raw = fs::read_to_string(&legacy).unwrap_or_default();
        let legacy_line_count = legacy_raw
            .lines()
            .filter(|line| !line.trim().is_empty())
            .count();
        let imported = read_stored(&legacy, false);
        let write_result = (|| -> io::Result<()> {
            fs::create_dir_all(store.parent().expect("store parent"))?;
            let mut file = fs::File::create(&store)?;
            for rule in &imported {
                let mut stored = StoredRule::from(rule);
                stored.source = Some(USER_ANSWER_SOURCE.to_owned());
                serde_json::to_writer(&mut file, &stored)?;
                file.write_all(b"\n")?;
            }
            fs::write(&marker, b"")?;
            Ok(())
        })();
        if write_result.is_err() {
            let _ = fs::remove_file(&store);
            let _ = fs::remove_file(&marker);
            return imported;
        }
        if imported.len() == legacy_line_count {
            let _ = fs::remove_file(&legacy);
        }
        return imported;
    }
    read_stored(&store, true)
}

pub fn append_approved(agent_dir: &Path, project_dir: &Path, rules: &[Rule]) -> io::Result<()> {
    if rules.is_empty() {
        return Ok(());
    }
    let path = approved_store_path(agent_dir, project_dir);
    if is_symlink(&path) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "refusing symlinked approval store",
        ));
    }
    fs::create_dir_all(path.parent().expect("store parent"))?;
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    for rule in rules {
        let mut stored = StoredRule::from(rule);
        stored.source = Some(USER_ANSWER_SOURCE.to_owned());
        serde_json::to_writer(&mut file, &stored)?;
        file.write_all(b"\n")?;
    }
    Ok(())
}

pub fn clear_approved(agent_dir: &Path, project_dir: &Path) -> io::Result<()> {
    let path = approved_store_path(agent_dir, project_dir);
    if is_symlink(&path) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "refusing symlinked approval store",
        ));
    }
    match fs::remove_file(path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        result => result,
    }
}

pub fn compact_approved(agent_dir: &Path, project_dir: &Path) -> io::Result<()> {
    let path = approved_store_path(agent_dir, project_dir);
    if !path.exists() {
        return Ok(());
    }
    let mut unique = Vec::<(String, Rule)>::new();
    for rule in load_approved(agent_dir, project_dir) {
        let key = format!("{}:{}", rule.permission, rule.pattern);
        if let Some(entry) = unique.iter_mut().find(|entry| entry.0 == key) {
            entry.1 = rule;
        } else {
            unique.push((key, rule));
        }
    }
    clear_approved(agent_dir, project_dir)?;
    append_approved(
        agent_dir,
        project_dir,
        &unique.into_iter().map(|(_, rule)| rule).collect::<Vec<_>>(),
    )
}
