//! Port of senpi `websearch/websearch/providers/response-stream.ts` (pin 903b8336).
use serde_json::{Map, Value, json};

fn stream_error_message(event: &Value) -> String {
    let sources = [event, &event["error"], &event["response"]["error"]];
    let pick = |key: &str| {
        sources
            .iter()
            .filter(|source| source.is_object())
            .filter_map(|source| source[key].as_str())
            .find(|value| !value.is_empty())
            .map(str::to_owned)
    };
    match (pick("code"), pick("message")) {
        (Some(code), Some(message)) => format!("{code}: {message}"),
        (None, Some(message)) => message,
        (Some(code), None) => code,
        (None, None) => "The search stream failed.".into(),
    }
}

fn event_data(block: &str) -> String {
    block
        .lines()
        .filter_map(|line| line.strip_prefix("data:"))
        .map(str::trim_start)
        .collect::<Vec<_>>()
        .join("\n")
}

/// Folds a streamed Responses reply into `{ output }`; a stream failure becomes `error.message`, a
/// JSON body passes through.
pub fn collect_response_stream(body_text: &str) -> Value {
    if body_text.trim_start().starts_with('{') {
        return match serde_json::from_str::<Value>(body_text) {
            Ok(value) if value.is_object() => value,
            _ => json!({}),
        };
    }
    let normalized = body_text.replace("\r\n", "\n");
    let mut done_items = Vec::new();
    let mut completed_output = Vec::new();
    let mut error = None;
    for block in normalized.split("\n\n") {
        let data = event_data(block);
        if data.is_empty() || data == "[DONE]" {
            continue;
        }
        let Ok(event) = serde_json::from_str::<Value>(&data) else {
            continue;
        };
        if !event.is_object() {
            continue;
        }
        match event["type"].as_str() {
            Some("response.output_item.done") => {
                if event["item"].is_object() {
                    done_items.push(event["item"].clone());
                }
            }
            Some("response.completed" | "response.done") => {
                completed_output = event["response"]["output"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default();
            }
            Some("error" | "response.failed") => error = Some(stream_error_message(&event)),
            _ => {}
        }
    }
    let mut payload = Map::new();
    payload.insert(
        "output".into(),
        Value::Array(if done_items.is_empty() {
            completed_output
        } else {
            done_items
        }),
    );
    if let Some(error) = error {
        payload.insert("error".into(), json!({"message": error}));
    }
    Value::Object(payload)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_bodies_pass_through() {
        assert_eq!(
            collect_response_stream(" {\"output\":[1]}"),
            json!({"output":[1]})
        );
        assert_eq!(collect_response_stream("{broken"), json!({}));
    }

    #[test]
    fn folds_done_items_and_prefers_them_over_completed_output() {
        let body = "event: x\r\ndata: {\"type\":\"response.output_item.done\",\"item\":{\"id\":\"a\"}}\r\n\r\ndata: {\"type\":\"response.completed\",\"response\":{\"output\":[{\"id\":\"b\"}]}}\n\ndata: [DONE]\n\n";
        assert_eq!(
            collect_response_stream(body),
            json!({"output":[{"id":"a"}]})
        );
        let completed =
            "data: {\"type\":\"response.done\",\"response\":{\"output\":[{\"id\":\"b\"}]}}\n\n";
        assert_eq!(
            collect_response_stream(completed),
            json!({"output":[{"id":"b"}]})
        );
    }

    #[test]
    fn stream_failures_become_error_messages() {
        let failed = "data: {\"type\":\"response.failed\",\"response\":{\"error\":{\"code\":\"server_error\",\"message\":\"boom\"}}}\n\n";
        assert_eq!(
            collect_response_stream(failed),
            json!({"output":[],"error":{"message":"server_error: boom"}})
        );
        let bare = "data: {\"type\":\"error\"}\n\n";
        assert_eq!(
            collect_response_stream(bare)["error"]["message"],
            "The search stream failed."
        );
    }
}
