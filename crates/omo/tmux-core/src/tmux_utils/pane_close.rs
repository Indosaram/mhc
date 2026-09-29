//! Gracefully close a pane: Ctrl+C, short delay, then kill-pane.

use serde_json::json;

use crate::tmux_utils::deps::{CloseTmuxPaneDependencies, TmuxDeps, strings};

const GRACEFUL_SHUTDOWN_DELAY_MS: u64 = 250;

/// [`close_tmux_pane_with_dependencies`] with default dependencies.
#[must_use]
pub fn close_tmux_pane(pane_id: &str) -> bool {
    close_tmux_pane_with_dependencies(pane_id, &TmuxDeps::default())
}

pub fn close_tmux_pane_with_dependencies(
    pane_id: &str,
    dependencies: &CloseTmuxPaneDependencies,
) -> bool {
    if !dependencies.inside_tmux() {
        dependencies.log("[closeTmuxPane] SKIP: not inside tmux", None);
        return false;
    }
    let Some(tmux) = (dependencies.get_tmux_path)() else {
        dependencies.log("[closeTmuxPane] SKIP: tmux not found", None);
        return false;
    };

    dependencies.log(
        "[closeTmuxPane] sending Ctrl+C for graceful shutdown",
        Some(json!({ "paneId": pane_id })),
    );
    dependencies.run(&tmux, &strings(&["send-keys", "-t", pane_id, "C-c"]));

    (dependencies.delay)(GRACEFUL_SHUTDOWN_DELAY_MS);

    dependencies.log(
        "[closeTmuxPane] killing pane",
        Some(json!({ "paneId": pane_id })),
    );
    let result = dependencies.run(&tmux, &strings(&["kill-pane", "-t", pane_id]));
    let trimmed_stderr = result.stderr.trim();
    let pane_already_gone =
        result.exit_code != 0 && trimmed_stderr.to_lowercase().contains("can't find pane");

    if pane_already_gone {
        dependencies.log(
            "[closeTmuxPane] SUCCESS (pane already closed by Ctrl+C)",
            Some(json!({ "paneId": pane_id })),
        );
        return true;
    }
    if result.exit_code != 0 {
        dependencies.log(
            "[closeTmuxPane] FAILED",
            Some(json!({
                "paneId": pane_id,
                "exitCode": result.exit_code,
                "stderr": trimmed_stderr,
            })),
        );
        return false;
    }

    dependencies.log(
        "[closeTmuxPane] SUCCESS",
        Some(json!({ "paneId": pane_id })),
    );
    true
}
