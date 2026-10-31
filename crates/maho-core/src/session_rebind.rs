//! Port of senpi packages/coding-agent/src/core/session-rebind.ts: repository classification.
//!
//! `readSessionCwd`, `readRecordedRepositoryIdentity`, `rebindSessionFile` and the sidecar move live in
//! `session_cwd.rs` (senpi-core-session lane); this module adds `classifySessionRepository` on top.

use std::path::Path;

use crate::repository_identity::compare_repository_identities;
use crate::session_cwd::read_recorded_repository_identity;
use crate::session_move::bundle::RepositoryIdentity;

/// Whether `cwd` is the git repository a session was recorded in (`"same"`, `"different"` or
/// `"unknown"`). The session side prefers the live checkout at its recorded cwd and falls back to the
/// identity the session recorded, because a moved repository leaves nothing at the old path to ask.
pub async fn classify_session_repository<F, Fut>(
    session_file: &str,
    session_cwd: &str,
    cwd: &str,
    read: F,
) -> &'static str
where
    F: Fn(String) -> Fut,
    Fut: std::future::Future<Output = Option<RepositoryIdentity>>,
{
    let current = read(cwd.to_owned());
    let live = async {
        if tokio::fs::try_exists(session_cwd).await.unwrap_or(false) {
            read(session_cwd.to_owned()).await
        } else {
            None
        }
    };
    let (current, live) = futures::join!(current, live);
    let recorded = match live {
        Some(live) => Some(live),
        None => read_recorded_repository_identity(Path::new(session_file)).await,
    };
    compare_repository_identities(recorded.as_ref(), current.as_ref())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn identity(root: &str) -> RepositoryIdentity {
        RepositoryIdentity {
            root_commits: vec![root.repeat(40)],
            origin_url: None,
        }
    }

    #[tokio::test]
    async fn falls_back_to_the_recorded_identity_when_the_old_checkout_is_gone() {
        let dir = tempfile::tempdir().expect("tempdir");
        let file = dir.path().join("s.jsonl");
        let lines = [
            json!({"type": "session", "id": "s"}).to_string(),
            json!({"type": "custom", "customType": "repository-identity", "data": {"rootCommits": ["a".repeat(40)]}})
                .to_string(),
        ];
        std::fs::write(&file, lines.join("\n")).expect("write");
        let file = file.to_string_lossy().into_owned();
        let read = |_: String| async { Some(identity("a")) };
        assert_eq!(
            classify_session_repository(&file, "/gone/away", "/here", read).await,
            "same"
        );
        let other = |dir: String| async move { (dir == "/here").then(|| identity("b")) };
        assert_eq!(
            classify_session_repository(&file, "/gone/away", "/here", other).await,
            "different"
        );
    }
}
