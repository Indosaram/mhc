//! What the TUI tells a person about memory sync (#10637).
//!
//! The footer shows ` sync blocked` while a failure is shown, and one notice says why and what
//! to do when it starts or changes kind, then one more when sync recovers. The engine reports
//! transitions only, so a cycle that keeps failing the same way never repeats the notice.

use std::collections::BTreeMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, Mutex};

pub const SYNC_RECOVERED_NOTICE: &str = "Memory sync is working again.";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SyncNoticeLevel {
    Info,
    Warning,
    Error,
}

pub trait SyncNoticeUi: Send + Sync {
    fn notify(&self, message: &str, level: SyncNoticeLevel);
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncSurfaceFailure {
    pub kind: String,
    pub reason: String,
    pub action: String,
    pub paths: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SyncSurfaceStatus {
    Synced,
    NeverSynced,
    Paused,
    Unavailable,
    ConfigInvalid { variable: String },
    Failing(SyncSurfaceFailure),
}

pub type SyncStatusReader = Arc<
    dyn Fn(&Path) -> Pin<Box<dyn Future<Output = Option<SyncSurfaceStatus>> + Send>> + Send + Sync,
>;

pub struct SyncStatusNotifier {
    read: SyncStatusReader,
    announced: Mutex<BTreeMap<PathBuf, String>>,
}

impl SyncStatusNotifier {
    pub fn new(read: SyncStatusReader) -> Self {
        Self {
            read,
            announced: Mutex::new(BTreeMap::new()),
        }
    }

    pub async fn changed(&self, repo: &Path, ui: Option<&dyn SyncNoticeUi>) {
        let status = (self.read)(repo).await;
        let failure = match status {
            Some(SyncSurfaceStatus::Failing(f)) => Some(f),
            _ => None,
        };
        let mut announced = self.announced.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let previous = announced.get(repo).cloned();
        let Some(failure) = failure else {
            announced.remove(repo);
            if previous.is_some()
                && let Some(ui) = ui {
                    ui.notify(SYNC_RECOVERED_NOTICE, SyncNoticeLevel::Info);
                }
            return;
        };
        let key = if failure.paths.is_empty() {
            format!("{}\n{}", failure.kind, failure.action)
        } else {
            format!("{}\n{}\n{}", failure.kind, failure.action, failure.paths.join(","))
        };
        if previous.as_deref() == Some(&key) {
            return;
        }
        announced.insert(repo.to_path_buf(), key);
        if let Some(ui) = ui {
            let message = format!("Memory sync is blocked. {} {}", failure.reason, failure.action);
            ui.notify(&message, SyncNoticeLevel::Warning);
        }
    }
}

pub struct SyncStatusChange<'a> {
    pub repo: &'a Path,
    pub is_active_repo: bool,
    pub notifier: Option<&'a SyncStatusNotifier>,
    pub ui: Option<&'a dyn SyncNoticeUi>,
    pub refresh_footer: Box<dyn Fn() + 'a>,
    pub push_rpc: Box<dyn Fn() -> Pin<Box<dyn Future<Output = ()> + Send>> + 'a>,
}

pub async fn deliver_sync_status_change(change: SyncStatusChange<'_>) {
    if !change.is_active_repo {
        if let Some(notifier) = change.notifier {
            notifier.changed(change.repo, None).await;
        }
        return;
    }
    if let Some(notifier) = change.notifier {
        notifier.changed(change.repo, change.ui).await;
    }
    (change.refresh_footer)();
    (change.push_rpc)().await;
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TestUi {
        notices: Mutex<Vec<(String, SyncNoticeLevel)>>,
    }

    impl TestUi {
        fn new() -> Self {
            Self {
                notices: Mutex::new(Vec::new()),
            }
        }
    }

    impl SyncNoticeUi for TestUi {
        fn notify(&self, message: &str, level: SyncNoticeLevel) {
            self.notices.lock().unwrap().push((message.to_string(), level));
        }
    }

    fn failing(kind: &str, reason: &str, action: &str) -> SyncSurfaceStatus {
        SyncSurfaceStatus::Failing(SyncSurfaceFailure {
            kind: kind.to_string(),
            reason: reason.to_string(),
            action: action.to_string(),
            paths: Vec::new(),
        })
    }

    #[tokio::test]
    async fn given_sync_transitions_when_notifier_hears_them_then_person_gets_one_notice_per_failure() {
        let current_status: Arc<Mutex<Option<SyncSurfaceStatus>>> =
            Arc::new(Mutex::new(Some(SyncSurfaceStatus::Synced)));
        let status_ref = current_status.clone();
        let reader: SyncStatusReader = Arc::new(move |_repo| {
            let s = status_ref.lock().unwrap().clone();
            Box::pin(async move { s })
        });
        let notifier = SyncStatusNotifier::new(reader);
        let ui = TestUi::new();

        notifier.changed(Path::new("/repo"), Some(&ui)).await;
        assert!(ui.notices.lock().unwrap().is_empty());

        *current_status.lock().unwrap() = Some(failing("offline", "Cannot reach the service.", "Check your connection."));
        notifier.changed(Path::new("/repo"), Some(&ui)).await;
        notifier.changed(Path::new("/repo"), Some(&ui)).await;

        *current_status.lock().unwrap() = Some(failing("auth_expired", "Sign-in expired.", "Sign in to OmO again."));
        notifier.changed(Path::new("/repo"), Some(&ui)).await;

        *current_status.lock().unwrap() = Some(SyncSurfaceStatus::Synced);
        notifier.changed(Path::new("/repo"), Some(&ui)).await;
        notifier.changed(Path::new("/repo"), Some(&ui)).await;

        let notices = ui.notices.lock().unwrap().clone();
        assert_eq!(
            notices,
            vec![
                (
                    "Memory sync is blocked. Cannot reach the service. Check your connection.".to_string(),
                    SyncNoticeLevel::Warning
                ),
                (
                    "Memory sync is blocked. Sign-in expired. Sign in to OmO again.".to_string(),
                    SyncNoticeLevel::Warning
                ),
                (SYNC_RECOVERED_NOTICE.to_string(), SyncNoticeLevel::Info),
            ]
        );
    }

    #[tokio::test]
    async fn given_a_stranded_lock_that_moves_then_new_notice_naming_new_path() {
        let current_status: Arc<Mutex<Option<SyncSurfaceStatus>>> = Arc::new(Mutex::new(Some(
            SyncSurfaceStatus::Failing(SyncSurfaceFailure {
                kind: "stranded_lock".to_string(),
                reason: "A git lock blocks sync.".to_string(),
                action: "Run /memory unlock /m/.git/index.lock.".to_string(),
                paths: vec!["/m/.git/index.lock".to_string()],
            }),
        )));
        let status_ref = current_status.clone();
        let reader: SyncStatusReader = Arc::new(move |_repo| {
            let s = status_ref.lock().unwrap().clone();
            Box::pin(async move { s })
        });
        let notifier = SyncStatusNotifier::new(reader);
        let ui = TestUi::new();

        notifier.changed(Path::new("/repo"), Some(&ui)).await;

        *current_status.lock().unwrap() = Some(SyncSurfaceStatus::Failing(SyncSurfaceFailure {
            kind: "stranded_lock".to_string(),
            reason: "A git lock blocks sync.".to_string(),
            action: "Run /memory unlock /m/.git/HEAD.lock.".to_string(),
            paths: vec!["/m/.git/HEAD.lock".to_string()],
        }));
        notifier.changed(Path::new("/repo"), Some(&ui)).await;
        notifier.changed(Path::new("/repo"), Some(&ui)).await;

        let notices = ui.notices.lock().unwrap().clone();
        assert_eq!(
            notices,
            vec![
                (
                    "Memory sync is blocked. A git lock blocks sync. Run /memory unlock /m/.git/index.lock.".to_string(),
                    SyncNoticeLevel::Warning
                ),
                (
                    "Memory sync is blocked. A git lock blocks sync. Run /memory unlock /m/.git/HEAD.lock.".to_string(),
                    SyncNoticeLevel::Warning
                ),
            ]
        );
    }
}
