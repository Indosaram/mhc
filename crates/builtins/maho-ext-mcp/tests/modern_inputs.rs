//! Unit ports for senpi mcp/modern/inputs.ts (`parseInputs`) and mcp/modern/diagnostics.ts
//! (`sanitizeModernMessage`). Upstream exercises both through the MRTR adapter (test/mcp/modern-mrtr.test.ts) and
//! the modern transports (modern/transport.test.ts, test/mcp/modern-mrtr-transport.test.ts); each test below ports
//! the case's input and the part of its assertion these two functions own.
use maho_ext_mcp::modern::{
    diagnostics::sanitize_modern_message,
    errors::McpMrtrErrorCode,
    inputs::{McpMrtrInput, parse_inputs},
};
use serde_json::{Map, Value, json};

/// Upstream fixture `formInput` (test/mcp/fixtures/mrtr-2026-server.ts).
fn form_input(message: &str) -> Value {
    json!({
        "method": "elicitation/create",
        "params": {"message": message, "requestedSchema": {"type": "object", "properties": {"value": {"type": "string"}}}},
    })
}
/// Upstream fixture `inputResult(state, inputs)`.
fn input_result(state: &str, inputs: Value) -> Map<String, Value> {
    json!({"resultType": "input_required", "requestState": state, "inputRequests": inputs})
        .as_object()
        .unwrap()
        .clone()
}
/// Upstream fixture `inputResult()` with its defaults.
fn default_input_result() -> Map<String, Value> {
    input_result(
        " opaque \u{2603} state ",
        json!({"question": form_input("synthetic question")}),
    )
}
fn object(value: Value) -> Map<String, Value> {
    value.as_object().unwrap().clone()
}
fn parse_error(result: &Map<String, Value>) -> McpMrtrErrorCode {
    parse_inputs(result)
        .expect_err("input must be rejected")
        .code
}

/// modern-mrtr.test.ts "tools/call waits for input_required then returns only complete content": the default
/// `inputResult()` parses into one form input keyed `question`.
#[test]
fn input_required_parses_the_form_input_request() {
    let inputs = parse_inputs(&default_input_result()).unwrap().unwrap();
    assert_eq!(inputs.len(), 1);
    let (
        key,
        McpMrtrInput {
            message,
            requested_schema,
            ..
        },
    ) = &inputs[0];
    assert_eq!(key, "question");
    assert_eq!(message, "synthetic question");
    assert_eq!(
        requested_schema,
        &json!({"type": "object", "properties": {"value": {"type": "string"}}})
    );
}

/// modern-mrtr.test.ts "server input keys cannot mutate object prototypes".
#[test]
fn server_input_keys_cannot_mutate_object_prototypes() {
    let form = json!({
        "method": "elicitation/create",
        "params": {"message": "question", "requestedSchema": {"type": "object", "properties": {}}},
    });
    let mut requests = Map::new();
    for key in ["__proto__", "constructor", "toString"] {
        requests.insert(key.into(), form.clone());
    }
    let inputs = parse_inputs(&input_result("state", Value::Object(requests)))
        .unwrap()
        .unwrap();
    assert_eq!(
        inputs
            .iter()
            .map(|(key, _)| key.as_str())
            .collect::<Vec<_>>(),
        ["__proto__", "constructor", "toString"]
    );
}

/// modern-mrtr.test.ts "each round replaces state and responses instead of merging": an empty `inputRequests`
/// batch yields an empty (present) input set, and a result with only `requestState` (even "") yields none.
#[test]
fn each_round_replaces_state_and_responses_instead_of_merging() {
    let empty = object(json!({"resultType": "input_required", "inputRequests": {}}));
    assert_eq!(parse_inputs(&empty).unwrap(), Some(vec![]));
    let state_only = object(json!({"resultType": "input_required", "requestState": ""}));
    assert_eq!(parse_inputs(&state_only).unwrap(), None);
}

/// modern-mrtr.test.ts "malformed input is rejected before UI (%j)" with `{ resultType: "input_required" }`.
#[test]
fn malformed_input_is_rejected_before_ui_without_state_or_requests() {
    let result = object(json!({"resultType": "input_required"}));
    assert_eq!(parse_error(&result), McpMrtrErrorCode::InvalidInput);
}

/// modern-mrtr.test.ts "malformed input is rejected before UI (%j)" with a numeric `requestState`.
#[test]
fn malformed_input_is_rejected_before_ui_with_a_numeric_request_state() {
    let result = object(json!({"resultType": "input_required", "requestState": 1}));
    assert_eq!(parse_error(&result), McpMrtrErrorCode::InvalidInput);
}

/// modern-mrtr.test.ts "malformed input is rejected before UI (%j)" with an array `inputRequests`.
#[test]
fn malformed_input_is_rejected_before_ui_with_array_input_requests() {
    let result = object(json!({"resultType": "input_required", "inputRequests": []}));
    assert_eq!(parse_error(&result), McpMrtrErrorCode::InvalidInput);
}

/// modern-mrtr.test.ts "undeclared %s stops locally without fabricated results" for `roots/list` and
/// `sampling/createMessage`.
#[test]
fn undeclared_capability_inputs_stop_locally_without_fabricated_results() {
    for method in ["roots/list", "sampling/createMessage"] {
        let result = input_result("state", json!({"input": {"method": method, "params": {}}}));
        assert_eq!(
            parse_error(&result),
            McpMrtrErrorCode::UnsupportedInput,
            "{method}"
        );
    }
}

