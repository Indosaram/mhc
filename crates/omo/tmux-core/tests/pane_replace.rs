//! Port of src/tmux-utils/pane-replace.test.ts

mod common;

use common::{Recorder, enabled_config, healthy_deps, strings, success};
use pretty_assertions::assert_eq;
use tmux_core::{ReplaceTmuxPaneRequest, SpawnPaneResult, replace_tmux_pane};

fn replace(description: &str, directory: &str) -> (Recorder, SpawnPaneResult) {
    let recorder = Recorder::new(vec![success(""), success(""), success("")]);
    let config = enabled_config();
    let result = replace_tmux_pane(
        &ReplaceTmuxPaneRequest {
            pane_id: "%42",
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
fn existing_pane_delegates_send_keys_respawn_and_select_pane_to_runner() {
    let (recorder, result) = replace("worker", "/tmp/omo-project/(replace)");
    assert_eq!(result, SpawnPaneResult::spawned("%42".into()));
    assert_eq!(
        recorder.call(0).1,
        strings(&["send-keys", "-t", "%42", "C-c"])
    );
    let respawn = recorder.call(1).1;
    let target = respawn
        .iter()
        .position(|arg| arg == "-t")
        .expect("Expected respawn-pane target argument");
    assert_eq!(respawn[..2], strings(&["respawn-pane", "-k"]));
    assert_eq!(respawn[target..target + 2], strings(&["-t", "%42"]));
    assert_eq!(
        recorder.call(2).1,
        strings(&["select-pane", "-t", "%42", "-T", "omo-subagent-worker"])
    );
    let command = recorder.command(1);
    assert!(command.contains("Focus this pane to attach."));
    assert!(command.contains("while :; do sleep 86400; done"));
    assert!(!command.contains("opencode attach"));
}

#[test]
fn description_with_spaces_is_included_in_placeholder() {
    let (recorder, _) = replace("worker with spaces", "/path with spaces/here");
    assert!(
        recorder
            .command(1)
            .contains("OMO subagent pane ready: worker with spaces")
    );
}

#[test]
fn empty_directory_keeps_placeholder_detached_from_attach() {
    let (recorder, _) = replace("worker", "");
    assert!(!recorder.command(1).contains("--dir"));
}

#[test]
fn description_with_shell_metacharacters_escapes_placeholder() {
    let (recorder, _) = replace(r#"worker "$(whoami)""#, "/path/with'quote");
    assert!(recorder.command(1).contains(r#"\""#));
    assert!(recorder.command(1).contains(r"\$"));
}
