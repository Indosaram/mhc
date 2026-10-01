//! `team/runtime-delete.test.ts`

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use pretty_assertions::assert_eq;
use serde_json::{Value, json};
use team_core::team_state_store::create_runtime_state;
use team_core::types::{SpecSource, TeamSpec};
use tempfile::TempDir;

use crate::lifecycle::port::DestroyCause;
use crate::team::member_map::{MemberTaskMap, write_member_task_map};
use crate::team::normalize::normalize_senpi_team_spec;
use crate::team::registry::TeamSpecSource;
use crate::team::runtime::{TeamRuntimeError, create_team, delete_team};
use crate::team::runtime_config::{TeamTaskBounds, to_team_core_config};
use crate::team::runtime_fakes::{
    CancelCall, DestroyCall, FakeDestruction, FakeTeamManager, FakeTeamManagerOptions, StartBehavior,
    TeamBoundsOverrides, task_status, team_bounds,
};
use crate::team::runtime_types::{
    CreateTeamDeps, CreateTeamResult, DeleteTeamDeps, TeamMemberCancelPort, TeamMemberDestructionPort,
    TeamMemberReadPort, TeamMemberStartSpec, TeamRuntimeManagerPort, TeamStartResult,
};
use crate::team::storage::{resolve_team_runtime_dirs, team_storage_base_dir};

// The messaging fixtures module lives in `crate::team::messaging::messaging_fakes` (a
// crate-visible `#[cfg(test)]` module); reuse it instead of compiling its source twice.
use crate::team::messaging::messaging_fakes as shared_messaging_fakes;

fn state_dir(project: &Path) -> crate::store::StateDirConfig {
    // Reference the remaining shared helpers so the included module stays warning-free.
    let _ = (
        shared_messaging_fakes::cleanup_messaging_tmp,
        shared_messaging_fakes::temp_project_dir,
    );
    shared_messaging_fakes::state_dir_config(project)
}

fn settings() -> TeamTaskBounds {
    team_bounds(TeamBoundsOverrides::default())
}

fn spec(raw: Value) -> TeamSpec {
    normalize_senpi_team_spec(&raw, "squad", None).expect("valid team spec")
}

fn create_deps(manager: &Arc<FakeTeamManager>, project: &Path) -> CreateTeamDeps {
    CreateTeamDeps {
        manager: manager.clone(),
        state_dir: state_dir(project),
        team_bounds: settings(),
        lead_session_id: "lead-session".to_string(),
        spawn_depth: 1,
        now: None,
        member_extension: None,
        write_member_map: None,
    }
}

fn delete_deps(manager: &Arc<FakeTeamManager>, destruction: &Arc<FakeDestruction>, project: &Path) -> DeleteTeamDeps {
    DeleteTeamDeps {
        manager: manager.clone(),
        destruction: destruction.clone(),
        state_dir: state_dir(project),
        team_bounds: settings(),
    }
}

fn runtime_dir_of(project: &Path, team_run_id: &str) -> PathBuf {
    resolve_team_runtime_dirs(&state_dir(project), team_run_id)
        .expect("runtime dirs")
        .runtime_dir
}

fn status_of(manager: &FakeTeamManager, task_id: &str) -> Option<String> {
    TeamMemberReadPort::get(manager, task_id).map(|record| record.status.as_str().to_string())
}

type BeforeCancel = Arc<dyn Fn(&str, &FakeDestruction) + Send + Sync>;

struct Harness {
    project: TempDir,
    created: CreateTeamResult,
    deps: DeleteTeamDeps,
    manager: Arc<FakeTeamManager>,
    destruction: Arc<FakeDestruction>,
    task_id: String,
}

impl Harness {
    fn team_run_id(&self) -> String {
        self.created.runtime_state.team_run_id.clone()
    }
}

fn completed_resident_team(status: &str, before_cancel_return: Option<BeforeCancel>) -> Harness {
    let project = tempfile::tempdir().expect("tempdir");
    let destruction = Arc::new(FakeDestruction::new());
    let hook_destruction = destruction.clone();
    let manager = Arc::new(FakeTeamManager::new(FakeTeamManagerOptions {
        default_behavior: Some(StartBehavior::Ok {
            status: Some(task_status(status)),
            resolved_model: None,
        }),
        before_cancel_return: before_cancel_return.map(|hook| {
            let wrapped: Arc<dyn Fn(&str) + Send + Sync> =
                Arc::new(move |task_id: &str| hook(task_id, &hook_destruction));
            wrapped
        }),
        ..Default::default()
    }));
    let team_spec = spec(json!({
        "members": [{ "name": "alpha", "kind": "category", "category": "quick", "prompt": "done" }]
    }));
    let created = create_team(&team_spec, TeamSpecSource::Project, &create_deps(&manager, project.path()))
        .expect("create team");
    let task_id = created
        .member_task_ids
        .values()
        .next()
        .cloned()
        .expect("expected one mapped member task");
    let deps = delete_deps(&manager, &destruction, project.path());
    Harness {
        project,
        created,
        deps,
        manager,
        destruction,
        task_id,
    }
}

