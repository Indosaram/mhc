// Port of senpi mcp/modern/era.test.ts.
use maho_ext_mcp::modern::era::*;
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};

fn options() -> ProbeOptions {
    ProbeOptions {
        client_info: maho_ext_mcp::modern::wire::ModernClientInfo {
            name: "senpi".into(),
            version: "test".into(),
        },
        client_capabilities: json!({"elicitation":{}}),
        supported_versions: vec!["2026-07-28".into(), "2026-06-01".into()],
    }
}
fn discovery() -> Value {
    json!({"jsonrpc":"2.0","id":1,"result":{"resultType":"complete","supportedVersions":["2026-07-28"],"capabilities":{"tools":{}},
        "_meta":{"io.modelcontextprotocol/serverInfo":{"name":"fixture","version":"1"}}}})
}
fn rpc_error(code: i64, message: &str, data: Value) -> Value {
    json!({"jsonrpc":"2.0","id":1,"error":{"code":code,"message":message,"data":data}})
}
fn reply(body: &Value, status: u16) -> ProbeHttpResponse {
    ProbeHttpResponse {
        status,
        content_type: Some("application/json".into()),
        body: body.to_string(),
    }
}
fn url() -> url::Url {
    url::Url::parse("https://fixture.test/mcp").unwrap()
}
/// A scripted fetch: answers in order, the last answer repeating; records each request.
type Requests = Arc<Mutex<Vec<ProbeHttpRequest>>>;
fn scripted(
    answers: Vec<Result<ProbeHttpResponse, ProbeFetchError>>,
) -> (
    Requests,
    impl Fn(
        url::Url,
        ProbeHttpRequest,
    ) -> std::future::Ready<Result<ProbeHttpResponse, ProbeFetchError>>,
) {
    let requests: Requests = Arc::default();
    let seen = requests.clone();
    let fetch = move |_url: url::Url, request: ProbeHttpRequest| {
        let mut seen = seen.lock().unwrap();
        let index = seen.len().min(answers.len() - 1);
        seen.push(request);
        std::future::ready(answers[index].clone())
    };
    (requests, fetch)
}
fn answer_with(body: Value) -> impl Fn(Value) -> std::future::Ready<Result<Value, ProbeSendError>> {
    move |request: Value| {
        let mut reply = body.clone();
        reply["id"] = request["id"].clone();
        std::future::ready(Ok(reply))
    }
}
fn is_legacy(outcome: &EraOutcome, restart: bool, silent: bool) -> bool {
    *outcome == EraOutcome::LegacyFallback { restart, silent }
}

#[tokio::test]
async fn http_and_stdio_fall_back_when_only_legacy_versions_are_mutual() {
    let error = rpc_error(
        -32022,
        "UnsupportedProtocolVersionError",
        json!({"supported":["2025-06-18","2025-03-26"]}),
    );
    let (requests, fetch) = scripted(vec![Ok(reply(&error, 400))]);
    assert!(is_legacy(
        &probe_http_era(&url(), fetch, &options()).await.unwrap(),
        false,
        false
    ));
    assert_eq!(requests.lock().unwrap().len(), 1);
    let sends = Arc::new(Mutex::new(0));
    let counted = sends.clone();
    let send = move |request: Value| {
        *counted.lock().unwrap() += 1;
        answer_with(error.clone())(request)
    };
    assert!(is_legacy(
        &probe_stdio_era(send, &options()).await,
        false,
        false
    ));
    assert_eq!(*sends.lock().unwrap(), 1);
}

#[tokio::test]
async fn names_unsupported_server_versions_in_an_actionable_error() {
    let error = rpc_error(
        -32022,
        "UnsupportedProtocolVersionError",
        json!({"supported":["2099-01-01","1900-01-01"]}),
    );
    let (_, fetch) = scripted(vec![Ok(reply(&error, 400))]);
    for outcome in [
        probe_http_era(&url(), fetch, &options()).await.unwrap(),
        probe_stdio_era(answer_with(error.clone()), &options()).await,
    ] {
        let EraOutcome::Indeterminate {
            reason,
            action,
            modern_era,
            ..
        } = outcome
        else {
            panic!("expected an actionable negotiation error")
        };
        assert_eq!(reason, "no-mutual-version");
        assert!(modern_era);
        assert!(
            action.contains("2099-01-01") && action.contains("1900-01-01"),
            "{action}"
        );
        assert!(action.to_lowercase().contains("update"));
    }
}

