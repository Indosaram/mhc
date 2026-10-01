//! `team/runtime-create-failure.test.ts`

use std::io;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use pretty_assertions::assert_eq;
use serde_json::json;
use team_core::team_state_store::load_runtime_state;
use team_core::types::TeamSpec;

use crate::team::member_map::MemberTaskMap;
use crate::team::messaging::test_support::state_dir_config;
use crate::team::normalize::normalize_senpi_team_spec;
use crate::team::registry::TeamSpecSource;
use crate::team::runtime::{TeamRuntimeError, create_team};
use crate::team::runtime_config::to_team_core_config;
use crate::team::runtime_fakes::{
    FakeTeamManager, FakeTeamManagerOptions, StartBehavior, TeamBoundsOverrides, team_bounds, temp_project_dir,
};
use crate::team::runtime_types::{CreateTeamDeps, TeamNowFn, TeamRuntimeManagerPort, WriteMemberMapFn};
use crate::team::storage::team_storage_base_dir;

// Note: the TS afterEach(cleanupTeamRuntimeTmp) is intentionally not mirrored per test: Rust tests
// run in parallel and the shared cleanup registry would remove temp dirs still in use by siblings.
// The temp dirs are dropped when the test process exits.

fn three_member_spec() -> TeamSpec {
    normalize_senpi_team_spec(
        &json!({
            "members": [
                { "name": "alpha", "kind": "category", "category": "quick", "prompt": "task alpha" },
                { "name": "beta", "kind": "category", "category": "deep", "prompt": "task beta" },
                { "name": "gamma", "kind": "subagent_type", "subagent_type": "sisyphus", "prompt": "task gamma" },
            ],
        }),
        "squad",
        None,
    )
    .expect("valid three-member spec")
}

fn runtime_code(error: &TeamRuntimeError) -> &'static str {
    match error {
        TeamRuntimeError::Runtime(runtime) => runtime.code.as_str(),
        other => panic!("expected SenpiTeamRuntimeError, got {other:?}"),
    }
}

fn team_run_id_from(manager: &FakeTeamManager) -> Option<String> {
    manager
        .started()
        .first()
        .and_then(|spec| spec.name.clone())
        .and_then(|name| name.split(':').nth(1).map(str::to_string))
}

#[test]
fn given_a_spec_exceeding_max_members_when_created_then_it_is_rejected_before_any_spawn() {
    // given
    let project_dir = temp_project_dir();
    let base_dir = team_storage_base_dir(&state_dir_config(&project_dir));
    let settings = team_bounds(TeamBoundsOverrides {
        max_members: Some(2),
        ..Default::default()
    });
    let manager = Arc::new(FakeTeamManager::new(FakeTeamManagerOptions::default()));
    let port: Arc<dyn TeamRuntimeManagerPort> = manager.clone();

    // when
    let attempt = create_team(
        &three_member_spec(),
        TeamSpecSource::Project,
        &CreateTeamDeps {
            manager: port,
            state_dir: state_dir_config(&project_dir),
            team_bounds: settings,
            lead_session_id: "lead-session".to_string(),
            spawn_depth: 1,
            now: None,
            member_extension: None,
            write_member_map: None,
        },
    );

    // then
    let error = attempt.expect_err("create must fail");
    assert_eq!(runtime_code(&error), "bounds_exceeded");
    assert_eq!(manager.started().len(), 0);
    assert_eq!(base_dir.join("runtime").exists(), false);
}

