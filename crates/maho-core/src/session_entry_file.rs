//! Port of the senpi packages/coding-agent/src/core/session-entry-file.ts additions of v2026.10.27-2:
//! fork-source classification (omo#10375), the identity-capturing loader (senpi#4272) and the
//! backed-up torn-final-line repair (senpi#3549, senpi#4295). The base loader stays
//! `session_manager::load_entries_from_file`, which this module shares its parsing with.

use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::path::Path;

use serde_json::Value;

use crate::event_loop_slicer::{EventLoopSlicer, EventLoopSlicerOptions};
use crate::paths::{PathInputOptions, normalize_path};
use crate::session_file_identity::{FileIdentity, capture_and_close_file_identity};
use crate::session_write_recovery::{SessionRecovery, SessionRecoveryCause, backup_torn_bytes};

const SESSION_READ_BUFFER_SIZE: usize = 1024 * 1024;

fn is_header(entry: &Value) -> bool {
    entry.get("type").and_then(Value::as_str) == Some("session")
        && entry.get("id").and_then(Value::as_str).is_some()
}

fn accept_session_entries(entries: Vec<Value>) -> Vec<Value> {
    match entries.first() {
        None => entries,
        Some(header) if is_header(header) => entries,
        Some(_) => Vec::new(),
    }
}

fn read_raw_entries(file: File) -> Vec<Value> {
    read_raw_entries_prepared(file, &mut |_| {})
}

fn read_raw_entries_prepared(file: File, prepare: &mut dyn FnMut(&mut Value)) -> Vec<Value> {
    let mut reader = BufReader::with_capacity(SESSION_READ_BUFFER_SIZE, file);
    let mut entries = Vec::new();
    let mut buffer: Vec<u8> = Vec::new();
    loop {
        buffer.clear();
        match reader.read_until(b'\n', &mut buffer) {
            Ok(0) | Err(_) => break,
            Ok(_) => {
                let line = String::from_utf8_lossy(&buffer);
                if let Some(mut entry) = crate::session_manager::parse_session_entry_line(
                    line.trim_end_matches(['\n', '\r']),
                ) {
                    prepare(&mut entry);
                    entries.push(entry);
                }
            }
        }
    }
    entries
}

fn read_raw_entries_from_file(file_path: &str) -> Vec<Value> {
    let resolved = normalize_path(file_path, &PathInputOptions::default());
    File::open(&resolved)
        .map(read_raw_entries)
        .unwrap_or_default()
}

/// `loadEntriesFromFile(path, true)`: the accepted entries and the identity of the descriptor they were
/// read from (both from the same descriptor, so a rename in between cannot pair them wrongly).
pub fn load_entries_from_file_with_identity(file_path: &str) -> (Vec<Value>, Option<FileIdentity>) {
    load_entries_from_file_with_identity_prepared(file_path, &mut |_| {})
}

/// `load_entries_from_file_with_identity` with `prepare` applied to each entry as its line is parsed.
pub(crate) fn load_entries_from_file_with_identity_prepared(
    file_path: &str,
    prepare: &mut dyn FnMut(&mut Value),
) -> (Vec<Value>, Option<FileIdentity>) {
    let resolved = normalize_path(file_path, &PathInputOptions::default());
    let Ok(file) = File::open(&resolved) else {
        return (Vec::new(), None);
    };
    let Ok(identity_handle) = file.try_clone() else {
        return (accept_session_entries(read_raw_entries_prepared(file, prepare)), None);
    };
    let entries = read_raw_entries_prepared(file, prepare);
    let identity = capture_and_close_file_identity(identity_handle, &resolved);
    (accept_session_entries(entries), identity)
}

