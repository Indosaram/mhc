//! Local SessionHost and workspace scope note (`thread/control/local-session-host.ts`).

use std::path::PathBuf;
use std::sync::Arc;

use serde_json::{Value, json};

use super::super::addressing::{ThreadAddressEntry, workspace_entries};
use super::super::contracts::results::ThreadSummary;
use super::super::tools::ports::{BoxFuture, ThreadHost};
use super::local_open::open_locally;
use super::session_host::{HostedThread, SessionHost, SessionOpenOutcome, SessionOpenRequest};

pub fn workspace_note(thread: &HostedThread, caller_workspace_root: &str) -> Value {
    if thread.summary.device.is_some() || caller_workspace_root.is_empty() {
        return json!({});
    }
    let entry = ThreadAddressEntry::from_summary(ThreadSummary {
        thread_id: thread.summary.thread_id.clone(),
        name: thread.summary.name.clone(),
        status: super::super::contracts_results::ThreadStatus::Live,
        cwd: thread.summary.cwd.clone(),
        created_at: thread.summary.created_at.clone(),
        updated_at: thread.summary.updated_at.clone(),
        error_note: None,
        endpoint: None,
        surface: None,
        alive: None,
        controls: None,
        skills: None,
        device: None,
        alias: None,
    });
    if !workspace_entries(&[entry], Some(caller_workspace_root)).is_empty() {
        return json!({});
    }
    json!({
        "workspace_scope": "outside",
        "scope_next_action": format!(
            "Thread {} is in {}, outside this session's workspace: pass all_scope: true when you address it, and to see it in thread_list (tool.thread_list({{ all_scope: true }}) in an eval cell).",
            thread.summary.thread_id,
            thread.summary.cwd
        ),
    })
}

pub fn default_worktree_root() -> PathBuf {
    let env: std::collections::BTreeMap<String, String> = std::env::vars().collect();
    let home = dirs::home_dir().unwrap_or_default();
    let cwd = std::env::current_dir().unwrap_or_default();
    maho_omo_agent_home::resolve_agent_home(&env, &home, &cwd, |p| p.exists()).join("worktrees")
}

pub struct LocalSessionHost {
    pub host: Arc<dyn ThreadHost>,
    pub visible: Vec<ThreadSummary>,
    pub worktree_root: Arc<dyn Fn() -> PathBuf + Send + Sync>,
}

impl SessionHost for LocalSessionHost {
    fn visible(&self) -> BoxFuture<Result<Vec<ThreadSummary>, String>> {
        let rows = self.visible.clone();
        Box::pin(async move { Ok(rows) })
    }

    fn open(
        &self,
        request: SessionOpenRequest,
        _confirm_reach: bool,
    ) -> BoxFuture<Result<SessionOpenOutcome, String>> {
        let host = self.host.clone();
        let root_fn = self.worktree_root.clone();
        Box::pin(async move { open_locally(host.as_ref(), request, &root_fn()).await })
    }
}