/// modern-mrtr.test.ts "oversized input is rejected before UI" with a 65537-byte `requestState`.
#[test]
fn oversized_input_is_rejected_before_ui_for_request_state() {
    let result = object(json!({"resultType": "input_required", "requestState": "x".repeat(65537)}));
    assert_eq!(parse_error(&result), McpMrtrErrorCode::InputLimit);
}

/// modern-mrtr.test.ts "oversized input is rejected before UI" with 33 input requests.
#[test]
fn oversized_input_is_rejected_before_ui_for_the_request_count() {
    let mut requests = Map::new();
    for index in 0..33 {
        requests.insert(
            index.to_string(),
            json!({
                "method": "elicitation/create",
                "params": {"message": "test", "requestedSchema": {"type": "object", "properties": {}}},
            }),
        );
    }
    let result = input_result("state", Value::Object(requests));
    assert_eq!(parse_error(&result), McpMrtrErrorCode::InputLimit);
}

/// modern-mrtr.test.ts "oversized input is rejected before UI" with a 16385-byte message.
#[test]
fn oversized_input_is_rejected_before_ui_for_a_string() {
    let result = input_result(
        "state",
        json!({"value": {
            "method": "elicitation/create",
            "params": {"message": "x".repeat(16385), "requestedSchema": {"type": "object", "properties": {}}},
        }}),
    );
    assert_eq!(parse_error(&result), McpMrtrErrorCode::InputLimit);
}

/// modern-mrtr.test.ts "unsolicited URL inputs terminate without navigation or fabricated decline".
#[test]
fn unsolicited_url_inputs_terminate_without_navigation_or_fabricated_decline() {
    let result = input_result(
        "state",
        json!({"url": {
            "method": "elicitation/create",
            "params": {"mode": "url", "message": "private", "url": "https://invalid.test", "elicitationId": "id"},
        }}),
    );
    assert_eq!(parse_error(&result), McpMrtrErrorCode::UnsupportedInput);
}

/// modern-mrtr.test.ts "modern diagnostic messages are suppressed before consent reaches the logger": the
/// server's `notifications/message` is replaced before admission, so no canary survives.
#[test]
fn modern_diagnostic_messages_are_suppressed_before_consent_reaches_the_logger() {
    let message = json!({
        "jsonrpc": "2.0",
        "method": "notifications/message",
        "params": {"level": "warning", "data": "PRIVATE_SERVER_LOG_CANARY", "logger": "PRIVATE_LOGGER_CANARY"},
    });
    let safe = sanitize_modern_message(&message).unwrap().unwrap();
    assert_eq!(
        safe,
        json!({
            "jsonrpc": "2.0",
            "method": "notifications/message",
            "params": {"level": "info", "data": "MCP server diagnostic suppressed."},
        })
    );
    assert!(!safe.to_string().contains("CANARY"));
}

/// modern/transport.test.ts "delivers JSON-RPC errors as messages rather than transport errors": the error keeps
/// its id and code, loses `data`, and no remote text survives.
#[test]
fn delivers_json_rpc_errors_as_messages_rather_than_transport_errors() {
    let error = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "error": {"code": -32022, "message": "REMOTE_ERROR_CANARY", "data": {"supported": ["PRIVATE_DATA_CANARY"]}},
    });
    let safe = sanitize_modern_message(&error).unwrap().unwrap();
    assert_eq!(safe["jsonrpc"], "2.0");
    assert_eq!(safe["id"], 1);
    assert_eq!(safe["error"]["code"], -32022);
    assert!(safe["error"].get("data").is_none());
    assert!(!safe.to_string().contains("CANARY"));
}

/// modern/transport.test.ts "consumes chunked SSE with multiline data and delivers the final response": a progress
/// notification without a token and the final response are delivered unchanged.
#[test]
fn consumes_chunked_sse_progress_and_final_response_unchanged() {
    let notification =
        json!({"jsonrpc": "2.0", "method": "notifications/progress", "params": {"progress": 1}});
    assert_eq!(
        sanitize_modern_message(&notification).unwrap(),
        Some(notification.clone())
    );
    let reply = json!({"jsonrpc": "2.0", "id": 1, "result": {"resultType": "complete", "text": "\u{D55C}\u{AE00}"}});
    assert_eq!(
        sanitize_modern_message(&reply).unwrap(),
        Some(reply.clone())
    );
}

/// modern/transport.test.ts "reports HTTP and malformed response failures through onerror and send" (the
/// `notifications/progress` without params case): the malformed progress is dropped, never delivered.
#[test]
fn malformed_progress_notifications_are_dropped() {
    let progress = json!({"jsonrpc": "2.0", "method": "notifications/progress"});
    assert_eq!(sanitize_modern_message(&progress).unwrap(), None);
}

/// modern-mrtr-transport.test.ts "stdio cancels only the live round once and drops completed or cancelled replies
/// and progress": a delivered progress notification is exactly `{ progressToken, progress }`.
#[test]
fn delivered_progress_keeps_only_token_and_progress() {
    let progress = json!({
        "jsonrpc": "2.0",
        "method": "notifications/progress",
        "params": {"progressToken": 2, "progress": 1},
    });
    assert_eq!(
        sanitize_modern_message(&progress).unwrap(),
        Some(progress.clone())
    );
}
