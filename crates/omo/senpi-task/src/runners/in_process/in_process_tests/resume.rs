//! `in-process-persistence.test.ts`, `in-process-resume.test.ts`,
//! `in-process-resume-session.test.ts`, `dag-child-policy.test.ts`.

use std::path::{MAIN_SEPARATOR, Path};
use std::sync::Arc;

use serde_json::json;

use super::fake_session::{FakeSession, OnPrompt};
use super::support::{
    Captured, base_spec, capturing_runner, last_options, make_tool, session_dir_in, session_header,
    strings, tmp, tool_names, write_session_file,
};
use crate::runners::in_process::child_handle::{ChildSession, RunnerFailureKind, RunnerOutcome};
use crate::runners::in_process::runner::{ChildSpec, InProcessRunner};
use crate::runners::in_process::runner_error::RunnerError;
use crate::runners::in_process::session_manager::ChildSessionManager;
use crate::runners::rpc::spawn::resolve_child_session_dir;

fn immediate_runner(
    shared: Vec<crate::runners::in_process::shared_tool_filter::ChildToolRef>,
) -> (InProcessRunner, Captured) {
    capturing_runner(shared, &[], None, || {
        FakeSession::immediate("resume-immediate", None)
    })
}

fn transcripts(dir: &str) -> Vec<String> {
    std::fs::read_dir(dir)
        .expect("session dir")
        .filter_map(Result::ok)
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name.ends_with(".jsonl"))
        .collect()
}

