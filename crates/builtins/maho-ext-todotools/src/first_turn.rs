// Copyright (c) 2025 Mario Zechner; Copyright (c) 2025-2026 Can Bölük.
// Adapted from oh-my-pi's MIT-licensed todo tool via senpi.
use serde_json::Value;

pub const FIRST_TURN_CUSTOM_TYPE: &str = "senpi.todo-first-turn";
pub const FIRST_TURN_REMINDER: &str = "<system-reminder>\nThis is the first request of the session. Before continuing, call the todo tool with op \"init\" and a phased list covering the whole request end to end, with only the phases this request needs - a question is one task, and its reply is the answer itself, with no handoff block. Give every task a unique 5-10 word label; phase names are short noun phrases. Then continue the request in this same turn - the init is not a turn by itself.\n</system-reminder>";
const SESSION_CONTROL_DELIVERY_TYPE: &str = "session_control_delivery";

pub struct FirstTurnInput<'a> {
    pub preview: bool,
    pub trigger: &'a str,
    pub prompt: &'a str,
    pub branch_entries: &'a [Value],
    pub todo_active: bool,
    pub setting: &'a str,
    pub mode: maho_ext_api::ExtensionMode,
}

fn is_trailing_closer(character: char) -> bool {
    character.is_whitespace()
        || matches!(character, '"' | '\'' | '`' | '\u{2018}' | '\u{2019}' | '\u{201C}' | '\u{201D}' | ')' | ']' | '}')
}

/// Arms only for the session's first work request: no preview, extension turn, question,
/// exclamation, ask-user answer frame, or print/json run, and no earlier request on the branch.
pub fn should_arm_first_turn(input: &FirstTurnInput<'_>) -> bool {
    if input.preview
        || (input.trigger != "prompt" && input.trigger != "delivery")
        || input.setting == "off"
        || !input.todo_active
    {
        return false;
    }
    if matches!(input.mode, maho_ext_api::ExtensionMode::Print | maho_ext_api::ExtensionMode::Json) {
        return false;
    }
    if crate::todo_storage::is_ask_user_answer_frame(input.prompt) {
        return false;
    }
    let request = input.prompt.trim().trim_end_matches(is_trailing_closer);
    if request.is_empty() || request.ends_with(['?', '!', '\u{FF1F}', '\u{FF01}']) {
        return false;
    }
    !input.branch_entries.iter().any(is_request_entry)
}

fn is_request_entry(entry: &Value) -> bool {
    let content = if entry.get("type").and_then(Value::as_str) == Some("message")
        && entry.pointer("/message/role").and_then(Value::as_str) == Some("user")
    {
        entry.pointer("/message/content")
    } else if is_admitted_delivery_entry(entry) {
        entry.get("content")
    } else {
        None
    };
    let Some(content) = content else { return false };
    crate::todo_storage::first_text_block(content).is_none_or(|text| !crate::todo_storage::is_ask_user_answer_frame(text))
}

fn is_admitted_delivery_entry(entry: &Value) -> bool {
    if entry.get("type").and_then(Value::as_str) != Some("custom_message")
        || entry.get("customType").and_then(Value::as_str) != Some(SESSION_CONTROL_DELIVERY_TYPE)
    {
        return false;
    }
    let Some(details) = entry.get("details") else { return false };
    details.get("delivery_id").is_some_and(Value::is_string)
        && details.get("source").and_then(Value::as_str) == Some("session_control")
        && matches!(details.get("deliverAs").and_then(Value::as_str), Some("steer" | "followUp"))
}

pub fn supports_named_tool_choice(model: Option<&maho_ext_api::Model>) -> bool {
    let Some(model) = model else { return false };
    if maho_ai::utils::tool_choice_fallback::has_refused_forced_tool_choice(model) {
        return false;
    }
    match model.api.as_str() {
        "anthropic-messages" => {
            let compat = maho_ai::utils::prompt_cache_ttl::get_anthropic_compat(model);
            compat.supports_tool_choice && compat.supports_forced_tool_choice
        }
        "openai-responses" => model.compat.as_ref().is_none_or(|compat| compat.openai_responses().supports_forced_tool_choice != Some(false)),
        "openai-completions" => model.compat.as_ref().is_none_or(|compat| compat.openai_completions().supports_forced_tool_choice != Some(false)),
        _ => false,
    }
}

pub fn named_tool_choice_payload(api: &str, tool_name: &str) -> Option<Value> {
    match api {
        "anthropic-messages" => Some(serde_json::json!({"type":"tool","name":tool_name})),
        "openai-responses" => Some(serde_json::json!({"type":"function","name":tool_name})),
        "openai-completions" => Some(serde_json::json!({"type":"function","function":{"name":tool_name}})),
        _ => None,
    }
}