/// `loadEntriesFromFileAsync`: the same accepted entries as `load_entries_from_file`, read with asynchronous
/// I/O so a move or rebind never blocks the runtime on a whole-file read (senpi#3518, no-sync move path),
/// and parsed in slices that yield once `slicer`'s budget is spent (a default slicer when `None`).
pub async fn load_entries_from_file_async(
    file_path: &std::path::Path,
    slicer: Option<EventLoopSlicer>,
) -> Vec<Value> {
    use tokio::io::AsyncBufReadExt;
    let resolved = normalize_path(&file_path.to_string_lossy(), &PathInputOptions::default());
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
                if let Some(entry) = crate::session_manager::parse_session_entry_line(
                    line.trim_end_matches(['\n', '\r']),
                ) {
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

/// What `fork_from` found in its source: the entries, whether the file had a header, and why it cannot be forked.
#[derive(Clone, Debug, PartialEq)]
pub enum ForkSource {
    Session(Vec<Value>),
    Headerless(Vec<Value>),
    Empty,
    NotASession,
}

fn is_session_shaped_entry(entry: &Value) -> bool {
    entry
        .get("type")
        .and_then(Value::as_str)
        .is_some_and(|kind| kind != "session")
        && entry.get("id").and_then(Value::as_str).is_some()
        && entry
            .get("parentId")
            .is_some_and(|parent| parent.is_null() || parent.is_string())
}

/// Read the source of a fork. A file whose line-1 header was lost but whose every other line is a session
/// entry (`type`, `id`, `parentId`) is still a whole conversation, and a fork writes its own new header
/// anyway, so it is forked rather than refused (omo#10375). Anything else that is not a headed session is
/// reported as what it is: no parsable line at all is empty, parsable lines of another shape are not a session.
pub fn read_fork_source(file_path: &str) -> ForkSource {
    let raw = read_raw_entries_from_file(file_path);
    if raw.is_empty() {
        return ForkSource::Empty;
    }
    if raw.first().is_some_and(is_header) {
        return ForkSource::Session(raw);
    }
    if raw.iter().all(is_session_shaped_entry) {
        return ForkSource::Headerless(raw);
    }
    ForkSource::NotASession
}

/// A torn final line was set aside, or could not be (the copy was refused) and must be set aside before the next append.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FinalLineRepair {
    SetAside(SessionRecovery),
    Deferred,
}

/// Make this process's appends start on a new line. A final line with no newline is either a whole entry
/// written without one (it parses: it is terminated and kept) or a line a crash tore part-way (it does not
/// parse: it was never committed, so it is copied to `<file>.torn-<time>` and the file is cut back to its
/// last complete line, senpi#3549, senpi#4295). Terminating a torn line instead would turn it into a
/// complete, corrupt line that no reader can tell from real corruption. Only the final segment is read,
/// never the whole transcript; anything but a regular file is left to the loader.
pub fn terminate_final_session_line(
    file_path: &Path,
    cause: SessionRecoveryCause,
) -> std::io::Result<Option<FinalLineRepair>> {
    if !std::fs::metadata(file_path).is_ok_and(|metadata| metadata.is_file()) {
        return Ok(None);
    }
    let mut file = OpenOptions::new().read(true).write(true).open(file_path)?;
    let size = file.metadata()?.len();
    let segment_start = final_segment_start(&mut file, size)?;
    if segment_start == size {
        return Ok(None);
    }
    let mut segment = vec![0u8; (size - segment_start) as usize];
    file.seek(SeekFrom::Start(segment_start))?;
    file.read_exact(&mut segment)?;
    if crate::session_manager::parse_session_entry_line(&String::from_utf8_lossy(&segment))
        .is_some()
    {
        file.seek(SeekFrom::Start(size))?;
        file.write_all(b"\n")?;
        return Ok(None);
    }
    // Deferred, not dropped: a full disk can refuse the copy, so the file is left as it is and the writer
    // sets the tail aside before its first append, which fails with this error if the disk is still full.
    let Ok(recovery) = backup_torn_bytes(file_path, &segment, cause) else {
        return Ok(Some(FinalLineRepair::Deferred));
    };
    // Not synced on purpose: a lost truncate only brings back an uncommitted tail, and the next append syncs.
    file.set_len(segment_start)?;
    Ok(Some(FinalLineRepair::SetAside(recovery)))
}

/// The offset just after the file's last newline (0 when it has none); `size` when it ends in one.
fn final_segment_start(file: &mut File, size: u64) -> std::io::Result<u64> {
    let mut chunk = vec![0u8; (64 * 1024).min(size) as usize];
    let mut end = size;
    while end > 0 {
        let start = end.saturating_sub(chunk.len() as u64);
        let read = (end - start) as usize;
        file.seek(SeekFrom::Start(start))?;
        file.read_exact(&mut chunk[..read])?;
        if let Some(newline) = chunk[..read].iter().rposition(|byte| *byte == b'\n') {
            return Ok(start + newline as u64 + 1);
        }
        end = start;
    }
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &Path, name: &str, content: &str) -> std::path::PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, content).expect("write");
        path
    }

    #[test]
    fn fork_sources_are_classified() {
        let dir = tempfile::tempdir().expect("tempdir");
        let headed = write(
            dir.path(),
            "headed.jsonl",
            "{\"type\":\"session\",\"id\":\"s\"}\n{\"type\":\"message\",\"id\":\"a\",\"parentId\":null}\n",
        );
        assert!(
            matches!(read_fork_source(&headed.to_string_lossy()), ForkSource::Session(entries) if entries.len() == 2)
        );
        let headerless = write(
            dir.path(),
            "headerless.jsonl",
            "{\"type\":\"message\",\"id\":\"a\",\"parentId\":null}\n{\"type\":\"message\",\"id\":\"b\",\"parentId\":\"a\"}\n",
        );
        assert!(
            matches!(read_fork_source(&headerless.to_string_lossy()), ForkSource::Headerless(entries) if entries.len() == 2)
        );
        let empty = write(dir.path(), "empty.jsonl", "\n not json\n");
        assert_eq!(
            read_fork_source(&empty.to_string_lossy()),
            ForkSource::Empty
        );
        assert_eq!(
            read_fork_source(&dir.path().join("missing.jsonl").to_string_lossy()),
            ForkSource::Empty
        );
        let other = write(dir.path(), "other.json", "{\"name\":\"package\"}\n");
        assert_eq!(
            read_fork_source(&other.to_string_lossy()),
            ForkSource::NotASession
        );
        let missing_parent = write(
            dir.path(),
            "partial.jsonl",
            "{\"type\":\"message\",\"id\":\"a\"}\n",
        );
        assert_eq!(
            read_fork_source(&missing_parent.to_string_lossy()),
            ForkSource::NotASession
        );
    }

    #[test]
    fn a_parsable_unterminated_line_is_terminated_and_kept() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = write(dir.path(), "s.jsonl", "{\"type\":\"session\",\"id\":\"s\"}");
        assert_eq!(
            terminate_final_session_line(&path, SessionRecoveryCause::Unknown).expect("repair"),
            None
        );
        assert_eq!(
            std::fs::read_to_string(&path).expect("read"),
            "{\"type\":\"session\",\"id\":\"s\"}\n"
        );
    }

    #[test]
    fn a_torn_line_is_backed_up_and_cut() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = write(
            dir.path(),
            "s.jsonl",
            "{\"type\":\"session\",\"id\":\"s\"}\n{\"type\":\"mess",
        );
        let Some(FinalLineRepair::SetAside(recovery)) =
            terminate_final_session_line(&path, SessionRecoveryCause::Unknown).expect("repair")
        else {
            panic!("set aside");
        };
        assert_eq!(
            std::fs::read_to_string(&path).expect("read"),
            "{\"type\":\"session\",\"id\":\"s\"}\n"
        );
        assert_eq!(
            std::fs::read_to_string(&recovery.backup_path).expect("backup"),
            "{\"type\":\"mess"
        );
        assert_eq!(recovery.dropped_bytes, 13);
    }

    #[test]
    fn a_terminated_file_and_a_directory_are_left_alone() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = write(dir.path(), "s.jsonl", "{\"a\":1}\n");
        assert_eq!(
            terminate_final_session_line(&path, SessionRecoveryCause::Unknown).expect("repair"),
            None
        );
        assert_eq!(
            terminate_final_session_line(dir.path(), SessionRecoveryCause::Unknown)
                .expect("repair"),
            None
        );
    }

    #[tokio::test]
    async fn the_async_loader_matches_the_sync_loader() {
        let dir = tempfile::tempdir().expect("tempdir");
        let headed = write(
            dir.path(),
            "s.jsonl",
            "{\"type\":\"session\",\"id\":\"s\"}\nnot json\n{\"type\":\"message\",\"id\":\"a\"}",
        );
        let loaded = load_entries_from_file_async(&headed, None).await;
        assert_eq!(
            loaded,
            crate::session_manager::load_entries_from_file(&headed.to_string_lossy())
        );
        assert_eq!(loaded.len(), 2);
        let headerless = write(
            dir.path(),
            "h.jsonl",
            "{\"type\":\"message\",\"id\":\"a\"}\n",
        );
        assert!(
            load_entries_from_file_async(&headerless, None)
                .await
                .is_empty()
        );
        assert!(
            load_entries_from_file_async(&dir.path().join("missing"), None)
                .await
                .is_empty()
        );
    }

    #[cfg(unix)]
    #[test]
    fn the_loader_returns_the_identity_of_the_descriptor_it_read() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = write(
            dir.path(),
            "s.jsonl",
            "{\"type\":\"session\",\"id\":\"s\"}\n",
        );
        let (entries, identity) = load_entries_from_file_with_identity(&path.to_string_lossy());
        assert_eq!(entries.len(), 1);
        let identity = identity.expect("identity");
        assert!(crate::session_file_identity::same_file(
            &std::fs::metadata(&path).expect("stat"),
            &identity
        ));
    }
}
