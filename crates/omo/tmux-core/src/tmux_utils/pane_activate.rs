//! Respawn a placeholder pane with the real `opencode attach` command.

use serde_json::json;

use crate::tmux_utils::deps::{ActivateTmuxPaneDeps, strings};
use crate::tmux_utils::pane_command::{
    build_pane_auth_environment_args_from, build_tmux_attach_command,
};

#[derive(Debug, Clone)]
pub struct ActivateTmuxPaneRequest<'a> {
    pub pane_id: &'a str,
    pub session_id: &'a str,
    pub server_url: &'a str,
    pub directory: &'a str,
}

pub fn activate_tmux_pane(
    request: &ActivateTmuxPaneRequest<'_>,
    deps: &ActivateTmuxPaneDeps,
) -> bool {
    let ids = json!({ "paneId": request.pane_id, "sessionId": request.session_id });
    if !deps.inside_tmux() {
        deps.log("[activateTmuxPane] SKIP: not inside tmux", Some(ids));
        return false;
    }

    let auth_env_args = build_pane_auth_environment_args_from(&*deps.environment);
    if deps.cmux_compat() && !auth_env_args.is_empty() {
        deps.log(
            "[activateTmuxPane] SKIP: authenticated cmux panes are unsupported",
            Some(ids),
        );
        return false;
    }

    let Some(tmux) = (deps.get_tmux_path)() else {
        deps.log("[activateTmuxPane] SKIP: tmux not found", Some(ids));
        return false;
    };

    let opencode_cmd = build_tmux_attach_command(
        request.server_url,
        request.session_id,
        Some(request.directory),
    );
    let mut args = strings(&["respawn-pane", "-k"]);
    args.extend(auth_env_args);
    args.extend(strings(&["-t", request.pane_id]));
    args.push(opencode_cmd);

    let result = deps.run(&tmux, &args);
    if result.exit_code != 0 {
        deps.log(
            "[activateTmuxPane] FAILED",
            Some(json!({
                "paneId": request.pane_id,
                "sessionId": request.session_id,
                "exitCode": result.exit_code,
                "stderr": result.stderr.trim(),
            })),
        );
        return false;
    }

    deps.log("[activateTmuxPane] SUCCESS", Some(ids));
    true
}
