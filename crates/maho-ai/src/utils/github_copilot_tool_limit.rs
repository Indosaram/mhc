use serde_json::{Value, json};

use crate::types::AssistantMessage;
use crate::utils::diagnostics::{
    AssistantMessageDiagnostic, append_assistant_message_diagnostic, now_ms,
};

pub const GITHUB_COPILOT_TOOL_LIMIT: usize = 128;
pub const GITHUB_COPILOT_TOOL_LIMIT_DIAGNOSTIC: &str = "github_copilot_tool_limit";

fn tool_name(tool: &Value) -> Option<&str> {
    if tool.get("type").and_then(Value::as_str) == Some("function") {
        tool.get("function")
            .and_then(|function| function.get("name"))
            .and_then(Value::as_str)
            .or_else(|| tool.get("name").and_then(Value::as_str))
    } else {
        tool.get("name").and_then(Value::as_str)
    }
}

pub fn limit_github_copilot_tools(
    provider: &str,
    tools: &mut Vec<Value>,
    choice: Option<&Value>,
) -> usize {
    if provider != "github-copilot" || tools.len() <= GITHUB_COPILOT_TOOL_LIMIT {
        return 0;
    }
    let omitted = tools.len() - GITHUB_COPILOT_TOOL_LIMIT;
    let forced = choice
        .filter(|choice| {
            matches!(
                choice.get("type").and_then(Value::as_str),
                Some("function" | "custom" | "tool")
            )
        })
        .and_then(tool_name);
    if let Some(index) =
        forced.and_then(|name| tools.iter().position(|tool| tool_name(tool) == Some(name)))
        && index >= GITHUB_COPILOT_TOOL_LIMIT
    {
        tools[GITHUB_COPILOT_TOOL_LIMIT - 1] = tools[index].clone();
    }
    tools.truncate(GITHUB_COPILOT_TOOL_LIMIT);
    omitted
}

pub fn record_github_copilot_tool_limit(message: &mut AssistantMessage, omitted_count: usize) {
    if omitted_count == 0 || has_limit(message) {
        return;
    }
    append_assistant_message_diagnostic(&mut message.diagnostics, AssistantMessageDiagnostic {
        kind: GITHUB_COPILOT_TOOL_LIMIT_DIAGNOSTIC.into(), timestamp: now_ms(), error: None,
        details: json!({"limit": GITHUB_COPILOT_TOOL_LIMIT, "omittedCount": omitted_count,
            "message": format!("GitHub Copilot accepts at most {GITHUB_COPILOT_TOOL_LIMIT} tools on endpoints without tool search; senpi omitted {omitted_count} excess tool definition{}.", if omitted_count == 1 { "" } else { "s" })})
            .as_object().cloned(),
    });
}

fn has_limit(message: &AssistantMessage) -> bool {
    message.diagnostics.as_ref().is_some_and(|items| {
        items
            .iter()
            .any(|item| item.kind == GITHUB_COPILOT_TOOL_LIMIT_DIAGNOSTIC)
    })
}

pub fn format_github_copilot_tool_limit_error(message: &AssistantMessage, error: String) -> String {
    if message.provider != "github-copilot"
        || !has_limit(message)
        || message
            .provider_diagnostic
            .as_ref()
            .and_then(|diagnostic| diagnostic.http_status)
            != Some(400)
        || !error.to_ascii_lowercase().ends_with("400 bad request")
    {
        return error;
    }
    format!(
        "GitHub Copilot rejected the request after senpi limited its tool list to {GITHUB_COPILOT_TOOL_LIMIT}. The endpoint may enforce a lower limit or count additional hosted tools. ({error})"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forced_tool_survives_truncation() {
        let mut tools: Vec<Value> = (0..130)
            .map(|index| json!({"name": format!("tool{index}")}))
            .collect();
        let omitted = limit_github_copilot_tools(
            "github-copilot",
            &mut tools,
            Some(&json!({"type":"function","function":{"name":"tool129"}})),
        );
        assert_eq!(omitted, 2);
        assert_eq!(tools.len(), 128);
        assert_eq!(tools[126]["name"], "tool126");
        assert_eq!(tools[127]["name"], "tool129");
    }

    #[test]
    fn other_providers_keep_all_tools() {
        let mut tools = vec![json!({"name":"tool"}); 129];
        assert_eq!(limit_github_copilot_tools("anthropic", &mut tools, None), 0);
        assert_eq!(tools.len(), 129);
    }
}
