use serde_json::{Map, Value};

use crate::internal::validate::{
    Node, ObjectSpec, PreprocessFn, any, enumeration, number, number_between, optional,
    positive_integer, record, required, strict_object, string, union,
};
use crate::issue::Issues;
use crate::schema::model_ref::{omo_model_ref_object_schema, omo_reasoning_schema};
use crate::schema::reasoning_vocabulary::{normalize_reasoning, split_reasoning_suffix};

pub fn omo_thinking_config_schema() -> Node {
    strict_object(vec![
        required("type", enumeration(&["enabled", "disabled"])),
        optional("budgetTokens", number()),
    ])
}

pub fn omo_reasoning_effort_schema() -> Node {
    omo_reasoning_schema()
}

fn canonical_reasoning(value: Option<&Value>) -> Option<String> {
    let Value::String(text) = value? else {
        return None;
    };
    let normalized = normalize_reasoning(text);
    normalized.level.or(normalized.passthrough)
}

fn is_model_token(text: &str) -> bool {
    let mut chars = text.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !first.is_ascii_alphabetic() {
        return false;
    }
    chars.all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-')
}

fn match_parenthesized(trimmed: &str) -> Option<(String, String)> {
    let mut cursor = trimmed.len();
    loop {
        let open = trimmed[..cursor].rfind('(')?;
        let rest = &trimmed[open + 1..];
        if let Some(close) = rest.find(')') {
            let inner = &rest[..close];
            let tail = &rest[close + 1..];
            if !inner.is_empty()
                && !inner.contains('(')
                && !inner.contains(')')
                && tail.trim().is_empty()
            {
                return Some((trimmed[..open].trim().to_string(), inner.trim().to_string()));
            }
        }
        if open == 0 {
            return None;
        }
        cursor = open;
    }
}

fn match_spaced(trimmed: &str) -> Option<(String, String)> {
    let chars: Vec<char> = trimmed.chars().collect();
    let mut cursor = chars.len();
    while cursor > 0 {
        let pos = (0..cursor)
            .rev()
            .find(|&index| chars[index].is_whitespace())?;
        let mut run_start = pos;
        while run_start > 0 && chars[run_start - 1].is_whitespace() {
            run_start -= 1;
        }
        let mut run_end = pos + 1;
        while run_end < chars.len() && chars[run_end].is_whitespace() {
            run_end += 1;
        }
        let head: String = chars[..run_start].iter().collect();
        let tail: String = chars[run_end..].iter().collect();
        let head_is_trimmed = head
            .chars()
            .last()
            .map(|ch| !ch.is_whitespace())
            .unwrap_or(false);
        if head_is_trimmed && is_model_token(&tail) {
            return Some((head.trim().to_string(), tail));
        }
        cursor = run_start;
    }
    None
}

pub fn canonical_model_string(model: &str) -> String {
    let colon = split_reasoning_suffix(model, Some(true));
    if let Some(level) = colon.level {
        return format!("{}:{}", colon.base, level);
    }

    let trimmed = model.trim();
    let candidate = match_parenthesized(trimmed).or_else(|| match_spaced(trimmed));
    let Some((base, token)) = candidate else {
        return trimmed.to_string();
    };
    match normalize_reasoning(&token).level {
        Some(level) => format!("{base}:{level}"),
        None => trimmed.to_string(),
    }
}

pub fn normalize_legacy_model_fields(value: &Value) -> Value {
    let Value::Object(entry) = value else {
        return value.clone();
    };

    let mut normalized = entry.clone();
    for key in [
        "variant",
        "reasoningEffort",
        "thinking",
        "textVerbosity",
        "providerOptions",
    ] {
        normalized.shift_remove(key);
    }

    if let Some(Value::String(model)) = entry.get("model") {
        normalized.insert("model".into(), Value::String(canonical_model_string(model)));
    }

    let explicit_reasoning = canonical_reasoning(entry.get("reasoning"));
    let variant = canonical_reasoning(entry.get("variant"));
    let reasoning_effort = canonical_reasoning(entry.get("reasoningEffort"));
    let thinking = entry.get("thinking").filter(|value| value.is_object());
    let thinking_disabled = thinking
        .map(|value| value.get("type").and_then(Value::as_str) == Some("disabled"))
        .unwrap_or(false);
    let reasoning = explicit_reasoning
        .or(reasoning_effort)
        .or(variant)
        .or_else(|| thinking_disabled.then(|| "off".to_string()));
    if let Some(reasoning) = reasoning {
        normalized.insert("reasoning".into(), Value::String(reasoning));
    }

    let mut provider_options: Map<String, Value> = match entry
        .get("provider_options")
        .filter(|value| value.is_object())
    {
        Some(value) => value.as_object().cloned().unwrap_or_default(),
        None => match entry
            .get("providerOptions")
            .filter(|value| value.is_object())
        {
            Some(value) => value.as_object().cloned().unwrap_or_default(),
            None => Map::new(),
        },
    };
    if let Some(thinking) = thinking
        && thinking.get("type").and_then(Value::as_str) == Some("enabled")
    {
        provider_options.insert("thinking".into(), thinking.clone());
    }
    if let Some(text_verbosity) = entry.get("textVerbosity") {
        provider_options.insert("textVerbosity".into(), text_verbosity.clone());
    }
    if !provider_options.is_empty() {
        normalized.insert("provider_options".into(), Value::Object(provider_options));
    }

    if let Some(max_tokens) = entry.get("max_tokens") {
        normalized.insert("max_tokens".into(), max_tokens.clone());
        if entry.get("maxTokens").is_none_or(Value::is_number) {
            normalized.shift_remove("maxTokens");
        }
    } else if let Some(max_tokens) = entry.get("maxTokens")
        && max_tokens.is_number()
    {
        normalized.insert("max_tokens".into(), max_tokens.clone());
        normalized.shift_remove("maxTokens");
    }

    Value::Object(normalized)
}

fn legacy_fallback_model_object_input_spec() -> ObjectSpec {
    ObjectSpec {
        fields: vec![
            required("model", string()),
            optional("reasoning", omo_reasoning_schema()),
            optional("temperature", number_between(0.0, 2.0)),
            optional("top_p", number_between(0.0, 1.0)),
            optional("max_tokens", positive_integer()),
            optional("provider_options", record(any())),
            optional("variant", string()),
            optional("reasoningEffort", omo_reasoning_effort_schema()),
            optional("thinking", omo_thinking_config_schema()),
            optional("textVerbosity", enumeration(&["low", "medium", "high"])),
            optional("maxTokens", number()),
            optional("providerOptions", record(any())),
        ],
        strict: true,
        preprocess: Some(normalize_legacy_model_fields as PreprocessFn),
        refine: None,
    }
}

pub fn omo_fallback_model_object_schema() -> Node {
    Node::Object(Box::new(legacy_fallback_model_object_input_spec()))
}

pub fn omo_fallback_model_entry_schema() -> Node {
    union(vec![string(), omo_fallback_model_object_schema()])
}

pub fn omo_fallback_models_schema() -> Node {
    union(vec![
        string(),
        crate::internal::validate::array(string()),
        crate::internal::validate::array(omo_fallback_model_object_schema()),
        crate::internal::validate::array(omo_fallback_model_entry_schema()),
    ])
}

pub fn normalize_legacy_model_entry(value: &Value) -> Result<Value, Issues> {
    let normalized = normalize_legacy_model_fields(value);
    crate::internal::validate::safe_parse(&omo_model_ref_object_schema(), &normalized)
}
