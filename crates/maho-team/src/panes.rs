//! The pane observer (spec §4): when a delegated task starts running it opens a pane that follows
//! the task's log and output (`tail -F`), and closes it a little after the task settles. Without a
//! multiplexer it does nothing (headless); open failures are kept per task and never affect the
//! run.
use crate::mux::{PaneBackend, PaneHandle, PaneRequest, shell_quote};
use crate::settings::{PaneMode, PaneSettings};
use maho_task::{StateDir, TaskObserver, TaskRecord};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[derive(Default)]
struct Panes {
    open: HashMap<String, PaneHandle>,
    /// Open split panes, oldest first (new splits stack off the newest).
    splits: Vec<String>,
    errors: HashMap<String, String>,
}

pub struct PaneObserver {
    backend: Option<Arc<dyn PaneBackend>>,
    settings: PaneSettings,
    panes: Mutex<Panes>,
}

/// The pane title of `record`.
pub fn pane_title(record: &TaskRecord) -> String {
    match &record.group {
        Some(team) => format!("team {team} · task {}", record.id),
        None => format!("task {}", record.id),
    }
}

/// The follower command shown in the pane.
pub fn follow_command(title: &str, log: &std::path::Path, output: &std::path::Path) -> Vec<String> {
    vec![
        "/bin/sh".into(),
        "-c".into(),
        format!(
            "printf '%s\\n\\n' {}; exec tail -n +1 -F {} {}",
            shell_quote(title),
            shell_quote(&log.to_string_lossy()),
            shell_quote(&output.to_string_lossy())
        ),
    ]
}

impl PaneObserver {
    pub fn new(backend: Option<Arc<dyn PaneBackend>>, settings: PaneSettings) -> Self {
        Self {
            backend,
            settings,
            panes: Mutex::default(),
        }
    }

    /// Detects the backend from the process environment.
    pub fn from_process_env() -> Self {
        let settings = PaneSettings::from_process_env();
        let backend = crate::mux::detect(&settings, &|key| std::env::var(key).ok()).map(Arc::from);
        Self::new(backend, settings)
    }

    /// `tmux`/`cmux`, or `None` when headless.
    pub fn backend_name(&self) -> Option<&'static str> {
        self.backend.as_ref().map(|backend| backend.kind().as_str())
    }

    /// Whether `record` gets a pane.
    pub fn wants(&self, record: &TaskRecord) -> bool {
        if self.backend.is_none() || self.settings.mode == PaneMode::Off {
            return false;
        }
        match record.pane {
            Some(choice) => choice,
            None => self.settings.mode == PaneMode::All || record.group.is_some(),
        }
    }

    /// The open pane of task `id`.
    pub fn pane_of(&self, id: &str) -> Option<PaneHandle> {
        lock(&self.panes).open.get(id).cloned()
    }

    /// Why the last pane of task `id` could not be opened.
    pub fn error_of(&self, id: &str) -> Option<String> {
        lock(&self.panes).errors.get(id).cloned()
    }

    fn close_now(backend: &Arc<dyn PaneBackend>, handle: &PaneHandle) {
        let _ = backend.close(handle);
    }
}

impl TaskObserver for PaneObserver {
    fn started_in(&self, record: &TaskRecord, state: &StateDir) {
        if !self.wants(record) {
            return;
        }
        let Some(backend) = &self.backend else {
            return;
        };
        let log = state.log_path(&record.id);
        let output = state.output_path(&record.id);
        // The follower starts before the child; `tail -F` copes with the truncation that follows.
        for path in [&log, &output] {
            if !path.exists() {
                let _ = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(path);
            }
        }
        let title = pane_title(record);
        let (anchor, first) = {
            let panes = lock(&self.panes);
            (panes.splits.last().cloned(), panes.splits.is_empty())
        };
        let request = PaneRequest {
            command: follow_command(&title, &log, &output),
            title,
            cwd: record.cwd.clone(),
            placement: self.settings.placement,
            anchor,
            first,
        };
        let mut panes = lock(&self.panes);
        match backend.open(&request) {
            Ok(handle) => {
                if !handle.window {
                    panes.splits.push(handle.id.clone());
                }
                panes.errors.remove(&record.id);
                if let Some(previous) = panes.open.insert(record.id.clone(), handle) {
                    Self::close_now(backend, &previous);
                }
            }
            Err(error) => {
                panes.errors.insert(record.id.clone(), error);
            }
        }
    }

