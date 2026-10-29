use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, LazyLock, Mutex};

use pretty_assertions::assert_eq;
use serde_json::{Value, json};
use utils::prompt_async_gate::*;
use utils::prompt_failure_classifier::{
    PromptDispatchFailure, is_ambiguous_post_dispatch_prompt_failure,
};

static TEST_LOCK: LazyLock<tokio::sync::Mutex<()>> = LazyLock::new(|| tokio::sync::Mutex::new(()));

type AsyncCallback = Arc<dyn Fn(&Value) -> BoxFuture<'static, Result<Value, String>> + Send + Sync>;
type StatusCallback = Arc<dyn Fn() -> BoxFuture<'static, Result<Value, String>> + Send + Sync>;
type MessagesCallback =
    Arc<dyn Fn(&str, &Value) -> BoxFuture<'static, Result<Value, String>> + Send + Sync>;

#[derive(Clone, Default)]
#[allow(dead_code)]
struct TestClient {
    prompt_async_fn: Option<AsyncCallback>,
    status_fn: Option<StatusCallback>,
    messages_fn: Option<MessagesCallback>,
    has_status_flag: bool,
    has_messages_flag: bool,
    has_prompt_async_flag: bool,
}

#[allow(dead_code)]
impl TestClient {
    fn with_prompt_async<F>(mut self, f: F) -> Self
    where
        F: Fn(&Value) -> Result<Value, String> + Send + Sync + 'static,
    {
        self.has_prompt_async_flag = true;
        self.prompt_async_fn = Some(Arc::new(move |val| {
            let res = f(val);
            Box::pin(async move { res })
        }));
        self
    }

    fn with_status<F>(mut self, f: F) -> Self
    where
        F: Fn() -> Result<Value, String> + Send + Sync + 'static,
    {
        self.has_status_flag = true;
        self.status_fn = Some(Arc::new(move || {
            let res = f();
            Box::pin(async move { res })
        }));
        self
    }

    fn with_messages<F>(mut self, f: F) -> Self
    where
        F: Fn(&str, &Value) -> Result<Value, String> + Send + Sync + 'static,
    {
        self.has_messages_flag = true;
        self.messages_fn = Some(Arc::new(move |sid, q| {
            let res = f(sid, q);
            Box::pin(async move { res })
        }));
        self
    }
}

impl PromptGateClient for TestClient {
    fn session_status(&self) -> impl std::future::Future<Output = Result<Value, String>> + Send {
        let f = self.status_fn.clone();
        async move {
            match f {
                Some(f) => f().await,
                None => Err("session.status unavailable".to_string()),
            }
        }
    }

    fn session_messages(
        &self,
        session_id: &str,
        query: &Value,
    ) -> impl std::future::Future<Output = Result<Value, String>> + Send {
        let f = self.messages_fn.clone();
        let sid = session_id.to_string();
        let q = query.clone();
        async move {
            match f {
                Some(f) => f(&sid, &q).await,
                None => Err("session.messages unavailable".to_string()),
            }
        }
    }

    fn prompt_async(
        &self,
        input: &Value,
    ) -> impl std::future::Future<Output = Result<Value, String>> + Send {
        let f = self.prompt_async_fn.clone();
        let inp = input.clone();
        async move {
            match f {
                Some(f) => f(&inp).await,
                None => Err("session.promptAsync unavailable".to_string()),
            }
        }
    }

    fn has_status(&self) -> bool {
        self.has_status_flag
    }

    fn has_messages(&self) -> bool {
        self.has_messages_flag
    }

    fn has_prompt_async(&self) -> bool {
        self.has_prompt_async_flag
    }
}

fn cleanup() {
    _set_prompt_gate_messages_fetch_timeout_ms_for_testing(None);
    release_all_prompt_async_reservations_for_testing();
}

fn reserve(session_id: &str, source: &str) {
    set_prompt_reservation(
        session_id,
        PromptAsyncReservation {
            source: source.to_string(),
            dedupe_key: "in-flight-stream".to_string(),
            reserved_at: 10_000,
            token: next_reservation_token(),
            expires_at: Some(70_000),
        },
    );
}

