//! Golden comparison for the maho-ai wire-API replay tests (todos 10-12).
//!
//! `tools/golden/ai-replay.mjs` drives the pinned senpi `stream()` for an api against a recorded
//! fixture and writes the resulting `AssistantMessageEvent[]` to
//! `crates/maho-ai/tests/golden/replay/<case>.json`. A wire-API test serves the same fixture from
//! [`super::mock_server`], drives its own `stream()` against it and compares the event sequence
//! with that golden through [`assert_stream_matches_golden`].
//!
//! Comparison semantics: the two sides are different serializers (Bun's
//! `JSON.stringify(events, null, "\t")` vs `serde_json`), so the comparison is on the parsed JSON
//! *values*: event order, every field name and every value must match, while whitespace and object
//! key order are not part of it (JSON objects are unordered, and the golden is generated, never
//! hand-edited). Two normalizations keep the comparison on what senpi actually records:
//! [`numbers_match`] (a float-typed field serializes as `0` on one side and `0.0` on the other) and
//! [`normalize_discriminators`] (senpi repeats a union's discriminator as a literal field, the Rust
//! port carries it as the enum's serde tag instead).

use std::collections::BTreeMap;
use std::future::Future;
use std::path::PathBuf;

use maho_ai::types::AssistantMessageEvent;
use maho_test_support::golden::fixture_path;
use serde_json::Value;

use super::mock_server::{with_timeout, MockFixtureServer};

/// Path of the golden `tools/golden/ai-replay.mjs` wrote for `case`.
pub fn golden_path(case: &str) -> PathBuf {
    fixture_path("maho-ai", &format!("replay/{case}.json"))
}

/// Compares `events` with the golden recorded from the pinned senpi `stream()` for `case`.
///
/// A mismatch is either a deviation from senpi (fix the Rust side) or a stale golden (regenerate
/// it with `bun tools/golden/ai-replay.mjs --all`); the panic names the first field that differs.
pub fn assert_matches_golden(case: &str, events: &[AssistantMessageEvent]) {
    let path = golden_path(case);
    let golden_text = std::fs::read_to_string(&path).unwrap_or_else(|error| {
        panic!(
            "golden {} is unreadable: {error} (generate it with bun tools/golden/ai-replay.mjs)",
            path.display()
        )
    });
    let golden: Value = serde_json::from_str(&golden_text)
        .unwrap_or_else(|error| panic!("golden {} is not JSON: {error}", path.display()));
    let mut actual = serde_json::to_value(events).expect("serialize replay events");
    normalize_discriminators(&mut actual);
    if let Some(difference) = first_difference(&golden, &actual, "events") {
        panic!("replay mismatch against {}\n{difference}", path.display());
    }
}

/// Drives one maho-ai `stream()` call against `server` and compares its events with the golden for
/// `case`: `drive` receives the mock server's base URL (to put on the model) and returns the event
/// sequence the stream produced.
pub async fn assert_stream_matches_golden<F, Fut>(case: &str, server: &MockFixtureServer, drive: F)
where
    F: FnOnce(String) -> Fut,
    Fut: Future<Output = Vec<AssistantMessageEvent>>,
{
    let events = with_timeout(30, drive(server.base_url().to_owned())).await;
    assert_matches_golden(case, &events);
}

/// Injects the discriminator fields senpi repeats as literals into the Rust event JSON.
///
/// senpi declares `Message.role` and `ToolCall.type` as literal fields, so every recorded event
/// carries them; the Rust port carries each as its enum's serde tag instead (`#[serde(tag =
/// "role")]` on `Message`, `#[serde(rename = "toolCall")]` on `ContentBlock::ToolCall`). Those
/// tags appear when the value sits inside its enum, but a bare `AssistantMessage` - what
/// `AssistantMessageEvent` embeds - and the bare `toolCall` of a `toolcall_end` event serialize
/// without them. Injecting the two constants here keeps the comparison meaningful for every other
/// field (content, usage, stopReason, ids, deltas, ...) while not re-asserting a value that is
/// fixed by the type.
fn normalize_discriminators(events: &mut Value) {
    let Value::Array(events) = events else { return };
    for event in events {
        let Value::Object(event) = event else { continue };
        for key in ["partial", "message", "error"] {
            if let Some(Value::Object(message)) = event.get_mut(key)
                && message.contains_key("api")
                && message.contains_key("content")
            {
                message.insert("role".to_owned(), Value::String("assistant".to_owned()));
            }
        }
        if let Some(Value::Object(tool_call)) = event.get_mut("toolCall")
            && tool_call.contains_key("id")
            && tool_call.contains_key("arguments")
        {
            tool_call.insert("type".to_owned(), Value::String("toolCall".to_owned()));
        }
    }
}

fn first_difference(golden: &Value, actual: &Value, path: &str) -> Option<String> {
    match (golden, actual) {
        (Value::Object(golden), Value::Object(actual)) => {
            for (key, expected) in golden {
                match actual.get(key) {
                    Some(produced) => {
                        if let Some(difference) =
                            first_difference(expected, produced, &format!("{path}.{key}"))
                        {
                            return Some(difference);
                        }
                    }
                    None => return Some(format!("{path}.{key}: missing on the maho side\n{}", render(expected))),
                }
            }
            actual
                .keys()
                .find(|key| !golden.contains_key(*key))
                .map(|key| format!("{path}.{key}: unexpected on the maho side\n{}", render(&actual[key])))
        }
        (Value::Array(golden), Value::Array(actual)) => {
            for (index, (expected, produced)) in golden.iter().zip(actual).enumerate() {
                if let Some(difference) = first_difference(expected, produced, &format!("{path}[{index}]")) {
                    return Some(difference);
                }
            }
            (golden.len() != actual.len()).then(|| {
                format!("{path}: senpi recorded {} element(s), maho produced {}", golden.len(), actual.len())
            })
        }
        (Value::Number(golden), Value::Number(actual)) => (!numbers_match(golden, actual))
            .then(|| format!("{path}: senpi {golden} vs maho {actual}")),
        _ if golden == actual => None,
        _ => Some(format!(
            "{path}: senpi {}\nvs maho {}",
            render(golden),
            render(actual)
        )),
    }
}

/// Compares two JSON numbers by value. `Usage.cost` fields are `f64` in the Rust port, so a zero
/// cost serializes as `0.0` there and `0` in the golden; both are the same number.
fn numbers_match(golden: &serde_json::Number, actual: &serde_json::Number) -> bool {
    golden.as_f64() == actual.as_f64()
}

fn render(value: &Value) -> String {
    serde_json::to_string(&sorted(value)).expect("render golden value")
}

fn sorted(value: &Value) -> Value {
    match value {
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(key, value)| (key.clone(), sorted(value)))
                .collect::<BTreeMap<_, _>>()
                .into_iter()
                .collect(),
        ),
        Value::Array(items) => Value::Array(items.iter().map(sorted).collect()),
        other => other.clone(),
    }
}
