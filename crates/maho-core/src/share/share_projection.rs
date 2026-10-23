//! Port of senpi packages/coding-agent/src/core/share/share-projection.ts.
//!
//! What `/share` uploads by default: the conversation, never what tools returned. Tool results, `!`
//! command output, extension messages, extension state and data, and context-edit replacements are
//! replaced by a one-line note unless the user explicitly includes tool outputs. The projection
//! trusts nothing: every output-bearing record is rewritten on every pass.

use serde_json::{Map, Value, json};

/// Counts the outputs a pass replaced; the caller keeps the largest count across passes.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct OutputOmissions {
    pub count: usize,
}

pub fn new_output_omissions() -> OutputOmissions {
    OutputOmissions::default()
}

const OMITTED_RESULT_HTML: [&str; 2] = ["resultHtmlCollapsed", "resultHtmlExpanded"];

fn note(label: &str) -> Value {
    json!({"type": "text", "text": format!("[output omitted: {label}]")})
}

fn string_field<'a>(record: &'a Map<String, Value>, key: &str) -> Option<&'a str> {
    record.get(key).and_then(Value::as_str)
}

fn project_record(record: &Map<String, Value>, omissions: &mut OutputOmissions) -> Option<Value> {
    let role = string_field(record, "role");
    let kind = string_field(record, "type");
    let mut next = record.clone();
    if role == Some("toolResult") {
        omissions.count += 1;
        let tool_name = string_field(record, "toolName").unwrap_or("tool");
        next.insert("content".to_owned(), Value::Array(vec![note(tool_name)]));
        next.remove("details");
        return Some(Value::Object(next));
    }
    if role == Some("bashExecution") {
        omissions.count += 1;
        next.insert(
            "output".to_owned(),
            Value::String("[output omitted: command]".to_owned()),
        );
        next.remove("fullOutputPath");
        return Some(Value::Object(next));
    }
    if role == Some("custom") || kind == Some("custom_message") {
        omissions.count += 1;
        let custom_type = string_field(record, "customType").unwrap_or("extension");
        next.insert("content".to_owned(), Value::Array(vec![note(custom_type)]));
        next.remove("details");
        return Some(Value::Object(next));
    }
    if kind == Some("custom") {
        next.remove("data");
        return Some(Value::Object(next));
    }
    if kind == Some("compaction") {
        next.remove("details");
        next.remove("systemMessage");
        return Some(Value::Object(next));
    }
    if kind == Some("branch_summary") {
        next.remove("details");
        return Some(Value::Object(next));
    }
    if kind == Some("context_edit") && record.get("replacement").is_some_and(Value::is_object) {
        omissions.count += 1;
        next.insert(
            "replacement".to_owned(),
            json!({"content": [note("context edit")]}),
        );
        return Some(Value::Object(next));
    }
    None
}

fn project_rendered_tools(value: &Value) -> Value {
    let Some(tools) = value.as_object() else {
        return value.clone();
    };
    Value::Object(
        tools
            .iter()
            .map(|(id, rendered)| {
                let kept = match rendered.as_object() {
                    Some(fields) => Value::Object(
                        fields
                            .iter()
                            .filter(|(key, _)| !OMITTED_RESULT_HTML.contains(&key.as_str()))
                            .map(|(k, v)| (k.clone(), v.clone()))
                            .collect(),
                    ),
                    None => rendered.clone(),
                };
                (id.clone(), kept)
            })
            .collect(),
    )
}

/// Rewrites every output-bearing record in `value`, counting each omitted output.
pub fn omit_tool_outputs(value: &Value, omissions: &mut OutputOmissions) -> Value {
    match value {
        Value::Array(items) => Value::Array(
            items
                .iter()
                .map(|item| omit_tool_outputs(item, omissions))
                .collect(),
        ),
        Value::Object(record) => {
            if let Some(projected) = project_record(record, omissions) {
                return projected;
            }
            let mut out = Map::new();
            for (key, child) in record {
                let next = if key == "renderedTools" {
                    project_rendered_tools(child)
                } else {
                    omit_tool_outputs(child, omissions)
                };
                out.insert(key.clone(), next);
            }
            Value::Object(out)
        }
        other => other.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_results_are_replaced_and_counted() {
        let mut omissions = new_output_omissions();
        let value = json!([{"role": "toolResult", "toolName": "read", "content": [{"type": "text", "text": "secret"}], "details": {"x": 1}}]);
        let out = omit_tool_outputs(&value, &mut omissions);
        assert_eq!(omissions.count, 1);
        assert_eq!(out[0]["content"][0]["text"], "[output omitted: read]");
        assert!(out[0].get("details").is_none());
    }

    #[test]
    fn look_alike_notes_are_rewritten_too() {
        let mut omissions = new_output_omissions();
        let value = json!({"role": "toolResult", "content": [{"type": "text", "text": "[output omitted: tool]"}]});
        omit_tool_outputs(&value, &mut omissions);
        assert_eq!(omissions.count, 1);
    }

    #[test]
    fn bash_custom_and_context_edit_records_lose_their_payload() {
        let mut omissions = new_output_omissions();
        let bash = omit_tool_outputs(
            &json!({"role": "bashExecution", "output": "x", "fullOutputPath": "/tmp/a"}),
            &mut omissions,
        );
        assert_eq!(bash["output"], "[output omitted: command]");
        assert!(bash.get("fullOutputPath").is_none());
        let custom = omit_tool_outputs(
            &json!({"type": "custom_message", "customType": "memo", "content": "hi", "details": 1}),
            &mut omissions,
        );
        assert_eq!(custom["content"][0]["text"], "[output omitted: memo]");
        let edit = omit_tool_outputs(
            &json!({"type": "context_edit", "replacement": {"content": "leak"}}),
            &mut omissions,
        );
        assert_eq!(
            edit["replacement"]["content"][0]["text"],
            "[output omitted: context edit]"
        );
        assert_eq!(omissions.count, 3);
    }

    #[test]
    fn state_records_lose_details_without_counting() {
        let mut omissions = new_output_omissions();
        let custom = omit_tool_outputs(
            &json!({"type": "custom", "customType": "s", "data": {"k": 1}}),
            &mut omissions,
        );
        assert!(custom.get("data").is_none());
        let compaction = omit_tool_outputs(
            &json!({"type": "compaction", "summary": "s", "details": 1, "systemMessage": "m"}),
            &mut omissions,
        );
        assert!(compaction.get("details").is_none() && compaction.get("systemMessage").is_none());
        let branch = omit_tool_outputs(
            &json!({"type": "branch_summary", "summary": "s", "details": 1}),
            &mut omissions,
        );
        assert!(branch.get("details").is_none());
        assert_eq!(omissions.count, 0);
    }

    #[test]
    fn rendered_tools_lose_result_html() {
        let mut omissions = new_output_omissions();
        let out = omit_tool_outputs(
            &json!({"renderedTools": {"a": {"callHtml": "c", "resultHtmlCollapsed": "r", "resultHtmlExpanded": "e"}}}),
            &mut omissions,
        );
        assert_eq!(out["renderedTools"]["a"], json!({"callHtml": "c"}));
    }
}
