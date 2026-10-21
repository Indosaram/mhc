use std::collections::HashSet;
use std::sync::{LazyLock, Mutex};

use serde_json::{Map, Value, json};

use crate::api::openai_responses_shared::{ConvertResponsesToolsOptions, convert_responses_tools};
use crate::openai_responses_compat::OpenAIResponsesCompat;
use crate::types::transcript::{get_current_tools, resolve_transcript_tools};
use crate::types::{Context, Model, TranscriptContext};
use crate::utils::deferred_tools::split_deferred_tools;
use crate::utils::tool_choice_fallback::HttpFailure;

type RefusalKey = (String, String, String, String);
static ALLOWED_TOOLS_REFUSALS: LazyLock<Mutex<HashSet<RefusalKey>>> =
    LazyLock::new(|| Mutex::new(HashSet::new()));

fn refusal_key(target: &Model) -> RefusalKey {
    (
        target.api.clone(),
        target.provider.clone(),
        target.base_url.clone(),
        target.id.clone(),
    )
}

pub fn has_refused_allowed_tools_choice(target: &Model) -> bool {
    ALLOWED_TOOLS_REFUSALS
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .contains(&refusal_key(target))
}

pub fn remember_allowed_tools_choice_refusal(target: &Model) {
    ALLOWED_TOOLS_REFUSALS
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .insert(refusal_key(target));
}

pub fn clear_allowed_tools_choice_refusals() {
    ALLOWED_TOOLS_REFUSALS
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .clear();
}

fn allowed_reference(tool: &Value) -> Value {
    match tool.get("type").and_then(Value::as_str) {
        Some("function" | "custom") => json!({"type": tool["type"], "name": tool["name"]}),
        Some("mcp") => json!({"type": "mcp", "server_label": tool["server_label"]}),
        _ => json!({"type": tool["type"]}),
    }
}

fn allowed_tools_choice(params: &Map<String, Value>) -> Option<&Vec<Value>> {
    let choice = params.get("tool_choice")?;
    if choice.get("type").and_then(Value::as_str) != Some("allowed_tools") {
        return None;
    }
    choice.get("tools")?.as_array()
}

pub fn is_allowed_tools_choice_refusal(
    error: &HttpFailure,
    params: &Map<String, Value>,
    before_content: bool,
) -> bool {
    if allowed_tools_choice(params).is_none() || (!before_content && error.status != Some(400)) {
        return false;
    }
    let names_allowed_tools = error
        .message
        .split(|character: char| !character.is_ascii_alphanumeric() && character != '_')
        .any(|word| word == "allowed_tools");
    let message = error.message.to_lowercase();
    let not_supported = message.match_indices("not").any(|(offset, _)| {
        let rest = &message[offset + 3..];
        let trimmed = rest.trim_start_matches(char::is_whitespace);
        trimmed.len() < rest.len() && trimmed.starts_with("supported")
    });
    names_allowed_tools
        && (message.contains("invalid") || message.contains("unsupported") || not_supported)
}

pub fn restrict_to_allowed_tools(params: &Map<String, Value>) -> Map<String, Value> {
    let Some(allowed) = allowed_tools_choice(params) else {
        return params.clone();
    };
    let allowed_names: HashSet<_> = allowed
        .iter()
        .filter_map(|tool| tool.get("name").and_then(Value::as_str))
        .collect();
    let tools = params
        .get("tools")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|tool| {
            !matches!(
                tool.get("type").and_then(Value::as_str),
                Some("function" | "custom")
            ) || tool
                .get("name")
                .and_then(Value::as_str)
                .is_some_and(|name| allowed_names.contains(name))
        })
        .cloned()
        .collect();
    let mut restricted = params.clone();
    restricted.insert("tools".into(), Value::Array(tools));
    restricted.remove("tool_choice");
    restricted
}

