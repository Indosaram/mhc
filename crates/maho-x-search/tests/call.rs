//! §8.6 bearer resolution, §8.7 request body, §8.8 status mapping, plus the extension path
//! (tool executor through the host credential store).

use crate::availability::env_of;
use crate::support::{self, FakeHttp, FakeRegistry, Reply, response};
use maho_ai::auth::types::CredentialType;
use maho_ai::utils::abort::AbortController;
use maho_ext_api::Extension;
use maho_x_search::call::{CallDeps, CredentialLookup, StoredCredential};
use maho_x_search::extension::registry_credentials;
use maho_x_search::request::HttpResponse;
use maho_x_search::{EnvLookup, run};
use serde_json::{Value, json};
use std::sync::Arc;
use std::time::Duration;

const BOUND: Duration = Duration::from_secs(5);

fn deps(http: Arc<FakeHttp>, env: EnvLookup) -> CallDeps {
    CallDeps {
        http,
        env,
        deadline: Duration::from_secs(60),
    }
}

fn stored(credential: StoredCredential) -> CredentialLookup {
    Arc::new(move || {
        let credential = credential.clone();
        let future: maho_x_search::call::CredentialFuture = Box::pin(async move { credential });
        future
    })
}

fn ok_body() -> Value {
    json!({"output": [], "usage": {}})
}

fn bearer(http: &FakeHttp) -> String {
    let requests = http.requests();
    let request = requests.last().expect("one request");
    request
        .headers
        .iter()
        .find(|(name, _)| name == "authorization")
        .map(|(_, value)| value.clone())
        .expect("authorization header")
}

fn code_of(details: &Value) -> Option<&str> {
    details.get("code").and_then(Value::as_str)
}

// ---- §8.6 bearer ------------------------------------------------------------------------

#[tokio::test]
async fn stored_credential_wins_over_env() {
    let http = FakeHttp::json(200, &ok_body());
    let out = run(
        &json!({"query": "q"}),
        &deps(http.clone(), env_of(&[("XAI_API_KEY", "env-key")])),
        &stored(StoredCredential::Present(Ok(Some(
            "  stored-key \n".into(),
        )))),
        None,
    )
    .await;
    assert!(!out.is_error, "{}", out.text);
    assert_eq!(bearer(&http), "Bearer stored-key");
}

#[tokio::test]
async fn stored_but_empty_or_failed_is_auth_without_env_fallback() {
    for credential in [
        StoredCredential::Present(Ok(Some("   ".into()))),
        StoredCredential::Present(Ok(None)),
        StoredCredential::Present(Err("refresh failed".into())),
    ] {
        let http = FakeHttp::json(200, &ok_body());
        let out = run(
            &json!({"query": "q"}),
            &deps(http.clone(), env_of(&[("XAI_API_KEY", "env-key")])),
            &stored(credential),
            None,
        )
        .await;
        assert!(out.is_error);
        assert_eq!(code_of(&out.details), Some("AUTH"));
        assert!(http.requests().is_empty());
    }
}

#[tokio::test]
async fn env_key_is_used_trimmed_when_nothing_is_stored() {
    let http = FakeHttp::json(200, &ok_body());
    let out = run(
        &json!({"query": "q"}),
        &deps(http.clone(), env_of(&[("XAI_API_KEY", "  env-key  ")])),
        &stored(StoredCredential::Absent),
        None,
    )
    .await;
    assert!(!out.is_error, "{}", out.text);
    assert_eq!(bearer(&http), "Bearer env-key");
}

#[tokio::test]
async fn no_credential_is_auth_and_sends_nothing() {
    let http = FakeHttp::json(200, &ok_body());
    let out = run(
        &json!({"query": "q"}),
        &deps(http.clone(), env_of(&[])),
        &stored(StoredCredential::Absent),
        None,
    )
    .await;
    assert!(out.is_error);
    assert_eq!(code_of(&out.details), Some("AUTH"));
    assert!(out.text.starts_with("x_search error [AUTH]: "));
    assert!(out.text.contains("/login") && out.text.contains("XAI_API_KEY"));
    assert!(http.requests().is_empty());
}

#[tokio::test]
async fn a_credential_changed_between_calls_is_picked_up() {
    let http = FakeHttp::json(200, &ok_body());
    let registry = FakeRegistry::new(Some(CredentialType::OAuth), Ok(Some("first".into())));
    let lookup = registry_credentials(registry.clone());
    let deps = deps(http.clone(), env_of(&[]));
    run(&json!({"query": "q"}), &deps, &lookup, None).await;
    assert_eq!(bearer(&http), "Bearer first");
    *registry.key.lock().expect("key") = Ok(Some("second".into()));
    run(&json!({"query": "q"}), &deps, &lookup, None).await;
    assert_eq!(bearer(&http), "Bearer second");
}

