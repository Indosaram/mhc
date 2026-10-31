//! Port of senpi `terminal/file-monitor-source.ts`: file watches - one regular file under a parent
//! directory that must exist, watched natively and fired once on create or modify. Registration
//! proves the target's identity without blocking on a wedged mount (every step races the
//! registration deadline through [`RegistrationGuard`]), and every check re-proves it.
//!
//! Native shape: the registry keeps the live records, the snapshot entries and the per-watch task
//! (upstream `#files`, `#settleFile`, `#checkFile`), so this module carries the two halves that are
//! specific to the source: the guarded asynchronous registration proof
//! ([`prove_file_registration`]) and the native parent-directory watcher ([`ParentWatcher`], the
//! `fs.watch` of upstream) whose events trigger an immediate check pass. The 250 ms poll of
//! `monitor-file-watch.ts` keeps running beside it, as upstream's `FileWatchLoop` does.
use crate::file_monitor::FileMonitor;
use crate::file_monitor_guard::{PendingFileRegistration, REGISTRATION_ABORTED, RegistrationGuard};
use crate::terminal_manifest_model::FileEvent;
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// Native watcher on a watched file's parent directory. An event naming the watched basename (or
/// naming nothing) wakes the watch; a watcher error is kept for the watch to settle with.
pub struct ParentWatcher {
    _watcher: notify::RecommendedWatcher,
    wake: Arc<tokio::sync::Notify>,
    error: Arc<Mutex<Option<String>>>,
}

#[cfg(test)]
pub(crate) type WatcherErrorInjector = Box<dyn Fn(String) + Send + Sync>;

/// Whether one native event concerns the watched file. Pure access events are dropped: the check
/// pass itself opens the file to digest it, and an open must not wake another pass.
pub fn event_concerns(event: &notify::Event, name: &OsStr) -> bool {
    use notify::event::{AccessKind, AccessMode, EventKind};
    if let EventKind::Access(kind) = &event.kind
        && *kind != AccessKind::Close(AccessMode::Write)
    {
        return false;
    }
    event.paths.is_empty()
        || event
            .paths
            .iter()
            .any(|path| path.file_name().is_none_or(|file| file == name))
}

impl ParentWatcher {
    /// `watch(canonicalWatchPath(parent))`, non-recursive, filtered to `file_name`.
    pub fn watch(parent: &Path, file_name: &OsStr) -> std::io::Result<Self> {
        use notify::Watcher;
        let wake = Arc::new(tokio::sync::Notify::new());
        let error = Arc::new(Mutex::new(None));
        let (event_wake, event_error) = (wake.clone(), error.clone());
        let name: OsString = file_name.to_os_string();
        let mut watcher =
            notify::recommended_watcher(move |event: notify::Result<notify::Event>| match event {
                Ok(event) => {
                    if event_concerns(&event, &name) {
                        event_wake.notify_one();
                    }
                }
                Err(error) => {
                    *event_error
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner) =
                        Some(format!("watcher error: {error}"));
                    event_wake.notify_one();
                }
            })
            .map_err(std::io::Error::other)?;
        watcher
            .watch(parent, notify::RecursiveMode::NonRecursive)
            .map_err(std::io::Error::other)?;
        Ok(Self {
            _watcher: watcher,
            wake,
            error,
        })
    }

    /// Notified once per batch of relevant events; a wake before anyone waits is kept as a permit.
    pub fn wake(&self) -> Arc<tokio::sync::Notify> {
        self.wake.clone()
    }

    /// Test seam: raises a watcher error as the native callback would.
    #[cfg(test)]
    pub(crate) fn error_injector(&self) -> WatcherErrorInjector {
        let (wake, error) = (self.wake.clone(), self.error.clone());
        Box::new(move |message| {
            *error
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(message);
            wake.notify_one();
        })
    }

    /// The watcher error to settle with (upstream `watcher.on("error")`), taken once.
    pub fn take_error(&self) -> Option<String> {
        self.error
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
    }
}

