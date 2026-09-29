//! Port of src/tmux-utils/pane-auth-cmux.test.ts

mod common;

use std::sync::Arc;

use common::{Recorder, env, healthy_deps};
use pretty_assertions::assert_eq;
use tmux_core::{
    ActivateTmuxPaneRequest, ReplaceTmuxPaneRequest, SpawnPaneResult, SpawnTmuxPaneRequest,
    SpawnTmuxSessionRequest, SpawnTmuxWindowRequest, SplitDirection, TmuxConfig, TmuxDeps,
    TmuxIsolation, TmuxLayout, activate_tmux_pane, replace_tmux_pane, spawn_tmux_pane,
    spawn_tmux_session, spawn_tmux_window,
};

const USERNAME: &str = "$(touch /tmp/omo-cmux-user-injected)";
const PASSWORD: &str = "$(touch /tmp/omo-cmux-password-injected)";

fn config() -> TmuxConfig {
    TmuxConfig {
        enabled: true,
        isolation: TmuxIsolation::Inline,
        layout: TmuxLayout::MainVertical,
        main_pane_size: 60,
        main_pane_min_width: 80,
        agent_pane_min_width: 40,
    }
}

/// Authenticated cmux environment; `fake_tmux` adds a cmuxterm TMUX value.
fn cmux_deps(recorder: &Recorder, fake_tmux: bool) -> TmuxDeps {
    let mut pairs = vec![
        ("CMUX_SOCKET_PATH", "/tmp/cmux.sock"),
        ("OPENCODE_SERVER_USERNAME", USERNAME),
        ("OPENCODE_SERVER_PASSWORD", PASSWORD),
    ];
    if fake_tmux {
        pairs.push(("TMUX", "/tmp/cmuxterm-test.sock,1234,0"));
    }
    TmuxDeps {
        environment: env(&pairs),
        ..healthy_deps(recorder, "cmux")
    }
}

#[test]
fn authenticated_cmux_spawning_pane_fails_before_credentials_reach_runner() {
    let recorder = Recorder::new(Vec::new());
    let deps = TmuxDeps {
        is_cmux_compat_environment: Some(Arc::new(|| true)),
        ..cmux_deps(&recorder, false)
    };
    let config = config();
    let result = spawn_tmux_pane(
        &SpawnTmuxPaneRequest {
            session_id: "session-cmux-auth",
            description: "worker",
            config: &config,
            server_url: "http://127.0.0.1:4096",
            directory: "/tmp/project",
            target_pane_id: Some("%0"),
            split_direction: SplitDirection::Horizontal,
        },
        &deps,
    );
    assert_eq!(result, SpawnPaneResult::failure());
    assert!(recorder.calls().is_empty());
}

#[test]
fn authenticated_cmux_activating_pane_fails_before_credentials_reach_runner() {
    let recorder = Recorder::new(Vec::new());
    let result = activate_tmux_pane(
        &ActivateTmuxPaneRequest {
            pane_id: "%42",
            session_id: "session-cmux-auth",
            server_url: "http://127.0.0.1:4096",
            directory: "/tmp/project",
        },
        &cmux_deps(&recorder, false),
    );
    assert!(!result);
    assert!(recorder.calls().is_empty());
}

#[test]
fn authenticated_cmux_replacing_pane_fails_before_credentials_reach_runner() {
    let recorder = Recorder::new(Vec::new());
    let config = config();
    let result = replace_tmux_pane(
        &ReplaceTmuxPaneRequest {
            pane_id: "%42",
            session_id: "session-cmux-auth",
            description: "worker",
            config: &config,
            server_url: "http://127.0.0.1:4096",
            directory: "/tmp/project",
        },
        &cmux_deps(&recorder, false),
    );
    assert_eq!(result, SpawnPaneResult::failure());
    assert!(recorder.calls().is_empty());
}

#[test]
fn authenticated_cmux_with_fake_tmux_spawning_window_fails_before_credentials_reach_runner() {
    let recorder = Recorder::new(Vec::new());
    let config = config();
    let result = spawn_tmux_window(
        &SpawnTmuxWindowRequest {
            session_id: "session-cmux-auth",
            description: "worker",
            config: &config,
            server_url: "http://127.0.0.1:4096",
            directory: "/tmp/project",
        },
        &cmux_deps(&recorder, true),
    );
    assert_eq!(result, SpawnPaneResult::failure());
    assert!(recorder.calls().is_empty());
}

#[test]
fn authenticated_cmux_with_fake_tmux_spawning_session_fails_before_credentials_reach_runner() {
    let recorder = Recorder::new(Vec::new());
    let config = config();
    let result = spawn_tmux_session(
        &SpawnTmuxSessionRequest {
            session_id: "session-cmux-auth",
            description: "worker",
            config: &config,
            server_url: "http://127.0.0.1:4096",
            directory: "/tmp/project",
            source_pane_id: Some("%0"),
            manager_id: None,
        },
        &cmux_deps(&recorder, true),
    );
    assert_eq!(result, SpawnPaneResult::failure());
    assert!(recorder.calls().is_empty());
}
