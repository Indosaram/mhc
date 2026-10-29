//! Translated from src/team-tasklist/*.test.ts.

use std::fs;
use std::sync::Arc;
use std::thread;
use std::time::{SystemTime, UNIX_EPOCH};

use team_core::TeamModeConfig;
use team_core::error::TeamCoreError;
use team_core::team_registry::{get_tasks_dir, resolve_base_dir};
use team_core::team_tasklist::{
    TaskInput, TaskListFilter, can_claim, claim_task, create_task, get_task, list_tasks,
    update_task_status,
};
use team_core::types::{Task, TaskStatus};
use tempfile::TempDir;

struct Fixture {
    root: TempDir,
    config: TeamModeConfig,
    team_run_id: String,
}

/// `createTasklistFixture`.
fn fixture() -> Fixture {
    let root = tempfile::Builder::new()
        .prefix("team-tasklist-")
        .tempdir()
        .expect("tempdir");
    let mut config = TeamModeConfig::with_base_dir(root.path().display().to_string());
    config.enabled = true;
    let team_run_id = uuid::Uuid::new_v4().to_string();
    let tasks = get_tasks_dir(&resolve_base_dir(&config), &team_run_id).expect("tasks dir");
    fs::create_dir_all(tasks.join("claims")).expect("claims dir");
    Fixture {
        root,
        config,
        team_run_id,
    }
}

fn now() -> i64 {
    i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_millis(),
    )
    .expect("ms")
}

fn input(subject: &str) -> TaskInput {
    TaskInput {
        subject: subject.to_owned(),
        ..TaskInput::default()
    }
}

fn owned(status: TaskStatus, owner: &str) -> TaskInput {
    TaskInput {
        status,
        owner: Some(owner.to_owned()),
        claimed_at: Some(now()),
        ..TaskInput::default()
    }
}

// --- claim.test.ts ---------------------------------------------------------------------------

#[test]
fn claim_task_allows_exactly_one_concurrent_claimant() {
    let fx = Arc::new(fixture());
    let task = create_task(&fx.team_run_id, TaskInput::default(), &fx.config).expect("create");
    let handles: Vec<_> = ["member-a", "member-b"]
        .into_iter()
        .map(|member| {
            let fx = Arc::clone(&fx);
            let id = task.id.clone();
            thread::spawn(move || claim_task(&fx.team_run_id, &id, member, &fx.config))
        })
        .collect();
    let results: Vec<_> = handles
        .into_iter()
        .map(|handle| handle.join().expect("join"))
        .collect();
    let successes = results.iter().filter(|result| result.is_ok()).count();
    let failures: Vec<_> = results
        .iter()
        .filter_map(|result| result.as_ref().err())
        .collect();
    assert_eq!(successes, 1);
    assert_eq!(failures.len(), 1);
    assert!(
        matches!(failures[0], TeamCoreError::AlreadyClaimed),
        "{:?}",
        failures[0]
    );
}

#[test]
fn claim_task_rejects_blocked_tasks_until_blockers_complete() {
    let fx = fixture();
    let blocker = create_task(&fx.team_run_id, input("blocker"), &fx.config).expect("blocker");
    let blocked = create_task(
        &fx.team_run_id,
        TaskInput {
            blocked_by: vec![blocker.id.clone()],
            ..input("blocked")
        },
        &fx.config,
    )
    .expect("blocked");
    let error = claim_task(&fx.team_run_id, &blocked.id, "member-a", &fx.config)
        .expect_err("blocked claim");
    assert!(matches!(&error, TeamCoreError::BlockedBy(ids) if ids == &vec![blocker.id.clone()]));
    assert_eq!(error.name(), "BlockedByError");

    claim_task(&fx.team_run_id, &blocker.id, "member-b", &fx.config).expect("claim blocker");
    update_task_status(
        &fx.team_run_id,
        &blocker.id,
        TaskStatus::InProgress,
        "member-b",
        &fx.config,
    )
    .expect("start");
    update_task_status(
        &fx.team_run_id,
        &blocker.id,
        TaskStatus::Completed,
        "member-b",
        &fx.config,
    )
    .expect("complete");

    let claimed = claim_task(&fx.team_run_id, &blocked.id, "member-a", &fx.config).expect("claim");
    assert_eq!(claimed.status, TaskStatus::Claimed);
    assert_eq!(claimed.owner.as_deref(), Some("member-a"));
}

#[test]
fn claim_task_reaps_a_stale_claim_lock_before_claiming() {
    let fx = fixture();
    let task = create_task(&fx.team_run_id, TaskInput::default(), &fx.config).expect("create");
    let tasks = get_tasks_dir(&resolve_base_dir(&fx.config), &fx.team_run_id).expect("dir");
    fs::write(
        tasks.join("claims").join(format!("{}.lock", task.id)),
        format!("member-z\n999999\n{}\n", now() - 600_000),
    )
    .expect("stale lock");
    let claimed = claim_task(&fx.team_run_id, &task.id, "member-a", &fx.config).expect("claim");
    assert_eq!(claimed.status, TaskStatus::Claimed);
    assert_eq!(claimed.owner.as_deref(), Some("member-a"));
}

