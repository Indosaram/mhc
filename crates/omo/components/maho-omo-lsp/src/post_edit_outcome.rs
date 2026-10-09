use lsp_core::post_edit::{PostEditDiagnosticsOutcome, PostEditNotInstalledOutcome};
use lsp_daemon::daemon_client::ToolExecutionResult;

pub fn post_edit_outcome_from_daemon_result(result: &ToolExecutionResult) -> PostEditDiagnosticsOutcome {
    let text = result.content.iter()
        .filter(|block| block.get("type").and_then(serde_json::Value::as_str) == Some("text"))
        .map(|block| block.get("text").and_then(serde_json::Value::as_str).unwrap_or_default())
        .collect::<Vec<_>>().join("\n");
    let Some(availability) = result.details.as_ref()
        .and_then(|details| details.get("availability"))
        .and_then(serde_json::Value::as_object) else {
        return PostEditDiagnosticsOutcome::Text(text);
    };
    match availability.get("kind").and_then(serde_json::Value::as_str) {
        Some("not_configured") => {
            if let Some(extension) = availability.get("extension").and_then(serde_json::Value::as_str) {
                return PostEditDiagnosticsOutcome::NotConfigured { extension: extension.into() };
            }
        }
        Some("not_installed") => {
            if let Some(server_id) = availability.get("serverId").and_then(serde_json::Value::as_str).filter(|id| !id.is_empty())
                && let Some(install_decision_tool) = availability.get("installDecisionTool").and_then(serde_json::Value::as_bool) {
                let decision = availability.get("decision").and_then(serde_json::Value::as_str)
                    .filter(|decision| matches!(*decision, "declined" | "allowed")).map(str::to_string);
                return PostEditDiagnosticsOutcome::NotInstalled(PostEditNotInstalledOutcome {
                    server_id: server_id.into(), install_decision_tool, decision, text,
                });
            }
        }
        _ => {}
    }
    PostEditDiagnosticsOutcome::Text(text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use lsp_core::post_edit::{classify_post_edit_file_location, PostEditFileLocationOptions};
    use crate::post_edit_diagnostics::{append_post_edit_diagnostics_with_locator, LspPostEditSessionState};
    use maho_ext_api::{ToolContent, ToolResultEvent};
    use serde_json::json;

    async fn append(
        root: &std::path::Path,
        agent: &std::path::Path,
        path: &str,
        session: &str,
        state: &mut LspPostEditSessionState,
        decision: Option<&str>,
    ) -> bool {
        let event = ToolResultEvent { tool_call_id: "write-test".into(), tool_name: "write".into(), input: json!({"path": path}), content: vec![ToolContent::text("written")], details: None, is_error: false, usage: None };
        let cache = state.get_or_create(Some(session));
        let cwd = root.to_string_lossy();
        let agent_dirs = vec![agent.to_string_lossy().into_owned()];
        let locate = |path: &str| classify_post_edit_file_location(path, &PostEditFileLocationOptions { cwd: &cwd, agent_dirs: &agent_dirs, temp_dirs: Some(&[]), home_dirs: None });
        append_post_edit_diagnostics_with_locator(&event, |path| {
            let server = if path.ends_with(".json") { "biome" } else { "typescript" };
            let outcome = post_edit_outcome_from_daemon_result(&ToolExecutionResult {
                content: vec![json!({"type":"text","text":"sentinel"})],
                details: Some(json!({"availability":{"kind":"not_installed","serverId":server,"installDecisionTool":false,"decision":decision}})),
                is_error: true,
            });
            async move { Ok(outcome) }
        }, Some(&cache), Some(&locate)).await.expect("processed").content.is_some()
    }

    #[tokio::test]
    async fn project_guidance_is_once_per_server_and_session_not_agent_config() {
        let root = tempfile::tempdir().expect("tempdir");
        std::fs::write(root.path().join("package.json"), "{}").expect("marker");
        let agent = root.path().join("agent");
        let mut state = LspPostEditSessionState::default();
        assert!(!append(root.path(), &agent, "agent/models.json", "a", &mut state, None).await);
        assert!(append(root.path(), &agent, "src/a.ts", "a", &mut state, None).await);
        assert!(!append(root.path(), &agent, "src/b.ts", "a", &mut state, None).await);
        assert!(append(root.path(), &agent, "tsconfig.json", "a", &mut state, None).await);
        assert!(append(root.path(), &agent, "src/c.ts", "b", &mut state, None).await);
    }

    #[tokio::test]
    async fn unmarked_scratch_file_does_not_append_guidance() {
        let root = tempfile::tempdir().expect("tempdir");
        assert!(!append(root.path(), &root.path().join("agent"), "scratch.ts", "a", &mut LspPostEditSessionState::default(), None).await);
    }

    #[tokio::test]
    async fn recorded_decline_keeps_guidance_on_each_edit() {
        let root = tempfile::tempdir().expect("tempdir");
        let mut state = LspPostEditSessionState::default();
        for path in ["a.ts", "b.ts"] {
            assert!(append(root.path(), &root.path().join("agent"), path, "a", &mut state, Some("declined")).await);
        }
    }
}
