use crate::{
    context::MemoryIdentityContext,
    status::{format_memory_status_line, format_relative_age, read_memory_status_segments},
    status_live::{MemoryFooterLive, MemoryFooterProbe, MemoryFooterUi},
    sync_status_cached::SyncStatusReader,
};
use memory_core::git::GitMemoryRepo;
use std::{collections::BTreeMap, path::PathBuf};
#[derive(Default)]
pub struct MemoryFooterStatusLive {
    live: MemoryFooterLive,
    repos: BTreeMap<PathBuf, GitMemoryRepo>,
    /// Reads each repository's sync status for the ` sync blocked` segment (`MemoryFooterStatusLiveOptions.syncStatus`).
    sync_status: Option<SyncStatusReader>,
}
struct Probe<'a> {
    context: &'a MemoryIdentityContext,
    session_id: &'a str,
    active: bool,
    now_ms: i64,
    repos: &'a mut BTreeMap<PathBuf, GitMemoryRepo>,
    sync_status: Option<&'a SyncStatusReader>,
}
impl Probe<'_> {
    fn repo(&mut self) -> Result<&GitMemoryRepo, String> {
        let path = &self.context.identity_paths.repo;
        if !self.repos.contains_key(path) {
            let repo = GitMemoryRepo::new(memory_core::git::repo_types::GitMemoryRepoOptions::new(
                path,
                "omo-status",
            ))
            .map_err(|e| e.to_string())?;
            self.repos.insert(path.clone(), repo);
        }
        self.repos
            .get(path)
            .ok_or_else(|| "status repository absent".into())
    }
}
impl MemoryFooterProbe for Probe<'_> {
    fn fingerprint(&mut self, identity: &str) -> Result<Option<String>, String> {
        if self.context.identity != identity {
            return Ok(None);
        }
        let context = self.context;
        let session = self.session_id;
        let now = self.now_ms;
        let active = self.active;
        let sync_status = self.sync_status;
        let repo = self.repo()?;
        let head = repo.head().map_err(|e| e.to_string())?;
        let Some(head) = head else {
            return Ok(Some(format!("none|{active}")));
        };
        let segments = read_memory_status_segments(repo, context, Some(session), now, sync_status);
        Ok(Some(format!(
            "{head}|{}|{}|{}|{active}|{}",
            if segments.dirty { "dirty" } else { "clean" },
            segments.backlog_steps,
            segments.streak,
            if segments.sync_blocked {
                "sync-blocked"
            } else {
                "sync-ok"
            }
        )))
    }
    fn refresh_segments(&mut self, identity: &str) -> Result<Option<String>, String> {
        if self.context.identity != identity {
            return Ok(None);
        }
        let context = self.context;
        let session = self.session_id;
        let now = self.now_ms;
        let sync_status = self.sync_status;
        let repo = self.repo()?;
        if repo.head().map_err(|e| e.to_string())?.is_none() {
            return Ok(None);
        }
        let Some(timestamp) = repo.head_commit_timestamp().map_err(|e| e.to_string())? else {
            return Ok(None);
        };
        let Some(age) = format_relative_age(
            timestamp.to_string().parse::<f64>().unwrap_or(f64::NAN) * 1000.0,
            now.to_string().parse().unwrap_or(f64::NAN),
        ) else {
            return Ok(None);
        };
        Ok(Some(format_memory_status_line(
            identity,
            &age,
            &read_memory_status_segments(repo, context, Some(session), now, sync_status),
        )))
    }
}
impl MemoryFooterStatusLive {
    /// Installs the sync-status reader every later fingerprint and refresh consults.
    pub fn set_sync_status(&mut self, sync_status: Option<SyncStatusReader>) {
        self.sync_status = sync_status;
    }
    fn probe<'a>(
        context: &'a MemoryIdentityContext,
        session_id: &'a str,
        active: bool,
        now_ms: i64,
        repos: &'a mut BTreeMap<PathBuf, GitMemoryRepo>,
        sync_status: Option<&'a SyncStatusReader>,
    ) -> Probe<'a> {
        Probe {
            context,
            session_id,
            active,
            now_ms,
            repos,
            sync_status,
        }
    }
    /// Paints this session's spinner for the run it launched. Sessions the run does not read never
    /// reach here, so an idle session sharing the identity keeps its plain `mem:` line.
    pub fn activate(
        &mut self,
        context: Option<&MemoryIdentityContext>,
        session_id: Option<&str>,
        run_id: &str,
        _now_ms: i64,
        ui: Option<&mut dyn MemoryFooterUi>,
    ) {
        let (Some(context), Some(session_id), Some(ui)) = (context, session_id, ui) else {
            return;
        };
        let identity = context.identity.clone();
        self.live.activate(session_id, &identity, run_id, ui);
    }
    /// Ends this session's spinner for `run_id` and restores its `mem:` line.
    pub fn settle(
        &mut self,
        context: Option<&MemoryIdentityContext>,
        session_id: Option<&str>,
        run_id: &str,
        now_ms: i64,
        ui: Option<&mut dyn MemoryFooterUi>,
    ) {
        let (Some(context), Some(session_id), Some(ui)) = (context, session_id, ui) else {
            return;
        };
        let identity = context.identity.clone();
        let mut probe = Self::probe(
            context,
            session_id,
            false,
            now_ms,
            &mut self.repos,
            self.sync_status.as_ref(),
        );
        self.live
            .settle(session_id, &identity, run_id, ui, &mut probe);
    }
    pub fn sync_active(
        &mut self,
        context: Option<&MemoryIdentityContext>,
        session_id: Option<&str>,
        active: bool,
        now_ms: i64,
        ui: Option<&mut dyn MemoryFooterUi>,
    ) {
        let (Some(context), Some(session_id), Some(ui)) = (context, session_id, ui) else {
            return;
        };
        let identity = context.identity.clone();
        let mut probe = Self::probe(
            context,
            session_id,
            active,
            now_ms,
            &mut self.repos,
            self.sync_status.as_ref(),
        );
        self.live
            .set_active(session_id, &identity, active, ui, &mut probe);
    }
    pub fn refresh(
        &mut self,
        context: Option<&MemoryIdentityContext>,
        session_id: Option<&str>,
        active: bool,
        now_ms: i64,
        ui: Option<&mut dyn MemoryFooterUi>,
    ) {
        let (Some(context), Some(session_id), Some(ui)) = (context, session_id, ui) else {
            return;
        };
        let identity = context.identity.clone();
        let mut probe = Self::probe(
            context,
            session_id,
            active,
            now_ms,
            &mut self.repos,
            self.sync_status.as_ref(),
        );
        self.live.refresh(session_id, &identity, ui, &mut probe);
    }
    pub fn tick_session(&mut self, session: &str, ui: &mut dyn MemoryFooterUi) {
        self.live.tick_session(session, ui);
    }
    pub fn animating_sessions(&self) -> Vec<String> {
        self.live.animating_sessions()
    }
    pub fn stop_session(&mut self, session: &str) {
        self.live.stop_session(session);
    }
    pub fn is_animating(&self) -> bool {
        self.live.is_animating()
    }
    pub fn stop(&mut self) {
        self.live.stop();
    }
    pub fn dispose(&mut self) {
        self.live.dispose();
    }
    pub fn is_disposed(&self) -> bool {
        self.live.is_disposed()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[derive(Default)]
    struct Ui(Vec<String>);
    impl MemoryFooterUi for Ui {
        fn set_status(&mut self, _: &str, text: &str) {
            self.0.push(text.into());
        }
    }
    #[test]
    fn unbound_does_not_create_repo_or_status() {
        let mut wiring = MemoryFooterStatusLive::default();
        let mut ui = Ui::default();
        wiring.sync_active(None, Some("session"), true, 0, Some(&mut ui));
        wiring.refresh(None, Some("session"), false, 0, Some(&mut ui));
        assert!(ui.0.is_empty());
        assert!(wiring.repos.is_empty());
    }
    #[test]
    fn real_repo_refresh_settle_and_dirty_fingerprint() {
        let dir = tempfile::tempdir().unwrap();
        let paths = memory_core::identity::layout::build_identity_paths(dir.path(), "agent");
        let engine = crate::engine_session::prepare_memory_engine_session(
            "agent",
            &paths,
            Default::default(),
        )
        .unwrap();
        let now = engine.repo.head_commit_timestamp().unwrap().unwrap() * 1000;
        let context = MemoryIdentityContext::new(
            "agent".into(),
            paths,
            crate::binding::MemorySessionBinding {
                identity: "agent".into(),
                repo_path_hash: "hash".into(),
                bound_at: 1.0,
            },
        );
        let mut wiring = MemoryFooterStatusLive::default();
        let mut ui = Ui::default();
        wiring.refresh(Some(&context), Some("session"), false, now, Some(&mut ui));
        assert_eq!(ui.0, ["mem:agent just now"]);
        wiring.refresh(Some(&context), Some("session"), false, now, Some(&mut ui));
        assert_eq!(ui.0.len(), 1);
        wiring.sync_active(Some(&context), Some("session"), true, now, Some(&mut ui));
        assert!(ui.0.last().unwrap().ends_with("reflecting"));
        wiring.sync_active(Some(&context), Some("session"), false, now, Some(&mut ui));
        assert_eq!(ui.0.last().unwrap(), "mem:agent just now");
        std::fs::write(context.identity_paths.repo.join("dirty.md"), b"dirty").unwrap();
        wiring.refresh(Some(&context), Some("session"), false, now, Some(&mut ui));
        assert_eq!(ui.0.last().unwrap(), "mem:agent just now*");
        assert_eq!(wiring.repos.len(), 1);
    }
    #[test]
    fn one_session_activates_without_touching_another() {
        let dir = tempfile::tempdir().unwrap();
        let paths = memory_core::identity::layout::build_identity_paths(dir.path(), "agent");
        let engine = crate::engine_session::prepare_memory_engine_session(
            "agent",
            &paths,
            Default::default(),
        )
        .unwrap();
        let now = engine.repo.head_commit_timestamp().unwrap().unwrap() * 1000;
        let context = MemoryIdentityContext::new(
            "agent".into(),
            paths,
            crate::binding::MemorySessionBinding {
                identity: "agent".into(),
                repo_path_hash: "hash".into(),
                bound_at: 1.0,
            },
        );
        let mut wiring = MemoryFooterStatusLive::default();
        let mut busy = Ui::default();
        let mut idle = Ui::default();
        wiring.activate(Some(&context), Some("busy"), "run", now, Some(&mut busy));
        assert_eq!(wiring.animating_sessions(), ["busy"]);
        assert_eq!(busy.0, ["mem:agent ⠋ reflecting"]);
        assert!(idle.0.is_empty());
        wiring.settle(Some(&context), Some("idle"), "run", now, Some(&mut idle));
        assert!(!idle.0.iter().any(|line| line.ends_with("reflecting")));
        assert_eq!(wiring.animating_sessions(), ["busy"]);
    }
}
