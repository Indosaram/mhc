use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, LazyLock};

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
async fn test_assistant_waiting_on_tools_no_prompt_sent() {
    let _guard = TEST_LOCK.lock().await;
    cleanup();
    let prompt_calls = Arc::new(AtomicUsize::new(0));
    let prompt_calls_clone = prompt_calls.clone();

    let client = TestClient::default()
        .with_status(|| Ok(json!({ "data": { "ses_waiting_tools": { "type": "idle" } } })))
        .with_messages(|_, _| {
            Ok(json!({
                "data": [
                    { "info": { "id": "msg_user", "role": "user" }, "parts": [{ "type": "text", "text": "run work" }] },
                    { "info": { "id": "msg_assistant", "role": "assistant", "finish": "tool-calls" }, "parts": [{ "type": "tool_use", "id": "toolu_pending", "state": { "status": "pending" } }] }
                ]
            }))
        })
        .with_prompt_async(move |_| {
            prompt_calls_clone.fetch_add(1, Ordering::SeqCst);
            Ok(json!({ "ok": true }))
        });

    let result = dispatch_internal_prompt(InternalPromptDispatchArgs {
        mode: InternalPromptDispatchMode::Async,
        client,
        session_id: "ses_waiting_tools".to_string(),
        input: json!({ "path": { "id": "ses_waiting_tools" }, "body": { "parts": [] } }),
        source: "test:waiting-tools".to_string(),
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

#[tokio::test]
async fn test_tool_calls_finish_without_pending_part_state_no_prompt_sent() {
    let _guard = TEST_LOCK.lock().await;
    cleanup();
    let prompt_calls = Arc::new(AtomicUsize::new(0));
    let prompt_calls_clone = prompt_calls.clone();

    let client = TestClient::default()
        .with_status(|| Ok(json!({ "data": { "ses_finish_waiting_tools": { "type": "idle" } } })))
        .with_messages(|_, _| {
            Ok(json!({
                "data": [
                    { "info": { "id": "msg_user", "role": "user" }, "parts": [{ "type": "text", "text": "run work" }] },
                    { "info": { "id": "msg_assistant", "role": "assistant", "finish": "tool-calls" }, "parts": [{ "type": "tool_use", "id": "toolu_pending" }] }
                ]
            }))
        })
        .with_prompt_async(move |_| {
            prompt_calls_clone.fetch_add(1, Ordering::SeqCst);
            Ok(json!({ "ok": true }))
        });

    let result = dispatch_internal_prompt(InternalPromptDispatchArgs {
        mode: InternalPromptDispatchMode::Async,
        client,
        session_id: "ses_finish_waiting_tools".to_string(),
        input: json!({ "path": { "id": "ses_finish_waiting_tools" }, "body": { "parts": [] } }),
        source: "test:finish-waiting-tools".to_string(),
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

#[tokio::test]
async fn test_assistant_running_tool_call_part_no_prompt_sent() {
    let _guard = TEST_LOCK.lock().await;
    cleanup();
    let prompt_calls = Arc::new(AtomicUsize::new(0));
    let prompt_calls_clone = prompt_calls.clone();

    let client = TestClient::default()
        .with_status(|| Ok(json!({ "data": { "ses_tool_call_part": { "type": "idle" } } })))
        .with_messages(|_, _| {
            Ok(json!({
                "data": [
                    { "info": { "id": "msg_user", "role": "user" }, "parts": [{ "type": "text", "text": "run work" }] },
                    { "info": { "id": "msg_assistant", "role": "assistant" }, "parts": [{ "type": "tool-call", "id": "call_pending", "state": { "status": "running" } }] }
                ]
            }))
        })
        .with_prompt_async(move |_| {
            prompt_calls_clone.fetch_add(1, Ordering::SeqCst);
            Ok(json!({ "ok": true }))
        });

    let result = dispatch_internal_prompt(InternalPromptDispatchArgs {
        mode: InternalPromptDispatchMode::Async,
        client,
        session_id: "ses_tool_call_part".to_string(),
        input: json!({ "path": { "id": "ses_tool_call_part" }, "body": { "parts": [] } }),
        source: "test:tool-call-part".to_string(),
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

#[tokio::test]
async fn test_assistant_streaming_without_finish_reason_no_prompt_sent() {
    let _guard = TEST_LOCK.lock().await;
    cleanup();
    let prompt_calls = Arc::new(AtomicUsize::new(0));
    let prompt_calls_clone = prompt_calls.clone();

    let client = TestClient::default()
        .with_status(|| Ok(json!({ "data": { "ses_streaming_assistant": { "type": "idle" } } })))
        .with_messages(|_, _| {
            Ok(json!({
                "data": [
                    { "info": { "id": "msg_user", "role": "user" }, "parts": [{ "type": "text", "text": "run work" }] },
                    { "info": { "id": "msg_assistant", "role": "assistant" }, "parts": [{ "type": "reasoning", "text": "still thinking" }] }
                ]
            }))
        })
        .with_prompt_async(move |_| {
            prompt_calls_clone.fetch_add(1, Ordering::SeqCst);
            Ok(json!({ "ok": true }))
        });

    let result = dispatch_internal_prompt(InternalPromptDispatchArgs {
        mode: InternalPromptDispatchMode::Async,
        client,
        session_id: "ses_streaming_assistant".to_string(),
        input: json!({ "path": { "id": "ses_streaming_assistant" }, "body": { "parts": [] } }),
        source: "test:streaming-assistant".to_string(),
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

#[tokio::test]
async fn test_assistant_unknown_finish_no_prompt_sent() {
    let _guard = TEST_LOCK.lock().await;
    cleanup();
    let prompt_calls = Arc::new(AtomicUsize::new(0));
    let prompt_calls_clone = prompt_calls.clone();

    let client = TestClient::default()
        .with_status(|| Ok(json!({ "data": { "ses_unknown_finish": { "type": "idle" } } })))
        .with_messages(|_, _| {
            Ok(json!({
                "data": [
                    { "info": { "id": "msg_user", "role": "user" }, "parts": [{ "type": "text", "text": "run work" }] },
                    { "info": { "id": "msg_assistant", "role": "assistant", "finish": "unknown" }, "parts": [{ "type": "reasoning", "text": "still resolving" }] }
                ]
            }))
        })
        .with_prompt_async(move |_| {
            prompt_calls_clone.fetch_add(1, Ordering::SeqCst);
            Ok(json!({ "ok": true }))
        });

    let result = dispatch_internal_prompt(InternalPromptDispatchArgs {
        mode: InternalPromptDispatchMode::Async,
        client,
        session_id: "ses_unknown_finish".to_string(),
        input: json!({ "path": { "id": "ses_unknown_finish" }, "body": { "parts": [] } }),
        source: "test:unknown-finish".to_string(),
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

#[tokio::test]
async fn test_assistant_boolean_finish_true_treated_as_terminal() {
    let _guard = TEST_LOCK.lock().await;
    cleanup();
    let prompt_calls = Arc::new(AtomicUsize::new(0));
    let prompt_calls_clone = prompt_calls.clone();

    let client = TestClient::default()
        .with_status(|| Ok(json!({ "data": { "ses_boolean_finish": { "type": "idle" } } })))
        .with_messages(|_, _| {
            Ok(json!({
                "data": [
                    { "info": { "id": "msg_user", "role": "user" }, "parts": [{ "type": "text", "text": "run work" }] },
                    { "info": { "id": "msg_assistant", "role": "assistant", "finish": true }, "parts": [{ "type": "text", "text": "done" }] }
                ]
            }))
        })
        .with_prompt_async(move |_| {
            prompt_calls_clone.fetch_add(1, Ordering::SeqCst);
            Ok(json!({ "ok": true }))
        });

    let result = dispatch_internal_prompt(InternalPromptDispatchArgs {
        mode: InternalPromptDispatchMode::Async,
        client,
        session_id: "ses_boolean_finish".to_string(),
        input: json!({ "path": { "id": "ses_boolean_finish" }, "body": { "parts": [] } }),
        source: "test:boolean-finish".to_string(),
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

    assert_eq!(result.status(), "dispatched");
    assert_eq!(prompt_calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn test_assistant_time_completed_treated_as_terminal() {
    let _guard = TEST_LOCK.lock().await;
    cleanup();
    let prompt_calls = Arc::new(AtomicUsize::new(0));
    let prompt_calls_clone = prompt_calls.clone();

    let client = TestClient::default()
        .with_status(|| Ok(json!({ "data": { "ses_completed_time": { "type": "idle" } } })))
        .with_messages(|_, _| {
            Ok(json!({
                "data": [
                    { "info": { "id": "msg_user", "role": "user" }, "parts": [{ "type": "text", "text": "run work" }] },
                    { "info": { "id": "msg_assistant", "role": "assistant", "time": { "completed": 1_762_000_000_000u64 } }, "parts": [{ "type": "text", "text": "done" }] }
                ]
            }))
        })
        .with_prompt_async(move |_| {
            prompt_calls_clone.fetch_add(1, Ordering::SeqCst);
            Ok(json!({ "ok": true }))
        });

    let result = dispatch_internal_prompt(InternalPromptDispatchArgs {
        mode: InternalPromptDispatchMode::Async,
        client,
        session_id: "ses_completed_time".to_string(),
        input: json!({ "path": { "id": "ses_completed_time" }, "body": { "parts": [] } }),
        source: "test:completed-time".to_string(),
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

    assert_eq!(result.status(), "dispatched");
    assert_eq!(prompt_calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn test_assistant_error_turn_completed_with_no_parts_treated_as_terminal() {
    let _guard = TEST_LOCK.lock().await;
    cleanup();
    let prompt_calls = Arc::new(AtomicUsize::new(0));
    let prompt_calls_clone = prompt_calls.clone();

    let client = TestClient::default()
        .with_status(|| Ok(json!({ "data": { "ses_completed_error_empty": { "type": "idle" } } })))
        .with_messages(|_, _| {
            Ok(json!({
                "data": [
                    { "info": { "id": "msg_user", "role": "user" }, "parts": [{ "type": "text", "text": "run work" }] },
                    { "info": { "id": "msg_assistant", "role": "assistant", "time": { "completed": 1_762_000_000_000u64 }, "error": { "name": "APIError" } }, "parts": [] }
                ]
            }))
        })
        .with_prompt_async(move |_| {
            prompt_calls_clone.fetch_add(1, Ordering::SeqCst);
            Ok(json!({ "ok": true }))
        });

    let result = dispatch_internal_prompt(InternalPromptDispatchArgs {
        mode: InternalPromptDispatchMode::Async,
        client,
        session_id: "ses_completed_error_empty".to_string(),
        input: json!({ "path": { "id": "ses_completed_error_empty" }, "body": { "parts": [] } }),
        source: "test:completed-error-empty".to_string(),
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

    assert_eq!(result.status(), "dispatched");
    assert_eq!(prompt_calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn test_internal_user_tail_follows_assistant_waiting_on_tools_no_prompt() {
    let _guard = TEST_LOCK.lock().await;
    cleanup();
    let prompt_calls = Arc::new(AtomicUsize::new(0));
    let prompt_calls_clone = prompt_calls.clone();

    let client = TestClient::default()
        .with_status(|| Ok(json!({ "data": { "ses_internal_tail_tools": { "type": "idle" } } })))
        .with_messages(|_, _| {
            Ok(json!({
                "data": [
                    { "info": { "id": "msg_user", "role": "user" }, "parts": [{ "type": "text", "text": "run work" }] },
                    { "info": { "id": "msg_assistant", "role": "assistant", "finish": "tool-calls" }, "parts": [{ "type": "tool_use", "id": "toolu_pending", "state": { "status": "running" } }] },
                    { "info": { "id": "msg_internal_user", "role": "user" }, "parts": [{ "type": "text", "text": "wake\n<!-- OMO_INTERNAL_INITIATOR -->" }] }
                ]
            }))
        })
        .with_prompt_async(move |_| {
            prompt_calls_clone.fetch_add(1, Ordering::SeqCst);
            Ok(json!({ "ok": true }))
        });

    let result = dispatch_internal_prompt(InternalPromptDispatchArgs {
        mode: InternalPromptDispatchMode::Async,
        client,
        session_id: "ses_internal_tail_tools".to_string(),
        input: json!({ "path": { "id": "ses_internal_tail_tools" }, "body": { "parts": [] } }),
        source: "test:internal-tail-tools".to_string(),
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

#[tokio::test]
async fn test_synthetic_user_tail_follows_assistant_waiting_on_tools_no_prompt() {
    let _guard = TEST_LOCK.lock().await;
    cleanup();
    let prompt_calls = Arc::new(AtomicUsize::new(0));
    let prompt_calls_clone = prompt_calls.clone();

    let client = TestClient::default()
        .with_status(|| Ok(json!({ "data": { "ses_synthetic_tail_tools": { "type": "idle" } } })))
        .with_messages(|_, _| {
            Ok(json!({
                "data": [
                    { "info": { "id": "msg_user", "role": "user" }, "parts": [{ "type": "text", "text": "run work" }] },
                    { "info": { "id": "msg_assistant", "role": "assistant", "finish": "tool-calls" }, "parts": [{ "type": "tool_use", "id": "toolu_pending", "state": { "status": "running" } }] },
                    { "info": { "id": "msg_synthetic_user", "role": "user" }, "parts": [{ "type": "text", "text": "continue", "synthetic": true }] }
                ]
            }))
        })
        .with_prompt_async(move |_| {
            prompt_calls_clone.fetch_add(1, Ordering::SeqCst);
            Ok(json!({ "ok": true }))
        });

    let result = dispatch_internal_prompt(InternalPromptDispatchArgs {
        mode: InternalPromptDispatchMode::Async,
        client,
        session_id: "ses_synthetic_tail_tools".to_string(),
        input: json!({ "path": { "id": "ses_synthetic_tail_tools" }, "body": { "parts": [] } }),
        source: "test:synthetic-tail-tools".to_string(),
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

#[tokio::test]
async fn test_mixed_real_user_tail_follows_assistant_waiting_on_tools_prompt_sent() {
    let _guard = TEST_LOCK.lock().await;
    cleanup();
    let prompt_calls = Arc::new(AtomicUsize::new(0));
    let prompt_calls_clone = prompt_calls.clone();

    let client = TestClient::default()
        .with_status(|| Ok(json!({ "data": { "ses_mixed_tail_tools": { "type": "idle" } } })))
        .with_messages(|_, _| {
            Ok(json!({
                "data": [
                    { "info": { "id": "msg_user", "role": "user" }, "parts": [{ "type": "text", "text": "run work" }] },
                    { "info": { "id": "msg_assistant", "role": "assistant", "finish": "tool-calls" }, "parts": [{ "type": "tool_use", "id": "toolu_pending", "state": { "status": "running" } }] },
                    { "info": { "id": "msg_mixed_user", "role": "user" }, "parts": [
                        { "type": "text", "text": "wake\n<!-- OMO_INTERNAL_INITIATOR -->" },
                        { "type": "text", "text": "real user follow-up" }
                    ] }
                ]
            }))
        })
        .with_prompt_async(move |_| {
            prompt_calls_clone.fetch_add(1, Ordering::SeqCst);
            Ok(json!({ "ok": true }))
        });

    let result = dispatch_internal_prompt(InternalPromptDispatchArgs {
        mode: InternalPromptDispatchMode::Async,
        client,
        session_id: "ses_mixed_tail_tools".to_string(),
        input: json!({ "path": { "id": "ses_mixed_tail_tools" }, "body": { "parts": [] } }),
        source: "test:mixed-tail-tools".to_string(),
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

    assert_eq!(result.status(), "dispatched");
    assert_eq!(prompt_calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn test_tool_state_check_disabled_prompt_sent() {
    let _guard = TEST_LOCK.lock().await;
    cleanup();
    let prompt_calls = Arc::new(AtomicUsize::new(0));
    let prompt_calls_clone = prompt_calls.clone();

    let client = TestClient::default()
        .with_status(|| Ok(json!({ "data": { "ses_recovery_tools": { "type": "idle" } } })))
        .with_messages(|_, _| {
            Ok(json!({
                "data": [{ "info": { "id": "msg_assistant", "role": "assistant", "finish": "tool-calls" }, "parts": [{ "type": "tool_use", "id": "toolu_pending", "state": { "status": "pending" } }] }]
            }))
        })
        .with_prompt_async(move |_| {
            prompt_calls_clone.fetch_add(1, Ordering::SeqCst);
            Ok(json!({ "ok": true }))
        });

    let result = dispatch_internal_prompt(InternalPromptDispatchArgs {
        mode: InternalPromptDispatchMode::Async,
        client,
        session_id: "ses_recovery_tools".to_string(),
        input: json!({ "path": { "id": "ses_recovery_tools" }, "body": { "parts": [] } }),
        source: "test:recovery-tools".to_string(),
        dedupe_key: None,
        queue_behavior: None,
        queue: None,
        queue_retry_ms: None,
        settle_ms: Some(0),
        post_dispatch_hold_ms: Some(0),
        semantic_dedupe_hold_ms: None,
        dispatch_timeout_ms: None,
        check_status: None,
        check_tool_state: Some(false),
        clock: None,
    })
    .await;

    assert_eq!(result.status(), "dispatched");
    assert_eq!(prompt_calls.load(Ordering::SeqCst), 1);
}
