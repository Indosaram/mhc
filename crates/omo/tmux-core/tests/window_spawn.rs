//! Port of src/tmux-utils/window-spawn.test.ts

mod common;

use common::{Recorder, enabled_config, healthy_deps, strings, success};
use pretty_assertions::assert_eq;
use tmux_core::{SpawnPaneResult, SpawnTmuxWindowRequest, spawn_tmux_window};

fn spawn(description: &str, directory: &str) -> (Recorder, SpawnPaneResult) {
    let recorder = Recorder::new(vec![success("%42"), success("")]);
    let config = enabled_config();
    let result = spawn_tmux_window(
        &SpawnTmuxWindowRequest {
            session_id: "session-1",
            description,
            config: &config,
            server_url: "http://127.0.0.1:1234",
            directory,
        },
        &healthy_deps(&recorder, "sh"),
    );
    (recorder, result)
}

#[test]
fn healthy_tmux_delegates_new_window_and_select_pane_to_runner() {
    let (recorder, result) = spawn("worker", "/tmp/omo-project/(window)");
    assert_eq!(result, SpawnPaneResult::spawned("%42".into()));
    assert_eq!(
        recorder.call(0).1[..7],
        strings(&[
            "new-window",
            "-d",
            "-n",
            "omo-agents",
            "-P",
            "-F",
            "#{pane_id}"
        ])
    );
    assert_eq!(
        recorder.call(1).1,
        strings(&["select-pane", "-t", "%42", "-T", "omo-subagent-worker"])
    );
    let command = recorder.command(0);
    assert!(command.contains("Focus this pane to attach."));
    assert!(command.contains("while :; do sleep 86400; done"));
    assert!(!command.contains("opencode attach"));
}

#[test]
fn description_with_spaces_is_included_in_placeholder() {
    let (recorder, _) = spawn("worker with spaces", "/path with spaces/here");
    assert!(
        recorder
            .command(0)
            .contains("OMO subagent pane ready: worker with spaces")
    );
}

#[test]
fn empty_directory_keeps_placeholder_detached_from_attach() {
    let (recorder, _) = spawn("worker", "");
    assert!(!recorder.command(0).contains("--dir"));
}

#[test]
fn description_with_shell_metacharacters_escapes_placeholder() {
    let (recorder, _) = spawn(r#"worker "$(whoami)""#, "/path/with'quote");
    assert!(recorder.command(0).contains(r#"\""#));
    assert!(recorder.command(0).contains(r"\$"));
}
