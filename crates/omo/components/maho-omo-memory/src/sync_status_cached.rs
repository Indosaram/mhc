//! What the TUI tells a person about memory sync (`sync-status-surface.ts`): the footer shows
//! ` sync blocked` while a failure is shown (`status.rs`), and one notice says why and what to do
//! when it starts or changes kind, then one more when sync recovers. The engine reports transitions
//! only, so a cycle that keeps failing the same way never repeats the notice.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, Mutex, PoisonError};

use crate::sync_runner::SyncRepoStatus;

pub const SYNC_RECOVERED_NOTICE: &str = "Memory sync is working again.";

/// One shown sync failure: what broke, and the next step the person can take.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncFailure {
    pub kind: String,
    pub reason: String,
    pub action: String,
}

/// What a repository's sync reader reports: the engine's status, a shown failure, or one of the two
/// "sync is off" reports (unavailable, invalid configuration), which are not failing cycles and so
/// are treated like "nothing to show" (the runner already logs why sync is off).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SyncStatusReport {
    Status(SyncRepoStatus),
    Failing(SyncFailure),
}

impl SyncStatusReport {
    fn failure(&self) -> Option<&SyncFailure> {
        match self {
            Self::Failing(failure) => Some(failure),
            Self::Status(_) => None,
        }
    }
}

/// Reads one repository's sync status. Synchronous because every footer refresh in this port runs
/// inside a sync probe; a runner keeps the last engine status per repository and hands it out here.
pub type SyncStatusReader = Arc<dyn Fn(&Path) -> Option<SyncStatusReport> + Send + Sync>;

/// The host surface the notices are written to.
pub trait SyncNoticeUi {
    fn notify(&self, message: &str, level: &str);
}

/// Remembers the failure kind last announced per repository, so only transitions speak.
pub struct SyncStatusNotifier {
    read: SyncStatusReader,
    announced: Mutex<BTreeMap<String, String>>,
}

impl SyncStatusNotifier {
    pub fn new(read: SyncStatusReader) -> Self {
        Self {
            read,
            announced: Mutex::new(BTreeMap::new()),
        }
    }

    pub fn changed(&self, repo: &Path, ui: Option<&dyn SyncNoticeUi>) {
        let status = (self.read)(repo);
        let key_repo = repo.to_string_lossy().into_owned();
        let mut announced = self
            .announced
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let failure = status.as_ref().and_then(|s| s.failure());
        let previous = announced.get(&key_repo).cloned();
        let Some(failure) = failure else {
            announced.remove(&key_repo);
            if previous.is_some()
                && let Some(ui) = ui
            {
                ui.notify(SYNC_RECOVERED_NOTICE, "info");
            }
            return;
        };
        let key = format!("{}\n{}", failure.kind, failure.action);
        if previous.as_deref() == Some(key.as_str()) {
            return;
        }
        announced.insert(key_repo, key);
        if let Some(ui) = ui {
            ui.notify(
                &format!(
                    "Memory sync is blocked. {} {}",
                    failure.reason, failure.action
                ),
                "warning",
            );
        }
    }
}

pub struct SyncStatusChange<'a> {
    pub repo: &'a Path,
    /// Whether the repo belongs to the active session; only that session's footer and notices show it.
    pub is_active_repo: bool,
    pub notifier: Option<&'a SyncStatusNotifier>,
    pub ui: Option<&'a dyn SyncNoticeUi>,
    /// Redraws the active session's footer, then pushes the desktop snapshot.
    pub apply: &'a mut dyn FnMut() -> Result<(), String>,
}

