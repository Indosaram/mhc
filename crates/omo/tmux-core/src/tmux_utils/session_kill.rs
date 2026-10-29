//! Kill a tmux session when it exists.

use serde_json::json;

use crate::tmux_utils::deps::{KillTmuxSessionDeps, strings};

pub fn kill_tmux_session_if_exists(session_name: &str, deps: &KillTmuxSessionDeps) -> bool {
    let name = json!({ "sessionName": session_name });
    if !deps.inside_tmux() {
        deps.log(
            "[killTmuxSessionIfExists] SKIP: not inside tmux",
            Some(name),
        );
        return false;
    }
    let Some(tmux) = (deps.get_tmux_path)() else {
        deps.log("[killTmuxSessionIfExists] SKIP: tmux not found", Some(name));
        return false;
    };

    if deps
        .run(&tmux, &strings(&["has-session", "-t", session_name]))
        .exit_code
        != 0
    {
        deps.log(
            "[killTmuxSessionIfExists] SKIP: session not found",
            Some(name),
        );
        return false;
    }

    let result = deps.run(&tmux, &strings(&["kill-session", "-t", session_name]));
    if result.exit_code != 0 {
        deps.log(
            "[killTmuxSessionIfExists] FAILED",
            Some(json!({
                "sessionName": session_name,
                "exitCode": result.exit_code,
                "stderr": result.stderr.trim(),
            })),
        );
        return false;
    }

    deps.log("[killTmuxSessionIfExists] SUCCESS", Some(name));
    true
}
