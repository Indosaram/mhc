//! Projects senpi task-store member statuses onto the team-core runtime state.

use std::path::Path;

use team_core::team_state_store::transition_runtime_state;
use team_core::types::{MemberStatus, RuntimeState, RuntimeStateMember};

use crate::state::TaskStatus;
use crate::team::member_map::{MemberTaskMap, read_member_task_map};
use crate::team::runtime_config::TeamCoreConfig;

/// The team-core member status vocabulary.
pub type RuntimeMemberStatus = MemberStatus;

/// Projects a senpi task-store status onto the team-core member status vocabulary: in-flight statuses
/// map straight through, an interrupted (paused, revivable) child reads as `idle`, and every
/// terminal-failure status collapses to `errored`.
pub fn project_member_status(status: TaskStatus) -> RuntimeMemberStatus {
    match status.as_str() {
        "pending" => MemberStatus::Pending,
        "running" => MemberStatus::Running,
        "interrupted" => MemberStatus::Idle,
        "completed" => MemberStatus::Completed,
        // "error" | "cancelled" | "lost"
        _ => MemberStatus::Errored,
    }
}

/// The narrow task-store view the projection reads for one member task.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemberTaskStatusView {
    pub status: TaskStatus,
    pub child_session_id: Option<String>,
}

/// The live resident handle view: only the (possibly not yet known) session id.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ResidentSessionRef {
    pub session_id: Option<String>,
}

pub trait MemberStatusPort {
    fn get(&self, task_id: &str) -> Option<MemberTaskStatusView>;
    fn get_resident_handle(&self, task_id: &str) -> Option<ResidentSessionRef>;
}

pub struct RefreshTeamMemberStatusesDeps<'a> {
    pub manager: &'a dyn MemberStatusPort,
    pub config: &'a TeamCoreConfig,
    pub runtime_dir: &'a Path,
}

fn refresh_member(member: &mut RuntimeStateMember, map: &MemberTaskMap, manager: &dyn MemberStatusPort) {
    let Some(task_id) = map.get(&member.name) else {
        return;
    };
    let Some(record) = manager.get(task_id) else {
        return;
    };
    let session_id = manager
        .get_resident_handle(task_id)
        .and_then(|handle| handle.session_id)
        .or(record.child_session_id)
        .or_else(|| member.session_id.clone());
    member.status = project_member_status(record.status);
    if session_id.is_some() {
        member.session_id = session_id;
    }
}

/// Re-projects each mapped member's task-store status (and best-known child session id) into the
/// team-core runtime state under a lock. The runtime status is left unchanged (a self-transition), so
/// this is safe to call for `active` teams whenever the tool layer needs a fresh status snapshot.
pub fn refresh_team_member_statuses(
    team_run_id: &str,
    deps: &RefreshTeamMemberStatusesDeps<'_>,
) -> team_core::Result<RuntimeState> {
    let map = read_member_task_map(deps.runtime_dir);
    let manager = deps.manager;
    transition_runtime_state(
        team_run_id,
        |state| {
            #[allow(clippy::redundant_clone)]
            let mut next = state.clone();
            for member in &mut next.members {
                refresh_member(member, &map, manager);
            }
            next
        },
        deps.config,
    )
}