fn declares_tool(tools: Option<&Value>, tool_name: &str) -> bool {
    tools.and_then(Value::as_array).is_some_and(|tools| {
        tools.iter().any(|tool| {
            tool.get("name").and_then(Value::as_str) == Some(tool_name)
                || tool.pointer("/function/name").and_then(Value::as_str) == Some(tool_name)
        })
    })
}

/// Anthropic rejects any forced tool use while extended thinking runs.
pub fn with_forced_todo_choice(payload: &Value, model: Option<&maho_ext_api::Model>) -> Option<Value> {
    if !supports_named_tool_choice(model) {
        return None;
    }
    let api = model.map(|model| model.api.as_str()).unwrap_or_default();
    let tool_choice = named_tool_choice_payload(api, "todo")?;
    let object = payload.as_object()?;
    if object.contains_key("tool_choice") || !declares_tool(object.get("tools"), "todo") {
        return None;
    }
    let thinking = object.get("thinking").and_then(|thinking| thinking.get("type")).and_then(Value::as_str);
    if api == "anthropic-messages" && matches!(thinking, Some("enabled" | "adaptive")) {
        return None;
    }
    let mut forced = object.clone();
    forced.insert("tool_choice".into(), tool_choice);
    Some(Value::Object(forced))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn input<'a>(prompt: &'a str, branch: &'a [Value]) -> FirstTurnInput<'a> {
        FirstTurnInput { preview: false, trigger: "prompt", prompt, branch_entries: branch, todo_active: true, setting: "force", mode: maho_ext_api::ExtensionMode::Tui }
    }
    #[test]
    fn arms_only_for_the_first_work_request() {
        assert!(should_arm_first_turn(&input("fix the parser", &[])));
        assert!(should_arm_first_turn(&input("고쳐라", &[])));
        assert!(!should_arm_first_turn(&input("why does it fail?", &[])));
        assert!(!should_arm_first_turn(&input("do it now!\")", &[])));
        assert!(!should_arm_first_turn(&input("이거 왜이럼？", &[])));
        assert!(!should_arm_first_turn(&input("[Answer to question q1]\nyes", &[])));
        assert!(!should_arm_first_turn(&FirstTurnInput { mode: maho_ext_api::ExtensionMode::Print, ..input("fix", &[]) }));
        assert!(!should_arm_first_turn(&FirstTurnInput { setting: "off", ..input("fix", &[]) }));
        assert!(!should_arm_first_turn(&FirstTurnInput { trigger: "extension", ..input("fix", &[]) }));
        assert!(!should_arm_first_turn(&FirstTurnInput { todo_active: false, ..input("fix", &[]) }));
        let earlier = [json!({"type":"message","message":{"role":"user","content":"first"}})];
        assert!(!should_arm_first_turn(&input("second", &earlier)));
        let answer = [json!({"type":"message","message":{"role":"user","content":[{"type":"text","text":"[Answer to question q]\nok"}]}})];
        assert!(should_arm_first_turn(&input("fix", &answer)));
        let delivery = [json!({"type":"custom_message","customType":"session_control_delivery","content":"go","details":{"delivery_id":"d","source":"session_control","deliverAs":"followUp"}})];
        assert!(!should_arm_first_turn(&input("fix", &delivery)));
    }
    #[test]
    fn forces_todo_only_when_the_wire_allows_it() {
        let model = |api: &str| -> maho_ext_api::Model { serde_json::from_value(json!({"id":"m","name":"m","api":api,"provider":"p","baseUrl":"","reasoning":false,"input":["text"],"cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0},"contextWindow":1,"maxTokens":1})).unwrap() };
        let payload = json!({"tools":[{"name":"todo"}]});
        assert_eq!(with_forced_todo_choice(&payload, Some(&model("anthropic-messages"))).unwrap()["tool_choice"], json!({"type":"tool","name":"todo"}));
        let openai = json!({"tools":[{"type":"function","function":{"name":"todo"}}]});
        assert_eq!(with_forced_todo_choice(&openai, Some(&model("openai-completions"))).unwrap()["tool_choice"], json!({"type":"function","function":{"name":"todo"}}));
        assert!(with_forced_todo_choice(&json!({"tools":[{"name":"read"}]}), Some(&model("anthropic-messages"))).is_none());
        assert!(with_forced_todo_choice(&json!({"tools":[{"name":"todo"}],"tool_choice":"auto"}), Some(&model("anthropic-messages"))).is_none());
        assert!(with_forced_todo_choice(&json!({"tools":[{"name":"todo"}],"thinking":{"type":"adaptive"}}), Some(&model("anthropic-messages"))).is_none());
        assert!(with_forced_todo_choice(&payload, Some(&model("google-generative-ai"))).is_none());
        assert!(with_forced_todo_choice(&payload, None).is_none());
    }
}
