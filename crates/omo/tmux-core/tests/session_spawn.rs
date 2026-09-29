//! Port of src/tmux-utils/session-spawn.test.ts

mod common;

use common::{Recorder, enabled_config, failed, healthy_deps, strings, success};
use pretty_assertions::assert_eq;
use tmux_core::{SpawnPaneResult, SpawnTmuxSessionRequest, spawn_tmux_session};

fn spawn(description: &str, directory: &str) -> (Recorder, SpawnPaneResult) {
    let recorder = Recorder::new(vec![
        success("120,40"),
        failed(),
        success("%42"),
        success(""),
    ]);
    let config = enabled_config();
    let result = spawn_tmux_session(
        &SpawnTmuxSessionRequest {
            session_id: "session-1",
            description,
            config: &config,
            server_url: "http://127.0.0.1:1234",
            directory,
            source_pane_id: Some("%0"),
            manager_id: None,
        },
        &healthy_deps(&recorder, "sh"),
    );
    (recorder, result)
}

#[test]
fn source_pane_available_delegates_display_has_session_new_session_and_select_pane() {
    let (recorder, result) = spawn("worker", "/tmp/omo-project/(session)");
    assert_eq!(result, SpawnPaneResult::spawned("%42".into()));
    assert_eq!(
        recorder.call(0).1,
        strings(&[
            "display",
            "-p",
            "-t",
            "%0",
            "#{window_width},#{window_height}"
        ])
    );
    let has_session = recorder.call(1).1;
    assert_eq!(has_session[..2], strings(&["has-session", "-t"]));
    assert!(has_session[2].starts_with("omo-agents-"));
    let new_session = recorder.call(2).1;
    assert_eq!(new_session[..3], strings(&["new-session", "-d", "-s"]));
    assert!(new_session[3].starts_with("omo-agents-"));
    assert_eq!(
        recorder.call(3).1,
        strings(&["select-pane", "-t", "%42", "-T", "omo-subagent-worker"])
    );
    let command = recorder.command(2);
    assert!(command.contains("Focus this pane to attach."));
    assert!(command.contains("while :; do sleep 86400; done"));
    assert!(!command.contains("opencode attach"));
}

#[test]
fn description_with_spaces_is_included_in_placeholder() {
    let (recorder, _) = spawn("worker with spaces", "/path with spaces/here");
    assert!(
        recorder
            .command(2)
            .contains("OMO subagent pane ready: worker with spaces")
    );
}

#[test]
fn empty_directory_keeps_placeholder_detached_from_attach() {
    let (recorder, _) = spawn("worker", "");
    assert!(!recorder.command(2).contains("--dir"));
}

#[test]
fn description_with_shell_metacharacters_escapes_placeholder() {
    let (recorder, _) = spawn(r#"worker "$(whoami)""#, "/path/with'quote");
    assert!(recorder.command(2).contains(r#"\""#));
    assert!(recorder.command(2).contains(r"\$"));
}
