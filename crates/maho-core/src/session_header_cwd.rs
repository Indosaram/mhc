//! Port of senpi packages/coding-agent/src/core/session-header-cwd.ts.

use std::io::ErrorKind;
use std::path::Path;

use serde_json::Value;
use tokio::io::AsyncReadExt;

use crate::session_entry_file::{SessionLineSplitter, load_entries_from_file_async};
use crate::session_manager::parse_session_entry_line;

const SCAN_LIMIT_BYTES: usize = 1024 * 1024;
const CHUNK_BYTES: usize = 4096;

enum Candidate {
    KeepScanning,
    Found(Option<String>),
}

fn candidate(line: &str) -> Candidate {
    let Some(entry) = parse_session_entry_line(line) else {
        return Candidate::KeepScanning;
    };
    if entry.get("type").and_then(Value::as_str) != Some("session")
        || entry.get("id").and_then(Value::as_str).is_none()
    {
        return Candidate::Found(None);
    }
    let cwd = entry
        .get("cwd")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_owned);
    Candidate::Found(cwd)
}

/// The folder a session file's header names, read as the session manager finds that header.
///
/// `Ok(None)` when there is no file yet (`ENOENT`, `ENOTDIR`) or the file names no folder;
/// every other failure to read an existing file returns `Err`, so a caller never mistakes an
/// unreadable file for one that names nothing.
pub async fn read_header_cwd(session_file: &Path) -> std::io::Result<Option<String>> {
    let mut file = match tokio::fs::File::open(session_file).await {
        Ok(file) => file,
        Err(err)
            if err.kind() == ErrorKind::NotFound
                || err.raw_os_error() == Some(20) /* ENOTDIR on unix */ =>
        {
            return Ok(None);
        }
        Err(err) => return Err(err),
    };

    let mut splitter = SessionLineSplitter::default();
    let mut buffer = vec![0u8; CHUNK_BYTES];
    let mut scanned = 0usize;
    while scanned < SCAN_LIMIT_BYTES {
        let max_len = buffer.len().min(SCAN_LIMIT_BYTES - scanned);
        let bytes_read = file.read(&mut buffer[..max_len]).await?;
        if bytes_read == 0 {
            return match candidate(&splitter.finish()) {
                Candidate::KeepScanning => Ok(None),
                Candidate::Found(cwd) => Ok(cwd),
            };
        }
        scanned += bytes_read;
        for line in splitter.push(&buffer[..bytes_read]) {
            if let Candidate::Found(cwd) = candidate(&line) {
                return Ok(cwd);
            }
        }
    }
    drop(file);
    Ok(header_cwd_from_whole_file(session_file).await)
}

async fn header_cwd_from_whole_file(session_file: &Path) -> Option<String> {
    for entry in load_entries_from_file_async(session_file, None).await {
        if let Ok(line) = serde_json::to_string(&entry)
            && let Candidate::Found(cwd) = candidate(&line)
        {
            return cwd;
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn header_line(cwd: Option<&str>) -> String {
        let mut obj = serde_json::Map::new();
        obj.insert("type".into(), json!("session"));
        obj.insert("id".into(), json!("session-1"));
        if let Some(c) = cwd {
            obj.insert("cwd".into(), json!(c));
        }
        format!("{}\n", Value::Object(obj))
    }

    #[tokio::test]
    async fn reads_named_unnamed_missing_and_past_scan_headers() {
        let dir = tempfile::tempdir().expect("tempdir");

        let named = dir.path().join("named.jsonl");
        tokio::fs::write(&named, header_line(Some("/work/project")))
            .await
            .expect("write");
        assert_eq!(
            read_header_cwd(&named).await.expect("read"),
            Some("/work/project".into())
        );

        let unnamed = dir.path().join("unnamed.jsonl");
        tokio::fs::write(&unnamed, header_line(None))
            .await
            .expect("write");
        assert_eq!(read_header_cwd(&unnamed).await.expect("read"), None);

        assert_eq!(
            read_header_cwd(&dir.path().join("not-there.jsonl"))
                .await
                .expect("read"),
            None
        );

        let long = format!(
            "{}\n{}",
            "x".repeat(1024 * 1024 + 64),
            header_line(Some("/work/late-header"))
        );
        let late = dir.path().join("late.jsonl");
        tokio::fs::write(&late, long).await.expect("write");
        assert_eq!(
            read_header_cwd(&late).await.expect("read"),
            Some("/work/late-header".into())
        );

        #[cfg(unix)]
        {
            let loop_a = dir.path().join("loop-a.jsonl");
            let loop_b = dir.path().join("loop-b.jsonl");
            std::os::unix::fs::symlink(&loop_b, &loop_a).expect("symlink a");
            std::os::unix::fs::symlink(&loop_a, &loop_b).expect("symlink b");
            assert!(read_header_cwd(&loop_a).await.is_err());
        }
    }
}
