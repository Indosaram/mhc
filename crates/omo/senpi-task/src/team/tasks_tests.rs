//! `team/tasks.test.ts`

use pretty_assertions::assert_eq;
use tempfile::TempDir;
use team_core::types::TaskStatus;

use crate::team::runtime_config::{TeamTaskBounds, to_team_core_config};
use crate::team::tasks::{
    CreateTeamTaskInput, TeamTaskFilter, TeamTasklistContext, can_claim_team_task, claim_team_task,
    create_team_task, get_team_task, list_team_tasks, update_team_task_status,
};

/// Returns the temp dir guard alongside the context so storage lives for the whole test.
fn temp_context() -> (TempDir, TeamTasklistContext) {
    let dir = tempfile::Builder::new()
        .prefix("senpi-team-tasks-")
        .tempdir()
        .expect("create temp dir");
    let bounds = TeamTaskBounds {
        max_members: 4,
        max_parallel_members: 2,
        max_wall_clock_minutes: 60,
    };
    let base_dir = dir.path().to_str().expect("utf-8 temp path").to_string();
    let config = to_team_core_config(&bounds, &base_dir).expect("valid team-core config");
    (
        dir,
        TeamTasklistContext {
            team_run_id: "team-run-tasks".to_string(),
            config,
        },
    )
}

fn input(subject: &str, description: &str) -> CreateTeamTaskInput {
    CreateTeamTaskInput {
        subject: subject.to_string(),
        description: description.to_string(),
        status: TaskStatus::Pending,
        owner: None,
        active_form: None,
        blocks: None,
        blocked_by: None,
        metadata: None,
    }
}

#[test]
fn given_a_task_input_when_create_team_task_runs_then_it_persists_a_pending_task_with_an_id() {
    // given
    let (_dir, ctx) = temp_context();

    // when
    let task = create_team_task(&ctx, input("build", "do it")).unwrap();

    // then
    assert_eq!(task.status, TaskStatus::Pending);
    assert_eq!(task.id, "1");
    let read_back = get_team_task(&ctx, &task.id).unwrap();
    assert_eq!(read_back.subject, "build");
}

#[test]
fn given_two_tasks_with_a_dependency_when_the_blocked_task_is_claimed_before_its_blocker_completes_then_it_is_rejected_until_the_blocker_is_completed()
 {
    // given
    let (_dir, ctx) = temp_context();
    let blocker = create_team_task(&ctx, input("a", "blocker")).unwrap();
    let mut dependent = input("b", "dependent");
    dependent.blocked_by = Some(vec![blocker.id.clone()]);
    let blocked = create_team_task(&ctx, dependent).unwrap();

    // when
    assert!(!can_claim_team_task(&ctx, &blocked.id).unwrap());
    let rejected = claim_team_task(&ctx, &blocked.id, "alpha");

    // then
    assert!(rejected.is_err());
    let unchanged = get_team_task(&ctx, &blocked.id).unwrap();
    assert_eq!(unchanged.status, TaskStatus::Pending);
    assert_eq!(unchanged.owner, None);

    // when the blocker is driven to completion
    update_team_task_status(&ctx, &blocker.id, TaskStatus::InProgress, "alpha").unwrap();
    update_team_task_status(&ctx, &blocker.id, TaskStatus::Completed, "alpha").unwrap();

    // then the dependent task is now claimable
    assert!(can_claim_team_task(&ctx, &blocked.id).unwrap());
    let claimed = claim_team_task(&ctx, &blocked.id, "alpha").unwrap();
    assert_eq!(claimed.status, TaskStatus::Claimed);
    assert_eq!(claimed.owner.as_deref(), Some("alpha"));
}

#[test]
fn given_a_claimed_task_when_a_non_owner_updates_its_status_then_a_typed_cross_owner_error_is_raised() {
    // given
    let (_dir, ctx) = temp_context();
    let task = create_team_task(&ctx, input("a", "x")).unwrap();
    claim_team_task(&ctx, &task.id, "alpha").unwrap();

    // when
    let rejected = update_team_task_status(&ctx, &task.id, TaskStatus::InProgress, "bravo");

    // then
    assert!(rejected.is_err());
    let unchanged = get_team_task(&ctx, &task.id).unwrap();
    assert_eq!(unchanged.status, TaskStatus::Claimed);
    assert_eq!(unchanged.owner.as_deref(), Some("alpha"));
}

#[test]
fn given_a_completed_task_when_a_reverse_transition_is_requested_then_a_typed_invalid_transition_error_is_raised() {
    // given
    let (_dir, ctx) = temp_context();
    let task = create_team_task(&ctx, input("a", "x")).unwrap();
    update_team_task_status(&ctx, &task.id, TaskStatus::InProgress, "alpha").unwrap();
    update_team_task_status(&ctx, &task.id, TaskStatus::Completed, "alpha").unwrap();

    // when
    let rejected = update_team_task_status(&ctx, &task.id, TaskStatus::InProgress, "alpha");

    // then
    assert!(rejected.is_err());
    let unchanged = get_team_task(&ctx, &task.id).unwrap();
    assert_eq!(unchanged.status, TaskStatus::Completed);
}

#[test]
fn given_an_already_claimed_task_when_a_second_member_claims_it_then_a_typed_already_claimed_error_is_raised() {
    // given
    let (_dir, ctx) = temp_context();
    let task = create_team_task(&ctx, input("a", "x")).unwrap();
    claim_team_task(&ctx, &task.id, "alpha").unwrap();

    // when
    let rejected = claim_team_task(&ctx, &task.id, "bravo");

    // then
    assert!(rejected.is_err());
    let unchanged = get_team_task(&ctx, &task.id).unwrap();
    assert_eq!(unchanged.owner.as_deref(), Some("alpha"));
}

#[test]
fn given_several_tasks_when_list_team_tasks_runs_with_an_owner_filter_then_only_that_owners_tasks_are_returned_in_id_order()
 {
    // given
    let (_dir, ctx) = temp_context();
    let first = create_team_task(&ctx, input("a", "x")).unwrap();
    create_team_task(&ctx, input("b", "y")).unwrap();
    claim_team_task(&ctx, &first.id, "alpha").unwrap();

    // when
    let all = list_team_tasks(&ctx, None).unwrap();
    let filter = TeamTaskFilter {
        owner: Some("alpha".to_string()),
        ..TeamTaskFilter::default()
    };
    let owned = list_team_tasks(&ctx, Some(&filter)).unwrap();

    // then
    assert_eq!(
        all.iter().map(|task| task.id.as_str()).collect::<Vec<_>>(),
        vec!["1", "2"]
    );
    assert_eq!(
        owned.iter().map(|task| task.id.as_str()).collect::<Vec<_>>(),
        vec!["1"]
    );
}