#[test]
fn test_bare_model_suggestion_retry_is_transient() {
    assert_eq!(
        is_transient_retry_reservation_owner(TRANSIENT_RETRY_RESERVATION_OWNER),
        true
    );
}

#[test]
fn test_model_suggestion_retry_variant_is_transient() {
    assert_eq!(
        is_transient_retry_reservation_owner("model-suggestion-retry:sync"),
        true
    );
    assert_eq!(
        is_transient_retry_reservation_owner("model-suggestion-retry:sync-retry"),
        true
    );
}

#[test]
fn test_unrelated_owner_is_not_transient() {
    assert_eq!(is_transient_retry_reservation_owner("user-prompt"), false);
    assert_eq!(is_transient_retry_reservation_owner("ralph-loop"), false);
    assert_eq!(
        is_transient_retry_reservation_owner("runtime-fallback:session.status"),
        false
    );
}

#[test]
fn test_matches_when_supersede_is_on_for_transient_retry() {
    assert_eq!(
        reservation_source_matches(
            "model-suggestion-retry",
            &["ralph-loop".to_string()],
            None,
            true,
        ),
        true
    );
    assert_eq!(
        reservation_source_matches(
            "model-suggestion-retry:sync",
            &["runtime-fallback:x".to_string()],
            None,
            true,
        ),
        true
    );
}

#[test]
fn test_does_not_match_foreign_source_when_supersede_is_off() {
    assert_eq!(
        reservation_source_matches(
            "model-suggestion-retry",
            &["ralph-loop".to_string()],
            None,
            false,
        ),
        false
    );
}

#[test]
fn test_user_prompt_reservation_with_supersede_is_not_matched() {
    assert_eq!(
        reservation_source_matches("user-prompt", &["ralph-loop".to_string()], None, true),
        false
    );
}

#[tokio::test]
async fn test_session_reserved_by_transient_retry_cleared_with_supersede() {
    let _guard = TEST_LOCK.lock().await;
    cleanup();
    let session_id = "session-transient-held";
    reserve(session_id, "model-suggestion-retry");

    let released = release_prompt_async_reservation(
        session_id,
        "ralph-loop",
        Some(
            &PromptAsyncReservationReleaseOptions::default()
                .with_supersede_transient_retry_owners(true),
        ),
    );

    assert_eq!(released, true);
    assert_eq!(get_prompt_reservation(session_id), None);
}

#[tokio::test]
async fn test_session_reserved_by_transient_retry_survives_without_supersede() {
    let _guard = TEST_LOCK.lock().await;
    cleanup();
    let session_id = "session-transient-deadlock";
    reserve(session_id, "model-suggestion-retry");

    let released = release_prompt_async_reservation(session_id, "ralph-loop", None);

    assert_eq!(released, false);
    let res = get_prompt_reservation(session_id);
    assert_eq!(
        res.map(|r| r.source),
        Some("model-suggestion-retry".to_string())
    );
}

#[tokio::test]
async fn test_user_prompt_reservation_preserved_with_supersede() {
    let _guard = TEST_LOCK.lock().await;
    cleanup();
    let session_id = "session-user-held";
    reserve(session_id, "user-prompt");

    let released = release_prompt_async_reservation(
        session_id,
        "runtime-fallback-abort:x",
        Some(
            &PromptAsyncReservationReleaseOptions::default()
                .with_reserved_by("runtime-fallback:x")
                .with_reserved_by_prefix("runtime-fallback:")
                .with_supersede_transient_retry_owners(true),
        ),
    );

    assert_eq!(released, false);
    let res = get_prompt_reservation(session_id);
    assert_eq!(res.map(|r| r.source), Some("user-prompt".to_string()));
}