#[tokio::test]
async fn registry_without_stored_xai_falls_back_to_env() {
    let http = FakeHttp::json(200, &ok_body());
    let registry = FakeRegistry::new(None, Ok(Some("unused".into())));
    run(
        &json!({"query": "q"}),
        &deps(http.clone(), env_of(&[("XAI_API_KEY", "env-key")])),
        &registry_credentials(registry),
        None,
    )
    .await;
    assert_eq!(bearer(&http), "Bearer env-key");
}

#[tokio::test]
async fn registered_tool_runs_through_the_host_credential_store() {
    let dir = tempfile::tempdir().expect("dir");
    let http = FakeHttp::json(200, &ok_body());
    let mut extension =
        crate::availability::extension(dir.path(), env_of(&[("XAI_API_KEY", "env-key")]));
    extension.http = http.clone() as Arc<dyn maho_x_search::request::HttpClient>;
    let mut api = support::api(dir.path());
    extension.register(&mut api);
    let execute = api
        .runtime
        .extension_tool_executor(support::PATH, "x_search")
        .expect("x_search executor");
    let registry = FakeRegistry::new(Some(CredentialType::ApiKey), Ok(Some("stored".into())));
    let ctx = support::context(dir.path(), registry);
    let result = execute("call-1", json!({"query": "q"}), None, None, &ctx)
        .await
        .expect("tool result");
    assert_eq!(result.is_error, None);
    assert_eq!(bearer(&http), "Bearer stored");
    let result = execute("call-2", json!({"query": ""}), None, None, &ctx)
        .await
        .expect("tool result");
    assert_eq!(result.is_error, Some(true));
    assert_eq!(code_of(&result.details), Some("INVALID_PARAMS"));
}

// ---- §8.7 request body ------------------------------------------------------------------

#[tokio::test]
async fn request_body_carries_the_fixed_fields_and_filters() {
    let http = FakeHttp::json(200, &ok_body());
    run(
        &json!({
            "query": "rust release",
            "from_date": "2024-01-01",
            "to_date": "2024-01-31",
            "allowed_x_handles": ["rustlang", "rust_foundation"],
            "mode": "top",
            "max_results": 7,
        }),
        &deps(http.clone(), env_of(&[("XAI_API_KEY", "k")])),
        &stored(StoredCredential::Absent),
        None,
    )
    .await;
    let requests = http.requests();
    let request = &requests[0];
    assert_eq!(request.url, "https://api.x.ai/v1/responses");
    assert!(
        request
            .headers
            .contains(&("content-type".into(), "application/json".into()))
    );
    let body = &request.body;
    assert_eq!(body["model"], json!("grok-4.20-0309-non-reasoning"));
    assert_eq!(body["tool_choice"], json!("required"));
    assert_eq!(body["max_turns"], json!(1));
    assert_eq!(body["parallel_tool_calls"], json!(false));
    assert_eq!(body["max_output_tokens"], json!(4000));
    assert_eq!(body["store"], json!(false));
    assert!(body.get("reasoning").is_none());
    assert_eq!(
        body["tools"],
        json!([{
            "type": "x_search",
            "from_date": "2024-01-01",
            "to_date": "2024-01-31",
            "allowed_x_handles": ["rustlang", "rust_foundation"],
            "enable_image_understanding": false,
            "enable_video_understanding": false,
        }])
    );
    let input = body["input"].as_array().expect("input");
    assert_eq!(input.len(), 1);
    assert_eq!(input[0]["role"], json!("user"));
    let prompt = input[0]["content"].as_str().expect("prompt");
    assert!(
        prompt.contains("rust release since:2024-01-01 until:2024-01-31"),
        "{prompt}"
    );
    assert!(prompt.contains("top"));
    assert!(prompt.contains('7'));
}

#[tokio::test]
async fn excluded_handles_and_flags_are_copied() {
    let http = FakeHttp::json(200, &ok_body());
    run(
        &json!({"query": "q", "excluded_x_handles": ["spam"], "enable_video_understanding": true}),
        &deps(http.clone(), env_of(&[("XAI_API_KEY", "k")])),
        &stored(StoredCredential::Absent),
        None,
    )
    .await;
    let tool = &http.requests()[0].body["tools"][0];
    assert_eq!(tool["excluded_x_handles"], json!(["spam"]));
    assert!(tool.get("allowed_x_handles").is_none());
    assert!(tool.get("from_date").is_none());
    assert_eq!(tool["enable_image_understanding"], json!(false));
    assert_eq!(tool["enable_video_understanding"], json!(true));
    let prompt = http.requests()[0].body["input"][0]["content"]
        .as_str()
        .expect("prompt")
        .to_owned();
    assert!(prompt.contains("latest") && prompt.contains("10"));
    assert!(!prompt.contains("since:") && !prompt.contains("until:"));
}

