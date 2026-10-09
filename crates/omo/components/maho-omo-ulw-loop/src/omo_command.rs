use std::path::Path;

pub struct CommandResult {
    pub code: i32,
    pub stdout: String,
}

pub async fn run_omo_status(cwd: &Path) -> CommandResult {
    match ulw_loop_core::plan_io::read_ulw_loop_plan(cwd, None) {
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
