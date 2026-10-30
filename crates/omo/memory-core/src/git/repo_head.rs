//! Resolves HEAD by reading `.git/HEAD`, loose refs and `packed-refs` without spawning git.
//!
//! Only plain layouts are served here: worktree config, config includes, reftable and the
//! `GIT_*` repository environment all return `None` so git stays authoritative.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use regex::Regex;

use crate::fs::resilient::symlink_metadata;

const FILE_CACHE_SIZE: usize = 256;
const REPO_ENV: [&str; 7] = [
    "GIT_DIR",
    "GIT_COMMON_DIR",
    "GIT_WORK_TREE",
    "GIT_NAMESPACE",
    "GIT_CONFIG",
    "GIT_CONFIG_COUNT",
    "GIT_CONFIG_PARAMETERS",
];

struct CachedFile {
    path: PathBuf,
    stamp: String,
    text: String,
}

// Repo instances are recreated for each prompt. Bound the process cache, not an instance cache.
fn files() -> &'static Mutex<Vec<CachedFile>> {
    static FILES: OnceLock<Mutex<Vec<CachedFile>>> = OnceLock::new();
    FILES.get_or_init(|| Mutex::new(Vec::new()))
}

/// A read file: missing, unsupported (not a regular file or changed while read), or its text.
enum Read {
    Missing,
    Unsupported,
    Text(String),
}

