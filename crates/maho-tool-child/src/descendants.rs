//! The server-owned record of every process a tool child is about to start (senpi#3566, plan todo 4); port of
//! `descendants.ts`.
//!
//! An entry is appended and fsynced before the server sends `launch_ack`, so a containment handle is on disk before the
//! command can run. That is what lets the server clean up descendants even when the child that started them is gone.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use tokio::io::AsyncWriteExt;
use tokio::sync::{mpsc, oneshot};

use crate::protocol::{LaunchApi, LaunchContainment, LaunchLifetime, LaunchOrigin};

/// Where a generation's ledger lives under a session's tool-child directory; the server writes it, the child reads it.
pub fn generation_ledger_path(dir: &Path, generation: u64) -> PathBuf {
    dir.join(format!("g{generation}"))
        .join("descendants.ledger")
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DescendantEntry {
    pub launch_id: String,
    pub generation: u64,
    pub origin: LaunchOrigin,
    pub api: LaunchApi,
    pub containment: LaunchContainment,
    pub lifetime: LaunchLifetime,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pid: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pgid: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub leader_start: Option<String>,
    pub at_ms: u64,
}

#[derive(Debug)]
pub struct DescendantsLedger {
    path: PathBuf,
    durable_dir: tokio::sync::Mutex<bool>,
    entries: Mutex<Vec<DescendantEntry>>,
}

impl DescendantsLedger {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            durable_dir: tokio::sync::Mutex::new(false),
            entries: Mutex::new(Vec::new()),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn entries(&self) -> Vec<DescendantEntry> {
        self.entries
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }

    pub async fn append(&self, entry: DescendantEntry) -> io::Result<()> {
        {
            let mut durable = self.durable_dir.lock().await;
            if !*durable {
                // The first entry creates the generation directory and the file: both directory entries are synced, or a
                // crash could leave an fsynced ledger that no directory names.
                let directory = self.path.parent().unwrap_or_else(|| Path::new("."));
                create_private_dir(directory).await?;
                open_append(&self.path).await?;
                sync_directory(directory).await?;
                sync_directory(directory.parent().unwrap_or_else(|| Path::new("."))).await?;
                *durable = true;
            }
        }
        let mut handle = open_append(&self.path).await?;
        let line = format!(
            "{}\n",
            serde_json::to_string(&entry).map_err(io::Error::other)?
        );
        handle.write_all(line.as_bytes()).await?;
        handle.flush().await?;
        handle.sync_data().await?;
        drop(handle);
        let mut entries = self.entries.lock().unwrap_or_else(|p| p.into_inner());
        match entries.iter().position(|e| e.launch_id == entry.launch_id) {
            Some(at) => entries[at] = entry,
            None => entries.push(entry),
        }
        Ok(())
    }

    /// Entries a previous server instance left behind (crash recovery). A torn final line is ignored.
    pub async fn read_persisted(path: &Path) -> Vec<DescendantEntry> {
        let Ok(bytes) = tokio::fs::read(path).await else {
            return Vec::new();
        };
        let text = String::from_utf8_lossy(&bytes);
        let mut entries = Vec::new();
        for line in text.split('\n') {
            if line.trim().is_empty() {
                continue;
            }
            // A torn last line from a crash mid-append is skipped; earlier lines were fsynced whole.
            if let Ok(entry) = serde_json::from_str::<DescendantEntry>(line) {
                entries.push(entry);
            }
        }
        entries
    }

    pub async fn remove(&self) -> io::Result<()> {
        self.entries
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clear();
        match tokio::fs::remove_file(&self.path).await {
            Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error),
            _ => Ok(()),
        }
    }
}

async fn create_private_dir(directory: &Path) -> io::Result<()> {
    let mut builder = tokio::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    builder.mode(0o700);
    builder.create(directory).await
}

async fn open_append(path: &Path) -> io::Result<tokio::fs::File> {
    let mut options = tokio::fs::OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    options.mode(0o600);
    options.open(path).await
}

/// fsync a directory so the entries created in it survive a crash; a platform that cannot open a directory skips it.
async fn sync_directory(directory: &Path) -> io::Result<()> {
    if cfg!(windows) {
        return Ok(());
    }
    let handle = tokio::fs::File::open(directory).await?;
    handle.sync_all().await
}

enum LogMessage {
    Line(String, oneshot::Sender<()>),
    Settle(oneshot::Sender<()>),
}

/// Append-only record of every barrier frame, for diagnostics and the QA driver. Not fsynced.
#[derive(Debug)]
pub struct LaunchLog {
    path: PathBuf,
    queue: mpsc::UnboundedSender<LogMessage>,
    failed_writes: Arc<AtomicUsize>,
}

impl std::fmt::Debug for LogMessage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("LogMessage")
    }
}

