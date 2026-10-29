use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;

use pretty_assertions::assert_eq;
use serde_json::{Value, json};
use utils::prompt_async_gate::*;

static TEST_LOCK: LazyLock<tokio::sync::Mutex<()>> = LazyLock::new(|| tokio::sync::Mutex::new(()));

type AsyncCallback = Arc<dyn Fn(&Value) -> BoxFuture<'static, Result<Value, String>> + Send + Sync>;
type StatusCallback = Arc<dyn Fn() -> BoxFuture<'static, Result<Value, String>> + Send + Sync>;
type MessagesCallback =
    Arc<dyn Fn(&str, &Value) -> BoxFuture<'static, Result<Value, String>> + Send + Sync>;

#[derive(Clone, Default)]
#[allow(dead_code)]
struct TestClient {
    prompt_async_fn: Option<AsyncCallback>,
    prompt_fn: Option<AsyncCallback>,
    status_fn: Option<StatusCallback>,
    messages_fn: Option<MessagesCallback>,
    has_status_flag: bool,
    has_messages_flag: bool,
    has_prompt_async_flag: bool,
    has_prompt_flag: bool,
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

    fn with_prompt<F>(mut self, f: F) -> Self
    where
        F: Fn(&Value) -> Result<Value, String> + Send + Sync + 'static,
    {
        self.has_prompt_flag = true;
        self.prompt_fn = Some(Arc::new(move |val| {
            let res = f(val);
            Box::pin(async move { res })
        }));
        self
    }

