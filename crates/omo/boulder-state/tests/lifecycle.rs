//! Behaviour of the write/lifecycle entry points that the TypeScript package exports but
//! does not test directly (`createBoulderState`, `addBoulderWork`, `appendSessionId*`,
//! task sessions and timers, `completeBoulder`, `clearBoulderState`, resume options,
//! plan discovery and plan-path resolution).

use std::path::Path;

use boulder_state::{
    BOULDER_STATE_PATH, BoulderSessionOrigin, BoulderTaskStatus, BoulderWorkInput,
    BoulderWorkStatus, CompleteBoulderInput, EndTaskTimerInput, PlanProgress, TaskSessionInput,
    TaskTimerInput, WorkLookupOptions, WorkOwner, add_boulder_work, append_session_id,
    append_session_id_for_work, clear_boulder_state, complete_boulder, create_boulder_state,
    end_task_timer, find_prometheus_plans, generate_work_id, get_active_works, get_boulder_works,
    get_plan_name, get_task_session_state, get_work_by_plan_name, get_work_resume_options,
    read_boulder_state, resolve_boulder_plan_path, start_task_timer, upsert_task_session_state,
    write_boulder_state,
};
use pretty_assertions::assert_eq;

fn seeded(plan_path: &str) -> (tempfile::TempDir, String) {
    let directory = tempfile::tempdir().expect("tempdir");
    let state = create_boulder_state(plan_path, "sess-1", &WorkOwner::default());
    write_boulder_state(directory.path(), &state).expect("write");
    let work_id = state.active_work_id().expect("work id").to_string();
    (directory, work_id)
}

fn read(directory: &Path) -> boulder_state::BoulderState {
    read_boulder_state(directory)
        .expect("readable")
        .expect("state present")
}

fn task(task_key: &str) -> TaskSessionInput {
    TaskSessionInput {
        task_key: task_key.to_string(),
        task_label: "1".to_string(),
        task_title: "First".to_string(),
        session_id: "sess-task".to_string(),
        agent: Some("hephaestus".to_string()),
        category: None,
    }
}

#[test]
fn generated_work_id_is_slug_plus_eight_hex_digits() {
    // given
    let names = ["  My Plan: v2!  ", "***", "already-slug"];

    // when
    let ids: Vec<String> = names.iter().map(|name| generate_work_id(name)).collect();

    // then
    let split: Vec<(&str, usize, bool)> = ids
        .iter()
        .map(|id| {
            let (slug, hex) = id.rsplit_once('-').expect("dash");
            (slug, hex.len(), hex.bytes().all(|b| b.is_ascii_hexdigit()))
        })
        .collect();
    assert_eq!(
        split,
        vec![
            ("my-plan-v2", 8, true),
            ("work", 8, true),
            ("already-slug", 8, true)
        ]
    );
}

#[test]
fn plan_name_is_the_basename_without_md() {
    assert_eq!(
        [
            get_plan_name(".omo/plans/alpha.md"),
            get_plan_name("dir//foo.md//"),
            get_plan_name(".md"),
            get_plan_name("a/.md"),
            get_plan_name("notes.txt"),
        ],
        ["alpha", "foo", "", ".md", "notes.txt"].map(str::to_string)
    );
}

#[test]
fn created_state_writes_schema_v2_with_one_active_work_and_gitignore() {
    // given
    let directory = tempfile::tempdir().expect("tempdir");
    let owner = WorkOwner {
        agent: Some("atlas".to_string()),
        worktree_path: None,
    };
    let state = create_boulder_state(".omo/plans/alpha.md", "sess-1", &owner);

    // when
    write_boulder_state(directory.path(), &state).expect("write");

    // then
    let reread = read(directory.path());
    let work_id = reread.active_work_id().expect("id").to_string();
    let work = reread.work(&work_id).expect("work");
    assert_eq!(
        (
            reread.schema_version(),
            work.plan_name(),
            work.status(),
            work.session_ids(),
            work.session_origin("opencode:sess-1"),
            work.agent(),
        ),
        (
            Some(2),
            Some("alpha"),
            Some(BoulderWorkStatus::Active),
            vec!["opencode:sess-1".to_string()],
            Some(BoulderSessionOrigin::Direct),
            Some("atlas"),
        )
    );
    assert_eq!(
        std::fs::read_to_string(directory.path().join(".omo/.gitignore")).expect("gitignore"),
        "*\n!/rules/\n!/rules/**\n"
    );
    assert!(directory.path().join(BOULDER_STATE_PATH).exists());
}

