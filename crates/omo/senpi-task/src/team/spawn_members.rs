//! Spawns team members as senpi-task children through the manager.

use team_core::types::{Member, MemberStatus, TeamSpec};

use crate::manager::execution_mode::ExecutionMode;
use crate::state::ResolvedModelRecord;
use crate::team::member_projection::{RuntimeMemberStatus, project_member_status};
use crate::team::runtime_types::{
    SenpiTeamRuntimeError, SenpiTeamRuntimeErrorCode, SpawnMemberExtensionConfig, TeamMemberReadPort,
    TeamMemberStartSpec, TeamRuntimeManagerPort, TeamStartResult,
};

#[derive(Debug, Clone)]
pub struct SpawnedMember {
    pub task_id: String,
    pub session_id: Option<String>,
    pub status: RuntimeMemberStatus,
    pub resolved_model: Option<ResolvedModelRecord>,
}

pub struct SpawnMembersInput<'a> {
    pub spec: &'a TeamSpec,
    pub team_run_id: &'a str,
    pub manager: &'a dyn TeamRuntimeManagerPort,
    pub lead_session_id: &'a str,
    pub spawn_depth: u32,
    pub max_parallel: u64,
    pub deadline_at: i64,
    pub now: &'a dyn Fn() -> i64,
    pub member_extension: Option<SpawnMemberExtensionConfig>,
}

/// Spawn outcomes in spawn order (JS `Map` insertion order) plus the first failure, if any.
#[derive(Debug, Clone, Default)]
pub struct SpawnMembersResult {
    pub spawned: Vec<(String, SpawnedMember)>,
    pub failure: Option<SenpiTeamRuntimeError>,
}

impl SpawnMembersResult {
    pub fn get(&self, member_name: &str) -> Option<&SpawnedMember> {
        self.spawned
            .iter()
            .find(|(name, _)| name == member_name)
            .map(|(_, member)| member)
    }
}

pub fn member_task_name(team_run_id: &str, member_name: &str) -> String {
    format!("team:{team_run_id}:{member_name}")
}

/// Spawns team members as senpi-task children through the manager, enforcing the create deadline
/// before each pull. Members run as durable background processes so their sessions remain
/// recoverable across parent or child crashes. The FIRST failure (a rejected start, a failed start,
/// or a deadline breach) stops further spawns so the caller rolls back the members already spawned.
///
/// The manager port is synchronous, so the `max_parallel` cooperating workers of the TS runtime
/// collapse to one sequential worker pulling members in spec order.
pub fn spawn_team_members(input: &SpawnMembersInput<'_>) -> SpawnMembersResult {
    let mut result = SpawnMembersResult::default();
    let mut next_index = 0usize;
    loop {
        if (input.now)() > input.deadline_at {
            result.failure = Some(SenpiTeamRuntimeError::new(
                format!("team '{}' creation exceeded max_wall_clock_minutes", input.spec.name),
                SenpiTeamRuntimeErrorCode::CreateDeadlineExceeded,
                input.team_run_id,
            ));
            return result;
        }
        let Some(member) = input.spec.members.get(next_index) else {
            return result;
        };
        next_index += 1;
        match spawn_one_member(input, member) {
            Ok(spawned) => result.spawned.push((member.name.clone(), spawned)),
            Err(error) => {
                result.failure = Some(error);
                return result;
            }
        }
    }
}

fn start_failure(team_run_id: &str, member_name: &str, detail: &str) -> SenpiTeamRuntimeError {
    SenpiTeamRuntimeError::new(
        format!("member '{member_name}' failed to start: {detail}"),
        SenpiTeamRuntimeErrorCode::MemberStartRejected,
        team_run_id,
    )
}

fn spawn_one_member(input: &SpawnMembersInput<'_>, member: &Member) -> Result<SpawnedMember, SenpiTeamRuntimeError> {
    if let Some(worktree_path) = &member.worktree_path {
        std::fs::create_dir_all(worktree_path)
            .map_err(|error| start_failure(input.team_run_id, &member.name, &error.to_string()))?;
    }

    let result = input
        .manager
        .start(&build_member_start_spec(input, member))
        .map_err(|message| start_failure(input.team_run_id, &member.name, &message))?;
    let started = match result {
        TeamStartResult::Started(started) => started,
        TeamStartResult::Rejected { reason, .. } => {
            return Err(start_failure(input.team_run_id, &member.name, &reason));
        }
    };

    let record = TeamMemberReadPort::get(input.manager, &started.task_id);
    let session_id = TeamRuntimeManagerPort::get_resident_handle(input.manager, &started.task_id)
        .and_then(|handle| handle.session_id)
        .or_else(|| record.as_ref().and_then(|record| record.child_session_id.clone()));
    let status = match &record {
        Some(record) => project_member_status(record.status),
        None if started.status.as_str() == "pending" => MemberStatus::Pending,
        None => MemberStatus::Running,
    };
    let resolved_model = started
        .resolved_model
        .or_else(|| record.and_then(|record| record.resolved_model));
    Ok(SpawnedMember {
        task_id: started.task_id,
        session_id,
        status,
        resolved_model,
    })
}

// The narrow manager start spec carries identity, prompt, lineage, mode and role; launch extras
// (extensions, member env, cwd) are resolved by the manager-side adapter.
fn build_member_start_spec(input: &SpawnMembersInput<'_>, member: &Member) -> TeamMemberStartSpec {
    let is_category = member.kind.as_str() == "category";
    TeamMemberStartSpec {
        name: Some(member_task_name(input.team_run_id, &member.name)),
        description: None,
        prompt: build_member_prompt(input.spec, member),
        parent_session_id: input.lead_session_id.to_string(),
        root_session_id: Some(input.lead_session_id.to_string()),
        depth: input.spawn_depth,
        execution_mode: ExecutionMode::parse("process"),
        model: None,
        category: if is_category { member.category.clone() } else { None },
        subagent_type: if is_category { None } else { member.subagent_type.clone() },
    }
}

// Every member bootstrap frames the injection protocol FIRST so members end their initial turn while
// remaining resident; later lead mail steers into that same session's running turn.
pub fn build_member_prompt(spec: &TeamSpec, member: &Member) -> String {
    let role = member
        .prompt
        .clone()
        .unwrap_or_else(|| format!("You are team member '{}' in team '{}'.", member.name, spec.name));
    [
        format!(
            "You are '{}', a member of team '{}' running under the senpi-task team runtime.",
            member.name, spec.name
        ),
        "Work arrives as injected messages from the lead and other members; coordinate with task_send.".to_string(),
        "After completing any immediate instructions below, report with task_send, then end your turn. Injected messages revive this resident session with more work.".to_string(),
        "When you finish assigned work, task_send the lead a summary, then end your turn and wait for an injected message.".to_string(),
        role,
    ]
    .join("\n\n")
}
