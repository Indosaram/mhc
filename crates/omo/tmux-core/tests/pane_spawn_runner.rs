//! Port of src/tmux-utils/pane-spawn-runner.test.ts

mod common;

use std::sync::Arc;

use common::{Recorder, enabled_config, env, healthy_deps, strings, success};
use pretty_assertions::assert_eq;
use tmux_core::{
    SpawnPaneResult, SpawnTmuxPaneRequest, SplitDirection, TmuxDeps,
    is_tmux_pane_compatible_environment, spawn_tmux_pane,
};

fn spawn_results() -> Recorder {
    Recorder::new(vec![success("%42"), success("")])
}

/// Mock-style deps: inside tmux and cmux detection are explicit predicates.
fn deps(recorder: &Recorder, cmux: bool) -> TmuxDeps {
    TmuxDeps {
        is_cmux_compat_environment: Some(Arc::new(move || cmux)),
        ..healthy_deps(recorder, "sh")
    }
}

fn spawn(session_id: &str, description: &str, directory: &str, deps: &TmuxDeps) -> SpawnPaneResult {
    let config = enabled_config();
    spawn_tmux_pane(
        &SpawnTmuxPaneRequest {
            session_id,
            description,
            config: &config,
            server_url: "http://127.0.0.1:1234",
            directory,
            target_pane_id: Some("%0"),
            split_direction: SplitDirection::Horizontal,
        },
        deps,
    )
}

#[test]
fn healthy_tmux_delegates_split_window_and_select_pane_to_runner() {
    let recorder = spawn_results();
    let result = spawn(
        "session-1",
        "worker",
        "/tmp/omo-project/(pane)",
        &deps(&recorder, false),
    );
    assert_eq!(result, SpawnPaneResult::spawned("%42".into()));
    assert_eq!(
        recorder.call(0).1[..8],
        strings(&[
            "split-window",
            "-h",
            "-d",
            "-P",
            "-F",
            "#{pane_id}",
            "-t",
            "%0"
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
    let recorder = spawn_results();
    spawn(
        "session-1",
        "worker with spaces",
        "/path with spaces/here",
        &deps(&recorder, false),
    );
    assert!(
        recorder
            .command(0)
            .contains("OMO subagent pane ready: worker with spaces")
    );
}

#[test]
fn empty_directory_keeps_placeholder_detached_from_attach() {
    let recorder = spawn_results();
    spawn("session-1", "worker", "", &deps(&recorder, false));
    assert!(!recorder.command(0).contains("--dir"));
}

#[test]
fn description_with_shell_metacharacters_escapes_placeholder() {
    let recorder = spawn_results();
    spawn(
        "session-1",
        r#"worker "$(whoami)""#,
        "/path/with'quote",
        &deps(&recorder, false),
    );
    assert!(recorder.command(0).contains(r#"\""#));
    assert!(recorder.command(0).contains(r"\$"));
}

// describe("cmux environment")

#[test]
fn cmux_compat_split_window_uses_attach_command_not_placeholder() {
    let recorder = spawn_results();
    spawn(
        "session-cmux-1",
        "worker",
        "/tmp/omo-project",
        &deps(&recorder, true),
    );
    let cmd = recorder.command(0);
    assert!(cmd.contains("opencode attach"));
    assert!(cmd.contains("--session 'session-cmux-1'"));
    assert!(cmd.contains("--dir"));
    assert!(!cmd.contains("Focus this pane to attach."));
    assert!(!cmd.contains("while :; do sleep 86400; done"));
}

#[test]
fn cmux_socket_without_tmux_reaches_eager_split_window_attach() {
    let recorder = spawn_results();
    let compatible = is_tmux_pane_compatible_environment(&*env(&[
        ("CMUX_SOCKET_PATH", "/tmp/cmux.sock"),
        ("TMUX_PANE", "%0"),
    ]));
    let deps = TmuxDeps {
        is_inside_tmux: Some(Arc::new(move || compatible)),
        ..deps(&recorder, true)
    };
    let result = spawn("session-cmux-only", "worker", "/tmp/omo-project", &deps);
    assert_eq!(result, SpawnPaneResult::spawned("%42".into()));
    assert!(recorder.command(0).contains("opencode attach"));
}

#[test]
fn cmux_compat_false_uses_placeholder() {
    let recorder = spawn_results();
    spawn("session-1", "worker", "/path", &deps(&recorder, false));
    let cmd = recorder.command(0);
    assert!(cmd.contains("Focus this pane to attach."));
    assert!(cmd.contains("while :; do sleep 86400; done"));
    assert!(!cmd.contains("opencode attach"));
}

#[test]
fn cmux_directory_with_spaces_is_shell_escaped() {
    let recorder = spawn_results();
    spawn(
        "session-1",
        "worker",
        "/home/user/my project/sub dir",
        &deps(&recorder, true),
    );
    let cmd = recorder.command(0);
    assert!(cmd.contains("opencode attach"));
    assert!(cmd.contains("/home/user/my"));
    assert!(cmd.contains("--dir '/home/user/my project/sub dir'"));
}

// describe("eligibility gate (headless cmux: CMUX_SOCKET_PATH without TMUX)")
// Headless deps leave tmux/cmux detection to the environment defaults.

fn headless_deps(recorder: &Recorder, pairs: &[(&str, &str)]) -> TmuxDeps {
    TmuxDeps {
        is_inside_tmux: None,
        is_cmux_compat_environment: None,
        environment: env(pairs),
        ..healthy_deps(recorder, "sh")
    }
}

#[test]
fn headless_cmux_socket_without_tmux_runs_attach_eagerly() {
    let recorder = spawn_results();
    let deps = headless_deps(
        &recorder,
        &[("CMUX_SOCKET_PATH", "/tmp/cmux-headless-test.sock")],
    );
    let result = spawn("session-headless", "worker", "/tmp/omo-project", &deps);
    assert_eq!(result, SpawnPaneResult::spawned("%42".into()));
    let cmd = recorder.command(0);
    assert!(cmd.contains("opencode attach"));
    assert!(!cmd.contains("Focus this pane to attach."));
}

#[test]
fn headless_without_tmux_or_cmux_skips_without_tmux_commands() {
    let recorder = spawn_results();
    let deps = headless_deps(&recorder, &[]);
    let result = spawn("session-none", "worker", "/tmp/omo-project", &deps);
    assert_eq!(result, SpawnPaneResult::failure());
    assert!(recorder.calls().is_empty());
}