#[tokio::test]
async fn test_fake_client_status_messages_prompt_async_all_invoked() {
    let _guard = TEST_LOCK.lock().await;
    cleanup();
    let methods_called = Arc::new(Mutex::new(Vec::new()));
    let methods_status = methods_called.clone();
    let methods_messages = methods_called.clone();
    let methods_prompt = methods_called.clone();

    let client = TestClient::default()
        .with_status(move || {
            methods_status.lock().unwrap().push("status".to_string());
            Ok(json!({}))
        })
        .with_messages(move |_, _| {
            methods_messages
                .lock()
                .unwrap()
                .push("messages".to_string());
            Ok(json!({ "data": [] }))
        })
        .with_prompt_async(move |_| {
            methods_prompt
                .lock()
                .unwrap()
                .push("promptAsync".to_string());
            Ok(json!({ "ok": true }))
        });

    let result = dispatch_internal_prompt(InternalPromptDispatchArgs {
        mode: InternalPromptDispatchMode::Async,
        client,
        session_id: "ses_identity_a".to_string(),
        input: json!({ "path": { "id": "ses_identity_a" }, "body": { "parts": [{ "type": "text", "text": "hi" }] } }),
        source: "test:identity:a".to_string(),
        dedupe_key: None,
        queue_behavior: None,
        queue: None,
        queue_retry_ms: None,
        settle_ms: Some(0),
        post_dispatch_hold_ms: Some(0),
        semantic_dedupe_hold_ms: None,
        dispatch_timeout_ms: None,
        check_status: Some(true),
        check_tool_state: Some(true),
        clock: None,
    })
    .await;

    assert_eq!(result.status(), "dispatched");
    let called = methods_called.lock().unwrap().clone();
    assert_eq!(called.contains(&"promptAsync".to_string()), true);
}

#[tokio::test]
async fn test_check_status_false_and_check_tool_state_false_never_invoked() {
    let _guard = TEST_LOCK.lock().await;
    cleanup();
    let methods_called = Arc::new(Mutex::new(Vec::new()));
    let methods_status = methods_called.clone();
    let methods_messages = methods_called.clone();
    let methods_prompt = methods_called.clone();

    let client = TestClient::default()
        .with_status(move || {
            methods_status.lock().unwrap().push("status".to_string());
            Ok(json!({}))
        })
        .with_messages(move |_, _| {
            methods_messages
                .lock()
                .unwrap()
                .push("messages".to_string());
            Ok(json!({ "data": [] }))
        })
        .with_prompt_async(move |_| {
            methods_prompt
                .lock()
                .unwrap()
                .push("promptAsync".to_string());
            Ok(json!({ "ok": true }))
        });

    let result = dispatch_internal_prompt(InternalPromptDispatchArgs {
        mode: InternalPromptDispatchMode::Async,
        client,
        session_id: "ses_identity_b".to_string(),
        input: json!({ "path": { "id": "ses_identity_b" }, "body": { "parts": [{ "type": "text", "text": "hi" }] } }),
        source: "test:identity:b".to_string(),
        dedupe_key: None,
        queue_behavior: None,
        queue: None,
        queue_retry_ms: None,
        settle_ms: Some(0),
        post_dispatch_hold_ms: Some(0),
        semantic_dedupe_hold_ms: None,
        dispatch_timeout_ms: None,
        check_status: Some(false),
        check_tool_state: Some(false),
        clock: None,
    })
    .await;

    assert_eq!(result.status(), "dispatched");
    let called = methods_called.lock().unwrap().clone();
    assert_eq!(called.contains(&"status".to_string()), false);
    assert_eq!(called.contains(&"messages".to_string()), false);
    assert_eq!(called.contains(&"promptAsync".to_string()), true);
}

#[tokio::test]
async fn test_session_status_busy_returns_active_prompt_not_called() {
    let _guard = TEST_LOCK.lock().await;
    cleanup();
    let prompt_called = Arc::new(Mutex::new(false));
    let prompt_called_clone = prompt_called.clone();

    let client = TestClient::default()
        .with_status(|| Ok(json!({ "data": { "ses_identity_c": { "type": "busy" } } })))
        .with_messages(|_, _| Ok(json!({ "data": [] })))
        .with_prompt_async(move |_| {
            *prompt_called_clone.lock().unwrap() = true;
            Ok(json!({ "ok": true }))
        });

    let result = dispatch_internal_prompt(InternalPromptDispatchArgs {
        mode: InternalPromptDispatchMode::Async,
        client,
        session_id: "ses_identity_c".to_string(),
        input: json!({ "path": { "id": "ses_identity_c" }, "body": { "parts": [{ "type": "text", "text": "hi" }] } }),
        source: "test:identity:c".to_string(),
        dedupe_key: None,
        queue_behavior: Some(InternalPromptQueueBehavior::Defer),
        queue: None,
        queue_retry_ms: None,
        settle_ms: Some(0),
        post_dispatch_hold_ms: Some(0),
        semantic_dedupe_hold_ms: None,
        dispatch_timeout_ms: None,
        check_status: Some(true),
        check_tool_state: None,
        clock: None,
    })
    .await;

    assert_eq!(result, InternalPromptDispatchResult::Active);
    assert_eq!(*prompt_called.lock().unwrap(), false);
}

