// Port of senpi mcp/modern/wire.test.ts.
use maho_ext_mcp::modern::wire::*;
use serde_json::{Value, json};

fn client() -> ModernClientInfo {
    ModernClientInfo {
        name: "senpi".into(),
        version: "test".into(),
    }
}
fn rpc_error(code: i64, data: Option<Value>) -> Value {
    let mut error = json!({"code": code, "message": "failure"});
    if let Some(data) = data {
        error["data"] = data;
    }
    json!({"jsonrpc": "2.0", "id": 1, "error": error})
}

#[test]
fn builds_discovery_with_only_the_metadata_triple() {
    let request = build_modern_request(json!(1), "server/discover", &client(), &json!({}), None);
    assert_eq!(
        request,
        json!({"jsonrpc":"2.0","id":1,"method":"server/discover","params":{"_meta":{
            "io.modelcontextprotocol/protocolVersion":"2026-07-28",
            "io.modelcontextprotocol/clientInfo":{"name":"senpi","version":"test"},
            "io.modelcontextprotocol/clientCapabilities":{}}}})
    );
}

#[test]
fn preserves_params_and_application_metadata_without_allowing_reserved_overrides() {
    let capabilities = json!({"elicitation":{"form":{}}});
    let params = json!({"name":"echo","arguments":{"text":"hello"},"_meta":{"progressToken":7,
        "io.modelcontextprotocol/protocolVersion":"old","io.modelcontextprotocol/clientInfo":{},"io.modelcontextprotocol/clientCapabilities":{}}});
    let request = build_modern_request(
        json!("call"),
        "tools/call",
        &client(),
        &capabilities,
        Some(&params),
    );
    assert_eq!(
        request["params"],
        json!({"name":"echo","arguments":{"text":"hello"},"_meta":{"progressToken":7,
            "io.modelcontextprotocol/protocolVersion":"2026-07-28","io.modelcontextprotocol/clientInfo":{"name":"senpi","version":"test"},
            "io.modelcontextprotocol/clientCapabilities":capabilities}})
    );
    assert_eq!(
        params["_meta"]["io.modelcontextprotocol/protocolVersion"],
        "old"
    );
}

#[test]
fn derives_matching_version_and_method_headers_without_session_headers() {
    for method in [
        "server/discover",
        "tools/list",
        "resources/list",
        "prompts/list",
        "ping",
    ] {
        let request = build_modern_request(json!(1), method, &client(), &json!({}), None);
        let headers = derive_modern_headers(&request);
        assert_eq!(headers.len(), 2, "{method}");
        assert_eq!(
            headers["MCP-Protocol-Version"],
            request["params"]["_meta"]["io.modelcontextprotocol/protocolVersion"]
        );
        assert_eq!(headers["Mcp-Method"], method);
    }
}

#[test]
fn encodes_the_routing_header() {
    for (method, params, expected) in [
        ("tools/call", json!({"name":"echo"}), "echo"),
        ("prompts/get", json!({"name":"summarize"}), "summarize"),
        (
            "resources/read",
            json!({"uri":"file:///notes.txt","name":"ignored"}),
            "file:///notes.txt",
        ),
        (
            "tools/call",
            json!({"name":"\u{D55C}\u{AE00}"}),
            "=?base64?7ZWc6riA?=",
        ),
        (
            "resources/read",
            json!({"uri":"x:\u{00FF}"}),
            "=?base64?eDrDvw==?=",
        ),
        (
            "prompts/get",
            json!({"name":"line\nbreak"}),
            "=?base64?bGluZQpicmVhaw==?=",
        ),
        (
            "tools/call",
            json!({"name":"base64:literal"}),
            "base64:literal",
        ),
        (
            "tools/call",
            json!({"name":"=?base64?literal?="}),
            "=?base64?PT9iYXNlNjQ/bGl0ZXJhbD89?=",
        ),
        ("tasks/get", json!({"taskId":"task-1"}), "task-1"),
    ] {
        let request = build_modern_request(json!(1), method, &client(), &json!({}), Some(&params));
        assert_eq!(
            derive_modern_headers(&request)
                .get("Mcp-Name")
                .map(String::as_str),
            Some(expected),
            "{method} {params}"
        );
    }
}

#[test]
fn omits_routing_names_on_methods_that_do_not_define_them() {
    let request = build_modern_request(
        json!(1),
        "tools/list",
        &client(),
        &json!({}),
        Some(&json!({"name":"not-a-route"})),
    );
    assert_eq!(derive_modern_headers(&request).get("Mcp-Name"), None);
}

