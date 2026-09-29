//! Port of src/tmux-utils/pane-auth-env.test.ts

mod common;

use common::{Recorder, enabled_config, env, failed, healthy_deps, strings, success};
use pretty_assertions::assert_eq;
use tmux_core::{
    ActivateTmuxPaneRequest, ReplaceTmuxPaneRequest, SpawnTmuxPaneRequest, SpawnTmuxSessionRequest,
    SpawnTmuxWindowRequest, SplitDirection, TmuxCommandResult, TmuxDeps, activate_tmux_pane,
    replace_tmux_pane, spawn_tmux_pane, spawn_tmux_session, spawn_tmux_window,
};

const NATIVE_TMUX: (&str, &str) = ("TMUX", "/tmp/tmux-1000/default,1234,0");

fn auth_deps(results: Vec<TmuxCommandResult>) -> (Recorder, TmuxDeps) {
    let recorder = Recorder::new(results);
    let deps = TmuxDeps {
        environment: env(&[
            NATIVE_TMUX,
            ("OPENCODE_SERVER_PASSWORD", "pa ss word"),
            ("OPENCODE_SERVER_USERNAME", "user name"),
        ]),
        ..healthy_deps(&recorder, "tmux")
    };
    (recorder, deps)
}

fn expect_auth_env_args(args: &[String]) {
    assert!(args.contains(&"-e".to_owned()));
    assert!(args.contains(&"OPENCODE_SERVER_PASSWORD=pa ss word".to_owned()));
    assert!(args.contains(&"OPENCODE_SERVER_USERNAME=user name".to_owned()));
}

fn expect_no_auth_env_args(args: &[String]) {
    assert!(!args.contains(&"-e".to_owned()));
    assert!(
        !args
            .iter()
            .any(|arg| arg.starts_with("OPENCODE_SERVER_PASSWORD="))
    );
    assert!(
        !args
            .iter()
            .any(|arg| arg.starts_with("OPENCODE_SERVER_USERNAME="))
    );
}

#[test]
fn auth_env_with_spaces_activate_respawns_attach_with_env_args_and_quoted_values() {
    // given
    let (recorder, deps) = auth_deps(vec![success("")]);

    // when
    activate_tmux_pane(
        &ActivateTmuxPaneRequest {
            pane_id: "%42",
            session_id: "session with spaces",
            server_url: "http://127.0.0.1:4321/path with spaces",
            directory: "/tmp/project with spaces",
        },
        &deps,
    );

    // then
    let args = recorder.call(0).1;
    assert_eq!(args[..2], strings(&["respawn-pane", "-k"]));
    expect_auth_env_args(&args);
    assert_eq!(
        args.last().unwrap(),
        r#"/bin/sh -c "opencode attach 'http://127.0.0.1:4321/path with spaces' --session 'session with spaces' --dir '/tmp/project with spaces'""#
    );
}

#[test]
fn auth_env_every_lifecycle_call_site_receives_env_args() {
    // given
    let config = enabled_config();
    let (pane, pane_deps) = auth_deps(vec![success("%pane"), success("")]);
    let (window, window_deps) = auth_deps(vec![success("%window"), success("")]);
    let (session, session_deps) = auth_deps(vec![
        success("120,40"),
        failed(),
        success("%session"),
        success(""),
    ]);
    let (existing, existing_deps) = auth_deps(vec![failed(), success("%existing"), success("")]);
    let (replace, replace_deps) = auth_deps(vec![success(""), success(""), success("")]);
    let session_request = |source_pane_id| SpawnTmuxSessionRequest {
        session_id: "session-1",
        description: "worker",
        config: &config,
        server_url: "http://127.0.0.1:4321",
        directory: "/tmp/project",
        source_pane_id,
        manager_id: None,
    };

    // when
    spawn_tmux_pane(
        &SpawnTmuxPaneRequest {
            session_id: "session-1",
            description: "worker",
            config: &config,
            server_url: "http://127.0.0.1:4321",
            directory: "/tmp/project",
            target_pane_id: Some("%target"),
            split_direction: SplitDirection::Horizontal,
        },
        &pane_deps,
    );
    spawn_tmux_window(
        &SpawnTmuxWindowRequest {
            session_id: "session-1",
            description: "worker",
            config: &config,
            server_url: "http://127.0.0.1:4321",
            directory: "/tmp/project",
        },
        &window_deps,
    );
    spawn_tmux_session(&session_request(Some("%source")), &session_deps);
    spawn_tmux_session(&session_request(None), &existing_deps);
    replace_tmux_pane(
        &ReplaceTmuxPaneRequest {
            pane_id: "%42",
            session_id: "session-1",
            description: "worker",
            config: &config,
            server_url: "http://127.0.0.1:4321",
            directory: "/tmp/project",
        },
        &replace_deps,
    );

    // then
    expect_auth_env_args(&pane.call(0).1);
    expect_auth_env_args(&window.call(0).1);
    expect_auth_env_args(&session.call(2).1);
    expect_auth_env_args(&existing.call(1).1);
    expect_auth_env_args(&replace.call(1).1);
}

#[test]
fn auth_env_unset_pane_spawn_receives_no_auth_env_args() {
    // given
    let recorder = Recorder::new(vec![success("%pane"), success("")]);
    let deps = TmuxDeps {
        environment: env(&[NATIVE_TMUX]),
        ..healthy_deps(&recorder, "tmux")
    };
    let config = enabled_config();

    // when
    spawn_tmux_pane(
        &SpawnTmuxPaneRequest {
            session_id: "session-1",
            description: "worker",
            config: &config,
            server_url: "http://127.0.0.1:4321",
            directory: "/tmp/project",
            target_pane_id: None,
            split_direction: SplitDirection::Horizontal,
        },
        &deps,
    );

    // then
    expect_no_auth_env_args(&recorder.call(0).1);
}