#[tokio::test]
async fn test_dispatch_hold_active_second_dispatch_coalesced() {
    let _guard = TEST_LOCK.lock().await;
    cleanup();
    let prompt_calls = Arc::new(AtomicUsize::new(0));
    let prompt_calls_clone = prompt_calls.clone();

    let client = TestClient::default().with_prompt_async(move |_| {
        prompt_calls_clone.fetch_add(1, Ordering::SeqCst);
        Ok(json!({ "ok": true }))
    });

    let first = dispatch_internal_prompt(InternalPromptDispatchArgs {
        mode: InternalPromptDispatchMode::Async,
        client: client.clone(),
        session_id: "ses_identity_d".to_string(),
        input: json!({ "path": { "id": "ses_identity_d" }, "body": { "parts": [{ "type": "text", "text": "first" }] } }),
        source: "test:identity:d:first".to_string(),
        dedupe_key: None,
        queue_behavior: None,
        queue: None,
        queue_retry_ms: None,
        settle_ms: Some(0),
        post_dispatch_hold_ms: Some(10_000),
        semantic_dedupe_hold_ms: None,
        dispatch_timeout_ms: None,
        check_status: None,
        check_tool_state: None,
        clock: None,
    })
    .await;

    let second = dispatch_internal_prompt(InternalPromptDispatchArgs {
        mode: InternalPromptDispatchMode::Async,
        client,
        session_id: "ses_identity_d".to_string(),
        input: json!({ "path": { "id": "ses_identity_d" }, "body": { "parts": [{ "type": "text", "text": "first" }] } }),
        source: "test:identity:d:second".to_string(),
        dedupe_key: None,
        queue_behavior: None,
        queue: None,
        queue_retry_ms: None,
        settle_ms: Some(0),
        post_dispatch_hold_ms: Some(10_000),
        semantic_dedupe_hold_ms: None,
        dispatch_timeout_ms: None,
        check_status: None,
        check_tool_state: None,
        clock: None,
    })
    .await;

    assert_eq!(first.status(), "dispatched");
    assert_eq!(
        second.status() == "queued" || second.status() == "reserved",
        true
    );
    assert_eq!(prompt_calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn test_prompt_async_rejects_result_is_failed_dispatch_attempted_ambiguous() {
    let _guard = TEST_LOCK.lock().await;
    cleanup();

    let client = TestClient::default()
        .with_prompt_async(|_| Err("unexpected eof while reading response".to_string()));

    let result = dispatch_internal_prompt(InternalPromptDispatchArgs {
        mode: InternalPromptDispatchMode::Async,
        client,
        session_id: "ses_identity_e".to_string(),
        input: json!({ "path": { "id": "ses_identity_e" }, "body": { "parts": [{ "type": "text", "text": "wake" }] } }),
        source: "test:identity:e".to_string(),
        dedupe_key: None,
        queue_behavior: None,
        queue: None,
        queue_retry_ms: None,
        settle_ms: Some(0),
        post_dispatch_hold_ms: Some(0),
        semantic_dedupe_hold_ms: None,
        dispatch_timeout_ms: None,
        check_status: Some(false),
        check_tool_state: Some(false),
        clock: None,
    })
    .await;

    assert_eq!(result.status(), "failed");
    if let InternalPromptDispatchResult::Failed {
        error,
        dispatch_attempted,
    } = result
    {
        assert_eq!(dispatch_attempted, true);
        let failure = PromptDispatchFailure {
            error,
            dispatch_attempted,
        };
        assert_eq!(is_ambiguous_post_dispatch_prompt_failure(&failure), true);
    }
}