#[test]
fn added_work_becomes_active_and_keeps_existing_work() {
    // given
    let (directory, first_id) = seeded(".omo/plans/alpha.md");

    // when
    let state = add_boulder_work(
        directory.path(),
        &BoulderWorkInput {
            plan_path: ".omo/plans/beta.md".to_string(),
            session_id: "sess-2".to_string(),
            owner: WorkOwner::default(),
            started_at: Some("2030-01-01T00:00:00.000Z".to_string()),
        },
    )
    .expect("writable")
    .expect("state present");

    // then
    let names: Vec<Option<String>> = get_boulder_works(&state)
        .iter()
        .map(|work| work.plan_name().map(str::to_string))
        .collect();
    assert_eq!(
        names,
        vec![Some("alpha".to_string()), Some("beta".to_string())]
    );
    assert_eq!(state.plan_name(), Some("beta"));
    assert_ne!(state.active_work_id(), Some(first_id.as_str()));
    assert_eq!(read(directory.path()).plan_name(), Some("beta"));
}

#[test]
fn add_work_without_state_is_none() {
    // given
    let directory = tempfile::tempdir().expect("tempdir");

    // when
    let state = add_boulder_work(directory.path(), &BoulderWorkInput::default()).expect("ok");

    // then
    assert_eq!(state, None);
}

#[test]
fn appended_session_joins_active_work_and_mirror() {
    // given
    let (directory, work_id) = seeded(".omo/plans/alpha.md");

    // when
    append_session_id(directory.path(), "sess-2", BoulderSessionOrigin::Appended).expect("ok");

    // then
    let state = read(directory.path());
    let work = state.work(&work_id).expect("work");
    let expected = vec!["opencode:sess-1".to_string(), "opencode:sess-2".to_string()];
    assert_eq!(
        (
            state.session_ids(),
            work.session_ids(),
            work.session_origin("opencode:sess-2")
        ),
        (
            expected.clone(),
            expected,
            Some(BoulderSessionOrigin::Appended)
        )
    );
}

#[test]
fn appending_to_unknown_work_is_none() {
    // given
    let (directory, _) = seeded(".omo/plans/alpha.md");

    // when
    let state = append_session_id_for_work(
        directory.path(),
        "missing",
        "sess-2",
        BoulderSessionOrigin::Direct,
    )
    .expect("ok");

    // then
    assert_eq!(state, None);
}

#[test]
fn appending_to_legacy_mirror_state_updates_root_session_ids() {
    // given
    let directory = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir_all(directory.path().join(".omo")).expect("mkdir");
    std::fs::write(
        directory.path().join(BOULDER_STATE_PATH),
        r#"{"active_plan":"p.md","plan_name":"p","started_at":"2026-01-01T00:00:00.000Z","session_ids":["a","b"]}"#,
    )
    .expect("write");

    // when
    let state = append_session_id(directory.path(), "c", BoulderSessionOrigin::Appended)
        .expect("ok")
        .expect("state");

    // then
    assert_eq!(
        (state.session_ids(), state.session_origin("opencode:c")),
        (
            ["opencode:a", "opencode:b", "opencode:c"]
                .map(str::to_string)
                .to_vec(),
            Some(BoulderSessionOrigin::Appended)
        )
    );
}

