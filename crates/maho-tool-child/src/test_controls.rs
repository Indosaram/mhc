//! Test-only injection switches for the tool child (senpi#3566, plan todo 4); port of `test-controls.ts`. Each is
//! `SENPI_TOOL_CHILD_TEST_<NAME>=1` (or `=<value>`); none exists in production because nothing sets them. Holds are
//! released by control files the test creates in `SENPI_TOOL_CHILD_TEST_CONTROL_DIR`, observed with a filesystem
//! watch (the `notify` crate stands in for `fs.watch`), never polled.

use std::collections::{HashMap, HashSet};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use notify::{RecursiveMode, Watcher};
use tokio::sync::{mpsc, watch};

const PREFIX: &str = "SENPI_TOOL_CHILD_TEST_";
pub const TEST_CONTROL_DIR_ENV: &str = "SENPI_TOOL_CHILD_TEST_CONTROL_DIR";

/// How long the sentinel is left unseen before it is written again.
const SENTINEL_RESEND: Duration = Duration::from_millis(50);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TestSwitch {
    HoldLaunchAck,
    HoldHeartbeat,
    HoldLaunchProof,
    StopAfterSpawn,
    DropControlBeforeAck,
    SkipAckWait,
    IgnoreCancel,
    StopSelf,
    ExitMidJob,
    ExitOnStart,
    HangBeforeReady,
    BlockReader,
}

impl TestSwitch {
    /// The upstream switch name (the suffix after `SENPI_TOOL_CHILD_TEST_`).
    pub fn name(self) -> &'static str {
        match self {
            Self::HoldLaunchAck => "HOLD_LAUNCH_ACK",
            Self::HoldHeartbeat => "HOLD_HEARTBEAT",
            Self::HoldLaunchProof => "HOLD_LAUNCH_PROOF",
            Self::StopAfterSpawn => "STOP_AFTER_SPAWN",
            Self::DropControlBeforeAck => "DROP_CONTROL_BEFORE_ACK",
            Self::SkipAckWait => "SKIP_ACK_WAIT",
            Self::IgnoreCancel => "IGNORE_CANCEL",
            Self::StopSelf => "STOP_SELF",
            Self::ExitMidJob => "EXIT_MID_JOB",
            Self::ExitOnStart => "EXIT_ON_START",
            Self::HangBeforeReady => "HANG_BEFORE_READY",
            Self::BlockReader => "BLOCK_READER",
        }
    }
}

#[derive(Debug, Clone)]
pub struct TestControls {
    dir: Option<PathBuf>,
    env: HashMap<String, String>,
}

/// The process environment in the shape `read_test_controls` takes (`process.env` upstream).
pub fn process_env() -> HashMap<String, String> {
    std::env::vars().collect()
}

pub fn read_test_controls(env: &HashMap<String, String>) -> TestControls {
    TestControls {
        dir: env.get(TEST_CONTROL_DIR_ENV).map(PathBuf::from),
        env: env.clone(),
    }
}

fn not_set() -> io::Error {
    io::Error::other(format!("{TEST_CONTROL_DIR_ENV} is not set"))
}

impl TestControls {
    pub fn value(&self, name: TestSwitch) -> Option<String> {
        match self.env.get(&format!("{PREFIX}{}", name.name())) {
            None => None,
            Some(raw) if raw.is_empty() || raw == "0" => None,
            Some(raw) => Some(raw.clone()),
        }
    }

    pub fn enabled(&self, name: TestSwitch) -> bool {
        self.value(name).is_some()
    }

    /// Resolves once `file` exists in the control directory (at once when it already does).
    pub async fn released(&self, file: &str) -> io::Result<()> {
        match &self.dir {
            None => Err(not_set()),
            Some(dir) => wait_for_file(dir, file).await,
        }
    }

    /// Calls `on_file` once per control file whose name starts with `prefix`, until `close()`.
    pub fn watch_prefix(
        &self,
        prefix: &str,
        on_file: impl Fn(&str) + Send + Sync + 'static,
    ) -> PrefixWatch {
        match &self.dir {
            None => {
                let (tx, rx) = watch::channel(Some(Err(not_set().to_string())));
                PrefixWatch {
                    ready: rx,
                    inner: None,
                    _keep: tx,
                }
            }
            Some(dir) => watch_prefix(dir, prefix, Arc::new(on_file)),
        }
    }
}

type ReadyState = Option<Result<(), String>>;

pub struct PrefixWatch {
    ready: watch::Receiver<ReadyState>,
    inner: Option<Arc<PrefixInner>>,
    _keep: watch::Sender<ReadyState>,
}