fn pattern(cell: &'static OnceLock<Regex>, source: &str) -> &'static Regex {
    cell.get_or_init(|| Regex::new(source).expect("static pattern"))
}

fn is_object_id(text: &str) -> bool {
    matches!(text.len(), 40 | 64) && text.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

fn object_id(text: &str) -> Option<String> {
    let value = text.strip_suffix('\n').unwrap_or(text);
    let value = value.strip_suffix('\r').unwrap_or(value);
    is_object_id(value).then(|| value.to_string())
}

fn stamp(meta: &std::fs::Metadata) -> String {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        format!(
            "{}:{}:{}:{}.{}:{}.{}",
            meta.dev(),
            meta.ino(),
            meta.size(),
            meta.mtime(),
            meta.mtime_nsec(),
            meta.ctime(),
            meta.ctime_nsec()
        )
    }
    #[cfg(not(unix))]
    {
        let modified = meta
            .modified()
            .ok()
            .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
            .map_or(0, |elapsed| elapsed.as_nanos());
        format!("{}:{modified}", meta.len())
    }
}

fn lstat(path: &Path) -> std::io::Result<Option<std::fs::Metadata>> {
    match symlink_metadata(path) {
        Ok(meta) => Ok(Some(meta)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

fn read_text(path: &Path) -> std::io::Result<Read> {
    let Some(meta) = lstat(path)? else {
        files()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .retain(|cached| cached.path != path);
        return Ok(Read::Missing);
    };
    if !meta.is_file() {
        return Ok(Read::Unsupported);
    }
    let current = stamp(&meta);
    {
        let mut cache = files().lock().unwrap_or_else(|e| e.into_inner());
        if let Some(index) = cache
            .iter()
            .position(|cached| cached.path == path && cached.stamp == current)
        {
            let hit = cache.remove(index);
            let text = hit.text.clone();
            cache.push(hit);
            return Ok(Read::Text(text));
        }
    }
    let text = crate::fs::resilient::read_to_string(path)?;
    match lstat(path)? {
        Some(after) if stamp(&after) == current => {}
        _ => return Ok(Read::Unsupported),
    }
    let mut cache = files().lock().unwrap_or_else(|e| e.into_inner());
    cache.retain(|cached| cached.path != path);
    cache.push(CachedFile {
        path: path.to_path_buf(),
        stamp: current,
        text: text.clone(),
    });
    if cache.len() > FILE_CACHE_SIZE {
        cache.remove(0);
    }
    Ok(Read::Text(text))
}

/// `None` asks the caller to use git's authoritative resolver for an unfamiliar layout.
pub fn read_head_from_files(repo_dir: &Path) -> Option<String> {
    if REPO_ENV.iter().any(|key| std::env::var_os(key).is_some()) {
        return None;
    }
    // Files can disappear, become unreadable, or be replaced while refs are updated. Git retains
    // responsibility for the error behavior.
    resolve(repo_dir).ok().flatten()
}

fn resolve(repo_dir: &Path) -> std::io::Result<Option<String>> {
    static POINTER: OnceLock<Regex> = OnceLock::new();
    static COMMON: OnceLock<Regex> = OnceLock::new();
    static CONFIG: OnceLock<Regex> = OnceLock::new();
    static HEAD_REF: OnceLock<Regex> = OnceLock::new();
    static PACKED: OnceLock<Regex> = OnceLock::new();

    let base = std::path::absolute(repo_dir)?;
    let marker = base.join(".git");
    let git_dir = match lstat(&marker)? {
        Some(meta) if meta.is_file() => {
            let Read::Text(pointer) = read_text(&marker)? else {
                return Ok(None);
            };
            let Some(target) = pattern(&POINTER, r"^gitdir: ([^\r\n]+)\r?\n?$")
                .captures(&pointer)
                .map(|caps| caps[1].to_string())
            else {
                return Ok(None);
            };
            base.join(target)
        }
        Some(meta) if meta.is_dir() => marker,
        _ => return Ok(None),
    };

    let common_dir = match read_text(&git_dir.join("commondir"))? {
        Read::Unsupported => return Ok(None),
        Read::Text(common) => {
            if common.is_empty() || !pattern(&COMMON, r"^[^\r\n]+\r?\n?$").is_match(&common) {
                return Ok(None);
            }
            git_dir.join(common.trim())
        }
        Read::Missing => git_dir.clone(),
    };
    let Read::Text(config) = read_text(&common_dir.join("config"))? else {
        return Ok(None);
    };
    // Includes, worktree config, and alternative ref backends are git's job.
    if pattern(
        &CONFIG,
        r"(?im)^\s*(?:\[include(?:if)?\b|(?:refstorage|worktreeconfig)\s*(?:=|$))",
    )
    .is_match(&config)
    {
        return Ok(None);
    }
    for dir in [&git_dir, &common_dir] {
        if lstat(&dir.join("config.worktree"))?.is_some() {
            return Ok(None);
        }
    }
    // Detect reftable storage even when selected through system/global rather than local config.
    if lstat(&common_dir.join("reftable"))?.is_some() {
        return Ok(None);
    }
    if !lstat(&common_dir.join("objects"))?.is_some_and(|meta| meta.is_dir()) {
        return Ok(None);
    }

    let Read::Text(head) = read_text(&git_dir.join("HEAD"))? else {
        return Ok(None);
    };
    if let Some(detached) = object_id(&head) {
        return Ok(Some(detached));
    }
    let Some(reference) = pattern(&HEAD_REF, r"^ref: (refs/heads/[A-Za-z0-9_./-]+)\r?\n?$")
        .captures(&head)
        .map(|caps| caps[1].to_string())
    else {
        return Ok(None);
    };
    if reference.contains("..")
        || reference.split('/').any(|part| {
            part.is_empty()
                || part.starts_with('.')
                || part.ends_with('.')
                || part.ends_with(".lock")
        })
    {
        return Ok(None);
    }
    match read_text(&common_dir.join(&reference))? {
        Read::Text(loose) => return Ok(object_id(&loose)),
        Read::Unsupported => return Ok(None),
        Read::Missing => {}
    }
    let Read::Text(packed) = read_text(&common_dir.join("packed-refs"))? else {
        return Ok(None);
    };
    let entry_pattern = pattern(&PACKED, r"^([0-9a-f]{40}|[0-9a-f]{64}) (refs/\S+)$");
    let mut found: Option<String> = None;
    for line in packed.lines() {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line.strip_prefix('^').is_some_and(is_object_id) {
            continue;
        }
        let Some(entry) = entry_pattern.captures(line) else {
            return Ok(None);
        };
        if &entry[2] == reference.as_str() {
            if found.is_some() {
                return Ok(None);
            }
            found = Some(entry[1].to_string());
        }
    }
    Ok(found)
}

#[cfg(test)]
#[path = "repo_head_tests.rs"]
mod tests;
