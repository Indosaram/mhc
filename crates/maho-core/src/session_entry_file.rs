//! Port of senpi packages/coding-agent/src/core/session-entry-file.ts.
//!
//! The synchronous loader is `session_manager::load_entries_from_file`; this adds the time-sliced loader
//! a multi-session host uses (senpi#3518) and `terminate_final_session_line` (senpi#3549). Both parse
//! lines byte-for-byte like the synchronous loader.

use std::io::{self, Read, Seek, SeekFrom, Write};

use serde_json::Value;
use tokio::io::AsyncBufReadExt;

use crate::event_loop_slicer::{EventLoopSlicer, EventLoopSlicerOptions};
use crate::paths::{PathInputOptions, normalize_path};

pub const SESSION_READ_BUFFER_SIZE: usize = 1024 * 1024;

pub fn parse_session_entry_line(line: &str) -> Option<Value> {
    if line.trim().is_empty() {
        return None;
    }
    // Skip malformed lines.
    serde_json::from_str::<Value>(line).ok()
}

fn accept_session_entries(entries: Vec<Value>) -> Vec<Value> {
    let Some(header) = entries.first() else {
        return entries;
    };
    if header.get("type").and_then(Value::as_str) != Some("session")
        || !header.get("id").is_some_and(Value::is_string)
    {
        return Vec::new();
    }
    entries
}

/// `load_entries_from_file` without holding the runtime: the same entries, read with asynchronous I/O and
/// parsed in slices that yield once `slicer`'s budget is spent.
pub async fn load_entries_from_file_async(
    file_path: &str,
    slicer: Option<EventLoopSlicer>,
) -> Vec<Value> {
    let resolved = normalize_path(file_path, &PathInputOptions::default());
    let Ok(file) = tokio::fs::File::open(&resolved).await else {
        return Vec::new();
    };
    let mut slicer =
        slicer.unwrap_or_else(|| EventLoopSlicer::new(EventLoopSlicerOptions::default()));
    let mut reader = tokio::io::BufReader::with_capacity(SESSION_READ_BUFFER_SIZE, file);
    let mut entries = Vec::new();
    let mut buffer: Vec<u8> = Vec::new();
    loop {
        buffer.clear();
        match reader.read_until(b'\n', &mut buffer).await {
            Ok(0) | Err(_) => break,
            Ok(_) => {
                let line = String::from_utf8_lossy(&buffer);
                if let Some(entry) = parse_session_entry_line(line.trim_end_matches(['\n', '\r'])) {
                    entries.push(entry);
                }
                if slicer.due() {
                    slicer.yield_now().await;
                }
            }
        }
    }
    accept_session_entries(entries)
}

/// The offset just after the file's last newline (0 when it has none); `size` when it ends in one.
fn final_segment_start(file: &mut std::fs::File, size: u64) -> io::Result<u64> {
    let mut chunk = vec![0u8; (64 * 1024).min(size) as usize];
    let mut end = size;
    while end > 0 {
        let start = end.saturating_sub(chunk.len() as u64);
        let length = (end - start) as usize;
        file.seek(SeekFrom::Start(start))?;
        file.read_exact(&mut chunk[..length])?;
        if let Some(newline) = chunk[..length].iter().rposition(|byte| *byte == b'\n') {
            return Ok(start + newline as u64 + 1);
        }
        end = start;
    }
    Ok(0)
}

/// Make this process's appends start on a new line. A final line with no newline is either a whole entry
/// written without one (it parses: it is terminated and kept) or a line a crash tore part-way (it does not
/// parse: it was never committed, so the file is cut back to its last complete line). Only the final
/// segment is read; anything but a regular file is left to the loader.
pub fn terminate_final_session_line(file_path: &str) -> io::Result<()> {
    match std::fs::metadata(file_path) {
        Ok(metadata) if metadata.is_file() => {}
        Ok(_) => return Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    }
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(file_path)?;
    let size = file.metadata()?.len();
    let segment_start = final_segment_start(&mut file, size)?;
    if segment_start == size {
        return Ok(());
    }
    let mut segment = vec![0u8; (size - segment_start) as usize];
    file.seek(SeekFrom::Start(segment_start))?;
    file.read_exact(&mut segment)?;
    // Not synced on purpose: a lost truncate only brings back an uncommitted tail, and the next append syncs.
    if parse_session_entry_line(&String::from_utf8_lossy(&segment)).is_none() {
        file.set_len(segment_start)
    } else {
        file.seek(SeekFrom::Start(size))?;
        file.write_all(b"\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn the_async_loader_matches_the_synchronous_one() {
        let Some(dir) = tempfile::tempdir().ok() else {
            panic!("tempdir")
        };
        let path = dir.path().join("s.jsonl").to_string_lossy().into_owned();
        assert!(
            std::fs::write(
                &path,
                "{\"type\":\"session\",\"id\":\"s\"}\nbad\n\n{\"type\":\"message\"}"
            )
            .is_ok()
        );
        let entries = load_entries_from_file_async(&path, None).await;
        assert_eq!(
            entries,
            crate::session_manager::load_entries_from_file(&path)
        );
        assert_eq!(entries.len(), 2);
        assert!(std::fs::write(&path, "{\"type\":\"message\"}\n").is_ok());
        assert!(load_entries_from_file_async(&path, None).await.is_empty());
        assert!(
            load_entries_from_file_async(&format!("{path}.missing"), None)
                .await
                .is_empty()
        );
    }

    #[test]
    fn terminates_a_whole_final_line_and_cuts_a_torn_one() {
        let Some(dir) = tempfile::tempdir().ok() else {
            panic!("tempdir")
        };
        let path = dir.path().join("s.jsonl").to_string_lossy().into_owned();
        assert!(std::fs::write(&path, "{\"a\":1}\n{\"b\":2}").is_ok());
        assert!(terminate_final_session_line(&path).is_ok());
        assert_eq!(
            std::fs::read_to_string(&path).ok().as_deref(),
            Some("{\"a\":1}\n{\"b\":2}\n")
        );
        assert!(terminate_final_session_line(&path).is_ok());
        assert_eq!(
            std::fs::read_to_string(&path).ok().as_deref(),
            Some("{\"a\":1}\n{\"b\":2}\n")
        );
        assert!(std::fs::write(&path, "{\"a\":1}\n{\"b\":").is_ok());
        assert!(terminate_final_session_line(&path).is_ok());
        assert_eq!(
            std::fs::read_to_string(&path).ok().as_deref(),
            Some("{\"a\":1}\n")
        );
        assert!(terminate_final_session_line(&format!("{path}.missing")).is_ok());
    }
}
