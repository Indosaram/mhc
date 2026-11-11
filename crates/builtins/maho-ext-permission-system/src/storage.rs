use crate::types::{Action, Rule, Ruleset};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashSet,
    fs,
    io::{self, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
};

const USER_ANSWER_SOURCE: &str = "user-answer";
const LEGACY_FILE: &str = ".omo/permissions-approved.jsonl";
/// The store is shared by every mhc process working in the same project and is reread on every
/// decision whose file version moved. Cap it so a runaway writer can never make a session repay a
/// multi-gigabyte parse (mhc incident 2026-11-11: 2.3 GB, 21M lines, 82% torn by concurrent
/// appends, every reload re-appending the whole ruleset).
const MAX_STORE_BYTES: u64 = 8 * 1024 * 1024;

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

/// Reads at most [`MAX_STORE_BYTES`] from the end of the store. Recent approvals are the ones a
/// decision needs, and a bounded read keeps an oversized or corrupt file from stalling startup.
fn read_bounded(path: &Path) -> Option<String> {
    let mut file = fs::File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    let mut bytes = Vec::new();
    if len > MAX_STORE_BYTES {
        file.seek(SeekFrom::Start(len - MAX_STORE_BYTES)).ok()?;
    }
    file.read_to_end(&mut bytes).ok()?;
    let text = String::from_utf8_lossy(&bytes).into_owned();
    if len <= MAX_STORE_BYTES {
        return Some(text);
    }
    // The seek landed mid-record: drop the torn first line.
    let start = text.find('\n').map_or(text.len(), |index| index + 1);
    Some(text[start..].to_owned())
}

fn read_stored(path: &Path, require_source: bool) -> Ruleset {
    if is_symlink(path) {
        return Vec::new();
    }
    let Some(content) = read_bounded(path) else {
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

/// An exclusive advisory lock over one store, so concurrent mhc processes append whole records
/// instead of interleaving partial lines. The lock lives beside the store (`<store>.lock`) because
/// the store itself is replaced atomically on compaction.
struct StoreLock(fs::File);

impl StoreLock {
    fn acquire(store: &Path) -> io::Result<Self> {
        let path = PathBuf::from(format!("{}.lock", store.display()));
        let file = fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(path)?;
        match file.lock() {
            Ok(()) => Ok(Self(file)),
            // A filesystem without advisory locks still behaves as before, unlocked.
            Err(error) if error.kind() == io::ErrorKind::Unsupported => Ok(Self(file)),
            Err(error) => Err(error),
        }
    }
}

impl Drop for StoreLock {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

fn rule_key(rule: &Rule) -> (String, String, Action) {
    (rule.permission.clone(), rule.pattern.clone(), rule.action)
}

fn append_new_rules(path: &Path, rules: &[Rule]) -> io::Result<()> {
    let existing: HashSet<(String, String, Action)> =
        read_stored(path, true).iter().map(rule_key).collect();
    let mut seen = existing;
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    for rule in rules {
        // A reload re-offers every rule it knows; only genuinely new approvals are persisted.
        if !seen.insert(rule_key(rule)) {
            continue;
        }
        let mut stored = StoredRule::from(rule);
        stored.source = Some(USER_ANSWER_SOURCE.to_owned());
        serde_json::to_writer(&mut file, &stored)?;
        file.write_all(b"\n")?;
    }
    Ok(())
}

/// Rewrites the store as its distinct rules. Called with the store lock held: the caller owns the
/// guard so the read, the rewrite, and the rename cannot interleave with another appender.
fn rewrite_unique(agent_dir: &Path, project_dir: &Path, path: &Path) -> io::Result<()> {
    let mut unique = Vec::<(String, Rule)>::new();
    let mut seen = HashSet::new();
    for rule in load_approved(agent_dir, project_dir) {
        let key = format!("{}:{}", rule.permission, rule.pattern);
        if seen.insert(key) {
            unique.push((rule.permission.clone(), rule));
        }
    }
    let temp = PathBuf::from(format!("{}.{}.tmp", path.display(), std::process::id()));
    {
        let mut file = fs::File::create(&temp)?;
        for (_, rule) in &unique {
            let mut stored = StoredRule::from(rule);
            stored.source = Some(USER_ANSWER_SOURCE.to_owned());
            serde_json::to_writer(&mut file, &stored)?;
            file.write_all(b"\n")?;
        }
        file.sync_all()?;
    }
    fs::rename(&temp, path).inspect_err(|_| {
        let _ = fs::remove_file(&temp);
    })
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
    let _lock = StoreLock::acquire(&path)?;
    append_new_rules(&path, rules)?;
    if fs::metadata(&path)?.len() > MAX_STORE_BYTES {
        rewrite_unique(agent_dir, project_dir, &path)?;
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
    if !path.exists() || is_symlink(&path) {
        return Ok(());
    }
    let _lock = StoreLock::acquire(&path)?;
    rewrite_unique(agent_dir, project_dir, &path)
}
