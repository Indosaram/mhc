use maho_codemode::bridge::protocol::*;
use serde_json::{Value, json};

#[test]
fn round_trips_core_message_kinds() {
    for message in [
        json!({"type":"init","sessionId":"s","connection":{"port":1,"token":"t"}}),
        json!({"type":"run","cellId":"c","code":"1+1","timeoutMs":1000}),
        json!({"type":"tool-reply","callId":"call","ok":true,"value":null}),
        json!({"type":"tool-reply","callId":"call","ok":false,"error":{"message":"failed"}}),
        json!({"type":"interrupt","reason":"user"}), json!({"type":"close"}),
        json!({"type":"ready"}), json!({"type":"closed"}),
        json!({"type":"init-failed","error":{"message":"bad init"}}),
        json!({"type":"text","stream":"stdout","data":"hello"}),
        json!({"type":"text","stream":"stderr","data":"warn"}),
        json!({"type":"display","mimeType":"image/png","dataBase64":"abc"}),
        json!({"type":"tool-call","callId":"call","toolName":"read","args":{}}),
        json!({"type":"log","message":"message"}), json!({"type":"phase","title":"Running"}),
        json!({"type":"result","cellId":"c","ok":true,"valueRepr":"2","durationMs":12}),
        json!({"type":"result","cellId":"c","ok":false,"error":{"message":"boom","stack":"stack"},"durationMs":15}),
    ] {
        let encoded = encode_bridge_frame(&message).unwrap();
        assert!(encoded.ends_with('\n'));
        assert_eq!(decode_bridge_frame(&encoded, None).unwrap(), message);
    }
}

#[test]
fn unknown_status_payload_preserved() {
    let message = json!({"type":"status","event":{"op":"future","nested":{"kept":true},"count":3}});
    assert_eq!(decode_bridge_frame(&encode_bridge_frame(&message).unwrap(), None).unwrap(), message);
}

#[test]
fn init_names_and_env_and_roots() {
    let message = json!({"type":"init","sessionId":"s","connection":{"port":4317,"token":"t","localRoots":{"local":"/tmp/local"},"artifactsDir":"/tmp"},"sessionEnv":{"PI_SESSION_ID":"s"},"hostToolNames":["read"],"foreignLanguageNames":["py_lookup"]});
    assert_eq!(decode_bridge_frame(&encode_bridge_frame(&message).unwrap(), None).unwrap(), message);
}

#[test]
fn oversized_status_rejected() {
    let frame = encode_bridge_frame(&json!({"type":"status","event":{"op":"large","data":"x".repeat(BRIDGE_FRAME_MAX_BYTES)}})).unwrap();
    assert_eq!(decode_bridge_frame(&frame, None).unwrap_err().code, BridgeDecodeErrorCode::FrameTooLarge);
}

#[test]
fn typed_decode_errors() {
    assert_eq!(decode_bridge_frame("{\"type\":\"run\"", None).unwrap_err().code, BridgeDecodeErrorCode::MalformedJson);
    assert_eq!(decode_bridge_frame("{\"type\":\"ready\"}\n", Some(4)).unwrap_err().code, BridgeDecodeErrorCode::FrameTooLarge);
    assert_eq!(decode_bridge_frame("{\"type\":\"run\",\"cellId\":\"cell\"}\n", None).unwrap_err().code, BridgeDecodeErrorCode::InvalidMessage);
}

#[test]
fn lf_record_framing() {
    assert_eq!(parse_bridge_json_line("{\"type\":\"ready\"}\n{\"type\":\"closed\"}", None).unwrap_err().code, BridgeDecodeErrorCode::MultipleFrames);
    assert_eq!(parse_bridge_json_line("{\"type\":\"ready\"}\n", None).unwrap(), json!({"type":"ready"}));
}

#[test]
fn empty_and_utf8_frame_size() {
    assert_eq!(parse_bridge_json_line("\n", None).unwrap_err().code, BridgeDecodeErrorCode::EmptyFrame);
    assert_eq!(parse_bridge_json_line("\"한\"", Some(4)).unwrap_err().code, BridgeDecodeErrorCode::FrameTooLarge);
}

#[test]
fn rejects_invalid_fields() {
    for value in [json!({"type":"result","cellId":"c","ok":true,"durationMs":-1}), json!({"type":"text","stream":"other","data":"x"}), json!({"type":"init","sessionId":"s","connection":{"port":0,"token":"t"}}), json!({"type":"run","cellId":"","code":"x"}), json!({"type":"status","event":{}}), Value::Null] {
        assert_eq!(decode_bridge_frame(&encode_bridge_frame(&value).unwrap(), None).unwrap_err().code, BridgeDecodeErrorCode::InvalidMessage);
    }
}

#[test]
fn host_message_is_not_kernel_message() {
    assert!(!is_kernel_to_host_message(&json!({"type":"run","cellId":"c","code":"x"})));
    assert!(is_kernel_to_host_message(&json!({"type":"ready"})));
}

#[test]
fn correlation_and_bearer_token() {
    let token = generate_bridge_token(32).unwrap();
    assert!(token.len() >= 32);
    assert!(verify_bridge_token(&token, &token).is_ok());
    assert!(verify_bridge_token(&token, &format!("{token}x")).is_err());
    assert!(uuid::Uuid::parse_str(&generate_correlation_id()).is_ok());
}

#[test]
fn kernel_tool_messages_round_trip() {
    for message in [
        json!({"type":"kernel-tool-describe","requestId":"d1","names":["lookup"]}),
        json!({"type":"kernel-tool-invoke","requestId":"i1","name":"lookup","kernel_generation":4,"definition_revision":2,"args":{"path":"x"},"call_id":"child-1"}),
        json!({"type":"kernel-tool-describe-reply","requestId":"d1","ok":true,"results":[{"name":"lookup","ok":true,"descriptor":{"name":"lookup","description":"","input_schema":{},"language":"js","kernel_generation":4,"definition_revision":2}}]}),
        json!({"type":"kernel-tool-invoke-reply","requestId":"i1","ok":false,"error":{"message":"stale","code":"kernel_tool_stale"}}),
    ] { assert_eq!(decode_bridge_frame(&encode_bridge_frame(&message).unwrap(), None).unwrap(), message); }
}

#[test]
fn refreshed_names_round_trip() {
    let message = json!({"type":"kernel-tools-names","hostToolNames":["read","mcp_attached"],"foreignLanguageNames":["py_lookup"]});
    assert_eq!(decode_bridge_frame(&encode_bridge_frame(&message).unwrap(), None).unwrap(), message);
}

#[test]
fn scopes_validate_policy_lists() {
    use maho_codemode::bridge::kernel_tools_protocol::valid_invoke_scope;
    assert!(valid_invoke_scope(&json!({"tools":{"allow":["read"],"deny":["write"]}})));
    assert!(!valid_invoke_scope(&json!({"tools":{"allow":[""]}})));
    assert!(!valid_invoke_scope(&json!({"tools":{"deny":"write"}})));
}

#[test]
fn host_denial_detail_round_trip() {
    let message = json!({"type":"kernel-tool-invoke-reply","requestId":"i1","ok":false,"error":{"message":"denied","code":"kernel_tool_host_denied","details":{"tool":"read","call_id":"child","reason":"deny"}}});
    assert_eq!(decode_bridge_frame(&encode_bridge_frame(&message).unwrap(), None).unwrap(), message);
}
