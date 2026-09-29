//! Spawn a pane in an isolated `omo-agents-<pid>` session.

use serde_json::json;

use crate::tmux_utils::deps::{SpawnTmuxSessionDeps, TmuxDeps, strings};
use crate::tmux_utils::pane_command::{
    build_pane_auth_environment_args_from, build_tmux_placeholder_command,
};
use crate::tmux_utils::pane_dimensions::parse_number_pair;
use crate::types::{SpawnPaneResult, TmuxConfig};

const ISOLATED_SESSION_NAME_PREFIX: &str = "omo-agents";

#[derive(Debug, Clone)]
pub struct SpawnTmuxSessionRequest<'a> {
    pub session_id: &'a str,
    pub description: &'a str,
    pub config: &'a TmuxConfig,
    pub server_url: &'a str,
    /// Unused (kept for signature parity with the TypeScript original).
    pub directory: &'a str,
    pub source_pane_id: Option<&'a str>,
    pub manager_id: Option<&'a str>,
}

/// `omo-agents-<pid>` or `omo-agents-<pid>-<managerId>`; `pid` defaults to this process.
#[must_use]
pub fn get_isolated_session_name(pid: Option<u32>, manager_id: Option<&str>) -> String {
    let pid = pid.unwrap_or_else(std::process::id);
    match manager_id.filter(|id| !id.is_empty()) {
        Some(manager_id) => format!("{ISOLATED_SESSION_NAME_PREFIX}-{pid}-{manager_id}"),
        None => format!("{ISOLATED_SESSION_NAME_PREFIX}-{pid}"),
    }
}

fn window_size_args(deps: &TmuxDeps, tmux: &str, source_pane_id: &str) -> Vec<String> {
    let result = deps.run(
        tmux,
        &strings(&[
            "display",
            "-p",
            "-t",
            source_pane_id,
            "#{window_width},#{window_height}",
        ]),
    );
    if result.exit_code != 0 {
        return Vec::new();
    }
    parse_number_pair(&result.output).map_or_else(Vec::new, |(width, height)| {
        vec![
            "-x".to_owned(),
            width.to_string(),
            "-y".to_owned(),
            height.to_string(),
        ]
    })
}

pub fn spawn_tmux_session(
    request: &SpawnTmuxSessionRequest<'_>,
    deps: &SpawnTmuxSessionDeps,
) -> SpawnPaneResult {
    deps.log(
        "[spawnTmuxSession] called",
        Some(json!({
            "sessionId": request.session_id,
            "description": request.description,
            "serverUrl": request.server_url,
            "configEnabled": request.config.enabled,
        })),
    );

    if !request.config.enabled {
        deps.log("[spawnTmuxSession] SKIP: config.enabled is false", None);
        return SpawnPaneResult::failure();
    }
    if !deps.inside_tmux() {
        deps.log(
            "[spawnTmuxSession] SKIP: not inside tmux",
            Some(json!({ "TMUX": deps.environment.var("TMUX") })),
        );
        return SpawnPaneResult::failure();
    }
    if !(deps.is_server_running)(request.server_url) {
        deps.log(
            "[spawnTmuxSession] SKIP: server not running",
            Some(json!({ "serverUrl": request.server_url })),
        );
        return SpawnPaneResult::failure();
    }

    let auth_env_args = build_pane_auth_environment_args_from(&*deps.environment);
    if deps.cmux_compat() && !auth_env_args.is_empty() {
        deps.log(
            "[spawnTmuxSession] SKIP: authenticated cmux sessions are unsupported",
            None,
        );
        return SpawnPaneResult::failure();
    }

    let Some(tmux) = (deps.get_tmux_path)() else {
        deps.log("[spawnTmuxSession] SKIP: tmux not found", None);
        return SpawnPaneResult::failure();
    };

    deps.log(
        "[spawnTmuxSession] all checks passed, creating isolated session...",
        None,
    );

    let placeholder_cmd = build_tmux_placeholder_command(request.description);
    let size_args = request
        .source_pane_id
        .filter(|pane| !pane.is_empty())
        .map_or_else(Vec::new, |pane| window_size_args(deps, &tmux, pane));

    let session_name = get_isolated_session_name(None, request.manager_id);
    let session_already_exists = deps
        .run(&tmux, &strings(&["has-session", "-t", &session_name]))
        .exit_code
        == 0;

    let mut args = if session_already_exists {
        strings(&["new-window", "-t", &session_name])
    } else {
        let mut args = strings(&["new-session", "-d", "-s", &session_name]);
        args.extend(size_args);
        args
    };
    args.extend(strings(&["-P", "-F", "#{pane_id}"]));
    args.extend(auth_env_args);
    args.push(placeholder_cmd);

    deps.log(
        "[spawnTmuxSession] spawning",
        Some(json!({
            "mode": if session_already_exists { "new-window" } else { "new-session" },
            "sessionName": session_name,
        })),
    );

    let result = deps.run(&tmux, &args);
    let pane_id = result.output;
    if result.exit_code != 0 || pane_id.is_empty() {
        deps.log(
            "[spawnTmuxSession] FAILED",
            Some(json!({ "exitCode": result.exit_code, "stderr": result.stderr.trim() })),
        );
        return SpawnPaneResult::failure();
    }

    deps.set_subagent_title("spawnTmuxSession", &tmux, &pane_id, request.description);
    deps.log(
        "[spawnTmuxSession] SUCCESS",
        Some(json!({ "paneId": pane_id, "sessionName": session_name })),
    );
    SpawnPaneResult::spawned(pane_id)
}