#[test]
fn task_session_upsert_and_timers_record_elapsed_time() {
    // given
    let (directory, work_id) = seeded(".omo/plans/alpha.md");
    upsert_task_session_state(directory.path(), &task("todo:1")).expect("ok");

    // when
    start_task_timer(
        directory.path(),
        &work_id,
        &TaskTimerInput {
            task: task("todo:1"),
            started_at: Some("2026-06-05T01:00:00.000Z".to_string()),
        },
    )
    .expect("ok")
    .expect("state");
    end_task_timer(
        directory.path(),
        &work_id,
        &EndTaskTimerInput {
            task_key: "todo:1".to_string(),
            ended_at: Some("2026-06-05T01:00:02.500Z".to_string()),
        },
    )
    .expect("ok")
    .expect("state");

    // then
    let session = get_task_session_state(directory.path(), "todo:1")
        .expect("ok")
        .expect("session");
    assert_eq!(
        (
            session.session_id(),
            session.agent(),
            session.status(),
            session.started_at(),
            session.elapsed_ms(),
        ),
        (
            Some("opencode:sess-task"),
            Some("hephaestus"),
            Some(BoulderTaskStatus::Completed),
            Some("2026-06-05T01:00:00.000Z"),
            Some(2_500),
        )
    );
    let mirror_session = read(directory.path())
        .task_session("todo:1")
        .expect("mirror");
    assert_eq!(mirror_session, session);
}

#[test]
fn upsert_keeps_timer_fields_of_an_existing_task_session() {
    // given
    let (directory, work_id) = seeded(".omo/plans/alpha.md");
    start_task_timer(
        directory.path(),
        &work_id,
        &TaskTimerInput {
            task: task("todo:1"),
            started_at: Some("2026-06-05T01:00:00.000Z".to_string()),
        },
    )
    .expect("ok");

    // when
    upsert_task_session_state(directory.path(), &task("todo:1")).expect("ok");

    // then
    let session = get_task_session_state(directory.path(), "todo:1")
        .expect("ok")
        .expect("session");
    assert_eq!(
        (session.started_at(), session.status()),
        (
            Some("2026-06-05T01:00:00.000Z"),
            Some(BoulderTaskStatus::Running)
        )
    );
}

#[test]
fn reserved_task_keys_are_rejected() {
    // given
    let (directory, _) = seeded(".omo/plans/alpha.md");

    // when
    let results: Vec<bool> = ["__proto__", "prototype", "constructor"]
        .iter()
        .map(|key| {
            upsert_task_session_state(directory.path(), &task(key))
                .expect("ok")
                .is_some()
        })
        .collect();

    // then
    assert_eq!(results, vec![false, false, false]);
}

#[test]
fn ending_an_unknown_task_is_none() {
    // given
    let (directory, work_id) = seeded(".omo/plans/alpha.md");

    // when
    let state = end_task_timer(
        directory.path(),
        &work_id,
        &EndTaskTimerInput {
            task_key: "todo:9".to_string(),
            ended_at: None,
        },
    )
    .expect("ok");

    // then
    assert_eq!(state, None);
}

#[test]
fn completing_marks_work_done_and_hides_it_from_active_lists() {
    // given
    let (directory, _) = seeded(".omo/plans/alpha.md");

    // when
    let state = complete_boulder(directory.path(), &CompleteBoulderInput::default())
        .expect("ok")
        .expect("state");

    // then
    assert_eq!(
        (
            state.status(),
            state.ended_at().is_some(),
            state.elapsed_ms().is_some()
        ),
        (Some(BoulderWorkStatus::Completed), true, true)
    );
    assert_eq!(get_active_works(directory.path()).expect("ok"), Vec::new());
    assert_eq!(
        get_work_resume_options(directory.path()).expect("ok"),
        Vec::new()
    );
}

#[test]
fn completing_an_already_completed_work_does_not_rewrite() {
    // given
    let (directory, _) = seeded(".omo/plans/alpha.md");
    complete_boulder(directory.path(), &CompleteBoulderInput::default()).expect("ok");
    let before = std::fs::read_to_string(directory.path().join(BOULDER_STATE_PATH)).expect("read");

    // when
    complete_boulder(
        directory.path(),
        &CompleteBoulderInput {
            work_id: None,
            ended_at: Some("2099-01-01T00:00:00.000Z".to_string()),
        },
    )
    .expect("ok");

    // then
    let after = std::fs::read_to_string(directory.path().join(BOULDER_STATE_PATH)).expect("read");
    assert_eq!(after, before);
}

