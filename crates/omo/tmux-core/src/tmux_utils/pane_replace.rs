//! Respawn an existing pane with a fresh placeholder.

use serde_json::json;

use crate::tmux_utils::deps::{ReplaceTmuxPaneDeps, strings};
use crate::tmux_utils::pane_command::{
    build_pane_auth_environment_args_from, build_tmux_placeholder_command,
};
use crate::types::{SpawnPaneResult, TmuxConfig};

#[derive(Debug, Clone)]
pub struct ReplaceTmuxPaneRequest<'a> {
    pub pane_id: &'a str,
    pub session_id: &'a str,
    pub description: &'a str,
    pub config: &'a TmuxConfig,
    /// Unused (kept for signature parity with the TypeScript original).
    pub server_url: &'a str,
    /// Unused (kept for signature parity with the TypeScript original).
    pub directory: &'a str,
}

pub fn replace_tmux_pane(
    request: &ReplaceTmuxPaneRequest<'_>,
    deps: &ReplaceTmuxPaneDeps,
) -> SpawnPaneResult {
    let pane_id = request.pane_id;
    deps.log(
        "[replaceTmuxPane] called",
        Some(json!({
            "paneId": pane_id,
            "sessionId": request.session_id,
            "description": request.description,
        })),
    );

    if !request.config.enabled || !deps.inside_tmux() {
        return SpawnPaneResult::failure();
    }

    let auth_env_args = build_pane_auth_environment_args_from(&*deps.environment);
    if deps.cmux_compat() && !auth_env_args.is_empty() {
        deps.log(
            "[replaceTmuxPane] SKIP: authenticated cmux panes are unsupported",
            Some(json!({ "paneId": pane_id, "sessionId": request.session_id })),
        );
        return SpawnPaneResult::failure();
    }

    let Some(tmux) = (deps.get_tmux_path)() else {
        return SpawnPaneResult::failure();
    };

    deps.log(
        "[replaceTmuxPane] sending Ctrl+C for graceful shutdown",
        Some(json!({ "paneId": pane_id })),
    );
    deps.run(&tmux, &strings(&["send-keys", "-t", pane_id, "C-c"]));

    let mut args = strings(&["respawn-pane", "-k"]);
    args.extend(auth_env_args);
    args.extend(strings(&["-t", pane_id]));
    args.push(build_tmux_placeholder_command(request.description));

    let result = deps.run(&tmux, &args);
    if result.exit_code != 0 {
        deps.log(
            "[replaceTmuxPane] FAILED",
            Some(json!({
                "paneId": pane_id,
                "exitCode": result.exit_code,
                "stderr": result.stderr.trim(),
            })),
        );
        return SpawnPaneResult::failure();
    }

    deps.set_subagent_title("replaceTmuxPane", &tmux, pane_id, request.description);
    deps.log(
        "[replaceTmuxPane] SUCCESS",
        Some(json!({ "paneId": pane_id, "sessionId": request.session_id })),
    );
    SpawnPaneResult::spawned(pane_id.to_owned())
}