struct PrefixInner {
    dir: PathBuf,
    sentinel: PathBuf,
    live: AtomicBool,
    watcher: Mutex<Option<notify::RecommendedWatcher>>,
    resend: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl PrefixWatch {
    /// Resolves once the watch has proven itself live (it observed its own sentinel), or fails with the write error.
    pub async fn ready(&self) -> io::Result<()> {
        let mut state = self.ready.clone();
        let settled = state
            .wait_for(|state| state.is_some())
            .await
            .map_err(|_| io::Error::other("watch closed before it was ready"))?
            .clone();
        match settled {
            Some(Err(message)) => Err(io::Error::other(message)),
            _ => Ok(()),
        }
    }

    pub fn close(&self) {
        let Some(inner) = &self.inner else { return };
        inner.live.store(true, Ordering::SeqCst);
        if let Some(task) = lock(&inner.resend).take() {
            task.abort();
        }
        drop(lock(&inner.watcher).take());
        let _ = std::fs::remove_file(&inner.sentinel);
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn sentinel_name() -> String {
    format!(".watch-{}-{}", std::process::id(), unique_suffix())
}

fn unique_suffix() -> String {
    let mut bytes = [0u8; 16];
    if getrandom::fill(&mut bytes).is_err() {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| elapsed.as_nanos())
            .unwrap_or(0);
        return format!("{nanos:x}");
    }
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn now_ms_text() -> String {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis().to_string())
        .unwrap_or_default()
}

fn watch_prefix(dir: &Path, prefix: &str, on_file: Arc<dyn Fn(&str) + Send + Sync>) -> PrefixWatch {
    let sentinel_file = sentinel_name();
    let (ready_tx, ready_rx) = watch::channel::<ReadyState>(None);
    let ready_tx = Arc::new(ready_tx);
    let inner = Arc::new(PrefixInner {
        dir: dir.to_path_buf(),
        sentinel: dir.join(&sentinel_file),
        live: AtomicBool::new(false),
        watcher: Mutex::new(None),
        resend: Mutex::new(None),
    });
    let seen: Arc<Mutex<HashSet<String>>> = Arc::new(Mutex::new(HashSet::new()));
    let prefix = prefix.to_owned();
    let report = {
        let seen = Arc::clone(&seen);
        let prefix = prefix.clone();
        move |name: &str| {
            if !name.starts_with(&prefix) || !lock(&seen).insert(name.to_owned()) {
                return;
            }
            on_file(name);
        }
    };
    let handler = {
        let inner = Arc::clone(&inner);
        let ready_tx = Arc::clone(&ready_tx);
        let sentinel_file = sentinel_file.clone();
        move |event: notify::Result<notify::Event>| {
            let Ok(event) = event else { return };
            for path in event.paths {
                let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
                    continue;
                };
                if name == sentinel_file {
                    inner.live.store(true, Ordering::SeqCst);
                    if let Some(task) = lock(&inner.resend).take() {
                        task.abort();
                    }
                    let _ = std::fs::remove_file(&inner.sentinel);
                    // Live from here on; files created before the watch existed are picked up once.
                    if let Ok(existing) = std::fs::read_dir(&inner.dir) {
                        for entry in existing.flatten() {
                            if let Some(existing) = entry.file_name().to_str() {
                                report(existing);
                            }
                        }
                    }
                    let _ = ready_tx.send(Some(Ok(())));
                    continue;
                }
                if inner.dir.join(name).exists() {
                    report(name);
                }
            }
        }
    };
    let watcher = notify::recommended_watcher(handler).and_then(|mut watcher| {
        watcher.watch(dir, RecursiveMode::NonRecursive)?;
        Ok(watcher)
    });
    match watcher {
        Ok(watcher) => *lock(&inner.watcher) = Some(watcher),
        Err(error) => {
            let _ = ready_tx.send(Some(Err(error.to_string())));
            return PrefixWatch {
                ready: ready_rx,
                inner: None,
                _keep: watch::channel(None).0,
            };
        }
    }
    // `touch`: write the sentinel until the watcher reports it.
    let touch_inner = Arc::clone(&inner);
    let touch_ready = Arc::clone(&ready_tx);
    let task = tokio::spawn(async move {
        loop {
            if touch_inner.live.load(Ordering::SeqCst) {
                return;
            }
            if let Err(error) = tokio::fs::write(&touch_inner.sentinel, now_ms_text()).await {
                let _ = touch_ready.send(Some(Err(error.to_string())));
                return;
            }
            if touch_inner.live.load(Ordering::SeqCst) {
                return;
            }
            tokio::time::sleep(SENTINEL_RESEND).await;
        }
    });
    *lock(&inner.resend) = Some(task);
    PrefixWatch {
        ready: ready_rx,
        inner: Some(inner),
        _keep: watch::channel(None).0,
    }
}

async fn wait_for_file(dir: &Path, file: &str) -> io::Result<()> {
    let path = dir.join(file);
    // macOS drops events while a watch is still starting. The one watcher proves itself live by observing its own
    // sentinel (re-sent until seen); after that, the existence re-check covers every later creation.
    let sentinel = dir.join(format!(".watch-{}-{}", std::process::id(), unique_suffix()));
    let (events_tx, mut events) = mpsc::unbounded_channel::<PathBuf>();
    let mut watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
        if let Ok(event) = event {
            for path in event.paths {
                let _ = events_tx.send(path);
            }
        }
    })
    .map_err(io::Error::other)?;
    watcher
        .watch(dir, RecursiveMode::NonRecursive)
        .map_err(io::Error::other)?;
    let outcome = async {
        let mut sentinel_seen = false;
        loop {
            // `touch`
            tokio::fs::write(&sentinel, now_ms_text()).await?;
            if path.exists() {
                return Ok(());
            }
            // Wait for events; until the sentinel is seen, write it again after each quiet resend interval.
            loop {
                let next = if sentinel_seen {
                    events.recv().await.map(Some)
                } else {
                    match tokio::time::timeout(SENTINEL_RESEND, events.recv()).await {
                        Ok(event) => event.map(Some),
                        Err(_) => Some(None),
                    }
                };
                match next {
                    None => return Err(io::Error::other("control directory watch ended")),
                    Some(None) => break,
                    Some(Some(changed)) => {
                        if path.exists() {
                            return Ok(());
                        }
                        if changed == sentinel {
                            sentinel_seen = true;
                        }
                    }
                }
            }
        }
    }
    .await;
    drop(watcher);
    let _ = std::fs::remove_file(&sentinel);
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
            .collect()
    }

    #[test]
    fn enables_a_switch_only_for_a_non_empty_non_zero_value_and_exposes_its_value() {
        let controls = read_test_controls(&env(&[
            ("SENPI_TOOL_CHILD_TEST_HOLD_LAUNCH_ACK", "1"),
            ("SENPI_TOOL_CHILD_TEST_HOLD_LAUNCH_PROOF", "detached"),
            ("SENPI_TOOL_CHILD_TEST_SKIP_ACK_WAIT", "0"),
        ]));
        assert!(controls.enabled(TestSwitch::HoldLaunchAck));
        assert_eq!(
            controls.value(TestSwitch::HoldLaunchProof).as_deref(),
            Some("detached")
        );
        assert!(!controls.enabled(TestSwitch::SkipAckWait));
        assert!(!controls.enabled(TestSwitch::StopAfterSpawn));
    }

    #[tokio::test]
    async fn releases_on_a_control_file_created_after_the_wait_began_and_at_once_when_it_already_exists()
     {
        let dir = tempfile::tempdir().unwrap();
        let controls = read_test_controls(&env(&[(
            "SENPI_TOOL_CHILD_TEST_CONTROL_DIR",
            dir.path().to_str().unwrap(),
        )]));
        let waiting_controls = controls.clone();
        let waiting = tokio::spawn(async move { waiting_controls.released("release-L1").await });
        tokio::task::yield_now().await;
        assert!(!waiting.is_finished());
        tokio::fs::write(dir.path().join("release-L1"), "")
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(10), waiting)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        tokio::time::timeout(Duration::from_secs(10), controls.released("release-L1"))
            .await
            .unwrap()
            .unwrap();
    }

    #[tokio::test]
    async fn refuses_to_wait_without_a_control_directory() {
        let error = read_test_controls(&HashMap::new())
            .released("release-L1")
            .await
            .unwrap_err();
        assert!(error.to_string().contains("CONTROL_DIR"));
    }

    #[tokio::test]
    async fn watch_prefix_reports_each_matching_file_once_including_ones_created_before_the_watch()
    {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("query-slot-early.1"), "").unwrap();
        let controls = read_test_controls(&env(&[(
            "SENPI_TOOL_CHILD_TEST_CONTROL_DIR",
            dir.path().to_str().unwrap(),
        )]));
        let (found_tx, mut found) = mpsc::unbounded_channel::<String>();
        let watching = controls.watch_prefix("query-slot-", move |name| {
            let _ = found_tx.send(name.to_owned());
        });
        tokio::time::timeout(Duration::from_secs(10), watching.ready())
            .await
            .unwrap()
            .unwrap();
        tokio::fs::write(dir.path().join("query-slot-late.2"), "")
            .await
            .unwrap();
        tokio::fs::write(dir.path().join("other"), "")
            .await
            .unwrap();
        let mut names = Vec::new();
        while names.len() < 2 {
            let name = tokio::time::timeout(Duration::from_secs(10), found.recv())
                .await
                .unwrap()
                .unwrap();
            names.push(name);
        }
        names.sort();
        watching.close();
        assert_eq!(names, vec!["query-slot-early.1", "query-slot-late.2"]);
    }
}
