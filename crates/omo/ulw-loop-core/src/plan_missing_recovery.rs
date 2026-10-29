//! Missing plan error messages and recovery instructions.

use std::collections::BTreeMap;
use crate::runtime::UlwLoopError;
use crate::surface::UlwLoopToolkitSurface;

pub const ULW_LOOP_CREATE_GOALS_COMMAND: &str = "mhc ulw-loop create-goals --brief \"<brief>\" --json";

/// Native divergence: upstream's omo-senpi surface names the JS `agentToolkit` SDK, which the Rust
/// product does not ship; both surfaces point at the native `mhc ulw-loop` CLI instead.
pub fn create_goals_action(_surface: &UlwLoopToolkitSurface) -> String {
    ULW_LOOP_CREATE_GOALS_COMMAND.to_string()
}

pub struct PlanMissingRecovery {
    pub message: String,
    pub existing_session_ids: Vec<String>,
}

pub fn plan_missing_recovery(
    existing_session_ids: &[String],
    surface: UlwLoopToolkitSurface,
) -> PlanMissingRecovery {
    let mut lines = vec![format!("Recovery: bootstrap the plan with `{}`.", create_goals_action(&surface))];
    if !existing_session_ids.is_empty() {
        let joined = existing_session_ids.join(", ");
        if surface == UlwLoopToolkitSurface::OmoSenpi {
            lines.push(format!(
                "Existing ulw-loop session ids under .omo/ulw-loop/: {joined}. `mhc ulw-loop` run from this session's bash is bound to the current session (PI_SESSION_ID); resume the owning session to target its plan."
            ));
        } else {
            lines.push(format!(
                "Existing ulw-loop session ids under .omo/ulw-loop/: {joined}. Re-run with `--session-id <id>` to target one of them."
            ));
        }
    }
    PlanMissingRecovery {
        message: lines.join("\n"),
        existing_session_ids: existing_session_ids.to_vec(),
    }
}

pub fn plan_missing_error(
    plan_path: &str,
    existing_session_ids: &[String],
    surface: Option<UlwLoopToolkitSurface>,
) -> UlwLoopError {
    let s = surface.unwrap_or(UlwLoopToolkitSurface::LazyCodex);
    let recovery = plan_missing_recovery(existing_session_ids, s);
    let mut details = BTreeMap::new();
    if !recovery.existing_session_ids.is_empty() {
        details.insert(
            "existingSessionIds".to_string(),
            serde_json::json!(recovery.existing_session_ids),
        );
    }
    UlwLoopError::with_details(
        format!("No ulw-loop plan found at {plan_path}.\n{}", recovery.message),
        "ULW_LOOP_PLAN_MISSING",
        details,
    )
}

pub fn session_scope_required_message(flag: &str, existing_session_ids: &[String]) -> String {
    let mut lines = vec![
        "No ulw-loop session scope: neither the session env (OMO_ULW_LOOP_SESSION_ID / CODEX_SESSION_ID / CODEX_THREAD_ID / PI_SESSION_ID) nor the flag names this run, and the shared .omo/ulw-loop root is never used implicitly because every session in this directory would read and overwrite it.".to_string(),
        format!("Recovery: pass the scope explicitly: `{flag} <id>` (subprocess, eval, and hook contexts do not inherit the session env)."),
    ];
    if !existing_session_ids.is_empty() {
        lines.push(format!(
            "Existing ulw-loop session ids under .omo/ulw-loop/: {}.",
            existing_session_ids.join(", ")
        ));
    }
    lines.join("\n")
}

pub fn session_id_required_message(flag: &str) -> String {
    [
        format!("{flag} requires a non-empty value."),
        "Recovery: subprocess, eval, and hook contexts do not inherit the ulw-loop session env (OMO_ULW_LOOP_SESSION_ID / CODEX_SESSION_ID / CODEX_THREAD_ID / PI_SESSION_ID),".to_string(),
        format!("so pass the scope explicitly: `{flag} <id>` (for example `{flag} 01a05b8a-6763-7780-834f-319423b071ce`)."),
    ].join("\n")
}
