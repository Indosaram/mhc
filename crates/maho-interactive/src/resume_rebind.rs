//! Port of senpi packages/coding-agent/src/modes/interactive/resume-rebind.ts.
//!
//! `/resume` lists this folder's sessions plus the moved sessions of its repository
//! (`current_scope_sessions`), marks the moved ones in the all-folders scope (`all_scope_sessions`),
//! and resolves a picked session through `choose_resume_path`: a session of this repository recorded
//! at another path gets the rebind question (#2181); a failed move is reported and opens nothing.

use std::future::Future;
use std::pin::Pin;

use maho_core::moved_sessions::{mark_moved_sessions_with, with_moved_sessions_with};
use maho_core::repository_identity::read_repository_identity;
use maho_core::resume_target::resolve_resume_target_with;
use maho_core::session_discovery::{SessionInfo, SessionListProgress};
use maho_core::session_manager::SessionManager;
use maho_core::session_move::bundle::RepositoryIdentity;

/// senpi `REBIND_PROMPT` (cli/cross-project-session.ts).
pub const REBIND_PROMPT: &str = "Move this session here and continue it?";

/// The parts of the session manager the scopes read (senpi `SessionScopeSource`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionScopeSource {
    pub cwd: String,
    pub session_dir: String,
    pub uses_default_session_dir: bool,
}

impl SessionScopeSource {
    pub fn of(manager: &SessionManager) -> Self {
        Self {
            cwd: manager.cwd().to_owned(),
            session_dir: manager.session_dir().to_owned(),
            uses_default_session_dir: manager.uses_default_session_dir(),
        }
    }

    /// senpi `sharedSessionDir`.
    fn shared_session_dir(&self) -> Option<&str> {
        (!self.uses_default_session_dir).then_some(self.session_dir.as_str())
    }
}

pub type ConfirmFuture<'a> = Pin<Box<dyn Future<Output = bool> + 'a>>;

/// senpi `ResumeRebindUi`.
pub trait ResumeRebindUi {
    fn confirm<'a>(&'a self, title: &'a str, message: &'a str) -> ConfirmFuture<'a>;
    fn show_error(&self, message: &str);
}

async fn git_identity(dir: String) -> Option<RepositoryIdentity> {
    read_repository_identity(&dir).await
}

/// senpi `currentScopeSessions`.
pub async fn current_scope_sessions(
    source: &SessionScopeSource,
    on_progress: Option<SessionListProgress<'_>>,
) -> Vec<SessionInfo> {
    current_scope_sessions_with(source, on_progress, git_identity).await
}

/// [`current_scope_sessions`] with an injected repository identity reader.
pub async fn current_scope_sessions_with<F, Fut>(
    source: &SessionScopeSource,
    on_progress: Option<SessionListProgress<'_>>,
    read_identity: F,
) -> Vec<SessionInfo>
where
    F: Fn(String) -> Fut,
    Fut: Future<Output = Option<RepositoryIdentity>>,
{
    let local = SessionManager::list(&source.cwd, Some(&source.session_dir), on_progress);
    with_moved_sessions_with(
        local,
        &source.cwd,
        source.shared_session_dir(),
        read_identity,
    )
    .await
}

/// senpi `allScopeSessions`.
pub async fn all_scope_sessions(
    source: &SessionScopeSource,
    on_progress: Option<SessionListProgress<'_>>,
) -> Vec<SessionInfo> {
    all_scope_sessions_with(source, on_progress, git_identity).await
}

/// [`all_scope_sessions`] with an injected repository identity reader.
pub async fn all_scope_sessions_with<F, Fut>(
    source: &SessionScopeSource,
    on_progress: Option<SessionListProgress<'_>>,
    read_identity: F,
) -> Vec<SessionInfo>
where
    F: Fn(String) -> Fut,
    Fut: Future<Output = Option<RepositoryIdentity>>,
{
    let sessions = SessionManager::list_all(source.shared_session_dir(), on_progress);
    mark_moved_sessions_with(&sessions, &source.cwd, read_identity).await
}

