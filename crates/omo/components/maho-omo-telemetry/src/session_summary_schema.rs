//! Port of `packages/omo-senpi/src/components/telemetry/session-summary-schema.ts`.
//!
//! `session_summary`: the assistant turns of one window, summed. Windows never overlap; a process numbers its
//! windows of a session with `summary_seq` inside a random `summary_epoch` that a reload in the same process
//! keeps and any hand-over to another process replaces, so (session, `summary_epoch`, `summary_seq`) is
//! unique. The event's uuid is the window id, so a re-sent window is deduplicated rather than counted twice.

use serde_json::{Map, Value, json};

use crate::model_vocabulary::{KNOWN_MODELS, KNOWN_PROVIDERS};
use crate::session_role::TELEMETRY_SESSION_ROLES;
use crate::session_summary_window::{
    ENDED_BY, MODEL_SLOTS, MODEL_SLOT_NUMBER_KEYS, summary_number_keys,
};

fn string_property() -> Value {
    json!({"type":"string"})
}

fn number_property() -> Value {
    json!({"type":"number"})
}

fn boolean_property() -> Value {
    json!({"type":"boolean"})
}

fn enum_property(values: Vec<&str>) -> Value {
    json!({"type":"string","values":values})
}

fn build_schema() -> Value {
    let mut schema = Map::new();
    schema.insert("$session_id".into(), string_property());
    schema.insert("ended_by".into(), enum_property(ENDED_BY.to_vec()));
    schema.insert("is_final".into(), boolean_property());
    schema.insert("raw_turns_sampled".into(), boolean_property());
    schema.insert(
        "session_role".into(),
        enum_property(TELEMETRY_SESSION_ROLES.to_vec()),
    );
    schema.insert("summary_epoch".into(), string_property());
    for key in summary_number_keys() {
        schema.insert(key.to_string(), number_property());
    }
    let mut models: Vec<&str> = KNOWN_MODELS
        .iter()
        .flat_map(|(_, models)| models.iter().copied())
        .collect();
    models.push("custom");
    models.push("none");
    models.sort_unstable();
    models.dedup();
    let mut providers: Vec<&str> = KNOWN_PROVIDERS.to_vec();
    providers.push("custom");
    providers.push("none");
    for slot in 1..=MODEL_SLOTS {
        schema.insert(
            format!("m{slot}_provider"),
            enum_property(providers.clone()),
        );
        schema.insert(format!("m{slot}_model"), enum_property(models.clone()));
        for key in MODEL_SLOT_NUMBER_KEYS {
            schema.insert(format!("m{slot}_{key}"), number_property());
        }
    }
    let mut sorted: Vec<(String, Value)> = schema.into_iter().collect();
    sorted.sort_by(|left, right| left.0.cmp(&right.0));
    Value::Object(sorted.into_iter().collect())
}

pub static SESSION_SUMMARY_SCHEMA: std::sync::LazyLock<Value> =
    std::sync::LazyLock::new(build_schema);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event_schemas::OMO_NATIVE_PROPERTY_ALLOWLISTS;

    #[test]
    fn schema_keys_match_the_capture_allowlist() {
        let schema = SESSION_SUMMARY_SCHEMA.as_object().unwrap();
        let mut expected: Vec<&str> = OMO_NATIVE_PROPERTY_ALLOWLISTS["session_summary"]
            .iter()
            .map(String::as_str)
            .collect();
        expected.sort_unstable();
        let mut actual: Vec<&str> = schema.keys().map(String::as_str).collect();
        actual.sort_unstable();
        assert_eq!(actual, expected);
    }

    #[test]
    fn vocabularies_are_closed() {
        let schema = SESSION_SUMMARY_SCHEMA.as_object().unwrap();
        assert_eq!(
            schema["ended_by"]["values"],
            json!(["checkpoint", "shutdown", "reload", "recovered"])
        );
        assert_eq!(
            schema["session_role"]["values"],
            json!(["root", "child", "unknown"])
        );
        assert_eq!(schema["is_final"]["type"], "boolean");
        assert_eq!(schema["summary_epoch"]["type"], "string");
        assert_eq!(schema["dropped_events"]["type"], "number");
        assert_eq!(schema["cost_1e4"]["type"], "number");
        let providers = schema["m1_provider"]["values"].as_array().unwrap();
        assert!(providers.contains(&json!("none")));
        assert!(providers.contains(&json!("custom")));
        let models = schema["m3_model"]["values"].as_array().unwrap();
        assert!(models.contains(&json!("none")));
        assert!(models.contains(&json!("gpt-5.6-sol")));
        assert!(!schema.contains_key("m4_provider"));
    }

    #[test]
    fn keys_are_sorted() {
        let keys: Vec<&str> = SESSION_SUMMARY_SCHEMA
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        let mut sorted = keys.clone();
        sorted.sort_unstable();
        assert_eq!(keys, sorted);
    }
}
