use std::path::Path;

pub struct CommandResult {
    pub code: i32,
    pub stdout: String,
}

/// `mhc ulw-loop status` never reads the shared `.omo/ulw-loop` root implicitly; the hook reads
/// the same session-scoped plan the CLI writes for this session (`PI_SESSION_ID`).
pub async fn run_omo_status(cwd: &Path, session_id: Option<&str>) -> CommandResult {
    let Some(session_id) = ulw_loop_core::paths::normalize_ulw_loop_session_id(session_id) else {
        let existing = ulw_loop_core::plan_io::list_ulw_loop_session_ids(cwd);
        let val = serde_json::json!({
            "ok": false,
            "error": {
                "code": "ULW_LOOP_SESSION_SCOPE_REQUIRED",
                "message": ulw_loop_core::plan_missing_recovery::session_scope_required_message("--session-id", &existing),
            }
        });
        return CommandResult { code: 1, stdout: serde_json::to_string(&val).unwrap_or_default() };
    };
    let scope = ulw_loop_core::paths::UlwLoopScope::new(session_id);
    match ulw_loop_core::plan_io::read_ulw_loop_plan(cwd, Some(&scope)) {
        Ok(plan) => {
            let summary = ulw_loop_core::plan_crud::summarize_ulw_loop_plan(&plan);
            let val = serde_json::json!({
                "ok": true,
                "plan": plan,
                "summary": summary,
            });
            CommandResult {
                code: 0,
                stdout: serde_json::to_string(&val).unwrap_or_default(),
            }
        }
        Err(err) => {
            let val = serde_json::json!({
                "ok": false,
                "error": {
                    "code": err.code,
                    "message": err.message,
                }
            });
            CommandResult {
                code: 1,
                stdout: serde_json::to_string(&val).unwrap_or_default(),
            }
        }
    }
}