#[test]
fn given_an_active_team_when_deleted_then_all_member_tasks_are_cancelled_and_the_runtime_dir_is_removed() {
    // given
    let project = tempfile::tempdir().expect("tempdir");
    let manager = Arc::new(FakeTeamManager::new(FakeTeamManagerOptions::default()));
    let team_spec = spec(json!({
        "members": [
            { "name": "alpha", "kind": "category", "category": "quick", "prompt": "a" },
            { "name": "beta", "kind": "category", "category": "deep", "prompt": "b" }
        ]
    }));
    let created = create_team(&team_spec, TeamSpecSource::Project, &create_deps(&manager, project.path()))
        .expect("create team");
    let team_run_id = created.runtime_state.team_run_id.clone();
    let runtime_dir = runtime_dir_of(project.path(), &team_run_id);
    let destruction = Arc::new(FakeDestruction::new());

    // when
    let result = delete_team(&team_run_id, &delete_deps(&manager, &destruction, project.path())).expect("delete");

    // then
    let mut cancelled = result.cancelled_task_ids.clone();
    cancelled.sort();
    assert_eq!(cancelled, vec!["st_000001".to_string(), "st_000002".to_string()]);
    assert_eq!(manager.cancelled().len(), 2);
    assert!(!runtime_dir.exists());
}

#[test]
fn given_a_team_still_in_creating_when_deleted_then_an_invalid_state_error_is_thrown() {
    // given
    let project = tempfile::tempdir().expect("tempdir");
    let manager = Arc::new(FakeTeamManager::new(FakeTeamManagerOptions::default()));
    let team_spec = spec(json!({
        "members": [{ "name": "alpha", "kind": "category", "category": "quick", "prompt": "a" }]
    }));
    let base_dir = team_storage_base_dir(&state_dir(project.path()))
        .to_string_lossy()
        .into_owned();
    let config = to_team_core_config(&settings(), &base_dir).expect("config");
    let seeded =
        create_runtime_state(&team_spec, Some("lead-session"), SpecSource::Project, &config).expect("seed state");
    let destruction = Arc::new(FakeDestruction::new());

    // when
    let attempt = delete_team(&seeded.team_run_id, &delete_deps(&manager, &destruction, project.path()));

    // then
    match attempt {
        Err(TeamRuntimeError::Runtime(error)) => assert_eq!(error.code.as_str(), "invalid_delete_state"),
        Err(other) => panic!("unexpected error: {other}"),
        Ok(_) => panic!("expected invalid_delete_state error"),
    }
}

#[test]
fn given_an_already_deleted_team_when_deleted_again_then_it_is_a_no_op() {
    // given
    let project = tempfile::tempdir().expect("tempdir");
    let manager = Arc::new(FakeTeamManager::new(FakeTeamManagerOptions::default()));
    let team_spec = spec(json!({
        "members": [{ "name": "alpha", "kind": "category", "category": "quick", "prompt": "a" }]
    }));
    let created = create_team(&team_spec, TeamSpecSource::Project, &create_deps(&manager, project.path()))
        .expect("create team");
    let team_run_id = created.runtime_state.team_run_id.clone();
    let destruction = Arc::new(FakeDestruction::new());
    let deps = delete_deps(&manager, &destruction, project.path());
    delete_team(&team_run_id, &deps).expect("first delete");

    // when
    let second = delete_team(&team_run_id, &deps).expect("second delete");

    // then
    assert_eq!(second.cancelled_task_ids, Vec::<String>::new());
}

#[test]
fn given_a_completed_resident_member_when_deleted_then_cancellation_stays_noop_while_lifecycle_destroys_it_and_the_runtime_is_removed()
 {
    // given
    let harness = completed_resident_team("completed", None);
    let team_run_id = harness.team_run_id();
    let runtime_dir = runtime_dir_of(harness.project.path(), &team_run_id);

    // when
    let result = delete_team(&team_run_id, &harness.deps).expect("delete");

    // then
    assert_eq!(
        harness.manager.cancelled(),
        vec![CancelCall {
            task_id: harness.task_id.clone(),
            reason: Some(format!("delete team {team_run_id}")),
        }]
    );
    assert_eq!(result.cancelled_task_ids, Vec::<String>::new());
    assert_eq!(
        harness.destruction.calls(),
        vec![DestroyCall {
            task_id: harness.task_id.clone(),
            cause: "cancel",
        }]
    );
    assert_eq!(status_of(&harness.manager, &harness.task_id), Some("completed".to_string()));
    assert!(!runtime_dir.exists());
}

