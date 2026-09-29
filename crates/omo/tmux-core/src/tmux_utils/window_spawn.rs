//! Spawn a pane in a dedicated `omo-agents` window.

use serde_json::json;

use crate::tmux_utils::deps::{SpawnTmuxWindowDeps, strings};
use crate::tmux_utils::pane_command::{
    build_pane_auth_environment_args_from, build_tmux_placeholder_command,
};
use crate::types::{SpawnPaneResult, TmuxConfig};

const ISOLATED_WINDOW_NAME: &str = "omo-agents";

#[derive(Debug, Clone)]
pub struct SpawnTmuxWindowRequest<'a> {
    pub session_id: &'a str,
    pub description: &'a str,
    pub config: &'a TmuxConfig,
    pub server_url: &'a str,
    /// Unused (kept for signature parity with the TypeScript original).
    pub directory: &'a str,
}

pub fn spawn_tmux_window(
    request: &SpawnTmuxWindowRequest<'_>,
    deps: &SpawnTmuxWindowDeps,
) -> SpawnPaneResult {
    deps.log(
        "[spawnTmuxWindow] called",
        Some(json!({
            "sessionId": request.session_id,
            "description": request.description,
            "serverUrl": request.server_url,
            "configEnabled": request.config.enabled,
        })),
    );

    if !request.config.enabled {
        deps.log("[spawnTmuxWindow] SKIP: config.enabled is false", None);
        return SpawnPaneResult::failure();
    }
    if !deps.inside_tmux() {
        deps.log(
            "[spawnTmuxWindow] SKIP: not inside tmux",
            Some(json!({ "TMUX": deps.environment.var("TMUX") })),
        );
        return SpawnPaneResult::failure();
    }
    if !(deps.is_server_running)(request.server_url) {
        deps.log(
            "[spawnTmuxWindow] SKIP: server not running",
            Some(json!({ "serverUrl": request.server_url })),
        );
        return SpawnPaneResult::failure();
    }

    let auth_env_args = build_pane_auth_environment_args_from(&*deps.environment);
    if deps.cmux_compat() && !auth_env_args.is_empty() {
        deps.log(
            "[spawnTmuxWindow] SKIP: authenticated cmux windows are unsupported",
            None,
        );
        return SpawnPaneResult::failure();
    }

    let Some(tmux) = (deps.get_tmux_path)() else {
        deps.log("[spawnTmuxWindow] SKIP: tmux not found", None);
        return SpawnPaneResult::failure();
    };

    deps.log(
        "[spawnTmuxWindow] all checks passed, creating isolated window...",
        None,
    );

    let mut args = strings(&[
        "new-window",
        "-d",
        "-n",
        ISOLATED_WINDOW_NAME,
        "-P",
        "-F",
        "#{pane_id}",
    ]);
    args.extend(auth_env_args);
    args.push(build_tmux_placeholder_command(request.description));

    let result = deps.run(&tmux, &args);
    let pane_id = result.output;
    if result.exit_code != 0 || pane_id.is_empty() {
        deps.log(
            "[spawnTmuxWindow] FAILED",
            Some(json!({ "exitCode": result.exit_code, "stderr": result.stderr.trim() })),
        );
        return SpawnPaneResult::failure();
    }

    deps.set_subagent_title("spawnTmuxWindow", &tmux, &pane_id, request.description);
    deps.log(
        "[spawnTmuxWindow] SUCCESS",
        Some(json!({ "paneId": pane_id, "windowName": ISOLATED_WINDOW_NAME })),
    );
    SpawnPaneResult::spawned(pane_id)
}