#[test]
fn claim_task_rejects_traversal_task_id_before_creating_escaped_lock() {
    let fx = fixture();
    let error =
        claim_task(&fx.team_run_id, "../escape", "member-a", &fx.config).expect_err("traversal");
    assert_eq!(error.to_string(), "team path escapes base directory");
}

// --- dependencies.test.ts --------------------------------------------------------------------

fn build_task(id: &str, status: TaskStatus, blocked_by: &[&str]) -> Task {
    Task {
        version: 1,
        id: id.to_owned(),
        subject: format!("subject-{id}"),
        description: format!("description-{id}"),
        active_form: None,
        status,
        owner: None,
        blocks: Vec::new(),
        blocked_by: blocked_by.iter().map(|id| (*id).to_owned()).collect(),
        metadata: None,
        created_at: now(),
        updated_at: now(),
        claimed_at: None,
    }
}

#[test]
fn can_claim_returns_false_when_a_blocker_is_not_completed() {
    let blocker = build_task("2", TaskStatus::InProgress, &[]);
    let dependent = build_task("1", TaskStatus::Pending, &["2"]);
    assert!(!can_claim(&dependent, &[dependent.clone(), blocker]));
}

#[test]
fn can_claim_ignores_missing_blockers_and_completed_blockers() {
    let blocker = build_task("2", TaskStatus::Completed, &[]);
    let dependent = build_task("1", TaskStatus::Pending, &["2", "999"]);
    assert!(can_claim(&dependent, &[dependent.clone(), blocker]));
}

// --- get.test.ts -----------------------------------------------------------------------------

#[test]
fn get_task_returns_a_persisted_task() {
    let fx = fixture();
    let created =
        create_task(&fx.team_run_id, input("persisted task"), &fx.config).expect("create");
    assert_eq!(
        get_task(&fx.team_run_id, &created.id, &fx.config).expect("get"),
        created
    );
}

#[test]
fn get_task_throws_when_the_task_file_is_missing() {
    let fx = fixture();
    let error = get_task(&fx.team_run_id, "999", &fx.config).expect_err("missing");
    assert!(error.is_not_found(), "{error:?}");
}

#[test]
fn get_task_rejects_traversal_task_id() {
    let fx = fixture();
    let error = get_task(&fx.team_run_id, "../escape", &fx.config).expect_err("traversal");
    assert_eq!(error.to_string(), "team path escapes base directory");
}

// --- list.test.ts ----------------------------------------------------------------------------

#[test]
fn list_tasks_returns_tasks_sorted_ascending_and_honors_filters() {
    let fx = fixture();
    let first = create_task(
        &fx.team_run_id,
        TaskInput {
            subject: "one".into(),
            ..owned(TaskStatus::Claimed, "member-a")
        },
        &fx.config,
    )
    .expect("first");
    create_task(&fx.team_run_id, input("two"), &fx.config).expect("second");
    let third = create_task(
        &fx.team_run_id,
        TaskInput {
            subject: "three".into(),
            ..owned(TaskStatus::Claimed, "member-a")
        },
        &fx.config,
    )
    .expect("third");
    update_task_status(
        &fx.team_run_id,
        &third.id,
        TaskStatus::InProgress,
        "member-a",
        &fx.config,
    )
    .expect("start");

    let all = list_tasks(&fx.team_run_id, &fx.config, None).expect("list");
    let filter = TaskListFilter {
        status: Some(TaskStatus::Claimed),
        owner: Some("member-a".into()),
    };
    let claimed = list_tasks(&fx.team_run_id, &fx.config, Some(&filter)).expect("filtered");
    assert_eq!(
        all.iter().map(|task| task.id.as_str()).collect::<Vec<_>>(),
        vec![first.id.as_str(), "2", third.id.as_str()]
    );
    assert_eq!(claimed.len(), 1);
    assert_eq!(claimed[0].id, first.id);
}

#[test]
fn list_tasks_skips_malformed_task_files() {
    let fx = fixture();
    let valid = create_task(&fx.team_run_id, TaskInput::default(), &fx.config).expect("create");
    let tasks = get_tasks_dir(&resolve_base_dir(&fx.config), &fx.team_run_id).expect("dir");
    fs::write(tasks.join("bad.json"), "{not-json").expect("bad");
    fs::write(tasks.join(".highwatermark"), "1").expect("watermark");
    let listed = list_tasks(&fx.team_run_id, &fx.config, None).expect("list");
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].id, valid.id);
}