pub fn apply_allowed_tools_choice(
    params: &Map<String, Value>,
    context: &TranscriptContext,
    active_tool_names: Option<&[String]>,
    compat: &OpenAIResponsesCompat,
) -> Result<Map<String, Value>, String> {
    let Some(active_tool_names) = active_tool_names else {
        return Ok(params.clone());
    };
    if compat.supports_allowed_tools != Some(true) || params.contains_key("tool_choice") {
        return Ok(params.clone());
    }
    let active: HashSet<_> = active_tool_names.iter().map(String::as_str).collect();
    let declared_tools = get_current_tools(&context.messages);
    if declared_tools
        .iter()
        .all(|tool| active.contains(tool.name.as_str()))
    {
        return Ok(params.clone());
    }

    let mut allowed = Vec::new();
    let mut named_in_tools = HashSet::new();
    for tool in params
        .get("tools")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        if matches!(
            tool.get("type").and_then(Value::as_str),
            Some("function" | "custom")
        ) {
            let name = tool.get("name").and_then(Value::as_str).unwrap_or_default();
            named_in_tools.insert(name);
            if !active.contains(name) {
                continue;
            }
        }
        allowed.push(allowed_reference(tool));
    }

    let supports_additions =
        compat.supports_additional_tools == Some(true) || compat.supports_tool_search == Some(true);
    let placement = resolve_transcript_tools(&context.messages, supports_additions);
    let deferred = split_deferred_tools(
        &Context {
            messages: context.messages.clone(),
            tools: Some(declared_tools.clone()),
            ..Context::default()
        },
        supports_additions,
        None,
    )
    .deferred;
    let request_tool_names: HashSet<_> = placement
        .request_tools
        .iter()
        .filter(|tool| !deferred.contains_key(&tool.name))
        .map(|tool| tool.name.as_str())
        .collect();
    for tool in &declared_tools {
        if named_in_tools.contains(tool.name.as_str())
            || !active.contains(tool.name.as_str())
            || request_tool_names.contains(tool.name.as_str())
        {
            continue;
        }
        let converted = convert_responses_tools(
            std::slice::from_ref(tool),
            &ConvertResponsesToolsOptions {
                supports_strict_mode: compat.supports_strict_mode,
                supports_openai_grammar_tools: compat.supports_openai_grammar_tools,
                ..ConvertResponsesToolsOptions::default()
            },
        )?;
        if let Some(tool) = converted.first() {
            allowed.push(allowed_reference(tool));
        }
    }
    let choice = if allowed.is_empty() {
        json!("none")
    } else {
        json!({"type": "allowed_tools", "mode": "auto", "tools": allowed})
    };
    let mut result = params.clone();
    result.insert("tool_choice".into(), choice);
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::transcript::normalize_context;

    #[test]
    fn transcript_loaded_active_tools_are_referenced_by_name() {
        let context = normalize_context(
            &serde_json::from_value(json!({"messages": [
            {"role":"system", "content":"", "toolsAdded":[
                {"name":"b", "description":"", "parameters":{}}
            ], "timestamp":1}
        ], "tools":[{"name":"a", "description":"", "parameters":{}}]}))
            .expect("context"),
        );
        let params = json!({"tools":[{"type":"function", "name":"a"}]})
            .as_object()
            .expect("params")
            .clone();
        let compat = OpenAIResponsesCompat {
            supports_additional_tools: Some(true),
            ..compat()
        };
        let selected = apply_allowed_tools_choice(&params, &context, Some(&["b".into()]), &compat)
            .expect("choice");
        assert_eq!(
            selected["tool_choice"]["tools"],
            json!([{"type":"function", "name":"b"}])
        );
    }

    #[test]
    fn refusal_cache_is_partitioned_by_wire_identity() {
        let mut model = crate::models_generated::get_builtin_model("anthropic", "claude-opus-4-8")
            .expect("model")
            .clone();
        model.id = "allowed-tools-cache-partition-test".into();
        remember_allowed_tools_choice_refusal(&model);
        assert!(has_refused_allowed_tools_choice(&model));
        for field in ["api", "provider", "baseUrl", "id"] {
            let mut other = model.clone();
            match field {
                "api" => other.api.push_str("-other"),
                "provider" => other.provider.push_str("-other"),
                "baseUrl" => other.base_url.push_str("/other"),
                "id" => other.id.push_str("-other"),
                _ => unreachable!(),
            }
            assert!(!has_refused_allowed_tools_choice(&other));
        }
    }

    fn context() -> TranscriptContext {
        normalize_context(
            &serde_json::from_value(json!({"messages": [], "tools": [
                {"name":"a", "description":"", "parameters":{}},
                {"name":"b", "description":"", "parameters":{}}
            ]}))
            .expect("context"),
        )
    }

    fn params() -> Map<String, Value> {
        json!({"tools": [
            {"type":"function", "name":"a", "parameters":{}},
            {"type":"custom", "name":"b", "format":{}},
            {"type":"web_search"}, {"type":"mcp", "server_label":"remote"}
        ]})
        .as_object()
        .expect("params")
        .clone()
    }

    fn compat() -> OpenAIResponsesCompat {
        OpenAIResponsesCompat {
            supports_allowed_tools: Some(true),
            ..Default::default()
        }
    }

    #[test]
    fn active_subset_preserves_declarations_and_hosted_tools() {
        let original = params();
        let selected =
            apply_allowed_tools_choice(&original, &context(), Some(&["a".into()]), &compat())
                .expect("choice");
        assert_eq!(selected["tools"], original["tools"]);
        assert_eq!(
            selected["tool_choice"],
            json!({"type":"allowed_tools", "mode":"auto", "tools":[
                {"type":"function", "name":"a"}, {"type":"web_search"}, {"type":"mcp", "server_label":"remote"}
            ]})
        );
    }

    #[test]
    fn empty_subset_forbids_calls_without_hosted_tools() {
        let params = json!({"tools":[{"type":"function", "name":"a"}]})
            .as_object()
            .expect("params")
            .clone();
        let selected =
            apply_allowed_tools_choice(&params, &context(), Some(&[]), &compat()).expect("choice");
        assert_eq!(selected["tool_choice"], "none");
    }

    #[test]
    fn explicit_choice_wins_over_active_subset() {
        let mut params = params();
        params.insert("tool_choice".into(), json!("required"));
        assert_eq!(
            apply_allowed_tools_choice(&params, &context(), Some(&[]), &compat()).expect("choice"),
            params
        );
    }

    #[test]
    fn payload_removed_tools_are_not_referenced() {
        let params = json!({"tools": []}).as_object().expect("params").clone();
        let selected =
            apply_allowed_tools_choice(&params, &context(), Some(&["a".into()]), &compat())
                .expect("choice");
        assert_eq!(selected["tool_choice"], "none");
    }

    #[test]
    fn fallback_removes_only_inactive_top_level_callable_tools() {
        let selected =
            apply_allowed_tools_choice(&params(), &context(), Some(&["a".into()]), &compat())
                .expect("choice");
        let restricted = restrict_to_allowed_tools(&selected);
        assert!(!restricted.contains_key("tool_choice"));
        assert_eq!(
            restricted["tools"]
                .as_array()
                .expect("tools")
                .iter()
                .map(|tool| tool["type"].as_str().expect("type"))
                .collect::<Vec<_>>(),
            vec!["function", "web_search", "mcp"]
        );
    }

    #[test]
    fn refusal_requires_allowed_choice_and_request_rejection() {
        let selected = apply_allowed_tools_choice(&params(), &context(), Some(&[]), &compat())
            .expect("choice");
        let error = HttpFailure {
            status: Some(400),
            message: "Invalid value: 'allowed_tools'".into(),
        };
        assert!(is_allowed_tools_choice_refusal(&error, &selected, false));
        assert!(!is_allowed_tools_choice_refusal(&error, &params(), false));
        let transport = HttpFailure {
            status: None,
            ..error
        };
        assert!(!is_allowed_tools_choice_refusal(
            &transport, &selected, false
        ));
        assert!(is_allowed_tools_choice_refusal(&transport, &selected, true));
    }
}
