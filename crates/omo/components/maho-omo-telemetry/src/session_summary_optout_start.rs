//! Port of `packages/omo-senpi/src/components/telemetry/session-summary-optout-start.ts`.
//!
//! The opt-out purge, run at registration and again at shutdown by a process that records nothing.
//!
//! 1. Every queued window and pending event that may never be sent is deleted, whoever wrote it, a live
//!    owner's included (its own persisted setting is off, so it would never send it).
//! 2. Only when files remain that need owner liveness to judge (an interrupted write's `.tmp`, a `.claimed-`
//!    copy) does the liveness-aware purge run.
//!
//! Consent is the caller's: the persisted setting only, never this process's own env opt-out.

use std::path::PathBuf;
use std::sync::Arc;

use crate::session_summary_liveness::PidProbe;
use crate::session_summary_purge::{purge_unconsented_queue, summary_dir};
use crate::session_summary_recovery::purge_opted_out_storage;
use crate::session_summary_store::ConsentScope;

pub struct StartOptOutPurgeInput {
    pub state_dir: PathBuf,
    pub is_scope_consented: Arc<dyn Fn(&ConsentScope) -> bool + Send + Sync>,
    /// True when this process's own persisted setting is off: unreadable summary files are deleted too.
    pub persisted_opt_out: bool,
    pub is_pid_alive: Option<PidProbe>,
}

pub fn start_opt_out_purge(input: StartOptOutPurgeInput) {
    purge_unconsented_queue(
        &input.state_dir,
        input.is_scope_consented.as_ref(),
        input.persisted_opt_out,
    );
    if !has_liveness_leftovers(&input.state_dir) {
        return;
    }
    purge_opted_out_storage(
        &input.state_dir,
        Arc::clone(&input.is_scope_consented),
        input.is_pid_alive.clone(),
    );
}

pub fn has_liveness_leftovers(state_dir: &std::path::Path) -> bool {
    let dir = summary_dir(state_dir);
    [dir.clone(), dir.join("pending")].into_iter().any(|folder| {
        crate::session_summary_files::list_names(&folder)
            .iter()
            .any(|name| name.ends_with(".tmp") || name.contains(".claimed-"))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session_summary_purge::pending_dir;
    use serde_json::json;

    fn consented_to(cwd: &'static str) -> Arc<dyn Fn(&ConsentScope) -> bool + Send + Sync> {
        Arc::new(move |scope: &ConsentScope| scope.cwd.as_deref() == Some(cwd))
    }

    fn queue_record(state: &std::path::Path, name: &str, cwd: &str) {
        let dir = summary_dir(state);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join(name),
            json!({"v":2,"consent":"enabled","scope":{"cwd":cwd},"sessionHash":"h","owner":{"pid":9,"bootId":"b"},"seq":0,"sampleRate":0.05,"windowId":null,"window":null})
                .to_string(),
        )
        .unwrap();
    }

    #[test]
    fn registration_purge_deletes_other_projects_windows() {
        let dir = tempfile::tempdir().unwrap();
        queue_record(dir.path(), "other.json", "/other");
        queue_record(dir.path(), "mine.json", "/mine");
        start_opt_out_purge(StartOptOutPurgeInput {
            state_dir: dir.path().to_path_buf(),
            is_scope_consented: consented_to("/mine"),
            persisted_opt_out: false,
            is_pid_alive: None,
        });
        let mut names = crate::session_summary_files::list_names(&summary_dir(dir.path()));
        names.sort();
        assert_eq!(names, vec!["mine.json".to_string()]);
    }

    #[test]
    fn liveness_leftovers_are_cleaned_only_when_they_exist() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!has_liveness_leftovers(dir.path()));
        std::fs::create_dir_all(pending_dir(dir.path())).unwrap();
        std::fs::write(pending_dir(dir.path()).join("x.json.7-boot-dead.tmp"), "{}").unwrap();
        assert!(has_liveness_leftovers(dir.path()));
        start_opt_out_purge(StartOptOutPurgeInput {
            state_dir: dir.path().to_path_buf(),
            is_scope_consented: consented_to("/mine"),
            persisted_opt_out: true,
            is_pid_alive: Some(Arc::new(|_| false)),
        });
        assert!(crate::session_summary_files::list_names(&pending_dir(dir.path())).is_empty());
    }

    #[test]
    fn a_quiet_directory_purges_without_touching_anything() {
        let dir = tempfile::tempdir().unwrap();
        start_opt_out_purge(StartOptOutPurgeInput {
            state_dir: dir.path().to_path_buf(),
            is_scope_consented: consented_to("/mine"),
            persisted_opt_out: true,
            is_pid_alive: None,
        });
        assert!(!summary_dir(dir.path()).exists());
    }
}
