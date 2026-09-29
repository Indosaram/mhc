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

    fn with_prompt_async_fut<F, Fut>(mut self, f: F) -> Self
    where
        F: Fn(&Value) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = Result<Value, String>> + Send + 'static,
    {
        self.has_prompt_async_flag = true;
        self.prompt_async_fn = Some(Arc::new(move |val| Box::pin(f(val))));
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

#[tokio::test]
async fn test_busy_session_queues_and_sends_after_idle() {
    let _guard = TEST_LOCK.lock().await;
    cleanup();
    let status_val = Arc::new(Mutex::new("busy"));
    let status_clone = status_val.clone();
    let prompt_calls = Arc::new(AtomicUsize::new(0));
    let prompt_calls_clone = prompt_calls.clone();
    let (tx, mut rx) = tokio::sync::mpsc::channel::<()>(1);

    let client = TestClient::default()
        .with_status(move || {
            let s = *status_clone.lock().unwrap();
            Ok(json!({ "data": { "ses_queue_busy": { "type": s } } }))
        })
        .with_prompt_async_fut(move |_| {
            let calls = prompt_calls_clone.clone();
            let tx = tx.clone();
            async move {
                calls.fetch_add(1, Ordering::SeqCst);
                let _ = tx.send(()).await;
                Ok(json!({ "ok": true }))
            }
        });

    let result = dispatch_internal_prompt(InternalPromptDispatchArgs {
        mode: InternalPromptDispatchMode::Async,
        client,
        session_id: "ses_queue_busy".to_string(),
        input: json!({ "path": { "id": "ses_queue_busy" }, "body": { "parts": [{ "type": "text", "text": "queued" }] } }),
        source: "test:queue-busy".to_string(),
        dedupe_key: None,
        queue_behavior: None,
        queue: None,
        queue_retry_ms: Some(1),
        settle_ms: Some(0),
        post_dispatch_hold_ms: None,
        semantic_dedupe_hold_ms: None,
        dispatch_timeout_ms: None,
        check_status: None,
        check_tool_state: None,
        clock: None,
    })
    .await;

    assert_eq!(result.status(), "queued");
    *status_val.lock().unwrap() = "idle";
    let _ = tokio::time::timeout(Duration::from_millis(1_000), rx.recv()).await;
    assert_eq!(prompt_calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn test_duplicate_queued_prompts_coalesce_when_idle() {
    let _guard = TEST_LOCK.lock().await;
    cleanup();
    let status_val = Arc::new(Mutex::new("busy"));
    let status_clone = status_val.clone();
    let prompt_calls = Arc::new(AtomicUsize::new(0));
    let prompt_calls_clone = prompt_calls.clone();
    let (tx, mut rx) = tokio::sync::mpsc::channel::<()>(1);

    let input = json!({
        "path": { "id": "ses_queue_dedupe" },
        "body": { "parts": [{ "type": "text", "text": "same" }] }
    });

    let client = TestClient::default()
        .with_status(move || {
            let s = *status_clone.lock().unwrap();
            Ok(json!({ "data": { "ses_queue_dedupe": { "type": s } } }))
        })
        .with_prompt_async_fut(move |_| {
            let calls = prompt_calls_clone.clone();
            let tx = tx.clone();
            async move {
                calls.fetch_add(1, Ordering::SeqCst);
                let _ = tx.send(()).await;
                Ok(json!({ "ok": true }))
            }
        });

    let first = dispatch_internal_prompt(InternalPromptDispatchArgs {
        mode: InternalPromptDispatchMode::Async,
        client: client.clone(),
        session_id: "ses_queue_dedupe".to_string(),
        input: input.clone(),
        source: "test:queue-dedupe".to_string(),
        dedupe_key: None,
        queue_behavior: None,
        queue: None,
        queue_retry_ms: Some(1),
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
        session_id: "ses_queue_dedupe".to_string(),
        input,
        source: "test:queue-dedupe".to_string(),
        dedupe_key: None,
        queue_behavior: None,
        queue: None,
        queue_retry_ms: Some(1),
        settle_ms: Some(0),
        post_dispatch_hold_ms: None,
        semantic_dedupe_hold_ms: None,
        dispatch_timeout_ms: None,
        check_status: None,
        check_tool_state: None,
        clock: None,
    })
    .await;

    assert_eq!(first.status(), "queued");
    assert_eq!(second.status(), "queued");
    *status_val.lock().unwrap() = "idle";
    let _ = tokio::time::timeout(Duration::from_millis(1_000), rx.recv()).await;
    assert_eq!(prompt_calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn test_queued_prompts_preserve_fifo_order_after_release() {
    let _guard = TEST_LOCK.lock().await;
    cleanup();
    let calls = Arc::new(Mutex::new(Vec::new()));
    let calls_clone = calls.clone();
    let (tx, mut rx) = tokio::sync::mpsc::channel::<()>(2);

    let client = TestClient::default().with_prompt_async_fut(move |input| {
        let calls_inner = calls_clone.clone();
        let tx = tx.clone();
        let text = input["body"]["parts"][0]["text"]
            .as_str()
            .unwrap_or("")
            .to_string();
        async move {
            let reached_two = {
                let mut c = calls_inner.lock().unwrap();
                c.push(text);
                c.len() == 2
            };
            if reached_two {
                let _ = tx.send(()).await;
            }
            Ok(json!({ "ok": true }))
        }
    });

    let first = dispatch_internal_prompt(InternalPromptDispatchArgs {
        mode: InternalPromptDispatchMode::Async,
        client: client.clone(),
        session_id: "ses_queue_fifo".to_string(),
        input: json!({ "path": { "id": "ses_queue_fifo" }, "body": { "parts": [{ "type": "text", "text": "first" }] } }),
        source: "test:queue-fifo:first".to_string(),
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
        session_id: "ses_queue_fifo".to_string(),
        input: json!({ "path": { "id": "ses_queue_fifo" }, "body": { "parts": [{ "type": "text", "text": "second" }] } }),
        source: "test:queue-fifo:second".to_string(),
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

    release_prompt_async_reservation(
        "ses_queue_fifo",
        "test:release-fifo",
        Some(
            &PromptAsyncReservationReleaseOptions::default()
                .with_reserved_by("test:queue-fifo:first"),
        ),
    );

    let _ = tokio::time::timeout(Duration::from_millis(1_000), rx.recv()).await;

    assert_eq!(first.status(), "dispatched");
    assert_eq!(second.status(), "queued");
    assert_eq!(
        *calls.lock().unwrap(),
        vec!["first".to_string(), "second".to_string()]
    );
}

#[tokio::test]
async fn test_stateful_route_defers_when_hold_active() {
    let _guard = TEST_LOCK.lock().await;
    cleanup();
    let calls = Arc::new(Mutex::new(Vec::new()));
    let calls_clone = calls.clone();

    let client = TestClient::default().with_prompt_async(move |input| {
        let text = input["body"]["parts"][0]["text"]
            .as_str()
            .unwrap_or("")
            .to_string();
        calls_clone.lock().unwrap().push(text);
        Ok(json!({ "ok": true }))
    });

    let first = dispatch_internal_prompt(InternalPromptDispatchArgs {
        mode: InternalPromptDispatchMode::Async,
        client: client.clone(),
        session_id: "ses_queue_defer_hold".to_string(),
        input: json!({ "path": { "id": "ses_queue_defer_hold" }, "body": { "parts": [{ "type": "text", "text": "first" }] } }),
        source: "test:queue-defer:first".to_string(),
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
        session_id: "ses_queue_defer_hold".to_string(),
        input: json!({ "path": { "id": "ses_queue_defer_hold" }, "body": { "parts": [{ "type": "text", "text": "second" }] } }),
        source: "test:queue-defer:second".to_string(),
        dedupe_key: None,
        queue_behavior: Some(InternalPromptQueueBehavior::Defer),
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

    release_prompt_async_reservation(
        "ses_queue_defer_hold",
        "test:queue-defer:release",
        Some(
            &PromptAsyncReservationReleaseOptions::default()
                .with_reserved_by("test:queue-defer:first"),
        ),
    );

    assert_eq!(first.status(), "dispatched");
    assert_eq!(
        second,
        InternalPromptDispatchResult::Reserved {
            reserved_by: "test:queue-defer:first".to_string(),
        }
    );
    assert_eq!(*calls.lock().unwrap(), vec!["first".to_string()]);
}

#[tokio::test]
async fn test_stateful_route_defer_does_not_cut_ahead_of_waiting_queue() {
    let _guard = TEST_LOCK.lock().await;
    cleanup();
    let status_val = Arc::new(Mutex::new("busy"));
    let status_clone = status_val.clone();
    let calls = Arc::new(Mutex::new(Vec::new()));
    let calls_clone = calls.clone();
    let (tx, mut rx) = tokio::sync::mpsc::channel::<()>(1);

    let client = TestClient::default()
        .with_status(move || {
            let s = *status_clone.lock().unwrap();
            Ok(json!({ "data": { "ses_queue_defer_existing": { "type": s } } }))
        })
        .with_prompt_async_fut(move |input| {
            let calls_inner = calls_clone.clone();
            let text = input["body"]["parts"][0]["text"]
                .as_str()
                .unwrap_or("")
                .to_string();
            let tx = tx.clone();
            async move {
                calls_inner.lock().unwrap().push(text);
                let _ = tx.send(()).await;
                Ok(json!({ "ok": true }))
            }
        });

    let first = dispatch_internal_prompt(InternalPromptDispatchArgs {
        mode: InternalPromptDispatchMode::Async,
        client: client.clone(),
        session_id: "ses_queue_defer_existing".to_string(),
        input: json!({ "path": { "id": "ses_queue_defer_existing" }, "body": { "parts": [{ "type": "text", "text": "first" }] } }),
        source: "test:queue-defer-existing:first".to_string(),
        dedupe_key: None,
        queue_behavior: None,
        queue: None,
        queue_retry_ms: Some(1),
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
        session_id: "ses_queue_defer_existing".to_string(),
        input: json!({ "path": { "id": "ses_queue_defer_existing" }, "body": { "parts": [{ "type": "text", "text": "second" }] } }),
        source: "test:queue-defer-existing:second".to_string(),
        dedupe_key: None,
        queue_behavior: Some(InternalPromptQueueBehavior::Defer),
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

    *status_val.lock().unwrap() = "idle";
    let _ = tokio::time::timeout(Duration::from_millis(1_000), rx.recv()).await;

    assert_eq!(first.status(), "queued");
    assert_eq!(
        second,
        InternalPromptDispatchResult::Reserved {
            reserved_by: "test:queue-defer-existing:first".to_string(),
        }
    );
    assert_eq!(*calls.lock().unwrap(), vec!["first".to_string()]);
}

#[tokio::test]
async fn test_busy_status_no_prompt_sent() {
    let _guard = TEST_LOCK.lock().await;
    cleanup();
    let prompt_calls = Arc::new(AtomicUsize::new(0));
    let prompt_calls_clone = prompt_calls.clone();

    let client = TestClient::default()
        .with_status(|| Ok(json!({ "data": { "ses_busy": { "type": "busy" } } })))
        .with_prompt_async(move |_| {
            prompt_calls_clone.fetch_add(1, Ordering::SeqCst);
            Ok(json!({ "ok": true }))
        });

    let result = dispatch_internal_prompt(InternalPromptDispatchArgs {
        mode: InternalPromptDispatchMode::Async,
        client,
        session_id: "ses_busy".to_string(),
        input: json!({ "path": { "id": "ses_busy" }, "body": { "parts": [] } }),
        source: "test:busy".to_string(),
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
    assert_eq!(prompt_calls.load(Ordering::SeqCst), 0);
}