/// One engine status change, delivered to the notice, the footer and the desktop snapshot. A repo that
/// is not the active session's still feeds the notifier (with no UI), so its notice state follows
/// every transition.
pub fn deliver_sync_status_change(change: SyncStatusChange<'_>) -> Result<(), String> {
    if !change.is_active_repo {
        if let Some(notifier) = change.notifier {
            notifier.changed(change.repo, None);
        }
        return Ok(());
    }
    if let Some(notifier) = change.notifier {
        notifier.changed(change.repo, change.ui);
    }
    (change.apply)()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    #[derive(Default)]
    struct Ui(Vec<(String, String)>);

    impl SyncNoticeUi for RefCell<Ui> {
        fn notify(&self, message: &str, level: &str) {
            self.borrow_mut().0.push((message.into(), level.into()));
        }
    }

    fn failing(kind: &str, reason: &str, action: &str) -> Option<SyncStatusReport> {
        Some(SyncStatusReport::Failing(SyncFailure {
            kind: kind.into(),
            reason: reason.into(),
            action: action.into(),
        }))
    }

    fn reader(status: Option<SyncStatusReport>) -> SyncStatusReader {
        Arc::new(move |_: &Path| status.clone())
    }

    #[test]
    fn the_first_failure_is_announced_once_and_a_repeat_is_silent() {
        let notifier =
            SyncStatusNotifier::new(reader(failing("auth", "Token expired.", "Run /login.")));
        let ui = RefCell::new(Ui::default());
        notifier.changed(Path::new("/repo"), Some(&ui));
        notifier.changed(Path::new("/repo"), Some(&ui));
        let notices = &ui.borrow().0;
        assert_eq!(notices.len(), 1);
        assert_eq!(
            notices[0].0,
            "Memory sync is blocked. Token expired. Run /login."
        );
        assert_eq!(notices[0].1, "warning");
    }

    #[test]
    fn a_changed_failure_kind_speaks_again_and_a_recovery_says_so_once() {
        let status = Arc::new(Mutex::new(failing("auth", "Token expired.", "Run /login.")));
        let seen = status.clone();
        let notifier =
            SyncStatusNotifier::new(Arc::new(move |_: &Path| seen.lock().unwrap().clone()));
        let ui = RefCell::new(Ui::default());
        notifier.changed(Path::new("/repo"), Some(&ui));
        *status.lock().unwrap() = failing("network", "Host unreachable.", "Check your connection.");
        notifier.changed(Path::new("/repo"), Some(&ui));
        *status.lock().unwrap() = None;
        notifier.changed(Path::new("/repo"), Some(&ui));
        notifier.changed(Path::new("/repo"), Some(&ui));
        let notices = ui.borrow().0.clone();
        assert_eq!(notices.len(), 3);
        assert!(notices[1].0.contains("Host unreachable."));
        assert_eq!(notices[2], (SYNC_RECOVERED_NOTICE.into(), "info".into()));
    }

    #[test]
    fn a_failed_read_changes_no_notice() {
        let calls = Arc::new(Mutex::new(0));
        let seen = calls.clone();
        let notifier = SyncStatusNotifier::new(Arc::new(move |_: &Path| {
            *seen.lock().unwrap() += 1;
            None
        }));
        let ui = RefCell::new(Ui::default());
        notifier.changed(Path::new("/repo"), Some(&ui));
        assert!(ui.borrow().0.is_empty());
        assert_eq!(*calls.lock().unwrap(), 1);
    }

    #[test]
    fn an_inactive_repository_feeds_the_notifier_without_touching_the_footer() {
        let notifier =
            SyncStatusNotifier::new(reader(failing("auth", "Token expired.", "Run /login.")));
        let ui = RefCell::new(Ui::default());
        let footer = std::cell::Cell::new(0);
        let rpc = std::cell::Cell::new(0);
        let mut apply = || {
            footer.set(footer.get() + 1);
            rpc.set(rpc.get() + 1);
            Ok(())
        };
        deliver_sync_status_change(SyncStatusChange {
            repo: Path::new("/other"),
            is_active_repo: false,
            notifier: Some(&notifier),
            ui: Some(&ui),
            apply: &mut apply,
        })
        .unwrap();
        assert!(ui.borrow().0.is_empty());
        assert_eq!((footer.get(), rpc.get()), (0, 0));
        // Its state still followed the transition, so the next active change knows it was already announced.
        let active = RefCell::new(Ui::default());
        let mut redraw = || {
            footer.set(footer.get() + 1);
            Ok(())
        };
        deliver_sync_status_change(SyncStatusChange {
            repo: Path::new("/other"),
            is_active_repo: true,
            notifier: Some(&notifier),
            ui: Some(&active),
            apply: &mut redraw,
        })
        .unwrap();
        assert!(active.borrow().0.is_empty());
        assert_eq!(footer.get(), 1);
    }
}

