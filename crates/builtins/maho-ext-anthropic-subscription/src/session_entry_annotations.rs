use crate::{session_registry::SessionEntry, session_registry_state::{SessionState, transition_session_state}};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionBranchInfo { pub old_leaf_id: String, pub new_leaf_id: String }

pub async fn switch_entry_model(entry: Option<&mut SessionEntry>, model: &str, now: u64) -> anyhow::Result<bool> {
    let Some(entry) = entry else { return Ok(false); };
    entry.set_model(model, now).await?; Ok(true)
}
pub fn annotate_pending_fork(entry: Option<&mut SessionEntry>, reason: &str, now: u64) {
    if let Some(entry) = entry { entry.pending_fork_reason = Some(reason.into()); entry.last_used_at = now; }
}
pub fn annotate_tainted(entry: Option<&mut SessionEntry>, reason: &str, now: u64) -> anyhow::Result<()> {
    if let Some(entry) = entry {
        entry.tainted_reason = Some(reason.into());
        if entry.pump.state != SessionState::Tainted { transition_session_state(&mut entry.pump.state, SessionState::Tainted)?; }
        entry.last_used_at = now;
    }
    Ok(())
}
pub fn annotate_branch_info(entry: Option<&mut SessionEntry>, info: &SessionBranchInfo, now: u64) {
    if let Some(entry) = entry { entry.branch_info = Some(info.clone()); entry.last_used_at = now; }
}
