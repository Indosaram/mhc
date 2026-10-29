//! Split a new pane next to the current one.

use serde_json::json;

use crate::tmux_utils::deps::{SpawnTmuxPaneDeps, strings};
use crate::tmux_utils::environment::SplitDirection;
use crate::tmux_utils::pane_command::{
    build_pane_auth_environment_args_from, build_tmux_attach_command,
    build_tmux_placeholder_command,
};
use crate::types::{SpawnPaneResult, TmuxConfig};

#[derive(Debug, Clone)]
pub struct SpawnTmuxPaneRequest<'a> {
    pub session_id: &'a str,
    pub description: &'a str,
    pub config: &'a TmuxConfig,
    pub server_url: &'a str,
    pub directory: &'a str,
    pub target_pane_id: Option<&'a str>,
    pub split_direction: SplitDirection,
}

const TAG: &str = "spawnTmuxPane";

pub fn spawn_tmux_pane(
    request: &SpawnTmuxPaneRequest<'_>,
    deps: &SpawnTmuxPaneDeps,
) -> SpawnPaneResult {
    deps.log(
        "[spawnTmuxPane] called",
        Some(json!({
            "sessionId": request.session_id,
            "description": request.description,
            "serverUrl": request.server_url,
            "configEnabled": request.config.enabled,
            "targetPaneId": request.target_pane_id,
            "splitDirection": request.split_direction.as_flag(),
        })),
    );

    if !request.config.enabled {
        deps.log("[spawnTmuxPane] SKIP: config.enabled is false", None);
        return SpawnPaneResult::failure();
    }
    if !deps.inside_tmux() && !deps.cmux_compat() {
        deps.log(
            "[spawnTmuxPane] SKIP: not inside tmux or cmux-compat environment",
            Some(json!({
                "TMUX": deps.environment.var("TMUX"),
                "CMUX_SOCKET_PATH": deps.environment.var("CMUX_SOCKET_PATH"),
            })),
        );
        return SpawnPaneResult::failure();
    }
    if !(deps.is_server_running)(request.server_url) {
        deps.log(
            "[spawnTmuxPane] SKIP: server not running",
            Some(json!({ "serverUrl": request.server_url })),
        );
        return SpawnPaneResult::failure();
    }
    let Some(tmux) = (deps.get_tmux_path)() else {
        deps.log("[spawnTmuxPane] SKIP: tmux not found", None);
        return SpawnPaneResult::failure();
    };

    deps.log("[spawnTmuxPane] all checks passed, spawning...", None);

    let auth_env_args = build_pane_auth_environment_args_from(&*deps.environment);
    let cmux = deps.cmux_compat();
    if cmux && !auth_env_args.is_empty() {
        deps.log(
            "[spawnTmuxPane] SKIP: authenticated cmux panes are unsupported",
            None,
        );
        return SpawnPaneResult::failure();
    }

    let initial_cmd = if cmux {
        build_tmux_attach_command(
            request.server_url,
            request.session_id,
            Some(request.directory),
        )
    } else {
        build_tmux_placeholder_command(request.description)
    };

    let mut args = strings(&[
        "split-window",
        request.split_direction.as_flag(),
        "-d",
        "-P",
        "-F",
        "#{pane_id}",
    ]);
    if let Some(target) = request.target_pane_id.filter(|target| !target.is_empty()) {
        args.extend(strings(&["-t", target]));
    }
    args.extend(auth_env_args);
    args.push(initial_cmd);

    let result = deps.run(&tmux, &args);
    let pane_id = result.output;
    if result.exit_code != 0 || pane_id.is_empty() {
        return SpawnPaneResult::failure();
    }

    deps.set_subagent_title(TAG, &tmux, &pane_id, request.description);
    SpawnPaneResult::spawned(pane_id)
}