#[tokio::test]
async fn still_retries_modern_when_both_eras_are_mutual() {
    let error = rpc_error(
        -32022,
        "UnsupportedProtocolVersionError",
        json!({"supported":["2025-06-18","2026-06-01"]}),
    );
    let (requests, fetch) = scripted(vec![Ok(reply(&error, 400)), Ok(reply(&discovery(), 200))]);
    let outcome = probe_http_era(&url(), fetch, &options()).await.unwrap();
    assert!(
        matches!(&outcome, EraOutcome::Modern { retry: true, version, .. } if version == "2026-06-01"),
        "{outcome:?}"
    );
    assert_eq!(requests.lock().unwrap().len(), 2);
    let calls = Arc::new(Mutex::new(0));
    let counted = calls.clone();
    let send = move |request: Value| {
        let mut calls = counted.lock().unwrap();
        *calls += 1;
        let body = if *calls == 1 {
            error.clone()
        } else {
            discovery()
        };
        answer_with(body)(request)
    };
    let outcome = probe_stdio_era(send, &options()).await;
    assert!(
        matches!(&outcome, EraOutcome::Modern { retry: true, version, .. } if version == "2026-06-01"),
        "{outcome:?}"
    );
    assert_eq!(*calls.lock().unwrap(), 2);
}

#[tokio::test]
async fn detects_modern_discovery_and_sends_matching_per_request_metadata_and_headers() {
    let (requests, fetch) = scripted(vec![Ok(reply(&discovery(), 200))]);
    let outcome = probe_http_era(&url(), fetch, &options()).await.unwrap();
    let EraOutcome::Modern {
        retry: false,
        version,
        discovery: Some(found),
        ..
    } = outcome
    else {
        panic!("expected modern-OK")
    };
    assert_eq!(version, "2026-07-28");
    assert_eq!(Value::Object(found), discovery()["result"]);
    let requests = requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    let headers = &requests[0].headers;
    assert_eq!(headers.len(), 4);
    assert_eq!(headers["Content-Type"], "application/json");
    assert_eq!(headers["Accept"], "application/json, text/event-stream");
    assert_eq!(headers["MCP-Protocol-Version"], "2026-07-28");
    assert_eq!(headers["Mcp-Method"], "server/discover");
    assert_eq!(
        serde_json::from_str::<Value>(&requests[0].body).unwrap(),
        json!({"jsonrpc":"2.0","id":1,"method":"server/discover","params":{"_meta":{
            "io.modelcontextprotocol/protocolVersion":"2026-07-28","io.modelcontextprotocol/clientInfo":{"name":"senpi","version":"test"},
            "io.modelcontextprotocol/clientCapabilities":{"elicitation":{}}}}})
    );
}

#[tokio::test]
async fn retries_exactly_once_with_the_newest_mutual_version() {
    let error = rpc_error(
        -32022,
        "UnsupportedProtocolVersionError",
        json!({"supported":["2026-01-01","2026-06-01","2026-07-28"]}),
    );
    let (requests, fetch) = scripted(vec![Ok(reply(&error, 400)), Ok(reply(&discovery(), 200))]);
    let outcome = probe_http_era(&url(), fetch, &options()).await.unwrap();
    assert!(
        matches!(&outcome, EraOutcome::Modern { retry: true, version, .. } if version == "2026-07-28")
    );
    let requests = requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    let body: Value = serde_json::from_str(&requests[1].body).unwrap();
    assert_eq!(
        requests[1].headers["MCP-Protocol-Version"],
        body["params"]["_meta"]["io.modelcontextprotocol/protocolVersion"]
    );
}

#[tokio::test]
async fn falls_back_on_an_empty_or_unrecognized_http_400_body() {
    for body in [
        "".to_owned(),
        "not JSON".to_owned(),
        rpc_error(-32601, "Method not found", json!({})).to_string(),
    ] {
        let (requests, fetch) = scripted(vec![Ok(ProbeHttpResponse {
            status: 400,
            content_type: None,
            body: body.clone(),
        })]);
        assert!(
            is_legacy(
                &probe_http_era(&url(), fetch, &options()).await.unwrap(),
                false,
                false
            ),
            "{body}"
        );
        assert_eq!(requests.lock().unwrap().len(), 1);
    }
}

#[tokio::test]
async fn does_not_infer_modern_from_generic_error_text() {
    for name in ["HeaderMismatch", "MissingCapabilityError"] {
        let error = rpc_error(-32000, name, json!({"type":name}));
        let (_, fetch) = scripted(vec![Ok(reply(&error, 400))]);
        assert!(is_legacy(
            &probe_http_era(&url(), fetch, &options()).await.unwrap(),
            false,
            false
        ));
        assert!(is_legacy(
            &probe_stdio_era(answer_with(error.clone()), &options()).await,
            false,
            false
        ));
    }
}

