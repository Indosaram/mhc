//! Team deletion helpers over the team-core runtime state.

use team_core::types::{MemberStatus, RuntimeState, RuntimeStateMember, ShutdownRequest};

/// Member statuses from which a member may be torn down during team deletion (omo
/// `shutdown-helpers.ts:6-10` parity): a member that has finished, been approved for shutdown, or
/// errored out is safe to remove. Any other status means the member is still live.
pub const DELETABLE_MEMBER_STATUSES: [MemberStatus; 3] =
    [MemberStatus::Completed, MemberStatus::ShutdownApproved, MemberStatus::Errored];

pub fn is_member_deletable(status: &MemberStatus) -> bool {
    DELETABLE_MEMBER_STATUSES.contains(status)
}

pub fn find_runtime_member<'a>(state: &'a RuntimeState, member_name: &str) -> Option<&'a RuntimeStateMember> {
    state.members.iter().find(|candidate| candidate.name == member_name)
}

/// Index of the most recent shutdown request for a member, scanning newest-first so a fresh request
/// that follows a resolved (approved/rejected) one wins. Returns `None` when the member has no request.
pub fn find_latest_shutdown_request_index(state: &RuntimeState, member_name: &str) -> Option<usize> {
    state
        .shutdown_requests
        .iter()
        .rposition(|request| request.member_id == member_name)
}

pub fn is_unresolved_request(request: Option<&ShutdownRequest>) -> bool {
    request.is_some_and(|request| request.approved_at.is_none() && request.rejected_at.is_none())
}