/// senpi `chooseResumePath`: the path `/resume` should open for a picked session, or `None` when
/// moving it failed (already reported).
pub async fn choose_resume_path(
    session_path: &str,
    source: &SessionScopeSource,
    ui: &dyn ResumeRebindUi,
) -> Option<String> {
    choose_resume_path_with(session_path, source, ui, git_identity).await
}

/// [`choose_resume_path`] with an injected repository identity reader.
pub async fn choose_resume_path_with<F, Fut>(
    session_path: &str,
    source: &SessionScopeSource,
    ui: &dyn ResumeRebindUi,
    read_identity: F,
) -> Option<String>
where
    F: Fn(String) -> Fut,
    Fut: Future<Output = Option<RepositoryIdentity>>,
{
    let cwd = source.cwd.as_str();
    let target = resolve_resume_target_with(
        session_path,
        cwd,
        source.shared_session_dir(),
        |session_cwd| async move {
            let message = format!(
                "Session found in different project: {session_cwd}\nThis directory is the same git repository: {cwd}\nChoose No to open it where it is."
            );
            ui.confirm(REBIND_PROMPT, &message).await
        },
        read_identity,
    )
    .await;
    match target {
        Ok(target) => Some(target.path),
        Err(error) => {
            ui.show_error(&format!("Could not move session: {error}"));
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::path::Path;

    use serde_json::json;

    use super::*;

    const ROOT_COMMIT: &str = "cccccccccccccccccccccccccccccccccccccccc";

    #[derive(Default)]
    struct FakeUi {
        answer: bool,
        asked: RefCell<Vec<(String, String)>>,
        errors: RefCell<Vec<String>>,
    }

    impl ResumeRebindUi for FakeUi {
        fn confirm<'a>(&'a self, title: &'a str, message: &'a str) -> ConfirmFuture<'a> {
            self.asked
                .borrow_mut()
                .push((title.to_owned(), message.to_owned()));
            let answer = self.answer;
            Box::pin(async move { answer })
        }
        fn show_error(&self, message: &str) {
            self.errors.borrow_mut().push(message.to_owned());
        }
    }

    fn same(_: String) -> std::future::Ready<Option<RepositoryIdentity>> {
        std::future::ready(Some(RepositoryIdentity {
            root_commits: vec![ROOT_COMMIT.to_owned()],
            origin_url: None,
        }))
    }

    struct Fixture {
        _root: tempfile::TempDir,
        old_cwd: String,
        cwd: String,
        target_dir: std::path::PathBuf,
        session_file: String,
    }

    /// A session of this repository recorded at a path that no longer exists.
    fn fixture() -> Fixture {
        let root = tempfile::tempdir().expect("root");
        let base = std::fs::canonicalize(root.path()).expect("real root");
        let old_cwd = base.join("old/repo");
        let cwd = base.join("new/repo");
        std::fs::create_dir_all(&cwd).expect("cwd");
        let dir = base.join("sessions/old");
        std::fs::create_dir_all(&dir).expect("session dir");
        let session_file = dir.join("2026-09-27_0197f6e4-4cf9-7f44-a2d8-f8f7f49ee9d3.jsonl");
        let lines = [
            json!({"type": "session", "version": 3, "id": "0197f6e4-4cf9-7f44-a2d8-f8f7f49ee9d3", "timestamp": "2026-09-27T00:00:00.000Z", "cwd": old_cwd}).to_string(),
            json!({"type": "message", "id": "u1", "parentId": null, "timestamp": "2026-09-27T00:00:00.000Z", "message": {"role": "user", "content": "hello", "timestamp": 1}}).to_string(),
            json!({"type": "custom", "customType": "repository-identity", "data": {"rootCommits": [ROOT_COMMIT]}}).to_string(),
        ];
        std::fs::write(&session_file, lines.join("\n") + "\n").expect("session file");
        Fixture {
            old_cwd: old_cwd.to_string_lossy().into_owned(),
            cwd: cwd.to_string_lossy().into_owned(),
            target_dir: base.join("sessions/new"),
            session_file: session_file.to_string_lossy().into_owned(),
            _root: root,
        }
    }

    fn shared(fixture: &Fixture, dir: &Path) -> SessionScopeSource {
        SessionScopeSource {
            cwd: fixture.cwd.clone(),
            session_dir: dir.to_string_lossy().into_owned(),
            uses_default_session_dir: false,
        }
    }

    #[tokio::test]
    async fn a_confirmed_rebind_opens_the_moved_file_and_asks_with_both_paths() {
        let fixture = fixture();
        let ui = FakeUi {
            answer: true,
            ..FakeUi::default()
        };
        let source = shared(&fixture, &fixture.target_dir);
        let path = choose_resume_path_with(&fixture.session_file, &source, &ui, same)
            .await
            .expect("path");
        assert_eq!(
            Path::new(&path).parent(),
            Some(fixture.target_dir.as_path())
        );
        assert!(!Path::new(&fixture.session_file).exists());
        assert_eq!(
            *ui.asked.borrow(),
            [(
                REBIND_PROMPT.to_owned(),
                format!(
                    "Session found in different project: {}\nThis directory is the same git repository: {}\nChoose No to open it where it is.",
                    fixture.old_cwd, fixture.cwd
                )
            )]
        );
        assert!(ui.errors.borrow().is_empty());
    }

    #[tokio::test]
    async fn declining_opens_the_session_where_it_is() {
        let fixture = fixture();
        let ui = FakeUi::default();
        let source = shared(&fixture, &fixture.target_dir);
        let path = choose_resume_path_with(&fixture.session_file, &source, &ui, same).await;
        assert_eq!(path.as_deref(), Some(fixture.session_file.as_str()));
        assert!(Path::new(&fixture.session_file).exists());
        assert_eq!(ui.asked.borrow().len(), 1);
    }

    #[tokio::test]
    async fn a_failed_move_is_reported_and_opens_nothing() {
        let fixture = fixture();
        // A regular file where the target session dir would be: the move cannot create it.
        let blocker = fixture.target_dir.with_file_name("blocker");
        std::fs::write(&blocker, "").expect("blocker");
        let ui = FakeUi {
            answer: true,
            ..FakeUi::default()
        };
        let source = shared(&fixture, &blocker.join("sessions"));
        let path = choose_resume_path_with(&fixture.session_file, &source, &ui, same).await;
        assert_eq!(path, None);
        let errors = ui.errors.borrow();
        assert_eq!(errors.len(), 1);
        assert!(
            errors[0].starts_with("Could not move session: "),
            "{errors:?}"
        );
        assert!(Path::new(&fixture.session_file).exists());
    }

    #[tokio::test]
    async fn the_current_scope_of_a_shared_dir_lists_the_moved_session_of_this_repository() {
        let fixture = fixture();
        let source = shared(
            &fixture,
            Path::new(&fixture.session_file).parent().expect("dir"),
        );
        let sessions = current_scope_sessions_with(&source, None, same).await;
        assert_eq!(
            sessions
                .iter()
                .map(|session| (session.path.as_str(), session.moved))
                .collect::<Vec<_>>(),
            [(fixture.session_file.as_str(), true)]
        );
    }

    #[tokio::test]
    async fn the_all_scope_marks_the_moved_session() {
        let fixture = fixture();
        let source = shared(
            &fixture,
            Path::new(&fixture.session_file).parent().expect("dir"),
        );
        let sessions = all_scope_sessions_with(&source, None, same).await;
        assert_eq!(sessions.len(), 1);
        assert!(sessions[0].moved);
        let unrelated = all_scope_sessions_with(&source, None, |_| std::future::ready(None)).await;
        assert!(!unrelated[0].moved);
    }
}