// --- store.test.ts ---------------------------------------------------------------------------

#[test]
fn create_task_assigns_distinct_ids_during_concurrent_creation() {
    let fx = Arc::new(fixture());
    let handles: Vec<_> = ["first task", "second task"]
        .into_iter()
        .map(|subject| {
            let fx = Arc::clone(&fx);
            thread::spawn(move || {
                create_task(&fx.team_run_id, input(subject), &fx.config).expect("create")
            })
        })
        .collect();
    let mut ids: Vec<i64> = handles
        .into_iter()
        .map(|handle| handle.join().expect("join").id.parse().expect("id"))
        .collect();
    ids.sort_unstable();
    let tasks = get_tasks_dir(&resolve_base_dir(&fx.config), &fx.team_run_id).expect("dir");
    assert_eq!(ids, vec![1, 2]);
    assert_eq!(
        fs::read_to_string(tasks.join(".highwatermark"))
            .expect("watermark")
            .trim(),
        "2"
    );
}

#[test]
fn create_task_rejects_traversal_team_run_id_without_escaping() {
    let fx = fixture();
    let escaped = fx.root.path().parent().expect("parent").join("escape");
    let error =
        create_task("../../escape", TaskInput::default(), &fx.config).expect_err("traversal");
    assert_eq!(error.to_string(), "team path escapes base directory");
    assert!(fs::metadata(escaped).is_err());
}

// --- update.test.ts --------------------------------------------------------------------------

#[test]
fn update_task_status_supports_the_one_way_claim_to_complete_flow() {
    let fx = fixture();
    let task = create_task(&fx.team_run_id, TaskInput::default(), &fx.config).expect("create");
    claim_task(&fx.team_run_id, &task.id, "member-a", &fx.config).expect("claim");
    update_task_status(
        &fx.team_run_id,
        &task.id,
        TaskStatus::InProgress,
        "member-a",
        &fx.config,
    )
    .expect("start");
    let completed = update_task_status(
        &fx.team_run_id,
        &task.id,
        TaskStatus::Completed,
        "member-a",
        &fx.config,
    )
    .expect("done");
    assert_eq!(completed.status, TaskStatus::Completed);
    assert_eq!(
        get_task(&fx.team_run_id, &task.id, &fx.config)
            .expect("get")
            .status,
        TaskStatus::Completed
    );
}

#[test]
fn update_task_status_auto_claims_when_a_member_starts_a_pending_task_directly() {
    let fx = fixture();
    let task = create_task(&fx.team_run_id, TaskInput::default(), &fx.config).expect("create");
    let started = update_task_status(
        &fx.team_run_id,
        &task.id,
        TaskStatus::InProgress,
        "member-a",
        &fx.config,
    )
    .expect("start");
    let loaded = get_task(&fx.team_run_id, &task.id, &fx.config).expect("get");
    for task in [&started, &loaded] {
        assert_eq!(task.status, TaskStatus::InProgress);
        assert_eq!(task.owner.as_deref(), Some("member-a"));
        assert!(task.claimed_at.is_some());
    }
}

#[test]
fn update_task_status_rejects_reverse_transitions() {
    let fx = fixture();
    let task = create_task(
        &fx.team_run_id,
        owned(TaskStatus::Completed, "member-a"),
        &fx.config,
    )
    .expect("create");
    let error = update_task_status(
        &fx.team_run_id,
        &task.id,
        TaskStatus::Claimed,
        "member-a",
        &fx.config,
    )
    .expect_err("reverse");
    assert_eq!(error.name(), "InvalidTaskTransitionError");
    assert_eq!(
        error.to_string(),
        "no reverse transitions from completed to claimed"
    );
}

#[test]
fn update_task_status_rejects_non_owner_updates_except_deletion() {
    let fx = fixture();
    let task = create_task(
        &fx.team_run_id,
        owned(TaskStatus::Claimed, "member-a"),
        &fx.config,
    )
    .expect("create");
    let error = update_task_status(
        &fx.team_run_id,
        &task.id,
        TaskStatus::InProgress,
        "member-b",
        &fx.config,
    )
    .expect_err("cross");
    assert!(matches!(error, TeamCoreError::CrossOwnerUpdate));
    let deleted = update_task_status(
        &fx.team_run_id,
        &task.id,
        TaskStatus::Deleted,
        "lead-member",
        &fx.config,
    )
    .expect("delete");
    assert_eq!(deleted.status, TaskStatus::Deleted);
}

#[test]
fn update_task_status_rejects_traversal_task_id() {
    let fx = fixture();
    let error = update_task_status(
        &fx.team_run_id,
        "../escape",
        TaskStatus::Completed,
        "member-a",
        &fx.config,
    )
    .expect_err("traversal");
    assert_eq!(error.to_string(), "team path escapes base directory");
}
