use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, LazyLock, Mutex};

use pretty_assertions::assert_eq;
use serde_json::{Value, json};
use utils::internal_initiator_marker::{
    create_internal_agent_continuation_text_part, create_internal_agent_text_part,
};
use utils::prompt_async_gate::*;

static TEST_LOCK: LazyLock<tokio::sync::Mutex<()>> = LazyLock::new(|| tokio::sync::Mutex::new(()));

type AsyncCallback = Arc<dyn Fn(&Value) -> BoxFuture<'static, Result<Value, String>> + Send + Sync>;

#[derive(Clone, Default)]
struct TestClient {
    prompt_async_fn: Option<AsyncCallback>,
    has_prompt_async_flag: bool,
}

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
}

impl PromptGateClient for TestClient {
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

    fn has_prompt_async(&self) -> bool {
        self.has_prompt_async_flag
    }
}

fn cleanup() {
    _set_prompt_gate_messages_fetch_timeout_ms_for_testing(None);
    release_all_prompt_async_reservations_for_testing();
}

#[tokio::test]
async fn test_same_semantic_prompt_returns_after_broad_hold_coalesced() {
    let _guard = TEST_LOCK.lock().await;
    cleanup();
    let clock = Arc::new(MockClock::new(10_000));
    let prompt_calls = Arc::new(Mutex::new(Vec::new()));
    let prompt_calls_clone = prompt_calls.clone();

    let input = json!({
        "path": { "id": "ses_semantic_duplicate" },
        "body": { "parts": [{ "type": "text", "text": "continue" }] },
        "query": { "directory": "/workspace/project" }
    });

    let client = TestClient::default().with_prompt_async(move |_| {
        prompt_calls_clone
            .lock()
            .unwrap()
            .push("prompt".to_string());
        Ok(json!({ "ok": true }))
    });

    let first = dispatch_internal_prompt(InternalPromptDispatchArgs {
        mode: InternalPromptDispatchMode::Async,
        client: client.clone(),
        session_id: "ses_semantic_duplicate".to_string(),
        input: input.clone(),
        source: "test:semantic:first".to_string(),
        dedupe_key: None,
        queue_behavior: None,
        queue: None,
        queue_retry_ms: None,
        settle_ms: Some(0),
        post_dispatch_hold_ms: Some(1),
        semantic_dedupe_hold_ms: None,
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
        session_id: "ses_semantic_duplicate".to_string(),
        input,
        source: "test:semantic:second".to_string(),
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
    assert_eq!(
        second,
        InternalPromptDispatchResult::Queued {
            queued_by: "test:semantic:first".to_string(),
            position: 0,
        }
    );
    assert_eq!(*prompt_calls.lock().unwrap(), vec!["prompt".to_string()]);
}

#[tokio::test]
async fn test_same_semantic_prompt_different_object_key_order_coalesced() {
    let _guard = TEST_LOCK.lock().await;
    cleanup();
    let clock = Arc::new(MockClock::new(10_000));
    let prompt_calls = Arc::new(Mutex::new(Vec::new()));
    let prompt_calls_clone = prompt_calls.clone();

    let client = TestClient::default().with_prompt_async(move |_| {
        prompt_calls_clone
            .lock()
            .unwrap()
            .push("prompt".to_string());
        Ok(json!({ "ok": true }))
    });

    let first_input = json!({
        "path": { "id": "ses_semantic_key_order" },
        "body": { "parts": [{ "type": "text", "text": "continue" }], "noReply": true },
        "query": { "directory": "/workspace/project" }
    });

    let second_input = json!({
        "query": { "directory": "/workspace/project" },
        "body": { "noReply": true, "parts": [{ "text": "continue", "type": "text" }] },
        "path": { "id": "ses_semantic_key_order" }
    });

    let first = dispatch_internal_prompt(InternalPromptDispatchArgs {
        mode: InternalPromptDispatchMode::Async,
        client: client.clone(),
        session_id: "ses_semantic_key_order".to_string(),
        input: first_input,
        source: "test:semantic-order:first".to_string(),
        dedupe_key: None,
        queue_behavior: None,
        queue: None,
        queue_retry_ms: None,
        settle_ms: Some(0),
        post_dispatch_hold_ms: Some(1),
        semantic_dedupe_hold_ms: None,
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
        session_id: "ses_semantic_key_order".to_string(),
        input: second_input,
        source: "test:semantic-order:second".to_string(),
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
    assert_eq!(
        second,
        InternalPromptDispatchResult::Queued {
            queued_by: "test:semantic-order:first".to_string(),
            position: 0,
        }
    );
    assert_eq!(*prompt_calls.lock().unwrap(), vec!["prompt".to_string()]);
}

#[tokio::test]
async fn test_distinct_semantic_prompt_follows_broad_hold_dispatches() {
    let _guard = TEST_LOCK.lock().await;
    cleanup();
    let clock = Arc::new(MockClock::new(10_000));
    let prompt_calls = Arc::new(Mutex::new(Vec::new()));
    let prompt_calls_clone = prompt_calls.clone();

    let client = TestClient::default().with_prompt_async(move |input| {
        let text = input["body"]["parts"][0]["text"]
            .as_str()
            .unwrap_or("")
            .to_string();
        prompt_calls_clone.lock().unwrap().push(text);
        Ok(json!({ "ok": true }))
    });

    let first = dispatch_internal_prompt(InternalPromptDispatchArgs {
        mode: InternalPromptDispatchMode::Async,
        client: client.clone(),
        session_id: "ses_semantic_distinct".to_string(),
        input: json!({
            "path": { "id": "ses_semantic_distinct" },
            "body": { "parts": [{ "type": "text", "text": "continue first" }] },
            "query": { "directory": "/workspace/project" }
        }),
        source: "test:semantic-distinct:first".to_string(),
        dedupe_key: None,
        queue_behavior: None,
        queue: None,
        queue_retry_ms: None,
        settle_ms: Some(0),
        post_dispatch_hold_ms: Some(1),
        semantic_dedupe_hold_ms: None,
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
        session_id: "ses_semantic_distinct".to_string(),
        input: json!({
            "path": { "id": "ses_semantic_distinct" },
            "body": { "parts": [{ "type": "text", "text": "continue second" }] },
            "query": { "directory": "/workspace/project" }
        }),
        source: "test:semantic-distinct:second".to_string(),
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
    assert_eq!(
        *prompt_calls.lock().unwrap(),
        vec!["continue first".to_string(), "continue second".to_string()]
    );
}

#[tokio::test]
async fn test_distinct_long_prompts_same_prefix_length_dispatches() {
    let _guard = TEST_LOCK.lock().await;
    cleanup();
    let clock = Arc::new(MockClock::new(10_000));
    let prompt_calls = Arc::new(Mutex::new(Vec::new()));
    let prompt_calls_clone = prompt_calls.clone();
    let long_prefix = "x".repeat(9000);

    let client = TestClient::default().with_prompt_async(move |input| {
        let text = input["body"]["parts"][0]["text"]
            .as_str()
            .unwrap_or("")
            .to_string();
        prompt_calls_clone.lock().unwrap().push(text);
        Ok(json!({ "ok": true }))
    });

    let first = dispatch_internal_prompt(InternalPromptDispatchArgs {
        mode: InternalPromptDispatchMode::Async,
        client: client.clone(),
        session_id: "ses_semantic_long_distinct".to_string(),
        input: json!({
            "path": { "id": "ses_semantic_long_distinct" },
            "body": { "parts": [{ "type": "text", "text": format!("{long_prefix}A") }] },
            "query": { "directory": "/workspace/project" }
        }),
        source: "test:semantic-long:first".to_string(),
        dedupe_key: None,
        queue_behavior: None,
        queue: None,
        queue_retry_ms: None,
        settle_ms: Some(0),
        post_dispatch_hold_ms: Some(1),
        semantic_dedupe_hold_ms: None,
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
        session_id: "ses_semantic_long_distinct".to_string(),
        input: json!({
            "path": { "id": "ses_semantic_long_distinct" },
            "body": { "parts": [{ "type": "text", "text": format!("{long_prefix}B") }] },
            "query": { "directory": "/workspace/project" }
        }),
        source: "test:semantic-long:second".to_string(),
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
    assert_eq!(
        *prompt_calls.lock().unwrap(),
        vec![format!("{long_prefix}A"), format!("{long_prefix}B")]
    );
}

#[tokio::test]
async fn test_synthetic_internal_prompts_without_continuation_metadata_remain_distinct() {
    let _guard = TEST_LOCK.lock().await;
    cleanup();
    let clock = Arc::new(MockClock::new(10_000));
    let prompt_calls = Arc::new(Mutex::new(Vec::new()));
    let prompt_calls_clone = prompt_calls.clone();

    let client = TestClient::default().with_prompt_async(move |input| {
        let text = input["body"]["parts"][0]["text"]
            .as_str()
            .unwrap_or("")
            .to_string();
        prompt_calls_clone.lock().unwrap().push(text);
        Ok(json!({ "ok": true }))
    });

    let mut part_a = create_internal_agent_text_part("internal route A");
    part_a["synthetic"] = json!(true);

    let mut part_b = create_internal_agent_text_part("internal route B");
    part_b["synthetic"] = json!(true);

    let first = dispatch_internal_prompt(InternalPromptDispatchArgs {
        mode: InternalPromptDispatchMode::Async,
        client: client.clone(),
        session_id: "ses_semantic_synthetic_non_continuation".to_string(),
        input: json!({
            "path": { "id": "ses_semantic_synthetic_non_continuation" },
            "body": { "parts": [part_a] }
        }),
        source: "test:semantic-synthetic:first".to_string(),
        dedupe_key: None,
        queue_behavior: None,
        queue: None,
        queue_retry_ms: None,
        settle_ms: Some(0),
        post_dispatch_hold_ms: Some(1),
        semantic_dedupe_hold_ms: None,
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
        session_id: "ses_semantic_synthetic_non_continuation".to_string(),
        input: json!({
            "path": { "id": "ses_semantic_synthetic_non_continuation" },
            "body": { "parts": [part_b] }
        }),
        source: "test:semantic-synthetic:second".to_string(),
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
    assert_eq!(
        *prompt_calls.lock().unwrap(),
        vec![
            "internal route A\n<!-- OMO_INTERNAL_INITIATOR -->".to_string(),
            "internal route B\n<!-- OMO_INTERNAL_INITIATOR -->".to_string(),
        ]
    );
}

#[tokio::test]
async fn test_continuation_prompts_differ_by_route_metadata_coalesce_intent_duplicates() {
    let _guard = TEST_LOCK.lock().await;
    cleanup();
    let clock = Arc::new(MockClock::new(10_000));
    let prompt_calls = Arc::new(AtomicUsize::new(0));
    let prompt_calls_clone = prompt_calls.clone();

    let client = TestClient::default().with_prompt_async(move |_| {
        prompt_calls_clone.fetch_add(1, Ordering::SeqCst);
        Ok(json!({ "ok": true }))
    });

    let first = dispatch_internal_prompt(InternalPromptDispatchArgs {
        mode: InternalPromptDispatchMode::Async,
        client: client.clone(),
        session_id: "ses_semantic_continuation_intent".to_string(),
        input: json!({
            "path": { "id": "ses_semantic_continuation_intent" },
            "body": {
                "parts": [create_internal_agent_continuation_text_part("continue from route A")],
                "agent": "sisyphus",
                "model": "openai/gpt-5"
            },
            "query": { "directory": "/workspace/project", "tools": ["task"], "route": "todo-enforcer" }
        }),
        source: "test:semantic-continuation:first".to_string(),
        dedupe_key: None,
        queue_behavior: None,
        queue: None,
        queue_retry_ms: None,
        settle_ms: Some(0),
        post_dispatch_hold_ms: Some(1),
        semantic_dedupe_hold_ms: None,
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
        session_id: "ses_semantic_continuation_intent".to_string(),
        input: json!({
            "path": { "id": "ses_semantic_continuation_intent" },
            "body": {
                "parts": [create_internal_agent_continuation_text_part("continue from route B")],
                "agent": "atlas",
                "model": "anthropic/claude-sonnet"
            },
            "query": { "directory": "/workspace/project", "tools": ["team_task_create"], "route": "team-mailbox" }
        }),
        source: "test:semantic-continuation:second".to_string(),
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
    assert_eq!(
        second,
        InternalPromptDispatchResult::Queued {
            queued_by: "test:semantic-continuation:first".to_string(),
            position: 0,
        }
    );
    assert_eq!(prompt_calls.load(Ordering::SeqCst), 1);
}

#[test]
fn test_dedupe_keys_are_stable_across_equivalent_structures() {
    let first = json!({
        "body": { "parts": [{ "type": "text", "text": "continue" }] },
        "path": { "id": "ses_stable_key" }
    });
    let second = json!({
        "path": { "id": "ses_stable_key" },
        "body": { "parts": [{ "text": "continue", "type": "text" }] }
    });
    let first_key = create_semantic_prompt_dedupe_key(&first);
    let second_key = create_semantic_prompt_dedupe_key(&second);
    assert_eq!(first_key, second_key);
}

#[tokio::test]
async fn test_prompt_async_rejects_retry_after_post_dispatch_hold_coalesces() {
    let _guard = TEST_LOCK.lock().await;
    cleanup();
    let clock = Arc::new(MockClock::new(10_000));
    let prompt_calls = Arc::new(AtomicUsize::new(0));
    let prompt_calls_clone = prompt_calls.clone();

    let input = json!({
        "path": { "id": "ses_reject_after_attempt_semantic" },
        "body": { "parts": [{ "type": "text", "text": "continue after failed accept" }] },
        "query": { "directory": "/workspace/project" }
    });

    let client = TestClient::default().with_prompt_async(move |_| {
        let count = prompt_calls_clone.fetch_add(1, Ordering::SeqCst);
        if count == 0 {
            Err("post-dispatch failure".to_string())
        } else {
            Ok(json!({ "accepted": true }))
        }
    });

    let first = dispatch_internal_prompt(InternalPromptDispatchArgs {
        mode: InternalPromptDispatchMode::Async,
        client: client.clone(),
        session_id: "ses_reject_after_attempt_semantic".to_string(),
        input: input.clone(),
        source: "test:reject-semantic:first".to_string(),
        dedupe_key: None,
        queue_behavior: None,
        queue: None,
        queue_retry_ms: None,
        settle_ms: Some(0),
        post_dispatch_hold_ms: Some(1),
        semantic_dedupe_hold_ms: Some(100),
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
        session_id: "ses_reject_after_attempt_semantic".to_string(),
        input,
        source: "test:reject-semantic:second".to_string(),
        dedupe_key: None,
        queue_behavior: None,
        queue: None,
        queue_retry_ms: None,
        settle_ms: Some(0),
        post_dispatch_hold_ms: Some(1),
        semantic_dedupe_hold_ms: Some(100),
        dispatch_timeout_ms: None,
        check_status: None,
        check_tool_state: None,
        clock: Some(clock.clone()),
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
            queued_by: "test:reject-semantic:first".to_string(),
            position: 0,
        }
    );
    assert_eq!(prompt_calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn test_failed_attempted_dispatch_released_retries_inside_semantic_window() {
    let _guard = TEST_LOCK.lock().await;
    cleanup();
    let clock = Arc::new(MockClock::new(10_000));
    let prompt_calls = Arc::new(AtomicUsize::new(0));
    let prompt_calls_clone = prompt_calls.clone();

    let input = json!({
        "path": { "id": "ses_reject_after_attempt_release" },
        "body": { "parts": [{ "type": "text", "text": "continue after release" }] },
        "query": { "directory": "/workspace/project" }
    });

    let client = TestClient::default().with_prompt_async(move |_| {
        let count = prompt_calls_clone.fetch_add(1, Ordering::SeqCst);
        if count == 0 {
            Err("post-dispatch failure".to_string())
        } else {
            Ok(json!({ "accepted": true }))
        }
    });

    let first = dispatch_internal_prompt(InternalPromptDispatchArgs {
        mode: InternalPromptDispatchMode::Async,
        client: client.clone(),
        session_id: "ses_reject_after_attempt_release".to_string(),
        input: input.clone(),
        source: "test:reject-release:first".to_string(),
        dedupe_key: None,
        queue_behavior: None,
        queue: None,
        queue_retry_ms: None,
        settle_ms: Some(0),
        post_dispatch_hold_ms: Some(100),
        semantic_dedupe_hold_ms: Some(1_000),
        dispatch_timeout_ms: None,
        check_status: None,
        check_tool_state: None,
        clock: Some(clock.clone()),
    })
    .await;

    let released = release_prompt_async_reservation(
        "ses_reject_after_attempt_release",
        "test:reject-release:first",
        None,
    );

    clock.advance_ms(2);

    let second = dispatch_internal_prompt(InternalPromptDispatchArgs {
        mode: InternalPromptDispatchMode::Async,
        client,
        session_id: "ses_reject_after_attempt_release".to_string(),
        input,
        source: "test:reject-release:second".to_string(),
        dedupe_key: None,
        queue_behavior: None,
        queue: None,
        queue_retry_ms: None,
        settle_ms: Some(0),
        post_dispatch_hold_ms: Some(100),
        semantic_dedupe_hold_ms: Some(1_000),
        dispatch_timeout_ms: None,
        check_status: None,
        check_tool_state: None,
        clock: Some(clock.clone()),
    })
    .await;

    assert_eq!(first.status(), "failed");
    if let InternalPromptDispatchResult::Failed {
        dispatch_attempted, ..
    } = first
    {
        assert_eq!(dispatch_attempted, true);
    }
    assert_eq!(released, true);
    assert_eq!(second.status(), "dispatched");
    assert_eq!(prompt_calls.load(Ordering::SeqCst), 2);
}
