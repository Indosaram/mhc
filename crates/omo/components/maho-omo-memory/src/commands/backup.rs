//! Repository backup/restore primitives for `/memfs`.
//! Port of `components/memory/commands/backup.ts` at pin 77f3067f1.

use std::{
    fs,
    path::{Path, PathBuf},
};

use super::types::{MemoryCommandDeps, MemoryCommandIdentity};

pub const BACKUP_PREFIX: &str = "memory-backup-";

pub fn backup_timestamp(epoch_ms: i64) -> String {
    match chrono::DateTime::from_timestamp_millis(epoch_ms) {
        Some(instant) => format!(
            "{}-{}",
            instant.format("%Y%m%d"),
            instant.format("%H%M%S")
        ),
        None => String::new(),
    }
}

fn backup_root(identity: &MemoryCommandIdentity) -> &Path {
    &identity.identity_paths.root
}

/// Copies the repository to a fresh sibling directory; never clobbers an existing backup.
pub fn create_repo_backup(
    deps: &MemoryCommandDeps,
    identity: &MemoryCommandIdentity,
) -> Result<PathBuf, String> {
    let stamp = backup_timestamp(deps.now_ms());
    let root = backup_root(identity);
    let mut candidate = root.join(format!("{BACKUP_PREFIX}{stamp}"));
    let mut suffix = 2u32;
    while candidate.exists() {
        candidate = root.join(format!("{BACKUP_PREFIX}{stamp}-{suffix}"));
        suffix += 1;
    }
    copy_dir_recursive(&identity.identity_paths.repo, &candidate).map_err(|error| error.to_string())?;
    Ok(candidate)
}

pub fn list_repo_backups(identity: &MemoryCommandIdentity) -> Vec<String> {
    let Ok(entries) = fs::read_dir(backup_root(identity)) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .filter_map(Result::ok)
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter(|name| name.starts_with(BACKUP_PREFIX))
        .collect();
    names.sort();
    names
}

pub struct RestoreResult {
    pub restored: String,
    pub safety_backup: Option<PathBuf>,
}

pub fn restore_repo_backup(
    deps: &MemoryCommandDeps,
    identity: &MemoryCommandIdentity,
    name: &str,
) -> Result<RestoreResult, String> {
    let source = backup_root(identity).join(name);
    let repo_dir = &identity.identity_paths.repo;
    let safety_backup = if repo_dir.exists() {
        Some(create_repo_backup(deps, identity)?)
    } else {
        None
    };
    if let Err(error) = fs::remove_dir_all(repo_dir) {
        if error.kind() != std::io::ErrorKind::NotFound {
            return Err(error.to_string());
        }
    }
    copy_dir_recursive(&source, repo_dir).map_err(|error| error.to_string())?;
    Ok(RestoreResult { restored: name.to_owned(), safety_backup })
}

fn copy_dir_recursive(source: &Path, destination: &Path) -> std::io::Result<()> {
    fs::create_dir_all(destination)?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let target = destination.join(entry.file_name());
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            copy_dir_recursive(&entry.path(), &target)?;
        } else if file_type.is_symlink() {
            let resolved = fs::canonicalize(entry.path())?;
            if resolved.is_dir() {
                copy_dir_recursive(&resolved, &target)?;
            } else {
                fs::copy(&resolved, &target)?;
            }
        } else {
            fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
}