#[test]
fn modernize_keeps_supplied_capabilities_and_sets_the_negotiated_version() {
    let params = json!({"_meta":{"io.modelcontextprotocol/clientCapabilities":{"elicitation":{}}}});
    let request = modernize_request(
        json!(3),
        "tools/call",
        &client(),
        &json!({}),
        "2026-08-01",
        Some(&params),
    );
    assert_eq!(
        request["params"]["_meta"]["io.modelcontextprotocol/clientCapabilities"],
        json!({"elicitation":{}})
    );
    assert_eq!(
        request["params"]["_meta"]["io.modelcontextprotocol/protocolVersion"],
        "2026-08-01"
    );
}

#[test]
fn preserves_result_envelopes_and_metadata() {
    for kind in ["complete", "input_required", "future_extension"] {
        let result = json!({"resultType":kind,"supportedVersions":["2026-07-28"],"capabilities":{},
            "_meta":{"io.modelcontextprotocol/serverInfo":{"name":"server","version":"1"}},"instructions":"fixture","ttlMs":100,"cacheScope":"public"});
        assert_eq!(
            Value::Object(
                parse_modern_result(&json!({"jsonrpc":"2.0","id":1,"result":result})).unwrap()
            ),
            result
        );
    }
}

#[test]
fn treats_an_absent_legacy_result_type_as_complete() {
    assert_eq!(
        Value::Object(
            parse_modern_result(&json!({"jsonrpc":"2.0","id":1,"result":{"tools":[]}})).unwrap()
        ),
        json!({"resultType":"complete","tools":[]})
    );
}

#[test]
fn rejects_malformed_result_envelopes() {
    for value in [
        Value::Null,
        json!([]),
        json!({}),
        json!({"result":null}),
        json!({"jsonrpc":"1.0","result":{}}),
        json!({"jsonrpc":"2.0","result":{"resultType":3}}),
        json!({"jsonrpc":"2.0","result":{},"error":{"code":-32603,"message":"failure"}}),
    ] {
        assert!(parse_modern_result(&value).is_err(), "{value}");
    }
}

#[test]
fn recognizes_unsupported_versions_by_code_and_retains_data_supported() {
    let body = rpc_error(
        -32022,
        Some(json!({"supported":["2026-07-28","future"],"requested":"old"})),
    );
    let error = classify_modern_error(&body).unwrap();
    assert_eq!(error.kind, ModernErrorKind::UnsupportedProtocolVersionError);
    assert_eq!(error.code, -32022);
    assert_eq!(error.message, "failure");
    assert_eq!(error.data, Some(body["error"]["data"].clone()));
    assert_eq!(
        error.supported_versions,
        Some(vec!["2026-07-28".to_owned(), "future".to_owned()])
    );
}

#[test]
fn keeps_modern_codes_modern_even_on_http_400_or_stdio() {
    for (code, kind) in [
        (-32020, ModernErrorKind::HeaderMismatch),
        (
            -32021,
            ModernErrorKind::MissingRequiredClientCapabilityError,
        ),
        (-32022, ModernErrorKind::UnsupportedProtocolVersionError),
    ] {
        let body = rpc_error(code, None);
        assert_eq!(
            classify_modern_error(&body).map(|error| error.kind),
            Some(kind)
        );
        assert_eq!(
            classify_http_probe_error(400, &Value::String(body.to_string())),
            ProbeErrorEra::Modern
        );
        assert_eq!(classify_stdio_probe_error(&body), ProbeErrorEra::Modern);
    }
}

#[test]
fn falls_back_only_for_empty_or_unrecognized_probe_bodies() {
    for body in [
        json!(""),
        json!("<html>bad request</html>"),
        json!("{"),
        Value::Null,
        json!({}),
        json!([]),
        rpc_error(-32601, None),
        rpc_error(-32602, None),
        rpc_error(-32603, None),
        rpc_error(-32099, None),
        json!({"jsonrpc":"2.0","error":{"code":-32022}}),
        json!({"error":{"code":-32022,"message":"failure"}}),
    ] {
        assert!(classify_modern_error(&body).is_none(), "{body}");
        assert_eq!(classify_http_probe_error(400, &body), ProbeErrorEra::Legacy);
        assert_eq!(classify_stdio_probe_error(&body), ProbeErrorEra::Legacy);
    }
}

#[test]
fn does_not_turn_other_http_statuses_into_a_legacy_fallback() {
    for status in [401, 403, 404, 429, 500] {
        assert_eq!(
            classify_http_probe_error(status, &json!("")),
            ProbeErrorEra::Indeterminate
        );
    }
}

#[test]
fn does_not_infer_modern_identity_from_an_error_message() {
    assert!(
        classify_modern_error(
            &json!({"jsonrpc":"2.0","error":{"code":-32602,"message":"HeaderMismatch"}})
        )
        .is_none()
    );
}