/// Upstream `RegisterFileMonitorOptions`, the half the proof needs.
pub struct FileRegistrationRequest {
    pub description: String,
    /// Absolute, or relative to `cwd`.
    pub path: PathBuf,
    pub cwd: PathBuf,
    pub event: FileEvent,
    pub approved_parent: Option<PathBuf>,
}

/// A registration that passed every guarded step: the proven record (its runtime id is assigned
/// when the registry adopts it) and the live native watcher of its parent.
pub struct ProvenFileWatch {
    pub record: FileMonitor,
    pub watcher: ParentWatcher,
}

async fn off_thread<T: Send + 'static>(
    work: impl FnOnce() -> std::io::Result<T> + Send + 'static,
) -> std::io::Result<T> {
    match tokio::task::spawn_blocking(work).await {
        Ok(result) => result,
        Err(error) => Err(std::io::Error::other(error)),
    }
}

/// The guarded half of `registerFile`: prove the target (`proveWatchTarget`), prove the parent
/// opens on the blocking pool before the native watcher opens it on this thread
/// (`probeDirectoryOpenable`), then start the watcher. Each blocking step races the registration
/// deadline; a dispose or `stopAllFiles` (which finish every pending registration) fails it too.
/// The caller finishes `pending` on an error.
pub async fn prove_file_registration(
    guard: &RegistrationGuard,
    pending: &PendingFileRegistration,
    request: FileRegistrationRequest,
) -> std::io::Result<ProvenFileWatch> {
    let FileRegistrationRequest {
        description,
        path,
        cwd,
        event,
        approved_parent,
    } = request;
    let path = cwd.join(path);
    let record = guard
        .guard(
            pending,
            off_thread(move || {
                FileMonitor::register(
                    String::new(),
                    description,
                    &path,
                    event,
                    approved_parent.as_deref(),
                )
            }),
            None::<fn(FileMonitor)>,
        )
        .await?;
    let parent = record.parent.clone();
    guard
        .guard(
            pending,
            off_thread(move || std::fs::read_dir(&parent).map(drop)),
            None::<fn(())>,
        )
        .await
        .map_err(|error| {
            std::io::Error::other(format!(
                "Cannot watch file {}: {error}",
                record.path.display()
            ))
        })?;
    let watcher = ParentWatcher::watch(&record.parent, record.path.file_name().unwrap_or_default())
        .map_err(|error| {
            std::io::Error::other(format!(
                "Cannot watch file {}: {error}",
                record.path.display()
            ))
        })?;
    if pending.is_aborted() {
        return Err(std::io::Error::other(REGISTRATION_ABORTED));
    }
    Ok(ProvenFileWatch { record, watcher })
}

#[cfg(test)]
mod tests {
    use super::*;
    use notify::event::{AccessKind, AccessMode, CreateKind, DataChange, EventKind, ModifyKind};

    fn event(kind: EventKind, path: &str) -> notify::Event {
        notify::Event::new(kind).add_path(PathBuf::from(path))
    }

    #[test]
    fn only_events_naming_the_watched_file_wake_it() {
        let name = OsStr::new("artifact");
        assert!(event_concerns(
            &event(EventKind::Create(CreateKind::File), "/dir/artifact"),
            name
        ));
        assert!(event_concerns(
            &event(
                EventKind::Modify(ModifyKind::Data(DataChange::Any)),
                "/dir/artifact"
            ),
            name
        ));
        assert!(!event_concerns(
            &event(EventKind::Create(CreateKind::File), "/dir/other"),
            name
        ));
        // A nameless event (upstream `!name`) always wakes the watch.
        assert!(event_concerns(&notify::Event::new(EventKind::Any), name));
        // The check pass's own open must not wake another pass; a closed writer does.
        assert!(!event_concerns(
            &event(
                EventKind::Access(AccessKind::Open(AccessMode::Any)),
                "/dir/artifact"
            ),
            name
        ));
        assert!(!event_concerns(
            &event(
                EventKind::Access(AccessKind::Close(AccessMode::Read)),
                "/dir/artifact"
            ),
            name
        ));
        assert!(event_concerns(
            &event(
                EventKind::Access(AccessKind::Close(AccessMode::Write)),
                "/dir/artifact"
            ),
            name
        ));
    }