    fn with_prompt_fut<F, Fut>(mut self, f: F) -> Self
    where
        F: Fn(&Value) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = Result<Value, String>> + Send + 'static,
    {
        self.has_prompt_flag = true;
        self.prompt_fn = Some(Arc::new(move |val| Box::pin(f(val))));
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

    fn with_status_fut<F, Fut>(mut self, f: F) -> Self
    where
        F: Fn() -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = Result<Value, String>> + Send + 'static,
    {
        self.has_status_flag = true;
        self.status_fn = Some(Arc::new(move || Box::pin(f())));
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

    fn with_messages_fut<F, Fut>(mut self, f: F) -> Self
    where
        F: Fn(&str, &Value) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = Result<Value, String>> + Send + 'static,
    {
        self.has_messages_flag = true;
        self.messages_fn = Some(Arc::new(move |sid, q| Box::pin(f(sid, q))));
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

    fn prompt(
        &self,
        input: &Value,
    ) -> impl std::future::Future<Output = Result<Value, String>> + Send {
        let f = self.prompt_fn.clone();
        let inp = input.clone();
        async move {
            match f {
                Some(f) => f(&inp).await,
                None => Err("session.prompt unavailable".to_string()),
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

    fn has_prompt(&self) -> bool {
        self.has_prompt_flag
    }
}

fn cleanup() {
    _set_prompt_gate_messages_fetch_timeout_ms_for_testing(None);
    release_all_prompt_async_reservations_for_testing();
}

#[tokio::test]
async fn test_prompt_async_rejects_post_dispatch_hold_blocks_duplicate() {
    let _guard = TEST_LOCK.lock().await;
    cleanup();
    let prompt_calls = Arc::new(AtomicUsize::new(0));
    let prompt_calls_clone = prompt_calls.clone();

    let client = TestClient::default().with_prompt_async(move |_| {
        prompt_calls_clone.fetch_add(1, Ordering::SeqCst);
        Err("post-dispatch failure".to_string())
    });

    let first = dispatch_internal_prompt(InternalPromptDispatchArgs {
        mode: InternalPromptDispatchMode::Async,
        client: client.clone(),
        session_id: "ses_post_dispatch_reject".to_string(),
        input: json!({ "path": { "id": "ses_post_dispatch_reject" }, "body": { "parts": [] } }),
        source: "test:reject:first".to_string(),
        dedupe_key: None,
        queue_behavior: None,
        queue: None,
        queue_retry_ms: None,
        settle_ms: Some(0),
        post_dispatch_hold_ms: None,
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
        session_id: "ses_post_dispatch_reject".to_string(),
        input: json!({ "path": { "id": "ses_post_dispatch_reject" }, "body": { "parts": [] } }),
        source: "test:reject:second".to_string(),
        dedupe_key: None,
        queue_behavior: None,
        queue: None,
        queue_retry_ms: None,
        settle_ms: Some(0),
        post_dispatch_hold_ms: None,
        semantic_dedupe_hold_ms: None,
        dispatch_timeout_ms: None,
        check_status: None,
        check_tool_state: None,
        clock: None,
    })
    .await;

    assert_eq!(first.status(), "failed");
    if let InternalPromptDispatchResult::Failed {
        dispatch_attempted, ..
    } = first
    {
        assert_eq!(dispatch_attempted, true);
    }
    assert_eq!(
        second,
        InternalPromptDispatchResult::Queued {
            queued_by: "test:reject:first".to_string(),
            position: 0,
        }
    );
    assert_eq!(prompt_calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn test_sibling_route_strict_family_prefix_does_not_clear_sibling() {
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
        session_id: "ses_prefix_sibling".to_string(),
        input: json!({ "path": { "id": "ses_prefix_sibling" }, "body": { "parts": [{ "type": "text", "text": "continue" }] } }),
        source: "model-fallbackx:message.updated".to_string(),
        dedupe_key: None,
        queue_behavior: None,
        queue: None,
        queue_retry_ms: None,
        settle_ms: Some(0),
        post_dispatch_hold_ms: None,
        semantic_dedupe_hold_ms: None,
        dispatch_timeout_ms: None,
        check_status: None,
        check_tool_state: None,
        clock: None,
    })
    .await;

    let released = release_prompt_async_reservation(
        "ses_prefix_sibling",
        "model-fallback-abort:session.error",
        Some(
            &PromptAsyncReservationReleaseOptions::default()
                .with_reserved_by_prefix("model-fallback:"),
        ),
    );

    let second = dispatch_internal_prompt(InternalPromptDispatchArgs {
        mode: InternalPromptDispatchMode::Async,
        client,
        session_id: "ses_prefix_sibling".to_string(),
        input: json!({ "path": { "id": "ses_prefix_sibling" }, "body": { "parts": [{ "type": "text", "text": "continue again" }] } }),
        source: "model-fallback:session.error".to_string(),
        dedupe_key: None,
        queue_behavior: None,
        queue: None,
        queue_retry_ms: None,
        settle_ms: Some(0),
        post_dispatch_hold_ms: Some(0),
        semantic_dedupe_hold_ms: None,
        dispatch_timeout_ms: None,
        check_status: None,
        check_tool_state: None,
        clock: None,
    })
    .await;

    assert_eq!(first.status(), "dispatched");
    assert_eq!(released, false);
    assert_eq!(
        second,
        InternalPromptDispatchResult::Queued {
            queued_by: "model-fallbackx:message.updated".to_string(),
            position: 1,
        }
    );
    assert_eq!(prompt_calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn test_two_sync_prompt_calls_race_only_one_accepted() {
    let _guard = TEST_LOCK.lock().await;
    cleanup();
    let prompt_calls = Arc::new(AtomicUsize::new(0));
    let prompt_calls_clone = prompt_calls.clone();
    let (gate_tx, gate_rx) = tokio::sync::oneshot::channel::<()>();
    let gate_rx = Arc::new(tokio::sync::Mutex::new(Some(gate_rx)));

    let client = TestClient::default()
        .with_status(|| Ok(json!({ "data": { "ses_prompt_race": { "type": "idle" } } })))
        .with_prompt_fut(move |_| {
            let calls = prompt_calls_clone.clone();
            let gate_rx_clone = gate_rx.clone();
            async move {
                calls.fetch_add(1, Ordering::SeqCst);
                if let Some(rx) = gate_rx_clone.lock().await.take() {
                    let _ = rx.await;
                }
                Ok(json!({ "ok": true }))
            }
        });

    let client_clone = client.clone();
    let first_handle = tokio::spawn(async move {
        dispatch_internal_prompt(InternalPromptDispatchArgs {
            mode: InternalPromptDispatchMode::Sync,
            client: client_clone,
            session_id: "ses_prompt_race".to_string(),
            input: json!({ "path": { "id": "ses_prompt_race" }, "body": { "parts": [] } }),
            source: "test:prompt:first".to_string(),
            dedupe_key: None,
            queue_behavior: None,
            queue: None,
            queue_retry_ms: None,
            settle_ms: Some(0),
            post_dispatch_hold_ms: Some(0),
            semantic_dedupe_hold_ms: None,
            dispatch_timeout_ms: None,
            check_status: None,
            check_tool_state: None,
            clock: None,
        })
        .await
    });

    tokio::time::sleep(Duration::from_millis(10)).await;

    let second = dispatch_internal_prompt(InternalPromptDispatchArgs {
        mode: InternalPromptDispatchMode::Sync,
        client,
        session_id: "ses_prompt_race".to_string(),
        input: json!({ "path": { "id": "ses_prompt_race" }, "body": { "parts": [] } }),
        source: "test:prompt:second".to_string(),
        dedupe_key: None,
        queue_behavior: None,
        queue: None,
        queue_retry_ms: None,
        settle_ms: Some(0),
        post_dispatch_hold_ms: Some(0),
        semantic_dedupe_hold_ms: None,
        dispatch_timeout_ms: None,
        check_status: None,
        check_tool_state: None,
        clock: None,
    })
    .await;

    let _ = gate_tx.send(());
    let first_result = first_handle.await.unwrap();

    assert_eq!(first_result.status(), "dispatched");
    assert_eq!(second.status(), "reserved");
    assert_eq!(prompt_calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn test_sync_prompt_default_dispatch_hold_keeps_session_reserved() {
    let _guard = TEST_LOCK.lock().await;
    cleanup();
    let prompt_calls = Arc::new(AtomicUsize::new(0));
    let prompt_calls_clone = prompt_calls.clone();

    let client = TestClient::default().with_prompt(move |_| {
        prompt_calls_clone.fetch_add(1, Ordering::SeqCst);
        Ok(json!({ "ok": true }))
    });

    let first = dispatch_internal_prompt(InternalPromptDispatchArgs {
        mode: InternalPromptDispatchMode::Sync,
        client: client.clone(),
        session_id: "ses_prompt_hold_after_dispatch".to_string(),
        input: json!({ "path": { "id": "ses_prompt_hold_after_dispatch" }, "body": { "parts": [] } }),
        source: "test:prompt-hold:first".to_string(),
        dedupe_key: None,
        queue_behavior: None,
        queue: None,
        queue_retry_ms: None,
        settle_ms: Some(0),
        post_dispatch_hold_ms: None,
        semantic_dedupe_hold_ms: None,
        dispatch_timeout_ms: None,
        check_status: None,
        check_tool_state: None,
        clock: None,
    })
    .await;

    let second = dispatch_internal_prompt(InternalPromptDispatchArgs {
        mode: InternalPromptDispatchMode::Sync,
        client,
        session_id: "ses_prompt_hold_after_dispatch".to_string(),
        input: json!({ "path": { "id": "ses_prompt_hold_after_dispatch" }, "body": { "parts": [] } }),
        source: "test:prompt-hold:second".to_string(),
        dedupe_key: None,
        queue_behavior: None,
        queue: None,
        queue_retry_ms: None,
        settle_ms: Some(0),
        post_dispatch_hold_ms: None,
        semantic_dedupe_hold_ms: None,
        dispatch_timeout_ms: None,
        check_status: None,
        check_tool_state: None,
        clock: None,
    })
    .await;

    assert_eq!(first.status(), "dispatched");
    assert_eq!(second.status(), "reserved");
    assert_eq!(prompt_calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn test_sdk_prompt_receiver_state_preserved() {
    let _guard = TEST_LOCK.lock().await;
    cleanup();
    struct ReceiverClient {
        accepted: bool,
    }
    impl PromptGateClient for ReceiverClient {
        fn has_prompt(&self) -> bool {
            true
        }
        fn prompt(
            &self,
            input: &Value,
        ) -> impl std::future::Future<Output = Result<Value, String>> + Send {
            let accepted = self.accepted;
            let sid = input["path"]["id"].as_str().unwrap_or("").to_string();
            async move { Ok(json!({ "accepted": accepted, "sessionID": sid })) }
        }
    }

    let client = ReceiverClient { accepted: true };
    let result = dispatch_internal_prompt(InternalPromptDispatchArgs {
        mode: InternalPromptDispatchMode::Sync,
        client,
        session_id: "ses_bound_prompt".to_string(),
        input: json!({ "path": { "id": "ses_bound_prompt" }, "body": { "parts": [] } }),
        source: "test:bound-prompt".to_string(),
        dedupe_key: None,
        queue_behavior: None,
        queue: None,
        queue_retry_ms: None,
        settle_ms: Some(0),
        post_dispatch_hold_ms: Some(0),
        semantic_dedupe_hold_ms: None,
        dispatch_timeout_ms: None,
        check_status: None,
        check_tool_state: None,
        clock: None,
    })
    .await;

    assert_eq!(
        result,
        InternalPromptDispatchResult::Dispatched {
            response: json!({ "accepted": true, "sessionID": "ses_bound_prompt" }),
        }
    );
}

#[tokio::test]
async fn test_session_status_never_resolves_times_out_and_dispatches() {
    let _guard = TEST_LOCK.lock().await;
    cleanup();
    let prompt_calls = Arc::new(AtomicUsize::new(0));
    let prompt_calls_clone = prompt_calls.clone();

    let client = TestClient::default()
        .with_status_fut(|| async {
            tokio::time::sleep(Duration::from_millis(500)).await;
            Ok(json!({ "data": { "ses_status_hang": { "type": "idle" } } }))
        })
        .with_prompt_async(move |_| {
            prompt_calls_clone.fetch_add(1, Ordering::SeqCst);
            Ok(json!({ "ok": true }))
        });

    let result = dispatch_internal_prompt(InternalPromptDispatchArgs {
        mode: InternalPromptDispatchMode::Async,
        client,
        session_id: "ses_status_hang".to_string(),
        input: json!({ "path": { "id": "ses_status_hang" }, "body": { "parts": [] } }),
        source: "test:status-hang".to_string(),
        dedupe_key: None,
        queue_behavior: None,
        queue: None,
        queue_retry_ms: None,
        settle_ms: Some(0),
        post_dispatch_hold_ms: Some(0),
        semantic_dedupe_hold_ms: None,
        dispatch_timeout_ms: Some(50),
        check_status: None,
        check_tool_state: None,
        clock: None,
    })
    .await;

    assert_eq!(result.status(), "dispatched");
    assert_eq!(prompt_calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn test_session_status_hangs_forever_dispatches_after_timeout() {
    let _guard = TEST_LOCK.lock().await;
    cleanup();
    let prompt_calls = Arc::new(AtomicUsize::new(0));
    let prompt_calls_clone = prompt_calls.clone();

    let client = TestClient::default()
        .with_status_fut(|| async {
            tokio::time::sleep(Duration::from_millis(500)).await;
            Ok(json!({}))
        })
        .with_prompt_async(move |_| {
            prompt_calls_clone.fetch_add(1, Ordering::SeqCst);
            Ok(json!({ "ok": true }))
        });

    let result = dispatch_internal_prompt(InternalPromptDispatchArgs {
        mode: InternalPromptDispatchMode::Async,
        client,
        session_id: "ses_status_timeout".to_string(),
        input: json!({ "path": { "id": "ses_status_timeout" }, "body": { "parts": [] } }),
        source: "test:status-timeout".to_string(),
        dedupe_key: None,
        queue_behavior: None,
        queue: None,
        queue_retry_ms: None,
        settle_ms: Some(0),
        post_dispatch_hold_ms: Some(0),
        semantic_dedupe_hold_ms: None,
        dispatch_timeout_ms: Some(50),
        check_status: None,
        check_tool_state: None,
        clock: None,
    })
    .await;

    assert_eq!(result.status(), "dispatched");
    assert_eq!(prompt_calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn test_sync_prompt_rejects_object_form_session_path_retries_with_string() {
    let _guard = TEST_LOCK.lock().await;
    cleanup();
    let calls = Arc::new(Mutex::new(Vec::new()));
    let calls_clone = calls.clone();

    let client = TestClient::default().with_prompt(move |input| {
        calls_clone.lock().unwrap().push(input["path"].clone());
        if !input["path"].is_string() {
            return Err("The \"path\" property must be of type string, got object".to_string());
        }
        Ok(json!({ "ok": true }))
    });

    let result = dispatch_internal_prompt(InternalPromptDispatchArgs {
        mode: InternalPromptDispatchMode::Sync,
        client,
        session_id: "ses_sync_path_compat".to_string(),
        input: json!({
            "path": { "id": "ses_sync_path_compat" },
            "body": { "parts": [] }
        }),
        source: "test:path-compat:sync".to_string(),
        dedupe_key: None,
        queue_behavior: Some(InternalPromptQueueBehavior::Defer),
        queue: None,
        queue_retry_ms: None,
        settle_ms: Some(0),
        post_dispatch_hold_ms: None,
        semantic_dedupe_hold_ms: None,
        dispatch_timeout_ms: None,
        check_status: Some(false),
        check_tool_state: Some(false),
        clock: None,
    })
    .await;

    assert_eq!(result.status(), "dispatched");
    assert_eq!(
        *calls.lock().unwrap(),
        vec![
            json!({ "id": "ses_sync_path_compat" }),
            json!("ses_sync_path_compat")
        ]
    );
}

#[tokio::test]
async fn test_async_prompt_rejects_object_form_session_path_retries_with_string() {
    let _guard = TEST_LOCK.lock().await;
    cleanup();
    let calls = Arc::new(Mutex::new(Vec::new()));
    let calls_clone = calls.clone();

    let client = TestClient::default().with_prompt_async(move |input| {
        calls_clone.lock().unwrap().push(input["path"].clone());
        if !input["path"].is_string() {
            return Err("The \"path\" property must be of type string, got object".to_string());
        }
        Ok(json!({ "ok": true }))
    });

    let result = dispatch_internal_prompt(InternalPromptDispatchArgs {
        mode: InternalPromptDispatchMode::Async,
        client,
        session_id: "ses_async_path_compat".to_string(),
        input: json!({
            "path": { "id": "ses_async_path_compat" },
            "body": { "parts": [] }
        }),
        source: "test:path-compat:async".to_string(),
        dedupe_key: None,
        queue_behavior: Some(InternalPromptQueueBehavior::Defer),
        queue: None,
        queue_retry_ms: None,
        settle_ms: Some(0),
        post_dispatch_hold_ms: None,
        semantic_dedupe_hold_ms: None,
        dispatch_timeout_ms: None,
        check_status: Some(false),
        check_tool_state: Some(false),
        clock: None,
    })
    .await;

    assert_eq!(result.status(), "dispatched");
    assert_eq!(
        *calls.lock().unwrap(),
        vec![
            json!({ "id": "ses_async_path_compat" }),
            json!("ses_async_path_compat")
        ]
    );
}

#[tokio::test]
async fn test_prompt_rejects_with_got_undefined_retries_with_string_form() {
    let _guard = TEST_LOCK.lock().await;
    cleanup();
    let calls = Arc::new(Mutex::new(Vec::new()));
    let calls_clone = calls.clone();

    let client = TestClient::default().with_prompt_async(move |input| {
        calls_clone.lock().unwrap().push(input["path"].clone());
        if !input["path"].is_string() {
            return Err("The \"path\" property must be of type string, got undefined".to_string());
        }
        Ok(json!({ "ok": true }))
    });

    let result = dispatch_internal_prompt(InternalPromptDispatchArgs {
        mode: InternalPromptDispatchMode::Async,
        client,
        session_id: "ses_async_path_compat_undef".to_string(),
        input: json!({
            "path": { "id": "ses_async_path_compat_undef" },
            "body": { "parts": [] }
        }),
        source: "test:path-compat:async-undef".to_string(),
        dedupe_key: None,
        queue_behavior: Some(InternalPromptQueueBehavior::Defer),
        queue: None,
        queue_retry_ms: None,
        settle_ms: Some(0),
        post_dispatch_hold_ms: None,
        semantic_dedupe_hold_ms: None,
        dispatch_timeout_ms: None,
        check_status: Some(false),
        check_tool_state: Some(false),
        clock: None,
    })
    .await;

    assert_eq!(result.status(), "dispatched");
    assert_eq!(
        *calls.lock().unwrap(),
        vec![
            json!({ "id": "ses_async_path_compat_undef" }),
            json!("ses_async_path_compat_undef")
        ]
    );
}

#[tokio::test]
async fn test_latest_message_fetch_hangs_no_prompt_sent() {
    let _guard = TEST_LOCK.lock().await;
    cleanup();
    _set_prompt_gate_messages_fetch_timeout_ms_for_testing(Some(5));
    let prompt_calls = Arc::new(AtomicUsize::new(0));
    let prompt_calls_clone = prompt_calls.clone();

    let client = TestClient::default()
        .with_status(|| Ok(json!({ "data": { "ses_messages_hang": { "type": "idle" } } })))
        .with_messages_fut(|_, _| async {
            tokio::time::sleep(Duration::from_millis(500)).await;
            Ok(json!({ "data": [] }))
        })
        .with_prompt_async(move |_| {
            prompt_calls_clone.fetch_add(1, Ordering::SeqCst);
            Ok(json!({ "ok": true }))
        });

    let result = dispatch_internal_prompt(InternalPromptDispatchArgs {
        mode: InternalPromptDispatchMode::Async,
        client,
        session_id: "ses_messages_hang".to_string(),
        input: json!({ "path": { "id": "ses_messages_hang" }, "body": { "parts": [] } }),
        source: "test:messages-hang".to_string(),
        dedupe_key: None,
        queue_behavior: None,
        queue: None,
        queue_retry_ms: None,
        settle_ms: Some(0),
        post_dispatch_hold_ms: Some(0),
        semantic_dedupe_hold_ms: None,
        dispatch_timeout_ms: Some(50),
        check_status: None,
        check_tool_state: None,
        clock: None,
    })
    .await;

    assert_eq!(result.status(), "queued");
    assert_eq!(prompt_calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn test_latest_message_fetch_throws_no_prompt_sent() {
    let _guard = TEST_LOCK.lock().await;
    cleanup();
    let prompt_calls = Arc::new(AtomicUsize::new(0));
    let prompt_calls_clone = prompt_calls.clone();

    let client = TestClient::default()
        .with_status(|| Ok(json!({ "data": { "ses_messages_throw": { "type": "idle" } } })))
        .with_messages(|_, _| Err("message endpoint failed".to_string()))
        .with_prompt_async(move |_| {
            prompt_calls_clone.fetch_add(1, Ordering::SeqCst);
            Ok(json!({ "ok": true }))
        });

    let result = dispatch_internal_prompt(InternalPromptDispatchArgs {
        mode: InternalPromptDispatchMode::Async,
        client,
        session_id: "ses_messages_throw".to_string(),
        input: json!({ "path": { "id": "ses_messages_throw" }, "body": { "parts": [] } }),
        source: "test:messages-throw".to_string(),
        dedupe_key: None,
        queue_behavior: None,
        queue: None,
        queue_retry_ms: None,
        settle_ms: Some(0),
        post_dispatch_hold_ms: Some(0),
        semantic_dedupe_hold_ms: None,
        dispatch_timeout_ms: Some(50),
        check_status: None,
        check_tool_state: None,
        clock: None,
    })
    .await;

    assert_eq!(result.status(), "queued");
    assert_eq!(prompt_calls.load(Ordering::SeqCst), 0);
}