impl LaunchLog {
    /// Must be called inside a tokio runtime: a single writer task keeps records in the order they were queued.
    pub fn new(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        let (queue, mut inbox) = mpsc::unbounded_channel::<LogMessage>();
        let failed_writes = Arc::new(AtomicUsize::new(0));
        let writer_path = path.clone();
        let writer_failures = Arc::clone(&failed_writes);
        tokio::spawn(async move {
            while let Some(message) = inbox.recv().await {
                match message {
                    LogMessage::Line(line, done) => {
                        if write_line(&writer_path, &line).await.is_err() {
                            writer_failures.fetch_add(1, Ordering::SeqCst);
                        }
                        let _ = done.send(());
                    }
                    LogMessage::Settle(done) => {
                        let _ = done.send(());
                    }
                }
            }
        });
        Self {
            path,
            queue,
            failed_writes,
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Records the log could not write; the log is diagnostic, so a failed write is counted, never thrown.
    pub fn failed_writes(&self) -> usize {
        self.failed_writes.load(Ordering::SeqCst)
    }

    /// Resolves once every record queued so far has been written (or counted as failed).
    pub async fn settled(&self) {
        let (done, wait) = oneshot::channel();
        if self.queue.send(LogMessage::Settle(done)).is_ok() {
            let _ = wait.await;
        }
    }

    /// Appends in order. The returned receiver resolves when this record was written; it never fails, and dropping
    /// it (the `void log.record(...)` form) loses nothing: one failed write neither poisons later records nor what
    /// awaits `settled()`. `record` must be an object; `atMs` is put first.
    pub fn record(&self, record: serde_json::Value) -> oneshot::Receiver<()> {
        let mut object = serde_json::Map::new();
        object.insert("atMs".to_owned(), serde_json::Value::from(now_ms()));
        if let serde_json::Value::Object(fields) = record {
            for (key, value) in fields {
                object.insert(key, value);
            }
        }
        let line = format!("{}\n", serde_json::Value::Object(object));
        let (done, wait) = oneshot::channel();
        if self.queue.send(LogMessage::Line(line, done)).is_err() {
            self.failed_writes.fetch_add(1, Ordering::SeqCst);
        }
        wait
    }
}

async fn write_line(path: &Path, line: &str) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        create_private_dir(parent).await?;
    }
    let mut file = open_append(path).await?;
    file.write_all(line.as_bytes()).await?;
    file.flush().await
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(launch_id: &str) -> DescendantEntry {
        DescendantEntry {
            launch_id: launch_id.to_owned(),
            generation: 1,
            origin: LaunchOrigin::Kernel,
            api: LaunchApi::CpSpawn,
            containment: LaunchContainment::OwnGroup,
            lifetime: LaunchLifetime::Owned,
            pid: Some(4100),
            pgid: Some(4100),
            leader_start: None,
            at_ms: 1,
        }
    }

    #[tokio::test]
    async fn is_on_disk_owner_only_when_append_resolves_and_a_later_instance_reads_it_back_in_order()
     {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("g1").join("descendants.ledger");
        let ledger = DescendantsLedger::new(&path);
        let detached = DescendantEntry {
            lifetime: LaunchLifetime::Detached,
            ..entry("b")
        };
        ledger.append(entry("a")).await.unwrap();
        ledger.append(detached.clone()).await.unwrap();
        assert_eq!(
            DescendantsLedger::read_persisted(&path).await,
            vec![entry("a"), detached]
        );
        assert_eq!(
            ledger
                .entries()
                .iter()
                .map(|e| e.launch_id.as_str())
                .collect::<Vec<_>>(),
            vec!["a", "b"]
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
            assert_eq!(
                std::fs::metadata(path.parent().unwrap())
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o700
            );
        }
    }

    #[tokio::test]
    async fn skips_a_torn_final_line_from_a_crash_mid_append_and_keeps_every_whole_line() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("descendants.ledger");
        DescendantsLedger::new(&path)
            .append(entry("whole"))
            .await
            .unwrap();
        let mut file = open_append(&path).await.unwrap();
        file.write_all(br#"{"launchId":"torn","gener"#)
            .await
            .unwrap();
        file.flush().await.unwrap();
        assert_eq!(
            DescendantsLedger::read_persisted(&path).await,
            vec![entry("whole")]
        );
    }

    #[tokio::test]
    async fn reads_nothing_when_no_ledger_was_ever_written_and_remove_clears_memory_and_disk() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("descendants.ledger");
        assert!(DescendantsLedger::read_persisted(&path).await.is_empty());
        let ledger = DescendantsLedger::new(&path);
        ledger.append(entry("a")).await.unwrap();
        ledger.remove().await.unwrap();
        assert!(ledger.entries().is_empty());
        assert!(DescendantsLedger::read_persisted(&path).await.is_empty());
    }

    #[tokio::test]
    async fn launch_log_keeps_the_order_frames_were_recorded_in_even_when_recorded_without_awaiting()
     {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("launches.jsonl");
        let log = LaunchLog::new(&path);
        for index in 0..50 {
            drop(
                log.record(serde_json::json!({"type": "launch", "launchId": format!("L{index}")})),
            );
        }
        log.settled().await;
        let text = tokio::fs::read_to_string(&path).await.unwrap();
        let lines: Vec<serde_json::Value> = text
            .trim()
            .split('\n')
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        let ids: Vec<&str> = lines
            .iter()
            .map(|l| l["launchId"].as_str().unwrap())
            .collect();
        let expected: Vec<String> = (0..50).map(|i| format!("L{i}")).collect();
        assert_eq!(ids, expected.iter().map(String::as_str).collect::<Vec<_>>());
        assert!(lines.iter().all(|line| line["atMs"].is_number()));
    }
}
