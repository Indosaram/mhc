use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};

use super::paths::codegraph_data_root;

pub const CODEGRAPH_PROJECT_SOURCE_METADATA_FILE: &str = "source.json";
pub const CODEGRAPH_PROJECT_SOURCE_METADATA_VERSION: u32 = 1;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PruneCodegraphStoreOptions {
    pub home_dir: Option<PathBuf>,
    pub max_age_days: u64,
    pub max_bytes: u64,
    pub now_ms: Option<u64>,
    pub prune_missing_sources: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PruneCodegraphStoreResult {
    pub remaining_bytes: u64,
    pub removed: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PruneDeadCodegraphProjectStoresOptions {
    pub home_dir: Option<PathBuf>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SourceMetadata {
    source_dir: String,
    version: u32,
}

pub fn write_codegraph_source_metadata(data_dir: &Path, source_dir: &Path) -> std::io::Result<()> {
    let metadata = SourceMetadata {
        source_dir: source_dir.to_string_lossy().into_owned(),
        version: CODEGRAPH_PROJECT_SOURCE_METADATA_VERSION,
    };
    let content = serde_json::to_string_pretty(&metadata)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    fs::write(
        data_dir.join(CODEGRAPH_PROJECT_SOURCE_METADATA_FILE),
        format!("{content}\n"),
    )
}

fn directory_size(path: &Path) -> u64 {
    let Ok(entry_stat) = fs::symlink_metadata(path) else {
        return 0;
    };
    if !entry_stat.is_dir() {
        return entry_stat.len();
    }
    let Ok(read_dir) = fs::read_dir(path) else {
        return 0;
    };
    let mut total = 0;
    for entry in read_dir.flatten() {
        total += directory_size(&entry.path());
    }
    total
}

struct StoreEntry {
    mtime_ms: u64,
    path: PathBuf,
    size_bytes: u64,
}

fn read_store_entries(projects_dir: &Path) -> Vec<StoreEntry> {
    if !projects_dir.exists() {
        return Vec::new();
    }
    let Ok(read_dir) = fs::read_dir(projects_dir) else {
        return Vec::new();
    };

    let mut entries = Vec::new();
    for entry in read_dir.flatten() {
        let path = entry.path();
        if path.is_dir() {
            let mtime_ms = fs::symlink_metadata(&path)
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0);
            let size_bytes = directory_size(&path);
            entries.push(StoreEntry {
                mtime_ms,
                path,
                size_bytes,
            });
        }
    }

    entries.sort_by(|left, right| {
        left.mtime_ms
            .cmp(&right.mtime_ms)
            .then_with(|| left.path.cmp(&right.path))
    });

    entries
}

fn read_recorded_source_dir(project_dir: &Path) -> Option<PathBuf> {
    let metadata_path = project_dir.join(CODEGRAPH_PROJECT_SOURCE_METADATA_FILE);
    if !metadata_path.exists() {
        return None;
    }

    let Ok(raw) = fs::read_to_string(&metadata_path) else {
        return None;
    };

    let Ok(parsed) = serde_json::from_str::<SourceMetadata>(&raw) else {
        return None;
    };

    let trimmed = parsed.source_dir.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(PathBuf::from(trimmed))
    }
}

fn recorded_source_is_missing(project_dir: &Path) -> bool {
    match read_recorded_source_dir(project_dir) {
        Some(source_dir) => !source_dir.exists(),
        None => false,
    }
}

pub fn prune_codegraph_store(options: &PruneCodegraphStoreOptions) -> PruneCodegraphStoreResult {
    let home_dir = options
        .home_dir
        .clone()
        .or_else(dirs::home_dir)
        .unwrap_or_else(|| PathBuf::from(""));
    let projects_dir = codegraph_data_root(&home_dir).join("projects");
    let now_ms = options.now_ms.unwrap_or_else(|| {
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0)
    });
    let max_age_ms = options.max_age_days * 24 * 60 * 60 * 1000;
    let mut removed = Vec::new();

    let mut entries = read_store_entries(&projects_dir);
    let mut total_bytes: u64 = entries.iter().map(|e| e.size_bytes).sum();

    if options.prune_missing_sources == Some(true) {
        for entry in &entries {
            if recorded_source_is_missing(&entry.path) {
                let _ = fs::remove_dir_all(&entry.path);
                removed.push(entry.path.to_string_lossy().into_owned());
                total_bytes = total_bytes.saturating_sub(entry.size_bytes);
            }
        }
    }

    entries.retain(|e| !removed.contains(&e.path.to_string_lossy().into_owned()));

    for entry in &entries {
        if now_ms.saturating_sub(entry.mtime_ms) > max_age_ms {
            let _ = fs::remove_dir_all(&entry.path);
            removed.push(entry.path.to_string_lossy().into_owned());
            total_bytes = total_bytes.saturating_sub(entry.size_bytes);
        }
    }

    entries.retain(|e| !removed.contains(&e.path.to_string_lossy().into_owned()));

    for entry in &entries {
        if total_bytes <= options.max_bytes {
            break;
        }
        let _ = fs::remove_dir_all(&entry.path);
        removed.push(entry.path.to_string_lossy().into_owned());
        total_bytes = total_bytes.saturating_sub(entry.size_bytes);
    }

    PruneCodegraphStoreResult {
        remaining_bytes: total_bytes,
        removed,
    }
}

pub fn prune_dead_codegraph_project_stores(
    options: &PruneDeadCodegraphProjectStoresOptions,
) -> PruneCodegraphStoreResult {
    let home_dir = options
        .home_dir
        .clone()
        .or_else(dirs::home_dir)
        .unwrap_or_else(|| PathBuf::from(""));
    let projects_dir = codegraph_data_root(&home_dir).join("projects");
    let mut removed = Vec::new();

    if !projects_dir.exists() {
        return PruneCodegraphStoreResult {
            remaining_bytes: 0,
            removed,
        };
    }

    let Ok(read_dir) = fs::read_dir(&projects_dir) else {
        return PruneCodegraphStoreResult {
            remaining_bytes: 0,
            removed,
        };
    };

    for entry in read_dir.flatten() {
        let path = entry.path();
        if path.is_dir() && recorded_source_is_missing(&path) {
            let _ = fs::remove_dir_all(&path);
            removed.push(path.to_string_lossy().into_owned());
        }
    }

    PruneCodegraphStoreResult {
        remaining_bytes: 0,
        removed,
    }
}
