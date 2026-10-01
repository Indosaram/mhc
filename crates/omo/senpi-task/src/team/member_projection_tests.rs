//! `team/member-projection.test.ts`

use std::sync::Arc;

use pretty_assertions::assert_eq;
use serde_json::json;
use team_core::team_state_store::load_runtime_state;
use team_core::types::MemberStatus;

use crate::team::member_projection::{RefreshTeamMemberStatusesDeps, project_member_status, refresh_team_member_statuses};
use crate::team::normalize::normalize_senpi_team_spec;
use crate::team::registry::TeamSpecSource;
use crate::team::runtime::create_team;
use crate::team::runtime_config::to_team_core_config;
use crate::team::runtime_fakes::{
    FakeTeamManager, FakeTeamManagerOptions, TeamBoundsOverrides, task_status, team_bounds, temp_project_dir,
};
use crate::team::runtime_types::{CreateTeamDeps, TeamRuntimeManagerPort};
use crate::team::storage::{resolve_team_runtime_dirs, team_storage_base_dir};

// The messaging fakes module is crate-visible for tests; reuse it directly so its source is not
// compiled a second time as a duplicate module.
use crate::team::messaging::messaging_fakes as messaging_fakes_shim;
use crate::team::messaging::messaging_fakes::state_dir_config;

fn assert_projection(task: &str, expected: MemberStatus) {
    assert_eq!(project_member_status(task_status(task)), expected);
}

#[test]
fn messaging_fakes_shim_items_are_reachable() {
    // Reference the remaining shim helpers so the included module has no unused items.
    let _cleanup: fn() = messaging_fakes_shim::cleanup_messaging_tmp;
    let _temp: fn() -> std::path::PathBuf = messaging_fakes_shim::temp_project_dir;
}

#[test]
fn given_task_status_pending_when_projected_then_member_status_is_pending() {
    assert_projection("pending", MemberStatus::Pending);
}

#[test]
fn given_task_status_running_when_projected_then_member_status_is_running() {
    assert_projection("running", MemberStatus::Running);
}

#[test]
fn given_task_status_interrupted_when_projected_then_member_status_is_idle() {
    assert_projection("interrupted", MemberStatus::Idle);
}

#[test]
fn given_task_status_completed_when_projected_then_member_status_is_completed() {
    assert_projection("completed", MemberStatus::Completed);
}

#[test]
fn given_task_status_error_when_projected_then_member_status_is_errored() {
    assert_projection("error", MemberStatus::Errored);
}

#[test]
fn given_task_status_cancelled_when_projected_then_member_status_is_errored() {
    assert_projection("cancelled", MemberStatus::Errored);
}

#[test]
fn given_task_status_lost_when_projected_then_member_status_is_errored() {
    assert_projection("lost", MemberStatus::Errored);
}

#[test]
fn given_a_member_task_moved_to_completed_when_refreshed_then_the_runtime_member_reflects_completed() {
    // given
    let project_dir = temp_project_dir();
    let state_dir = state_dir_config(&project_dir);
    let settings = team_bounds(TeamBoundsOverrides::default());
    let manager = Arc::new(FakeTeamManager::new(FakeTeamManagerOptions::default()));
    let spec = normalize_senpi_team_spec(
        &json!({ "members": [{ "name": "alpha", "kind": "category", "category": "quick", "prompt": "work" }] }),
        "squad",
        None,
    )
    .expect("valid spec");
    let manager_port: Arc<dyn TeamRuntimeManagerPort> = manager.clone();
    let created = create_team(
        &spec,
        TeamSpecSource::Project,
        &CreateTeamDeps {
            manager: manager_port,
            state_dir: state_dir.clone(),
            team_bounds: settings,
            lead_session_id: "lead-session".to_string(),
            spawn_depth: 1,
            now: None,
            member_extension: None,
            write_member_map: None,
        },
    )
    .expect("team created");
    let task_id = created.member_task_ids.get("alpha").expect("alpha task id").clone();
    manager.set_status(&task_id, task_status("completed"));

    // when
    let base_dir = team_storage_base_dir(&state_dir).to_string_lossy().into_owned();
    let config = to_team_core_config(&settings, &base_dir).expect("team core config");
    let team_run_id = created.runtime_state.team_run_id.clone();
    let runtime_dir = resolve_team_runtime_dirs(&state_dir, &team_run_id)
        .expect("runtime dirs")
        .runtime_dir;
    refresh_team_member_statuses(
        &team_run_id,
        &RefreshTeamMemberStatusesDeps {
            manager: manager.as_ref(),
            config: &config,
            runtime_dir: &runtime_dir,
        },
    )
    .expect("refreshed");

    // then
    let reloaded = load_runtime_state(&team_run_id, &config).expect("reloaded");
    assert_eq!(reloaded.members.first().map(|member| member.status), Some(MemberStatus::Completed));
}