#[cfg(test)]
mod rpc_tests {
    use super::*;
    #[test]
    fn sync_rpc_preserves_failure_details_and_machine_consumed_fields() {
        use memory_core::sync::engine::{
            EngineStatus,
            failure::{SyncFailure as EngineFailure, SyncFailureKind},
        };
        let value = rpc_status(&crate::sync_runner::SyncRepoStatus::Engine(
            EngineStatus::Failing {
                failure: EngineFailure {
                    kind: SyncFailureKind::StrandedLock,
                    paths: vec!["/repo/.git/index.lock".into()],
                    reason: "locked".into(),
                    action: "unlock".into(),
                },
                since: 0,
                attempts: 3,
            },
        ));
        assert_eq!(
            value,
            serde_json::json!({"state":"failing","failure":{"kind":"stranded_lock","reason":"locked","action":"unlock","sinceISO":"1970-01-01T00:00:00.000Z","attempts":3,"paths":["/repo/.git/index.lock"]}})
        );
        assert_eq!(
            rpc_status(&crate::sync_runner::SyncRepoStatus::Engine(
                EngineStatus::Synced {
                    at: 0,
                    updated_while_offline: 0
                }
            )),
            serde_json::json!({"state":"synced","lastSyncedAtISO":"1970-01-01T00:00:00.000Z"})
        );
    }
}

pub fn report(status: crate::sync_runner::SyncRepoStatus) -> SyncStatusReport {
    match status {
        crate::sync_runner::SyncRepoStatus::Engine(
            memory_core::sync::engine::EngineStatus::Failing { failure, .. },
        ) => SyncStatusReport::Failing(SyncFailure {
            kind: failure_kind(failure.kind).into(),
            reason: failure.reason,
            action: failure.action,
        }),
        other => SyncStatusReport::Status(other),
    }
}
pub fn failure_kind(kind: memory_core::sync::engine::failure::SyncFailureKind) -> &'static str {
    use memory_core::sync::engine::failure::SyncFailureKind::*;
    match kind {
        StrandedLock => "stranded_lock",
        AuthExpired => "auth_expired",
        Offline => "offline",
        Unknown => "unknown",
    }
}
pub fn rpc_status(status: &crate::sync_runner::SyncRepoStatus) -> serde_json::Value {
    use memory_core::sync::engine::EngineStatus;
    let iso = |time| {
        chrono::DateTime::from_timestamp_millis(time)
            .map(|time| time.to_rfc3339_opts(chrono::SecondsFormat::Millis, true))
    };
    match status {
        crate::sync_runner::SyncRepoStatus::Unavailable { .. } => {
            serde_json::json!({"state":"unavailable"})
        }
        crate::sync_runner::SyncRepoStatus::Engine(state) => match state {
            EngineStatus::NeverSynced => serde_json::json!({"state":"never_synced"}),
            EngineStatus::Synced { at, .. } => {
                serde_json::json!({"state":"synced","lastSyncedAtISO":iso(*at)})
            }
            EngineStatus::Paused { .. } => serde_json::json!({"state":"paused"}),
            EngineStatus::Failing {
                failure,
                since,
                attempts,
            } => {
                let mut value = serde_json::json!({"state":"failing","failure":{"kind":failure_kind(failure.kind),"reason":failure.reason,"action":failure.action,"sinceISO":iso(*since),"attempts":attempts}});
                if failure.kind == memory_core::sync::engine::failure::SyncFailureKind::StrandedLock
                {
                    value["failure"]["paths"] = serde_json::json!(failure.paths);
                }
                value
            }
        },
    }
}
