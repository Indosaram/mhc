//! Every durable write of the engine goes through here: write a temporary file in the target's own
//! directory, fsync it, rename it over the target, fsync the directory. A crash at any instant
//! leaves either the old file or the new one, never a half-written one.

use std::path::Path;

use serde::Serialize;
use serde::de::DeserializeOwned;

use super::error::EngineResult;
use crate::fs::resilient;
use crate::journal::{sync_journal_directory, sync_journal_file};
use crate::support::random::random_id;

pub const TEMP_MARK: &str = ".omo-sync-tmp-";

fn parent_of(path: &Path) -> &Path {
    match path.parent() {
        Some(dir) if !dir.as_os_str().is_empty() => dir,
        _ => Path::new("."),
    }
}

pub fn write_atomic(path: &Path, data: &[u8]) -> EngineResult<()> {
    let dir = parent_of(path);
    resilient::create_dir_all(dir)?;
    let base = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let suffix: String = random_id().chars().take(12).collect();
    let temp = dir.join(format!("{TEMP_MARK}{base}.{suffix}"));
    let landed = (|| -> EngineResult<()> {
        resilient::write(&temp, data)?;
        sync_journal_file(&temp)?;
        resilient::rename(&temp, path)?;
        Ok(())
    })();
    if let Err(error) = landed {
        let _ = resilient::remove_file(&temp);
        return Err(error);
    }
    sync_journal_directory(dir)?;
    Ok(())
}

pub fn remove_atomic(path: &Path) -> EngineResult<()> {
    match resilient::remove_file(path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    sync_journal_directory(parent_of(path))?;
    Ok(())
}

/// Reads and parses a JSON file; a missing file answers `fallback`.
pub fn read_json<T: DeserializeOwned>(path: &Path, fallback: T) -> EngineResult<T> {
    let text = match resilient::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(fallback),
        Err(error) => return Err(error.into()),
    };
    Ok(serde_json::from_str(&text)?)
}

pub fn write_json<T: Serialize>(path: &Path, value: &T) -> EngineResult<()> {
    let mut text = serde_json::to_string(value)?;
    text.push('\n');
    write_atomic(path, text.as_bytes())
}

#[cfg(test)]
#[path = "atomic_tests.rs"]
mod tests;