    /// The native watcher delivers a create of the watched file (awaited on its wake, bounded).
    #[tokio::test]
    async fn the_parent_watcher_wakes_on_the_watched_file() {
        let dir = tempfile::tempdir().unwrap();
        let watcher = ParentWatcher::watch(dir.path(), OsStr::new("artifact")).unwrap();
        let wake = watcher.wake();
        std::fs::write(dir.path().join("artifact"), b"created").unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(10), wake.notified())
            .await
            .expect("native create event");
        assert!(watcher.take_error().is_none());
    }

    #[tokio::test]
    async fn a_proven_registration_carries_the_record_and_a_live_watcher() {
        let dir = tempfile::tempdir().unwrap();
        let guard = RegistrationGuard::default();
        let pending = guard.open(None, 5_000);
        let proven = prove_file_registration(
            &guard,
            &pending,
            FileRegistrationRequest {
                description: "artifact".into(),
                path: "artifact".into(),
                cwd: dir.path().to_path_buf(),
                event: FileEvent::Create,
                approved_parent: None,
            },
        )
        .await
        .unwrap();
        assert!(!proven.record.checkpoint.present);
        assert_eq!(proven.record.path, dir.path().join("artifact"));
        let wake = proven.watcher.wake();
        std::fs::write(dir.path().join("artifact"), b"created").unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(10), wake.notified())
            .await
            .expect("native create event");
    }

    #[tokio::test]
    async fn invalid_targets_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        let guard = RegistrationGuard::default();
        let request = |path: PathBuf| FileRegistrationRequest {
            description: "bad".into(),
            path,
            cwd: dir.path().to_path_buf(),
            event: FileEvent::Modify,
            approved_parent: None,
        };
        let pending = guard.open(None, 5_000);
        let missing = prove_file_registration(&guard, &pending, request("missing/x".into()))
            .await
            .err()
            .expect("missing parent");
        assert!(
            missing
                .to_string()
                .contains("parent directory does not exist")
        );
        #[cfg(unix)]
        {
            let outside = tempfile::tempdir().unwrap();
            std::fs::write(outside.path().join("secret"), b"secret").unwrap();
            std::os::unix::fs::symlink(outside.path().join("secret"), dir.path().join("link"))
                .unwrap();
            let symlinked = prove_file_registration(&guard, &pending, request("link".into()))
                .await
                .err()
                .expect("symlinked target");
            assert!(symlinked.to_string().contains("identity"), "{symlinked}");
        }
    }

    /// A registration whose deadline already passed (or that was finished by a dispose or
    /// `stopAllFiles`) fails before any filesystem step and gives its capacity back.
    #[tokio::test(start_paused = true)]
    async fn an_expired_or_finished_registration_fails_and_releases_capacity() {
        let dir = tempfile::tempdir().unwrap();
        let mut manager = crate::manager::TerminalManager::new(1);
        let guard = RegistrationGuard::default();
        let request = || FileRegistrationRequest {
            description: "late".into(),
            path: "artifact".into(),
            cwd: dir.path().to_path_buf(),
            event: FileEvent::Create,
            approved_parent: None,
        };
        let expired = guard.open(manager.reserve().unwrap(), 10);
        tokio::time::advance(std::time::Duration::from_millis(10)).await;
        let error = prove_file_registration(&guard, &expired, request())
            .await
            .err()
            .expect("expired");
        assert_eq!(error.to_string(), REGISTRATION_ABORTED);
        guard.finish(&expired, false);
        assert_eq!(manager.active_size().unwrap(), 0);

        let disposed = guard.open(manager.reserve().unwrap(), 5_000);
        guard.finish_all();
        assert_eq!(manager.active_size().unwrap(), 0);
        let error = prove_file_registration(&guard, &disposed, request())
            .await
            .err()
            .expect("disposed");
        assert!(error.to_string().contains("disposed"));
    }
}