#[tokio::test]
async fn model_override_env_is_respected() {
    for (env, expected) in [
        (vec![("OMO_X_SEARCH_MODEL", "grok-custom")], "grok-custom"),
        (
            vec![("OMO_X_SEARCH_MODEL", "  ")],
            "grok-4.20-0309-non-reasoning",
        ),
        (
            vec![
                ("OMO_X_SEARCH_MODEL", "grok-custom"),
                ("MAHO_X_SEARCH_MODEL", "grok-neutral"),
            ],
            "grok-neutral",
        ),
    ] {
        let http = FakeHttp::json(200, &ok_body());
        let mut pairs = env.clone();
        pairs.push(("XAI_API_KEY", "k"));
        run(
            &json!({"query": "q"}),
            &deps(http.clone(), env_of(&pairs)),
            &stored(StoredCredential::Absent),
            None,
        )
        .await;
        assert_eq!(http.requests()[0].body["model"], json!(expected), "{env:?}");
    }
}

// ---- §8.8 status mapping ----------------------------------------------------------------

async fn outcome(reply: Reply) -> maho_x_search::ToolOutput {
    let http = FakeHttp::new(reply);
    run(
        &json!({"query": "q"}),
        &deps(http, env_of(&[("XAI_API_KEY", "k")])),
        &stored(StoredCredential::Absent),
        None,
    )
    .await
}

#[tokio::test]
async fn http_statuses_map_to_codes() {
    let with_retry = HttpResponse {
        status: 429,
        retry_after: Some(30),
        body: String::new(),
    };
    for (reply, code) in [
        (Reply::Respond(response(401, "no")), "AUTH"),
        (Reply::Respond(response(403, "no")), "AUTH"),
        (Reply::Respond(response(429, "slow down")), "RATE_LIMITED"),
        (Reply::Respond(with_retry), "RATE_LIMITED"),
        (Reply::Respond(response(500, "boom")), "UPSTREAM"),
        (Reply::Respond(response(503, "busy")), "UPSTREAM"),
        (Reply::Respond(response(200, "<html>")), "PROTOCOL"),
        (Reply::Fail("connection reset".into()), "UPSTREAM"),
    ] {
        let out = outcome(reply).await;
        assert!(out.is_error);
        assert_eq!(code_of(&out.details), Some(code), "{}", out.text);
    }
    assert!(
        outcome(Reply::Respond(response(503, "")))
            .await
            .text
            .contains("503")
    );
    assert!(
        outcome(Reply::Fail("connection reset".into()))
            .await
            .text
            .contains("connection reset")
    );
}

#[test]
fn retry_after_is_parsed_when_numeric() {
    use maho_x_search::request::parse_retry_after;
    assert_eq!(parse_retry_after(Some(" 12 ")), Some(12));
    assert_eq!(
        parse_retry_after(Some("Wed, 21 Oct 2015 07:28:00 GMT")),
        None
    );
    assert_eq!(parse_retry_after(None), None);
}

#[tokio::test(start_paused = true)]
async fn deadline_is_timeout_with_the_ms() {
    let http = FakeHttp::new(Reply::Hang);
    let mut deps = deps(http, env_of(&[("XAI_API_KEY", "k")]));
    deps.deadline = Duration::from_millis(25);
    let out = run(
        &json!({"query": "q"}),
        &deps,
        &stored(StoredCredential::Absent),
        None,
    )
    .await;
    assert_eq!(code_of(&out.details), Some("TIMEOUT"));
    assert!(out.text.contains("25 ms"), "{}", out.text);
}

#[tokio::test]
async fn pre_cancelled_call_sends_nothing() {
    let http = FakeHttp::json(200, &ok_body());
    let controller = AbortController::new();
    controller.abort(None);
    let signal = controller.signal();
    let out = run(
        &json!({"query": "q"}),
        &deps(http.clone(), env_of(&[("XAI_API_KEY", "k")])),
        &stored(StoredCredential::Absent),
        Some(&signal),
    )
    .await;
    assert_eq!(code_of(&out.details), Some("UPSTREAM"));
    assert!(out.text.contains("cancel"));
    assert!(http.requests().is_empty());
}

#[tokio::test]
async fn mid_flight_cancellation_returns_immediately() {
    let http = FakeHttp::new(Reply::Hang);
    let controller = AbortController::new();
    let signal = controller.signal();
    let task = {
        let http = http.clone();
        tokio::spawn(async move {
            run(
                &json!({"query": "q"}),
                &deps(http, env_of(&[("XAI_API_KEY", "k")])),
                &stored(StoredCredential::Absent),
                Some(&signal),
            )
            .await
        })
    };
    tokio::time::timeout(BOUND, http.received.notified())
        .await
        .expect("request reached the transport");
    controller.abort(None);
    let out = tokio::time::timeout(BOUND, task)
        .await
        .expect("cancelled call settles")
        .expect("task");
    assert_eq!(code_of(&out.details), Some("UPSTREAM"));
    assert!(out.text.contains("cancel"));
}