#[tokio::test]
async fn retains_modern_classification_on_modern_error_codes() {
    for (code, name) in [
        (-32020, "HeaderMismatch"),
        (-32021, "MissingRequiredClientCapabilityError"),
    ] {
        let error = rpc_error(code, name, json!({}));
        let (_, fetch) = scripted(vec![Ok(reply(&error, 400))]);
        let outcome = probe_http_era(&url(), fetch, &options()).await.unwrap();
        assert!(
            matches!(&outcome, EraOutcome::Modern { retry: false, error: Some(found), .. } if found.code == code),
            "{outcome:?}"
        );
    }
}

#[tokio::test]
async fn surfaces_an_actionable_error_on_http_failure_statuses() {
    for status in [401, 403, 404, 429, 500] {
        let (_, fetch) = scripted(vec![Ok(reply(&json!({}), status))]);
        let outcome = probe_http_era(&url(), fetch, &options()).await.unwrap();
        assert!(
            matches!(&outcome, EraOutcome::Indeterminate { reason, status: Some(found), .. } if reason == "http-status" && *found == status)
        );
    }
}

#[tokio::test]
async fn falls_back_when_any_status_carries_a_generic_json_rpc_error() {
    for status in [200, 404] {
        let (requests, fetch) = scripted(vec![Ok(reply(
            &rpc_error(-32601, "Method not found", json!({})),
            status,
        ))]);
        assert!(is_legacy(
            &probe_http_era(&url(), fetch, &options()).await.unwrap(),
            false,
            false
        ));
        assert_eq!(requests.lock().unwrap().len(), 1);
    }
}

#[tokio::test]
async fn passes_an_authorization_failure_through_instead_of_hiding_it() {
    let needs_auth = maho_ext_mcp::errors::McpError::new(
        maho_ext_mcp::errors::McpErrorKind::Auth,
        "unauthorized",
    );
    let (_, fetch) = scripted(vec![Err(ProbeFetchError::NeedsAuth(Box::new(needs_auth)))]);
    let error = probe_http_era(&url(), fetch, &options()).await.unwrap_err();
    assert_eq!(error.message, "unauthorized");
}

#[tokio::test]
async fn surfaces_network_failures_rather_than_selecting_legacy() {
    let (_, fetch) = scripted(vec![Err(ProbeFetchError::Transport("offline".into()))]);
    let outcome = probe_http_era(&url(), fetch, &options()).await.unwrap();
    assert!(
        matches!(&outcome, EraOutcome::Indeterminate { reason, action, .. } if reason == "transport" && !action.is_empty())
    );
}

#[tokio::test]
async fn does_not_downgrade_on_absent_mutual_versions_or_a_rejected_retry() {
    let error = rpc_error(
        -32022,
        "UnsupportedProtocolVersionError",
        json!({"supported":["2026-06-01"]}),
    );
    let (requests, fetch) = scripted(vec![Ok(reply(&error, 400))]);
    let outcome = probe_http_era(&url(), fetch, &options()).await.unwrap();
    assert!(
        matches!(&outcome, EraOutcome::Indeterminate { reason, modern_era: true, .. } if reason == "version-retry-rejected")
    );
    assert_eq!(requests.lock().unwrap().len(), 2);
    let (_, fetch) = scripted(vec![Ok(reply(&error, 400))]);
    let only_latest = ProbeOptions {
        supported_versions: vec!["2026-07-28".into()],
        ..options()
    };
    let outcome = probe_http_era(&url(), fetch, &only_latest).await.unwrap();
    assert!(
        matches!(&outcome, EraOutcome::Indeterminate { reason, modern_era: true, .. } if reason == "no-mutual-version")
    );
}

#[tokio::test]
async fn rejects_a_malformed_success_instead_of_assuming_legacy() {
    let (_, fetch) = scripted(vec![Ok(reply(&json!({"result":{}}), 200))]);
    let outcome = probe_http_era(&url(), fetch, &options()).await.unwrap();
    assert!(
        matches!(&outcome, EraOutcome::Indeterminate { reason, .. } if reason == "invalid-discovery")
    );
}

