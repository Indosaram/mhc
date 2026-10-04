use serde_json::Value;

fn js_string(value: &Value) -> String {
    match value {
        Value::String(value) => value.clone(),
        Value::Object(_) => "[object Object]".into(),
        Value::Array(values) => values.iter().map(|v| if v.is_null() { String::new() } else { js_string(v) }).collect::<Vec<_>>().join(","),
        value => value.to_string(),
    }
}
pub fn call_headers(args: &Value) -> String {
    args.get("questions").and_then(Value::as_array).map_or_else(|| "Question".into(), |questions| {
        questions.iter().map(|q| q.get("header").map_or_else(|| "[Question]".into(), |header| format!("[{}]",js_string(header)))).collect::<Vec<_>>().join(" ")
    })
}
pub fn wait_for_answer(args: &Value) -> bool {
    args.get("waitForAnswer").and_then(Value::as_bool) == Some(true) || args.get("wait_for_answer").and_then(Value::as_bool) == Some(true)
}
pub fn result_text(result: &Value) -> String {
    let mut lines = Vec::new();
    if let Some(details) = result.get("details") && let Some(status) = details.get("status") {
        let mut summary = js_string(status);
        if let Some(answers) = details.get("answers") {
            let count = answers.as_object().map(|v| v.len()).or_else(|| answers.as_array().map(Vec::len));
            if let Some(count) = count { summary.push_str(&format!("; {count} answered")); }
        }
        if let Some(unanswered) = details.get("unanswered").and_then(Value::as_array) { summary.push_str(&format!("; {} unanswered", unanswered.len())); }
        if !summary.is_empty() { lines.push(summary); }
    }
    for content in result.get("content").and_then(Value::as_array).into_iter().flatten() {
        if content.get("type").and_then(Value::as_str) == Some("text") && let Some(text) = content.get("text").and_then(Value::as_str).filter(|text| !text.is_empty()) { lines.push(text.into()); }
    }
    lines.join("\n")
}
pub fn supports_tool(tool_name: &str) -> bool { matches!(tool_name,"request_user_input"|"ask_user_question") }
pub fn renderers()->maho_ext_api::ToolRenderers<(),Value>{
    maho_ext_api::ToolRenderers{
        render_call:Some(std::sync::Arc::new(|args,theme,_|{
            let color=theme.colors.get("toolTitle").map_or("\x1b[39m",String::as_str);
            Box::new(maho_tui::components::text::Text::with_padding(format!("{color}{}\x1b[39m {}",call_headers(args),if wait_for_answer(args){"wait for answer"}else{"answer later"}),0,0))
        })),
        render_result:Some(std::sync::Arc::new(|result,_,_,_|{
            let value=serde_json::json!({"details":result.details,"content":result.content});
            Box::new(maho_tui::components::text::Text::with_padding(result_text(&value),0,0))
        })),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn wait_aliases_and_tool_dispatch_are_exact() {
        assert!(wait_for_answer(&json!({"wait_for_answer":true})));
        assert!(wait_for_answer(&json!({"waitForAnswer":true})));
        assert!(!wait_for_answer(&json!({"waitForAnswer":1})));
        assert!(supports_tool("request_user_input"));
        assert!(supports_tool("ask_user_question"));
        assert!(!supports_tool("other"));
    }
}