    fn finished_in(&self, record: &TaskRecord, _state: &StateDir) {
        let Some(backend) = &self.backend else {
            return;
        };
        let handle = {
            let mut panes = lock(&self.panes);
            let Some(handle) = panes.open.remove(&record.id) else {
                return;
            };
            panes.splits.retain(|id| *id != handle.id);
            handle
        };
        let linger = self.settings.linger;
        if linger.is_zero() {
            Self::close_now(backend, &handle);
            return;
        }
        let backend = Arc::clone(backend);
        let _ = std::thread::Builder::new()
            .name("pane-close".into())
            .spawn(move || {
                std::thread::sleep(linger);
                Self::close_now(&backend, &handle);
            });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::{MuxKind, Placement};
    use std::time::Duration;

    #[derive(Default)]
    struct Recorder {
        calls: Mutex<Vec<String>>,
        fail: bool,
    }

    impl PaneBackend for Recorder {
        fn kind(&self) -> MuxKind {
            MuxKind::Tmux
        }
        fn open(&self, request: &PaneRequest) -> Result<PaneHandle, String> {
            if self.fail {
                return Err("split-window failed (exit 1)".into());
            }
            let mut calls = lock(&self.calls);
            let id = format!("%{}", calls.len() + 1);
            calls.push(format!(
                "open {} first={} anchor={:?}",
                request.title, request.first, request.anchor
            ));
            Ok(PaneHandle { id, window: false })
        }
        fn close(&self, handle: &PaneHandle) -> Result<(), String> {
            lock(&self.calls).push(format!("close {}", handle.id));
            Ok(())
        }
    }

    fn record(id: &str, group: Option<&str>, pane: Option<bool>) -> TaskRecord {
        let mut record: TaskRecord = serde_json::from_value(serde_json::json!({
            "id": id, "agent": "task", "description": "d", "status": "running", "model": null,
            "thinking": null, "model_source": "parent", "session_id": "s", "cwd": "/",
            "background": true, "pid": null, "exit_code": null, "error": null, "runs": 1,
            "started_at_ms": 1, "finished_at_ms": null
        }))
        .expect("record");
        record.group = group.map(str::to_owned);
        record.pane = pane;
        record
    }

    #[test]
    fn opens_stacks_and_closes() {
        let dir = tempfile::tempdir().expect("tmp");
        let state = StateDir::new(dir.path());
        let backend = Arc::new(Recorder::default());
        let settings = PaneSettings {
            linger: Duration::ZERO,
            placement: Placement::Split,
            ..PaneSettings::default()
        };
        let observer =
            PaneObserver::new(Some(Arc::clone(&backend) as Arc<dyn PaneBackend>), settings);
        let plain = record("Plain", None, None);
        assert!(!observer.wants(&plain));
        assert!(observer.wants(&record("Forced", None, Some(true))));
        assert!(!observer.wants(&record("Off", Some("t"), Some(false))));
        observer.started_in(&plain, &state);
        let a = record("t-a", Some("t"), None);
        let b = record("t-b", Some("t"), None);
        observer.started_in(&a, &state);
        observer.started_in(&b, &state);
        assert!(state.log_path("t-a").exists());
        assert_eq!(observer.pane_of("t-b").map(|h| h.id).as_deref(), Some("%2"));
        observer.finished_in(&a, &state);
        observer.finished_in(&b, &state);
        assert_eq!(
            *lock(&backend.calls),
            [
                "open team t · task t-a first=true anchor=None",
                "open team t · task t-b first=false anchor=Some(\"%1\")",
                "close %1",
                "close %2"
            ]
        );
        assert!(observer.pane_of("t-a").is_none());
    }

    #[test]
    fn headless_and_failures() {
        let dir = tempfile::tempdir().expect("tmp");
        let state = StateDir::new(dir.path());
        let headless = PaneObserver::new(None, PaneSettings::default());
        assert!(headless.backend_name().is_none());
        assert!(!headless.wants(&record("t-a", Some("t"), Some(true))));
        let failing = PaneObserver::new(
            Some(Arc::new(Recorder {
                fail: true,
                ..Recorder::default()
            })),
            PaneSettings {
                mode: PaneMode::All,
                ..PaneSettings::default()
            },
        );
        let plain = record("Plain", None, None);
        failing.started_in(&plain, &state);
        assert!(
            failing
                .error_of("Plain")
                .is_some_and(|e| e.contains("split-window"))
        );
        failing.finished_in(&plain, &state);
    }

    #[test]
    fn follower_quotes_paths() {
        let command = follow_command(
            "it's",
            std::path::Path::new("/a b/x.log"),
            std::path::Path::new("/c/x.md"),
        );
        assert_eq!(
            command[2],
            "printf '%s\\n\\n' 'it'\\''s'; exec tail -n +1 -F '/a b/x.log' '/c/x.md'"
        );
    }
}