#[test]
fn given_a_member_sidecar_points_at_a_foreign_task_when_deleted_then_that_task_is_untouched() {
    // given
    let harness = completed_resident_team("completed", None);
    let team_run_id = harness.team_run_id();
    let foreign = harness
        .manager
        .start(&TeamMemberStartSpec {
            name: Some("foreign-task".to_string()),
            description: None,
            prompt: "foreign work".to_string(),
            parent_session_id: "lead-session".to_string(),
            root_session_id: None,
            depth: 1,
            execution_mode: None,
            model: None,
            category: None,
            subagent_type: None,
        })
        .expect("start foreign");
    let TeamStartResult::Started(foreign) = foreign else {
        panic!("expected foreign task to start");
    };
    let runtime_dir = runtime_dir_of(harness.project.path(), &team_run_id);
    let mut map = MemberTaskMap::new();
    map.insert("alpha".to_string(), foreign.task_id.clone());
    write_member_task_map(&runtime_dir, &map).expect("write member map");

    // when
    let result = delete_team(&team_run_id, &harness.deps).expect("delete");

    // then
    assert_eq!(result.cancelled_task_ids, Vec::<String>::new());
    assert_eq!(harness.manager.cancelled(), Vec::<CancelCall>::new());
    assert_eq!(harness.destruction.calls(), Vec::<DestroyCall>::new());
    assert_eq!(status_of(&harness.manager, &foreign.task_id), Some("completed".to_string()));
}

#[test]
fn given_a_completed_resident_member_already_disposed_when_deleted_then_destruction_is_not_repeated() {
    // given
    let harness = completed_resident_team("completed", None);
    harness.manager.set_residency(
        &harness.task_id,
        crate::state::ResidencyState::parse("disposed").expect("known residency state"),
    );

    // when
    let result = delete_team(&harness.team_run_id(), &harness.deps).expect("delete");

    // then
    assert_eq!(result.cancelled_task_ids, Vec::<String>::new());
    assert_eq!(harness.destruction.calls(), Vec::<DestroyCall>::new());
}

#[test]
fn given_a_completed_resident_member_revives_between_residency_checks_when_deleted_then_destruction_is_skipped() {
    // given
    let harness = completed_resident_team("completed", None);
    harness.manager.set_get_hook(
        &harness.task_id,
        Box::new(|record, read_count| {
            let mut next = record.clone();
            if read_count == 3 {
                next.status = task_status("running");
            }
            next
        }),
    );

    // when
    let result = delete_team(&harness.team_run_id(), &harness.deps).expect("delete");

    // then
    assert_eq!(result.cancelled_task_ids, Vec::<String>::new());
    assert_eq!(harness.destruction.calls(), Vec::<DestroyCall>::new());
    assert_eq!(status_of(&harness.manager, &harness.task_id), Some("running".to_string()));
}

#[test]
fn given_a_resident_member_cancellation_is_in_flight_when_deleted_then_deletion_does_not_race_its_destruction() {
    // given
    let destruction_attempts = Arc::new(AtomicUsize::new(0));
    let attempts = destruction_attempts.clone();
    let hook: BeforeCancel = Arc::new(move |task_id: &str, destruction: &FakeDestruction| {
        attempts.fetch_add(1, Ordering::SeqCst);
        destruction
            .destroy_resident_task(task_id, DestroyCause::Cancel)
            .expect("destroy resident task");
    });
    let harness = completed_resident_team("running", Some(hook));
    let external_outcome = harness
        .manager
        .cancel_task(&harness.task_id, Some("external cancellation"));

    // when
    let result = delete_team(&harness.team_run_id(), &harness.deps).expect("delete");

    // then
    // Deletion completed without a duplicate destruction attempt.
    assert_eq!(destruction_attempts.load(Ordering::SeqCst), 1);
    assert_eq!(external_outcome.kind(), "cancelled");
    assert_eq!(result.cancelled_task_ids, Vec::<String>::new());
    assert_eq!(
        harness.destruction.calls(),
        vec![DestroyCall {
            task_id: harness.task_id.clone(),
            cause: "cancel",
        }]
    );
}

#[test]
fn given_two_delete_calls_for_one_team_when_invoked_concurrently_then_they_share_one_cancellation_and_one_destruction()
{
    // given
    let harness = completed_resident_team("completed", None);
    let team_run_id = harness.team_run_id();

    // when
    let first = delete_team(&team_run_id, &harness.deps).expect("first delete");
    let second = delete_team(&team_run_id, &harness.deps).expect("second delete");

    // then
    assert_eq!(first.team_run_id, second.team_run_id);
    assert_eq!(first.cancelled_task_ids, second.cancelled_task_ids);
    assert_eq!(harness.manager.cancelled().len(), 1);
    assert_eq!(harness.destruction.calls().len(), 1);
}