#[test]
fn given_the_2nd_member_spawn_throws_when_created_then_the_team_fails_and_the_1st_member_is_cancelled() {
    // given
    let project_dir = temp_project_dir();
    let base_dir = team_storage_base_dir(&state_dir_config(&project_dir));
    let settings = team_bounds(TeamBoundsOverrides {
        max_parallel_members: Some(1),
        ..Default::default()
    });
    let manager = Arc::new(FakeTeamManager::new(FakeTeamManagerOptions {
        behaviors: vec![
            StartBehavior::Ok {
                status: None,
                resolved_model: None,
            },
            StartBehavior::Throw {
                message: "spawn boom".to_string(),
            },
        ],
        ..Default::default()
    }));
    let port: Arc<dyn TeamRuntimeManagerPort> = manager.clone();
    let spec = normalize_senpi_team_spec(
        &json!({
            "members": [
                { "name": "alpha", "kind": "category", "category": "quick", "prompt": "a" },
                { "name": "beta", "kind": "category", "category": "deep", "prompt": "b" },
            ],
        }),
        "squad",
        None,
    )
    .expect("valid spec");

    // when
    let attempt = create_team(
        &spec,
        TeamSpecSource::Project,
        &CreateTeamDeps {
            manager: port,
            state_dir: state_dir_config(&project_dir),
            team_bounds: settings,
            lead_session_id: "lead-session".to_string(),
            spawn_depth: 1,
            now: None,
            member_extension: None,
            write_member_map: None,
        },
    );

    // then
    let error = attempt.expect_err("create must fail");
    assert!(matches!(error, TeamRuntimeError::Runtime(_)), "expected SenpiTeamRuntimeError, got {error:?}");
    let cancelled: Vec<String> = manager.cancelled().into_iter().map(|entry| entry.task_id).collect();
    assert_eq!(cancelled, vec!["st_000001".to_string()]);
    let config = to_team_core_config(&settings, &base_dir.to_string_lossy()).expect("config");
    let team_run_id = team_run_id_from(&manager);
    assert!(team_run_id.is_some());
    let reloaded = load_runtime_state(&team_run_id.unwrap_or_default(), &config).expect("reload state");
    assert_eq!(reloaded.status.as_str(), "failed");
}

#[test]
fn given_the_member_sidecar_write_throws_when_created_then_members_are_cancelled_the_team_is_failed_and_it_never_activates()
 {
    // given
    let project_dir = temp_project_dir();
    let base_dir = team_storage_base_dir(&state_dir_config(&project_dir));
    let settings = team_bounds(TeamBoundsOverrides::default());
    let manager = Arc::new(FakeTeamManager::new(FakeTeamManagerOptions::default()));
    let port: Arc<dyn TeamRuntimeManagerPort> = manager.clone();
    let write_member_map: WriteMemberMapFn =
        Arc::new(|_dir: &Path, _map: &MemberTaskMap| Err(io::Error::other("disk full")));

    // when
    let attempt = create_team(
        &three_member_spec(),
        TeamSpecSource::Project,
        &CreateTeamDeps {
            manager: port,
            state_dir: state_dir_config(&project_dir),
            team_bounds: settings,
            lead_session_id: "lead-session".to_string(),
            spawn_depth: 1,
            now: None,
            member_extension: None,
            write_member_map: Some(write_member_map),
        },
    );

    // then
    let error = attempt.expect_err("create must fail");
    assert_eq!(runtime_code(&error), "sidecar_write_failed");
    let mut cancelled: Vec<String> = manager.cancelled().into_iter().map(|entry| entry.task_id).collect();
    cancelled.sort();
    assert_eq!(
        cancelled,
        vec!["st_000001".to_string(), "st_000002".to_string(), "st_000003".to_string()]
    );
    let config = to_team_core_config(&settings, &base_dir.to_string_lossy()).expect("config");
    let team_run_id = team_run_id_from(&manager).unwrap_or_default();
    let reloaded = load_runtime_state(&team_run_id, &config).expect("reload state");
    assert_eq!(reloaded.status.as_str(), "failed");
}

#[test]
fn given_a_create_deadline_already_passed_when_created_then_it_fails_with_a_deadline_error_and_no_spawns() {
    // given
    let project_dir = temp_project_dir();
    let settings = team_bounds(TeamBoundsOverrides {
        max_wall_clock_minutes: Some(1),
        ..Default::default()
    });
    let manager = Arc::new(FakeTeamManager::new(FakeTeamManagerOptions::default()));
    let port: Arc<dyn TeamRuntimeManagerPort> = manager.clone();
    let clock: [i64; 2] = [1_000, 10_000_000];
    let tick = Arc::new(AtomicUsize::new(0));
    let now: TeamNowFn = Arc::new(move || {
        let index = tick.fetch_add(1, Ordering::SeqCst);
        clock.get(index.min(clock.len() - 1)).copied().unwrap_or(0)
    });

    // when
    let attempt = create_team(
        &three_member_spec(),
        TeamSpecSource::Project,
        &CreateTeamDeps {
            manager: port,
            state_dir: state_dir_config(&project_dir),
            team_bounds: settings,
            lead_session_id: "lead-session".to_string(),
            spawn_depth: 1,
            now: Some(now),
            member_extension: None,
            write_member_map: None,
        },
    );

    // then
    let error = attempt.expect_err("create must fail");
    assert_eq!(runtime_code(&error), "create_deadline_exceeded");
    assert_eq!(manager.started().len(), 0);
}
