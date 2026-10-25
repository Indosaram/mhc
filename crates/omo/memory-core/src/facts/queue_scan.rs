//! Scan hot or parked queue files, filtering names before reading payloads.
use super::schema::{FactsQueueEntry, parse_queue_entry, queue_conversation_hash};
use std::{
    fs, io,
    path::{Path, PathBuf},
};
pub struct ScannedQueueEntry {
    pub entry: FactsQueueEntry,
    pub file_path: PathBuf,
}
pub fn scan_queue_entries(
    dir: &Path,
    conversation_id: Option<&str>,
) -> io::Result<Vec<ScannedQueueEntry>> {
    let directory = match fs::read_dir(dir) {
        Ok(dir) => dir,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };
    let mut names = directory
        .map(|entry| entry.map(|entry| entry.file_name()))
        .collect::<io::Result<Vec<_>>>()?;
    names.sort();
    let part = conversation_id.map(|id| format!("-{}-", queue_conversation_hash(id)));
    let mut entries = Vec::new();
    for name in names {
        let name = name.to_string_lossy();
        if !name.ends_with(".json")
            || ["consumed.json", "failures.json", "claims.json"].contains(&name.as_ref())
            || part.as_ref().is_some_and(|part| !name.contains(part))
        {
            continue;
        }
        let file_path = dir.join(name.as_ref());
        let Some(entry) = fs::read_to_string(&file_path)
            .ok()
            .and_then(|raw| parse_queue_entry(&raw))
        else {
            continue;
        };
        entries.push(ScannedQueueEntry { entry, file_path });
    }
    Ok(entries)
}
