//! Validation & "did you mean" field guard for thread tool parameters (`thread/contracts/parse.ts`).

use serde::de::DeserializeOwned;
use serde_json::{Map, Value};

use super::super::errors::thread_tool_failure;
use super::results::ThreadDataError;

const MEANT: &[(&str, &[&str])] = &[
    ("message", &["prompt"]),
    ("prompt", &["message"]),
    ("text", &["message", "prompt"]),
    ("body", &["message", "prompt"]),
    ("content", &["message", "prompt"]),
    ("brief", &["prompt", "message"]),
    ("task", &["prompt", "message"]),
    ("instructions", &["prompt", "message"]),
    ("target", &["thread"]),
    ("thread_id", &["thread"]),
    ("session", &["thread"]),
    ("session_id", &["thread"]),
    ("id", &["thread"]),
    ("title", &["name"]),
    ("mode", &["delivery"]),
    ("level", &["reasoning"]),
    ("thinking", &["reasoning", "level"]),
    ("thinking_level", &["reasoning", "level"]),
    ("directory", &["cwd"]),
    ("folder", &["cwd"]),
    ("key", &["idempotency_key"]),
];

fn validate_against_schema(schema: &Value, input: &Value) -> Result<(), String> {
    let Some(obj) = input.as_object() else {
        return Err("expected an object".to_owned());
    };
    if let Some(req) = schema.get("required").and_then(Value::as_array) {
        for r in req {
            if let Some(key) = r.as_str()
                && obj.get(key).is_none_or(|v| v.is_null())
            {
                return Err(format!("missing required field \"{key}\""));
            }
        }
    }
    if let Some(props) = schema.get("properties").and_then(Value::as_object) {
        for (k, v) in obj {
            if v.is_null() {
                continue;
            }
            if let Some(prop_schema) = props.get(k) {
                validate_field(k, v, prop_schema)?;
            }
        }
    }
    Ok(())
}

fn validate_field(field: &str, val: &Value, schema: &Value) -> Result<(), String> {
    if let Some(expected_type) = schema.get("type").and_then(Value::as_str) {
        match expected_type {
            "string" => {
                let Some(s) = val.as_str() else {
                    return Err(format!("/{field}: expected string"));
                };
                if let Some(min) = schema.get("minLength").and_then(Value::as_u64)
                    && (s.chars().count() as u64) < min
                {
                    return Err(format!("/{field}: string shorter than minLength {min}"));
                }
                if let Some(max) = schema.get("maxLength").and_then(Value::as_u64)
                    && (s.len() as u64) > max
                {
                    return Err(format!("/{field}: exceeds maxLength {max}"));
                }
                if let Some(enums) = schema.get("enum").and_then(Value::as_array)
                    && !enums.iter().any(|e| e.as_str() == Some(s))
                {
                    return Err(format!("/{field}: invalid enum value \"{s}\""));
                }
            }
            "integer" => {
                let Some(n) = val.as_i64() else {
                    return Err(format!("/{field}: expected integer"));
                };
                if let Some(min) = schema.get("minimum").and_then(Value::as_i64)
                    && n < min
                {
                    return Err(format!("/{field}: must be >= {min}"));
                }
                if let Some(max) = schema.get("maximum").and_then(Value::as_i64)
                    && n > max
                {
                    return Err(format!("/{field}: must be <= {max}"));
                }
            }
            "boolean" if !val.is_boolean() => {
                return Err(format!("/{field}: expected boolean"));
            }
            "boolean" => {}
            "array" => {
                let Some(arr) = val.as_array() else {
                    return Err(format!("/{field}: expected array"));
                };
                if let Some(min) = schema.get("minItems").and_then(Value::as_u64)
                    && (arr.len() as u64) < min
                {
                    return Err(format!("/{field}: expected at least {min} item(s)"));
                }
                if let Some(max) = schema.get("maxItems").and_then(Value::as_u64)
                    && (arr.len() as u64) > max
                {
                    return Err(format!("/{field}: expected at most {max} item(s)"));
                }
                if let Some(item_schema) = schema.get("items") {
                    for (idx, item) in arr.iter().enumerate() {
                        validate_field(&format!("{field}/{idx}"), item, item_schema)?;
                    }
                }
            }
            "object" => {
                let Some(sub) = val.as_object() else {
                    return Err(format!("/{field}: expected object"));
                };
                if schema.get("additionalProperties").and_then(Value::as_bool) == Some(false)
                    && let Some(sub_props) = schema.get("properties").and_then(Value::as_object)
                {
                    for sk in sub.keys() {
                        if !sub_props.contains_key(sk) {
                            return Err(format!("/{field}/{sk}: unknown property"));
                        }
                    }
                }
                validate_against_schema(schema, val)?;
            }
            _ => {}
        }
    }
    Ok(())
}