#[test]
fn resume_options_report_plan_progress_and_current_mirror() {
    // given
    let (directory, work_id) = seeded(".omo/plans/alpha.md");
    std::fs::create_dir_all(directory.path().join(".omo/plans")).expect("mkdir");
    std::fs::write(
        directory.path().join(".omo/plans/alpha.md"),
        "## TODOs\n- [x] 1. One\n- [ ] 2. Two\n",
    )
    .expect("write plan");

    // when
    let options = get_work_resume_options(directory.path()).expect("ok");

    // then
    let summary: Vec<(String, BoulderWorkStatus, usize, PlanProgress, bool)> = options
        .into_iter()
        .map(|option| {
            (
                option.work_id,
                option.status,
                option.session_count,
                option.progress,
                option.is_current_mirror,
            )
        })
        .collect();
    assert_eq!(
        summary,
        vec![(
            work_id,
            BoulderWorkStatus::Active,
            1,
            PlanProgress {
                total: 2,
                completed: 1,
                is_complete: false
            },
            true
        )]
    );
}

#[test]
fn work_by_plan_name_honours_worktree_filter() {
    // given
    let (directory, _) = seeded(".omo/plans/alpha.md");

    // when
    let unfiltered =
        get_work_by_plan_name(directory.path(), "alpha", &WorkLookupOptions::default())
            .expect("ok");
    let filtered = get_work_by_plan_name(
        directory.path(),
        "alpha",
        &WorkLookupOptions {
            worktree_path: Some("/elsewhere".to_string()),
        },
    )
    .expect("ok");

    // then
    assert_eq!((unfiltered.is_some(), filtered.is_some()), (true, false));
}

#[test]
fn clearing_removes_the_file_and_tolerates_a_missing_one() {
    // given
    let (directory, _) = seeded(".omo/plans/alpha.md");

    // when
    clear_boulder_state(directory.path()).expect("first clear");
    clear_boulder_state(directory.path()).expect("second clear");

    // then
    assert_eq!(read_boulder_state(directory.path()).expect("ok"), None);
}

#[test]
fn prometheus_plans_are_found_in_current_and_legacy_dirs() {
    // given
    let directory = tempfile::tempdir().expect("tempdir");
    for dir in [".omo/plans", ".sisyphus/plans"] {
        std::fs::create_dir_all(directory.path().join(dir)).expect("mkdir");
    }
    std::fs::write(directory.path().join(".omo/plans/a.md"), "a").expect("write");
    std::fs::write(directory.path().join(".omo/plans/skip.txt"), "x").expect("write");
    std::fs::write(directory.path().join(".sisyphus/plans/b.md"), "b").expect("write");

    // when
    let mut names: Vec<String> = find_prometheus_plans(directory.path())
        .iter()
        .map(|path| {
            path.file_name()
                .expect("name")
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    names.sort();

    // then
    assert_eq!(names, vec!["a.md".to_string(), "b.md".to_string()]);
}

#[test]
fn plan_path_prefers_existing_worktree_copy() {
    // given
    let directory = tempfile::tempdir().expect("tempdir");
    let worktree = directory.path().join("wt");
    std::fs::create_dir_all(worktree.join(".omo/plans")).expect("mkdir");
    std::fs::write(worktree.join(".omo/plans/alpha.md"), "x").expect("write");
    let with_copy = create_boulder_state(
        ".omo/plans/alpha.md",
        "s",
        &WorkOwner {
            agent: None,
            worktree_path: Some("wt".to_string()),
        },
    );
    let without_copy = create_boulder_state(
        ".omo/plans/beta.md",
        "s",
        &WorkOwner {
            agent: None,
            worktree_path: Some("wt".to_string()),
        },
    );

    // when
    let resolved = resolve_boulder_plan_path(directory.path(), &with_copy);
    let fallback = resolve_boulder_plan_path(directory.path(), &without_copy);

    // then
    assert_eq!(resolved, worktree.join(".omo/plans/alpha.md"));
    assert_eq!(fallback, directory.path().join(".omo/plans/beta.md"));
}