fn append_turn(session_manager: &ChildSessionManager) {
    let stamp = chrono::Utc::now().timestamp_millis();
    session_manager
        .append_message(&json!({
            "role": "user",
            "content": [{ "type": "text", "text": "child prompt marker" }],
            "timestamp": stamp,
        }))
        .expect("append user");
    session_manager
        .append_message(&json!({
            "role": "assistant",
            "content": [{ "type": "text", "text": "child assistant marker" }],
            "api": "faux-api", "provider": "faux-provider", "model": "faux-model",
            "usage": { "input": 1, "output": 1, "cacheRead": 0, "cacheWrite": 0, "totalTokens": 2,
                "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "total": 0 } },
            "stopReason": "stop",
            "timestamp": stamp,
        }))
        .expect("append assistant");
}

/// A runner whose sessions append a real turn to the captured session manager on prompt.
fn appending_runner() -> (InProcessRunner, Captured) {
    let captured: Captured = Arc::default();
    let sink = Arc::clone(&captured);
    let runner = InProcessRunner::new(crate::runners::in_process::runner::InProcessRunnerOptions {
        shared_parent_tools: Vec::new(),
        ui_only_tool_names: Vec::new(),
        max_depth: None,
        create_session: Arc::new(move |options| {
            let manager = Arc::clone(&options.session_manager);
            sink.lock().expect("captured").push(options);
            let on_prompt: OnPrompt = Box::new(move || append_turn(&manager));
            let session: Arc<dyn ChildSession> = FakeSession::immediate("persist", Some(on_prompt));
            Ok(session)
        }),
    });
    (runner, captured)
}

fn expect_kind(result: Result<impl Sized, RunnerError>, kind: RunnerFailureKind) {
    match result {
        Err(error) => assert_eq!(error.kind(), kind, "{error}"),
        Ok(_) => panic!("expected a {kind:?} failure"),
    }
}

fn legacy_spec(task_id: &str) -> ChildSpec {
    ChildSpec {
        task_id: task_id.to_string(),
        session_dir: None,
        ..base_spec("")
    }
}

fn persist_spec(dir: &tempfile::TempDir, task_id: &str) -> ChildSpec {
    let children = dir.path().join("children").join(task_id);
    ChildSpec {
        task_id: task_id.to_string(),
        ..base_spec(&resolve_child_session_dir(
            &children.to_string_lossy(),
            task_id,
        ))
    }
}

#[test]
fn given_a_child_spec_with_a_resolved_child_session_dir_when_started_then_the_session_manager_persists_under_children_task_id_sessions()
 {
    let dir = tmp();
    let (runner, captured) = immediate_runner(Vec::new());
    let spec = persist_spec(&dir, "task-persist-1");
    runner.start(&spec).expect("start").wait_for_idle();
    let options = last_options(&captured);
    assert!(options.session_manager.is_persisted());
    let expected = Path::new("children")
        .join(&spec.task_id)
        .join("sessions")
        .join(&spec.task_id);
    assert!(
        options
            .session_manager
            .session_dir()
            .contains(&*expected.to_string_lossy())
    );
}

#[test]
fn given_allow_and_deny_tool_lists_when_the_child_session_is_constructed_then_tools_carries_the_allowlist_and_exclude_tools_carries_the_denylist()
 {
    let dir = tmp();
    let (runner, captured) = immediate_runner(Vec::new());
    let spec = ChildSpec {
        tool_allowlist: Some(strings(&["read", "bash"])),
        tool_denylist: Some(strings(&["write", "edit"])),
        ..persist_spec(&dir, "task-persist-1")
    };
    runner.start(&spec).expect("start").wait_for_idle();
    let options = last_options(&captured);
    assert_eq!(options.tools, Some(strings(&["read", "bash"])));
    assert_eq!(options.exclude_tools, Some(strings(&["write", "edit"])));
}

#[test]
fn given_no_tool_denylist_when_the_child_session_is_constructed_then_exclude_tools_stays_unset_so_senpi_keeps_its_defaults()
 {
    let dir = tmp();
    let (runner, captured) = immediate_runner(Vec::new());
    let spec = ChildSpec {
        tool_allowlist: Some(strings(&["read"])),
        ..persist_spec(&dir, "task-persist-1")
    };
    runner.start(&spec).expect("start").wait_for_idle();
    let options = last_options(&captured);
    assert_eq!(options.tools, Some(strings(&["read"])));
    assert_eq!(options.exclude_tools, None);
}

#[test]
fn given_an_unwritable_session_dir_when_start_is_called_then_a_typed_session_create_failure_surfaces_with_no_in_memory_fallback()
 {
    let dir = tmp();
    let blocker = dir.path().join("blocker");
    std::fs::write(&blocker, "not a directory").expect("blocker");
    let (runner, captured) = immediate_runner(Vec::new());
    let session_dir = format!(
        "{}{MAIN_SEPARATOR}",
        blocker.join("sessions").join("task-x").display()
    );
    expect_kind(
        runner.start(&base_spec(&session_dir)),
        RunnerFailureKind::SessionCreateFailed,
    );
    assert!(captured.lock().expect("captured").is_empty());
}

#[test]
fn given_a_legacy_spec_without_a_session_dir_when_start_is_called_then_a_typed_session_create_failure_surfaces_instead_of_a_silent_default_dir_fallback()
 {
    let (runner, captured) = immediate_runner(Vec::new());
    expect_kind(
        runner.start(&legacy_spec("task-legacy")),
        RunnerFailureKind::SessionCreateFailed,
    );
    assert!(captured.lock().expect("captured").is_empty());
}

#[test]
fn given_the_real_default_session_manager_when_one_stubbed_turn_runs_then_a_jsonl_transcript_with_the_child_messages_appears_under_the_session_dir()
 {
    let dir = tmp();
    let (runner, captured) = appending_runner();
    let handle = runner
        .start(&persist_spec(&dir, "task-persist-1"))
        .expect("start");
    assert_eq!(handle.wait_for_idle(), RunnerOutcome::completed("done"));
    let session_dir = last_options(&captured)
        .session_manager
        .session_dir()
        .to_string();
    let files = transcripts(&session_dir);
    assert_eq!(files.len(), 1);
    let transcript =
        std::fs::read_to_string(Path::new(&session_dir).join(&files[0])).expect("read");
    assert!(transcript.contains("child prompt marker"));
    assert!(transcript.contains("child assistant marker"));
}

#[test]
fn given_a_curated_spec_with_allowlist_denylist_and_member_scoped_names_when_resumed_then_the_tool_surface_matches_the_start_surface_exactly()
 {
    let dir = tmp();
    let task_send = make_tool("task_send");
    let parent_bash = make_tool("bash");
    let (runner, captured) = immediate_runner(vec![
        make_tool("grep"),
        Arc::clone(&parent_bash),
        Arc::clone(&task_send),
        make_tool("task_create"),
    ]);
    let spec = ChildSpec {
        agent_type: Some("explore".to_string()),
        tool_allowlist: Some(strings(&["read", "bash", "task_send"])),
        tool_denylist: Some(strings(&["grep"])),
        member_scoped_tools: Some(vec![Arc::clone(&task_send)]),
        member_scoped_tool_names: Some(strings(&["task_send"])),
        ..base_spec(&session_dir_in(&dir, "task-resume-1"))
    };
    let session_file_dir = tmp();
    let session_path =
        write_session_file(&session_file_dir, &session_header("resume-test-session"));
    runner.start(&spec).expect("start").wait_for_idle();
    runner.resume(&spec, &session_path).expect("resume");
    let all = captured.lock().expect("captured").clone();
    let (start_options, resume_options) = (&all[0], &all[1]);
    let resume_names = tool_names(resume_options);
    assert_eq!(resume_names, tool_names(start_options));
    assert!(!resume_names.contains(&"task_create".to_string()));
    let resumed_send = resume_options
        .custom_tools
        .iter()
        .find(|tool| tool.name() == "task_send")
        .expect("task_send");
    assert!(Arc::ptr_eq(resumed_send, &task_send));
    let bash: Vec<_> = resume_options
        .custom_tools
        .iter()
        .filter(|tool| tool.name() == "bash")
        .collect();
    assert_eq!(bash.len(), 1);
    assert!(bash[0].description().contains("read-only"));
    assert!(!Arc::ptr_eq(bash[0], &parent_bash));
    assert_eq!(
        resume_options.tools,
        Some(strings(&["read", "bash", "task_send"]))
    );
    assert_eq!(resume_options.exclude_tools, Some(strings(&["grep"])));
    let excluded = resume_options.exclude_tools.clone().unwrap_or_default();
    assert!(
        !resume_names
            .iter()
            .any(|name| !excluded.contains(name) && name == "grep")
    );
}

#[test]
fn given_a_resumed_child_when_the_handle_is_created_then_no_prompt_is_sent_and_the_next_follow_up_starts_a_fresh_tracked_turn()
 {
    let dir = tmp();
    let fake = FakeSession::new("resume-session-1");
    fake.set_last_text("restored text");
    let session = Arc::clone(&fake);
    let (runner, _captured) = capturing_runner(Vec::new(), &[], None, move || session.clone());
    let session_file_dir = tmp();
    let session_path =
        write_session_file(&session_file_dir, &session_header("resume-test-session"));
    let handle = runner
        .resume(
            &base_spec(&session_dir_in(&dir, "task-resume-1")),
            &session_path,
        )
        .expect("resume");
    assert_eq!(fake.prompt_calls(), 0);
    assert_eq!(
        handle.wait_for_idle(),
        RunnerOutcome::completed("restored text")
    );
    handle.follow_up_turn("keep going").expect("follow up");
    fake.wait_prompt_calls(1);
    fake.set_last_text("new text");
    fake.resolve_prompt();
    assert_eq!(handle.wait_for_idle(), RunnerOutcome::completed("new text"));
}

fn resume_member_scoped(shared: &[&str], names: &[&str]) -> (Result<(), RunnerError>, usize) {
    let dir = tmp();
    let (runner, captured) = immediate_runner(shared.iter().map(|name| make_tool(name)).collect());
    let spec = ChildSpec {
        member_scoped_tool_names: Some(strings(names)),
        ..base_spec(&session_dir_in(&dir, "task-resume-1"))
    };
    let session_path = write_session_file(&dir, &session_header("resume-test-session"));
    let result = runner.resume(&spec, &session_path).map(|_| ());
    let created = captured.lock().expect("captured").len();
    (result, created)
}

#[test]
fn given_a_member_scoped_name_missing_from_the_live_parent_tools_when_resumed_then_a_typed_tools_unavailable_failure_surfaces_and_no_session_is_created()
 {
    let (result, created) = resume_member_scoped(&["grep"], &["task_send"]);
    expect_kind(result, RunnerFailureKind::ToolsUnavailable);
    assert_eq!(created, 0);
}

#[test]
fn given_a_duplicated_member_scoped_name_when_resumed_then_a_typed_tools_unavailable_failure_surfaces()
 {
    let (result, _created) = resume_member_scoped(&["task_send"], &["task_send", "task_send"]);
    expect_kind(result, RunnerFailureKind::ToolsUnavailable);
}

#[test]
fn given_a_member_scoped_name_matching_two_live_tools_when_resumed_then_a_typed_tools_unavailable_failure_surfaces()
 {
    let (result, _created) = resume_member_scoped(&["task_send", "task_send"], &["task_send"]);
    expect_kind(result, RunnerFailureKind::ToolsUnavailable);
}

fn resume_session_path(path: &Path, spec: &ChildSpec) -> (Result<(), RunnerError>, usize) {
    let (runner, captured) = immediate_runner(Vec::new());
    let result = runner.resume(spec, path).map(|_| ());
    let created = captured.lock().expect("captured").len();
    (result, created)
}

#[test]
fn given_a_corrupt_session_file_when_resumed_then_a_typed_session_unavailable_failure_surfaces_and_no_raw_parse_error_escapes()
 {
    let dir = tmp();
    let path = write_session_file(&dir, "this is not json\n{nope\n");
    let (result, created) = resume_session_path(
        &path,
        &base_spec(&session_dir_in(&dir, "task-resume-session-1")),
    );
    expect_kind(result, RunnerFailureKind::SessionUnavailable);
    assert_eq!(created, 0);
}

#[test]
fn given_a_missing_session_file_when_resumed_then_a_typed_session_unavailable_failure_surfaces() {
    let dir = tmp();
    let path = dir.path().join("does-not-exist.jsonl");
    let (result, _created) = resume_session_path(
        &path,
        &base_spec(&session_dir_in(&dir, "task-resume-session-1")),
    );
    expect_kind(result, RunnerFailureKind::SessionUnavailable);
}

#[test]
fn given_a_directory_as_the_session_path_when_resumed_then_a_typed_session_unavailable_failure_surfaces()
 {
    let dir = tmp();
    let path = dir.path().join("a-directory");
    std::fs::create_dir(&path).expect("mkdir");
    let (result, _created) = resume_session_path(
        &path,
        &base_spec(&session_dir_in(&dir, "task-resume-session-1")),
    );
    expect_kind(result, RunnerFailureKind::SessionUnavailable);
}

#[test]
fn given_a_session_file_whose_first_entry_is_not_a_session_header_when_resumed_then_a_typed_session_unavailable_failure_surfaces()
 {
    let dir = tmp();
    let path = write_session_file(
        &dir,
        &format!(
            "{}\n",
            json!({ "type": "message", "id": "m1", "role": "user" })
        ),
    );
    let (result, _created) = resume_session_path(
        &path,
        &base_spec(&session_dir_in(&dir, "task-resume-session-1")),
    );
    expect_kind(result, RunnerFailureKind::SessionUnavailable);
}

#[test]
fn given_a_legacy_spec_without_a_session_dir_when_resumed_then_a_typed_session_create_failure_surfaces_instead_of_a_silent_default_dir_fallback()
 {
    let dir = tmp();
    let path = write_session_file(&dir, &session_header("resume-session-file"));
    let (result, created) = resume_session_path(&path, &legacy_spec("task-legacy-resume"));
    expect_kind(result, RunnerFailureKind::SessionCreateFailed);
    assert_eq!(created, 0);
}

#[test]
fn given_a_real_persisted_child_transcript_written_by_the_start_path_when_resumed_then_the_same_session_file_is_reopened_and_a_follow_up_produces_a_tracked_turn()
 {
    let dir = tmp();
    let spec = persist_spec(&dir, "task-resume-integration");
    let session_dir = spec.session_dir.clone().expect("session dir");
    let (runner, captured) = appending_runner();
    let started = runner.start(&spec).expect("start");
    assert!(matches!(
        started.wait_for_idle(),
        RunnerOutcome::Completed { .. }
    ));
    let files = transcripts(&session_dir);
    assert_eq!(files.len(), 1);
    let session_path = Path::new(&session_dir).join(&files[0]);
    let resumed = runner.resume(&spec, &session_path).expect("resume");
    assert_eq!(transcripts(&session_dir).len(), 1);
    assert!(
        std::fs::read_to_string(&session_path)
            .expect("read")
            .contains("child prompt marker")
    );
    let all = captured.lock().expect("captured").clone();
    assert!(all[1].session_manager.is_persisted());
    assert_eq!(
        all[1].session_manager.session_dir(),
        all[0].session_manager.session_dir()
    );
    assert_eq!(
        all[1].session_manager.session_file(),
        session_path.as_path()
    );
    resumed.follow_up_turn("continue").expect("follow up");
    assert_eq!(resumed.wait_for_idle(), RunnerOutcome::completed("done"));
}

fn dag_spec(dir: &tempfile::TempDir, task_id: &str) -> ChildSpec {
    ChildSpec {
        task_id: task_id.to_string(),
        depth: 1,
        parent_session_id: "parent-session".to_string(),
        root_session_id: "root-session".to_string(),
        prompt: "execute the DAG node".to_string(),
        ..base_spec(&session_dir_in(dir, "st_dag_policy"))
    }
}

#[test]
fn given_a_dag_node_child_when_its_session_options_are_built_then_no_orchestration_capable_shared_tool_reaches_it()
 {
    let dir = tmp();
    let shared = [
        "grep",
        "task",
        "task_create",
        "task_send",
        "team_create",
        "dag",
    ];
    let (runner, captured) = immediate_runner(shared.iter().map(|name| make_tool(name)).collect());
    runner
        .start(&dag_spec(&dir, "st_dag_policy"))
        .expect("start")
        .wait_for_idle();
    assert_eq!(tool_names(&last_options(&captured)), strings(&["grep"]));
}

#[test]
fn given_a_persisted_dag_node_child_when_resumed_then_the_shared_orchestration_filter_is_re_applied()
 {
    let dir = tmp();
    let shared = ["grep", "task_update", "team_send", "dag"];
    let (runner, captured) = immediate_runner(shared.iter().map(|name| make_tool(name)).collect());
    let path = write_session_file(&dir, &session_header("dag-policy-session"));
    runner
        .resume(&dag_spec(&dir, "st_dag_policy"), &path)
        .expect("resume");
    assert_eq!(tool_names(&last_options(&captured)), strings(&["grep"]));
}

#[test]
fn given_plain_and_team_member_children_when_their_tool_sets_are_built_then_existing_sanctioned_sets_stay_exact_and_dag_is_absent()
 {
    let dir = tmp();
    let task_send = make_tool("task_send");
    let shared = ["grep", "task", "team_create", "dag"];
    let (runner, captured) = immediate_runner(shared.iter().map(|name| make_tool(name)).collect());
    let plain = runner.start(&dag_spec(&dir, "st_plain")).expect("plain");
    let member = runner
        .start(&ChildSpec {
            member_scoped_tools: Some(vec![task_send]),
            member_scoped_tool_names: Some(strings(&["task_send"])),
            ..dag_spec(&dir, "st_member")
        })
        .expect("member");
    plain.wait_for_idle();
    member.wait_for_idle();
    let all: Vec<Vec<String>> = captured
        .lock()
        .expect("captured")
        .iter()
        .map(tool_names)
        .collect();
    assert_eq!(
        all,
        vec![strings(&["grep"]), strings(&["grep", "task_send"])]
    );
}
