use std::sync::Arc;
use senpi_task::{state::{TaskRecord, ResidencyState}, team::liveness_ownership::{TeamMemberOwnershipDeps, is_owned_team_member_task}};
use crate::member_liveness::TeamMemberLivenessNotifier;
pub fn create_owned_member_liveness_notifier(deps: TeamMemberOwnershipDeps, session: Arc<dyn Fn() -> Option<String> + Send + Sync>, notifier: Arc<TeamMemberLivenessNotifier>) -> impl Fn(&TaskRecord) + Send + Sync {
    move |record| {
        if matches!(record.residency_state, ResidencyState::PersistedOnly | ResidencyState::RpcDetached) { return; }
        let captured = session();
        if is_owned_team_member_task(record.name.as_deref(), captured.as_deref(), &deps) && session() == captured { notifier.notify_terminal(record); }
    }
}
