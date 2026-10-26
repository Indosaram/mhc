//! Host warm-up forwards the caller's authentication root.
use serde_json::Value;
use std::path::Path;
pub const HOST_WARMUP_TASK_ID: &str = "host-warmup";

pub fn warm_task_host(
    socket: &str,
    cwd: &str,
    agent_dir: Option<&Path>,
    state_dir: &Path,
    ask: impl Fn(&str, Value, u64, Option<&Path>) -> Result<Option<Value>, String>,
    fallback: impl Fn(Value) -> Result<(), String>,
) -> Result<String, String> {
    let context = serde_json::json!({"role":"child","task_id":HOST_WARMUP_TASK_ID,"state_dir":state_dir,"host_warmup":"1"});
    let reply=ask(socket,serde_json::json!({"id":"omo-task-host-warm","type":"warm","cwd":cwd,"kind":"worker","context":context}),60_000,agent_dir)?.ok_or_else(||"host warm refused: no_answer".to_string())?;
    if reply["success"] == true
        && let Some(state @ ("warmed" | "already_warm")) = reply["data"]["state"].as_str() {
            return Ok(state.into());
        }
    if let Some(error) = reply["error"].as_str()
        && [
            "host_draining",
            "warm_failed",
            "invalid_session_kind",
            "invalid_session_context",
            "invalid_path",
        ]
        .iter()
        .any(|code| error == *code || error.starts_with(&format!("{code}:")))
        {
            return Err(format!("host warm refused: {error}"));
        }
    fallback(context)?;
    Ok("warm_up_session".into())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn explicit_agent_root_reaches_warm_request() {
        let root = std::path::Path::new("/custom-agent");
        let result = warm_task_host(
            "endpoint",
            "/cwd",
            Some(root),
            std::path::Path::new("/store"),
            |_, command, _, agent_dir| {
                assert_eq!(agent_dir, Some(root));
                assert_eq!(command["context"]["host_warmup"], "1");
                Ok(Some(
                    serde_json::json!({"success":true,"data":{"state":"warmed"}}),
                ))
            },
            |_| panic!("unexpected fallback"),
        );
        assert_eq!(result.unwrap(), "warmed");
    }
}
