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

    fn with_prompt_async_fut<F, Fut>(mut self, f: F) -> Self
    where
        F: Fn(&Value) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = Result<Value, String>> + Send + 'static,
    {
        self.has_prompt_async_flag = true;
        self.prompt_async_fn = Some(Arc::new(move |val| Box::pin(f(val))));
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
async fn test_async_mode_uses_prompt_async() {
    let _guard = TEST_LOCK.lock().await;
    cleanup();
    let calls = Arc::new(Mutex::new(Vec::new()));
    let calls_clone = calls.clone();

    let client = TestClient::default()
        .with_prompt_async(move |input| {
            let id = input["path"]["id"].as_str().unwrap_or("");
            calls_clone.lock().unwrap().push(format!("async:{id}"));
            Ok(json!({ "route": "async", "sessionID": id }))
        })
        .with_prompt(|input| {
            let id = input["path"]["id"].as_str().unwrap_or("");
            Ok(json!({ "route": "sync", "sessionID": id }))
        });

    let result = dispatch_internal_prompt(InternalPromptDispatchArgs {
        mode: InternalPromptDispatchMode::Async,
        client,
        session_id: "ses_unified_async".to_string(),
        input: json!({ "path": { "id": "ses_unified_async" }, "body": { "parts": [] } }),
        source: "test:unified-async".to_string(),
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
            response: json!({ "route": "async", "sessionID": "ses_unified_async" }),
        }
    );
    assert_eq!(
        *calls.lock().unwrap(),
        vec!["async:ses_unified_async".to_string()]
    );
}

#[tokio::test]
async fn test_sync_mode_uses_prompt() {
    let _guard = TEST_LOCK.lock().await;
    cleanup();
    let calls = Arc::new(Mutex::new(Vec::new()));
    let calls_clone = calls.clone();

    let client = TestClient::default()
        .with_prompt_async(|input| {
            let id = input["path"]["id"].as_str().unwrap_or("");
            Ok(json!({ "route": "async", "sessionID": id }))
        })
        .with_prompt(move |input| {
            let id = input["path"]["id"].as_str().unwrap_or("");
            calls_clone.lock().unwrap().push(format!("sync:{id}"));
            Ok(json!({ "route": "sync", "sessionID": id }))
        });

    let result = dispatch_internal_prompt(InternalPromptDispatchArgs {
        mode: InternalPromptDispatchMode::Sync,
        client,
        session_id: "ses_unified_sync".to_string(),
        input: json!({ "path": { "id": "ses_unified_sync" }, "body": { "parts": [] } }),
        source: "test:unified-sync".to_string(),
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
            response: json!({ "route": "sync", "sessionID": "ses_unified_sync" }),
        }
    );
    assert_eq!(
        *calls.lock().unwrap(),
        vec!["sync:ses_unified_sync".to_string()]
    );
}

#[tokio::test]
async fn test_async_dispatch_holds_session_reservation_sync_mode_defers() {
    let _guard = TEST_LOCK.lock().await;
    cleanup();
    let calls = Arc::new(Mutex::new(Vec::new()));
    let calls_async = calls.clone();
    let calls_sync = calls.clone();

    let client = TestClient::default()
        .with_prompt_async(move |_| {
            calls_async.lock().unwrap().push("async".to_string());
            Ok(json!({ "ok": true }))
        })
        .with_prompt(move |_| {
            calls_sync.lock().unwrap().push("sync".to_string());
            Ok(json!({ "ok": true }))
        });

    let first = dispatch_internal_prompt(InternalPromptDispatchArgs {
        mode: InternalPromptDispatchMode::Async,
        client: client.clone(),
        session_id: "ses_unified_shared_reservation".to_string(),
        input: json!({ "path": { "id": "ses_unified_shared_reservation" }, "body": { "parts": [] } }),
        source: "test:unified-shared:first".to_string(),
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
        session_id: "ses_unified_shared_reservation".to_string(),
        input: json!({ "path": { "id": "ses_unified_shared_reservation" }, "body": { "parts": [] } }),
        source: "test:unified-shared:second".to_string(),
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
    assert_eq!(
        second,
        InternalPromptDispatchResult::Reserved {
            reserved_by: "test:unified-shared:first".to_string(),
        }
    );
    assert_eq!(*calls.lock().unwrap(), vec!["async".to_string()]);
}

#[tokio::test]
async fn test_concurrent_prompt_async_calls_only_one_accepted() {
    let _guard = TEST_LOCK.lock().await;
    cleanup();
    let prompt_calls = Arc::new(AtomicUsize::new(0));
    let prompt_calls_clone = prompt_calls.clone();
    let (gate_tx, gate_rx) = tokio::sync::oneshot::channel::<()>();
    let gate_rx = Arc::new(tokio::sync::Mutex::new(Some(gate_rx)));

    let client = TestClient::default()
        .with_status(|| Ok(json!({ "data": { "ses_race": { "type": "idle" } } })))
        .with_prompt_async_fut(move |_| {
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
            mode: InternalPromptDispatchMode::Async,
            client: client_clone,
            session_id: "ses_race".to_string(),
            input: json!({ "path": { "id": "ses_race" }, "body": { "parts": [] } }),
            source: "test:first".to_string(),
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
        mode: InternalPromptDispatchMode::Async,
        client,
        session_id: "ses_race".to_string(),
        input: json!({ "path": { "id": "ses_race" }, "body": { "parts": [] } }),
        source: "test:second".to_string(),
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
    assert_eq!(second.status(), "queued");
    assert_eq!(prompt_calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn test_default_dispatch_hold_keeps_session_reserved() {
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
        session_id: "ses_hold_after_dispatch".to_string(),
        input: json!({ "path": { "id": "ses_hold_after_dispatch" }, "body": { "parts": [] } }),
        source: "test:hold:first".to_string(),
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
        session_id: "ses_hold_after_dispatch".to_string(),
        input: json!({ "path": { "id": "ses_hold_after_dispatch" }, "body": { "parts": [] } }),
        source: "test:hold:second".to_string(),
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
    assert_eq!(second.status(), "queued");
    assert_eq!(prompt_calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn test_sdk_prompt_async_receiver_state_preserved() {
    let _guard = TEST_LOCK.lock().await;
    cleanup();
    struct ReceiverClient {
        accepted: bool,
    }
    impl PromptGateClient for ReceiverClient {
        fn has_prompt_async(&self) -> bool {
            true
        }
        fn prompt_async(
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
        mode: InternalPromptDispatchMode::Async,
        client,
        session_id: "ses_bound_prompt_async".to_string(),
        input: json!({ "path": { "id": "ses_bound_prompt_async" }, "body": { "parts": [] } }),
        source: "test:bound-prompt-async".to_string(),
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
            response: json!({ "accepted": true, "sessionID": "ses_bound_prompt_async" }),
        }
    );
}

#[tokio::test]
async fn test_sdk_messages_receiver_state_preserved() {
    let _guard = TEST_LOCK.lock().await;
    cleanup();
    struct ReceiverClient {
        messages: Vec<Value>,
    }
    impl PromptGateClient for ReceiverClient {
        fn has_messages(&self) -> bool {
            true
        }
        fn has_prompt_async(&self) -> bool {
            true
        }
        fn session_messages(
            &self,
            _session_id: &str,
            _query: &Value,
        ) -> impl std::future::Future<Output = Result<Value, String>> + Send {
            let data = self.messages.clone();
            async move { Ok(json!({ "data": data })) }
        }
        async fn prompt_async(&self, _input: &Value) -> Result<Value, String> {
            Ok(json!({ "ok": true }))
        }
    }

    let client = ReceiverClient {
        messages: vec![
            json!({ "info": { "id": "msg_user", "role": "user" }, "parts": [{ "type": "text", "text": "run work" }] }),
            json!({ "info": { "id": "msg_assistant", "role": "assistant", "finish": "tool-calls" }, "parts": [{ "type": "tool_use", "id": "toolu_pending", "state": { "status": "running" } }] }),
        ],
    };

    let result = dispatch_internal_prompt(InternalPromptDispatchArgs {
        mode: InternalPromptDispatchMode::Async,
        client,
        session_id: "ses_bound_messages".to_string(),
        input: json!({ "path": { "id": "ses_bound_messages" }, "body": { "parts": [] } }),
        source: "test:bound-messages".to_string(),
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

    assert_eq!(result.status(), "queued");
}

#[tokio::test]
async fn test_dispatch_hold_expired_next_prompt_accepted() {
    let _guard = TEST_LOCK.lock().await;
    cleanup();
    let clock = Arc::new(MockClock::new(1_000));
    let prompt_calls = Arc::new(AtomicUsize::new(0));
    let prompt_calls_clone = prompt_calls.clone();

    let client = TestClient::default().with_prompt_async(move |_| {
        prompt_calls_clone.fetch_add(1, Ordering::SeqCst);
        Ok(json!({ "ok": true }))
    });

    let first = dispatch_internal_prompt(InternalPromptDispatchArgs {
        mode: InternalPromptDispatchMode::Async,
        client: client.clone(),
        session_id: "ses_expired_hold".to_string(),
        input: json!({ "path": { "id": "ses_expired_hold" }, "body": { "parts": [] } }),
        source: "test:expired:first".to_string(),
        dedupe_key: None,
        queue_behavior: None,
        queue: None,
        queue_retry_ms: None,
        settle_ms: Some(0),
        post_dispatch_hold_ms: Some(1),
        semantic_dedupe_hold_ms: Some(0),
        dispatch_timeout_ms: None,
        check_status: None,
        check_tool_state: None,
        clock: Some(clock.clone()),
    })
    .await;

    clock.advance_ms(2);

    let second = dispatch_internal_prompt(InternalPromptDispatchArgs {
        mode: InternalPromptDispatchMode::Async,
        client,
        session_id: "ses_expired_hold".to_string(),
        input: json!({ "path": { "id": "ses_expired_hold" }, "body": { "parts": [] } }),
        source: "test:expired:second".to_string(),
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
        clock: Some(clock.clone()),
    })
    .await;

    assert_eq!(first.status(), "dispatched");
    assert_eq!(second.status(), "dispatched");
    assert_eq!(prompt_calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn test_peer_message_hold_unrelated_release_leaves_session_reserved() {
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
        session_id: "ses_release_scope".to_string(),
        input: json!({ "path": { "id": "ses_release_scope" }, "body": { "parts": [{ "type": "text", "text": "<peer_message from=\"teammate\">hello</peer_message>" }] } }),
        source: "team-live-delivery".to_string(),
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

    release_prompt_async_reservation("ses_release_scope", "ralph-loop:activity", None);

    let second = dispatch_internal_prompt(InternalPromptDispatchArgs {
        mode: InternalPromptDispatchMode::Async,
        client,
        session_id: "ses_release_scope".to_string(),
        input: json!({ "path": { "id": "ses_release_scope" }, "body": { "parts": [{ "type": "text", "text": "continue" }] } }),
        source: "todo-continuation-enforcer".to_string(),
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
    assert_eq!(
        second,
        InternalPromptDispatchResult::Queued {
            queued_by: "team-live-delivery".to_string(),
            position: 1,
        }
    );
    assert_eq!(prompt_calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn test_route_family_hold_released_by_same_family_abort() {
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
        session_id: "ses_release_family_scope".to_string(),
        input: json!({ "path": { "id": "ses_release_family_scope" }, "body": { "parts": [{ "type": "text", "text": "continue" }] } }),
        source: "model-fallback:message.updated".to_string(),
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
        "ses_release_family_scope",
        "model-fallback-abort:session.error",
        Some(
            &PromptAsyncReservationReleaseOptions::default()
                .with_reserved_by_prefix("model-fallback:"),
        ),
    );

    let second = dispatch_internal_prompt(InternalPromptDispatchArgs {
        mode: InternalPromptDispatchMode::Async,
        client,
        session_id: "ses_release_family_scope".to_string(),
        input: json!({ "path": { "id": "ses_release_family_scope" }, "body": { "parts": [{ "type": "text", "text": "continue again" }] } }),
        source: "model-fallback:session.error".to_string(),
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
    assert_eq!(released, true);
    assert_eq!(second.status(), "dispatched");
    assert_eq!(prompt_calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn test_dispatch_timeout_releases_reservation_for_next_caller() {
    let _guard = TEST_LOCK.lock().await;
    cleanup();
    let prompt_calls = Arc::new(AtomicUsize::new(0));
    let prompt_calls_clone = prompt_calls.clone();

    let client = TestClient::default().with_prompt_async_fut(move |_| {
        prompt_calls_clone.fetch_add(1, Ordering::SeqCst);
        async move {
            tokio::time::sleep(Duration::from_millis(500)).await;
            Ok(json!({ "ok": true }))
        }
    });

    let first = dispatch_internal_prompt(InternalPromptDispatchArgs {
        mode: InternalPromptDispatchMode::Async,
        client: client.clone(),
        session_id: "ses_dispatch_timeout".to_string(),
        input: json!({ "path": { "id": "ses_dispatch_timeout" }, "body": { "parts": [] } }),
        source: "test:timeout:first".to_string(),
        dedupe_key: None,
        queue_behavior: None,
        queue: None,
        queue_retry_ms: None,
        settle_ms: Some(0),
        post_dispatch_hold_ms: Some(0),
        semantic_dedupe_hold_ms: None,
        dispatch_timeout_ms: Some(1),
        check_status: None,
        check_tool_state: None,
        clock: None,
    })
    .await;

    let second = dispatch_internal_prompt(InternalPromptDispatchArgs {
        mode: InternalPromptDispatchMode::Async,
        client,
        session_id: "ses_dispatch_timeout".to_string(),
        input: json!({ "path": { "id": "ses_dispatch_timeout" }, "body": { "parts": [] } }),
        source: "test:timeout:second".to_string(),
        dedupe_key: None,
        queue_behavior: None,
        queue: None,
        queue_retry_ms: None,
        settle_ms: Some(0),
        post_dispatch_hold_ms: Some(0),
        semantic_dedupe_hold_ms: None,
        dispatch_timeout_ms: Some(1),
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
    assert_eq!(second.status(), "failed");
    if let InternalPromptDispatchResult::Failed {
        dispatch_attempted, ..
    } = second
    {
        assert_eq!(dispatch_attempted, true);
    }
    assert_eq!(prompt_calls.load(Ordering::SeqCst), 2);
}