/// Validates raw tool parameters, flagging unknown top-level keys with `MEANT` suggestions before deserializing.
#[allow(clippy::result_large_err)]
pub fn parse_thread_params<T: DeserializeOwned>(
    schema: &Value,
    input: &Value,
) -> Result<T, ThreadDataError> {
    if let Some(obj) = input.as_object()
        && let Some(props) = schema.get("properties").and_then(Value::as_object)
    {
        let known: Vec<&str> = props.keys().map(String::as_str).collect();
        if let Some((unknown, _)) = obj
            .iter()
            .find(|(k, v)| !v.is_null() && !props.contains_key(k.as_str()))
        {
            let suggestion = MEANT
                .iter()
                .find(|(alias, _)| *alias == unknown.as_str())
                .and_then(|(_, candidates)| {
                    candidates.iter().find(|c| props.contains_key(**c)).copied()
                });
            let mut details = Map::new();
            details.insert("field".into(), Value::String(unknown.clone()));
            details.insert(
                "accepted".into(),
                Value::Array(
                    known
                        .iter()
                        .map(|s| Value::String((*s).to_owned()))
                        .collect(),
                ),
            );
            let msg = match suggestion {
                Some(s) => format!(
                    "This tool takes no field \"{unknown}\"; did you mean \"{s}\"? Nothing was done."
                ),
                None => format!("This tool takes no field \"{unknown}\". Nothing was done."),
            };
            let next = match suggestion {
                Some(s) => format!("Call the tool again with \"{s}\" in place of \"{unknown}\"."),
                None => format!(
                    "Call the tool again without \"{unknown}\"; it takes: {}.",
                    known.join(", ")
                ),
            };
            if let Some(s) = suggestion {
                details.insert("suggestion".into(), Value::String(s.to_owned()));
            }
            return Err(ThreadDataError::of(thread_tool_failure(
                "invalid_arguments",
                msg,
                next,
                Some(Value::Object(details)),
            )));
        }
    }

    if let Err(err) = validate_against_schema(schema, input) {
        return Err(ThreadDataError::of(thread_tool_failure(
            "invalid_arguments",
            format!("Parameter validation failed: {err}"),
            "Fix the flagged field and call the tool again with a valid payload.",
            None,
        )));
    }

    serde_json::from_value::<T>(input.clone()).map_err(|err| {
        ThreadDataError::of(thread_tool_failure(
            "invalid_arguments",
            format!("Parameter validation failed: {err}"),
            "Fix the flagged field and call the tool again with a valid payload.",
            None,
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::thread::contracts::params::{ThreadCreateInput, thread_tool_param_schema};

    #[test]
    fn suggests_prompt_when_message_passed_to_thread_create() {
        let schema = thread_tool_param_schema("thread_create").unwrap();
        let err = parse_thread_params::<ThreadCreateInput>(
            &schema,
            &serde_json::json!({ "message": "hello" }),
        )
        .unwrap_err();
        let ThreadDataError::Error { error } = err;
        assert_eq!(error.code.as_str(), "invalid_arguments");
        assert!(error.message.contains("did you mean \"prompt\""));
    }

    #[test]
    fn validates_all_thread_tool_schemas_with_valid_payloads() {
        for (tool, sample) in [
            (
                "thread_list",
                serde_json::json!({ "all_scope": true, "state": ["idle", "archived"] }),
            ),
            (
                "thread_read",
                serde_json::json!({ "thread": "alpha", "max_bytes": 4096 }),
            ),
            (
                "thread_send",
                serde_json::json!({ "thread": "alpha", "message": "hi", "delivery": "steer" }),
            ),
            (
                "thread_handoff",
                serde_json::json!({ "thread": "alpha", "message": "hi", "match": "fuzzy" }),
            ),
            (
                "thread_create",
                serde_json::json!({ "name": "worker", "permission_preset": "accept-edits" }),
            ),
            (
                "thread_interrupt",
                serde_json::json!({ "thread": "alpha", "turn_id": "t-1" }),
            ),
            (
                "thread_close",
                serde_json::json!({ "thread": "alpha", "graceful": true, "wait_ms": 5000 }),
            ),
            (
                "thread_rename",
                serde_json::json!({ "thread": "alpha", "name": "beta" }),
            ),
            (
                "thread_archive",
                serde_json::json!({ "thread": "alpha", "archived": true }),
            ),
            (
                "thread_set_model",
                serde_json::json!({ "thread": "alpha", "model": "gpt-5.4:high" }),
            ),
            (
                "thread_set_reasoning",
                serde_json::json!({ "thread": "alpha", "level": "medium" }),
            ),
        ] {
            let schema = thread_tool_param_schema(tool).unwrap_or_else(|| panic!("schema {tool}"));
            let parsed: serde_json::Value = parse_thread_params(&schema, &sample)
                .unwrap_or_else(|e| panic!("{tool} failed: {e:?}"));
            assert!(parsed.is_object(), "{tool}");
        }
    }

    #[test]
    fn rejects_unknown_fields_without_mutating_state() {
        let schema = thread_tool_param_schema("thread_send").unwrap();
        let err = parse_thread_params::<serde_json::Value>(
            &schema,
            &serde_json::json!({ "thread": "a", "message": "b", "bogus": 1 }),
        )
        .unwrap_err();
        let ThreadDataError::Error { error } = err;
        assert_eq!(error.code.as_str(), "invalid_arguments");
        assert!(error.message.contains("bogus"));
    }

    #[test]
    fn rejects_invalid_enums_and_empty_required_strings() {
        let send_schema = thread_tool_param_schema("thread_send").unwrap();
        assert!(
            parse_thread_params::<serde_json::Value>(
                &send_schema,
                &serde_json::json!({ "thread": "a", "message": "b", "delivery": "urgent" }),
            )
            .is_err()
        );
        assert!(
            parse_thread_params::<serde_json::Value>(
                &send_schema,
                &serde_json::json!({ "thread": "a", "message": "b", "collapse_key": "" }),
            )
            .is_err()
        );
        let model_schema = thread_tool_param_schema("thread_set_model").unwrap();
        assert!(
            parse_thread_params::<serde_json::Value>(
                &model_schema,
                &serde_json::json!({ "model": "" }),
            )
            .is_err()
        );
    }

    #[test]
    fn validates_relay_tool_schemas() {
        for (tool, sample) in [
            (
                "thread_report",
                serde_json::json!({ "binding_id": "b1", "kind": "milestone", "text": "done" }),
            ),
            (
                "thread_outbox",
                serde_json::json!({ "binding_id": "b1", "limit": 10 }),
            ),
            (
                "thread_outbox_ack",
                serde_json::json!({ "binding_id": "b1", "cursor": 5 }),
            ),
            (
                "thread_answer",
                serde_json::json!({ "binding_id": "b1", "reply_token": "tok", "answer": "yes" }),
            ),
        ] {
            let schema = thread_tool_param_schema(tool).unwrap();
            assert!(
                parse_thread_params::<serde_json::Value>(&schema, &sample).is_ok(),
                "{tool}"
            );
        }
    }

    #[test]
    fn rejects_non_object_input_and_invalid_numeric_bounds() {
        let read_schema = thread_tool_param_schema("thread_read").unwrap();
        assert!(
            parse_thread_params::<serde_json::Value>(&read_schema, &serde_json::json!("bad"))
                .is_err()
        );
        assert!(
            parse_thread_params::<serde_json::Value>(
                &read_schema,
                &serde_json::json!({ "thread": "a", "wait_ms": 0 }),
            )
            .is_err()
        );
        assert!(
            parse_thread_params::<serde_json::Value>(
                &read_schema,
                &serde_json::json!({ "thread": "a", "max_bytes": 0 }),
            )
            .is_err()
        );
    }

    #[test]
    fn accepts_thread_create_fork_and_skills() {
        let schema = thread_tool_param_schema("thread_create").unwrap();
        let parsed: ThreadCreateInput = parse_thread_params(
            &schema,
            &serde_json::json!({
                "name": "child",
                "fork_from": "parent-1",
                "skills": ["search"],
                "worktree": { "branch": "feat/x" }
            }),
        )
        .expect("valid create input");
        assert_eq!(parsed.fork_from.as_deref(), Some("parent-1"));
        assert!(parsed.worktree.is_some());
    }

    #[test]
    fn suggests_close_typo_and_handles_missing_required_fields() {
        let schema = thread_tool_param_schema("thread_close").unwrap();
        let err =
            parse_thread_params::<serde_json::Value>(&schema, &serde_json::json!({ "thred": "x" }))
                .unwrap_err();
        let ThreadDataError::Error { error } = err;
        assert!(error.message.contains("thred"));
    }

    #[test]
    fn handles_archive_list_and_reasoning_schemas() {
        let arch = thread_tool_param_schema("thread_archive").unwrap();
        assert!(
            parse_thread_params::<serde_json::Value>(
                &arch,
                &serde_json::json!({ "archived": true })
            )
            .is_err()
        );
        let list = thread_tool_param_schema("thread_list").unwrap();
        assert!(
            parse_thread_params::<serde_json::Value>(
                &list,
                &serde_json::json!({ "state": ["bogus"] }),
            )
            .is_err()
        );
        let reason = thread_tool_param_schema("thread_set_reasoning").unwrap();
        assert!(
            parse_thread_params::<serde_json::Value>(
                &reason,
                &serde_json::json!({ "thread": "a", "level": "invalid" }),
            )
            .is_err()
        );
    }
}