#[tokio::test]
async fn consumes_a_per_request_sse_discovery_response() {
    let body = format!("event: message\ndata: {}\n\n", discovery());
    let (requests, fetch) = scripted(vec![Ok(ProbeHttpResponse {
        status: 200,
        content_type: Some("text/event-stream".into()),
        body,
    })]);
    assert!(matches!(
        probe_http_era(&url(), fetch, &options()).await.unwrap(),
        EraOutcome::Modern { retry: false, .. }
    ));
    assert_eq!(requests.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn sends_discover_with_only_metadata_under_a_string_id() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let recorded = seen.clone();
    let send = move |request: Value| {
        recorded.lock().unwrap().push(request.clone());
        answer_with(discovery())(request)
    };
    assert!(matches!(
        probe_stdio_era(send, &options()).await,
        EraOutcome::Modern { retry: false, .. }
    ));
    let request = seen.lock().unwrap()[0].clone();
    assert_eq!(request["method"], "server/discover");
    assert!(request["id"].is_string());
    assert_eq!(
        request["params"]
            .as_object()
            .unwrap()
            .keys()
            .collect::<Vec<_>>(),
        vec!["_meta"]
    );
}

#[tokio::test]
async fn does_not_accept_a_discovery_reply_addressed_to_another_id() {
    let outcome = probe_stdio_era(|_| std::future::ready(Ok(discovery())), &options()).await;
    assert!(is_legacy(&outcome, true, false));
}

#[tokio::test]
async fn recognizes_modern_stdio_errors() {
    for code in [-32020, -32021] {
        let outcome = probe_stdio_era(
            answer_with(rpc_error(code, &code.to_string(), json!({}))),
            &options(),
        )
        .await;
        assert!(matches!(outcome, EraOutcome::Modern { retry: false, .. }));
    }
}

#[tokio::test]
async fn stdio_retries_once_with_the_newest_mutual_version_on_32022() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let recorded = seen.clone();
    let send = move |request: Value| {
        let mut seen = recorded.lock().unwrap();
        seen.push(request.clone());
        let body = if seen.len() == 1 {
            rpc_error(
                -32022,
                "Unsupported",
                json!({"supported":["2026-06-01","2027-01-01"]}),
            )
        } else {
            discovery()
        };
        answer_with(body)(request)
    };
    let outcome = probe_stdio_era(send, &options()).await;
    assert!(
        matches!(&outcome, EraOutcome::Modern { retry: true, version, .. } if version == "2026-06-01")
    );
    let seen = seen.lock().unwrap();
    assert_eq!(seen.len(), 2);
    assert_eq!(
        seen[1]["params"]["_meta"]["io.modelcontextprotocol/protocolVersion"],
        "2026-06-01"
    );
    assert_ne!(seen[1]["id"], seen[0]["id"]);
}

#[tokio::test]
async fn keeps_the_process_for_a_clean_nonmodern_error_reply() {
    let outcome = probe_stdio_era(
        answer_with(rpc_error(-32601, "Method not found", json!({}))),
        &options(),
    )
    .await;
    assert!(is_legacy(&outcome, false, false));
}

#[tokio::test]
async fn asks_for_a_fresh_process_after_an_unclean_reply() {
    for body in [json!({}), Value::Null, json!({"jsonrpc":"2.0","result":{}})] {
        let outcome = probe_stdio_era(|_| std::future::ready(Ok(body.clone())), &options()).await;
        assert!(is_legacy(&outcome, true, false), "{body}");
    }
}

#[tokio::test]
async fn keeps_the_process_on_silence_so_a_slow_start_is_not_killed() {
    let outcome = probe_stdio_era(
        |_| std::future::ready(Err(ProbeSendError::Silence)),
        &options(),
    )
    .await;
    assert!(is_legacy(&outcome, false, true));
}

#[tokio::test]
async fn never_downgrades_a_server_that_already_answered_32022() {
    for retry in [
        Err(ProbeSendError::Silence),
        Err(ProbeSendError::Closed("closed".into())),
        Ok(rpc_error(-32601, "Method not found", json!({}))),
    ] {
        let calls = Arc::new(Mutex::new(0));
        let counted = calls.clone();
        let send = move |request: Value| {
            let mut calls = counted.lock().unwrap();
            *calls += 1;
            if *calls == 1 {
                return answer_with(rpc_error(
                    -32022,
                    "Unsupported",
                    json!({"supported":["2026-06-01"]}),
                ))(request);
            }
            std::future::ready(retry.clone().map(|mut body| {
                body["id"] = request["id"].clone();
                body
            }))
        };
        let outcome = probe_stdio_era(send, &options()).await;
        assert!(
            matches!(
                outcome,
                EraOutcome::Indeterminate {
                    modern_era: true,
                    ..
                }
            ),
            "{outcome:?}"
        );
    }
}

#[tokio::test]
async fn asks_for_a_fresh_process_after_a_transport_failure_or_exit() {
    let outcome = probe_stdio_era(
        |_| std::future::ready(Err(ProbeSendError::Closed("closed".into()))),
        &options(),
    )
    .await;
    assert!(is_legacy(&outcome, true, false));
}
