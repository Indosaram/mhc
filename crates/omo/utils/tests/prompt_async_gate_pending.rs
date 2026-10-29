use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, LazyLock};

use pretty_assertions::assert_eq;
use serde_json::{Value, json};
use utils::internal_initiator_marker::{
    OMO_INTERNAL_INITIATOR_MARKER, OMO_INTERNAL_NOREPLY_MARKER,
};
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
async fn test_completed_assistant_question_has_no_real_user_answer_no_prompt() {
    let _guard = TEST_LOCK.lock().await;
    cleanup();
    let prompt_calls = Arc::new(AtomicUsize::new(0));
    let prompt_calls_clone = prompt_calls.clone();

    let client = TestClient::default()
        .with_status(|| Ok(json!({ "data": { "ses_completed_question": { "type": "idle" } } })))
        .with_messages(|_, _| {
            Ok(json!({
                "data": [
                    {
                        "info": {
                            "id": "msg_assistant",
                            "role": "assistant",
                            "finish": "tool-calls",
                            "time": { "completed": 1_762_000_000_000u64 }
                        },
                        "parts": [{ "type": "tool", "tool": "question", "state": { "status": "error" } }]
                    }
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
        session_id: "ses_completed_question".to_string(),
        input: json!({ "path": { "id": "ses_completed_question" }, "body": { "parts": [] } }),
        source: "test:completed-question".to_string(),
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

#[test]
fn test_metadata_empty_unknown_assistant_turn_blocks() {
    let messages = vec![
        json!({
            "info": { "role": "user", "time": { "created": 1000 } }
        }),
        json!({
            "info": { "role": "assistant", "finish": "unknown", "time": { "created": 2000, "completed": 3000 } }
        }),
    ];
    let blocks = latest_assistant_turn_blocks_internal_prompt(&messages);
    assert_eq!(blocks, true);
}

#[test]
fn test_metadata_completed_tool_calls_assistant_turn_blocks() {
    let messages = vec![
        json!({
            "info": { "role": "user", "time": { "created": 1000 } }
        }),
        json!({
            "info": { "role": "assistant", "finish": "tool-calls", "time": { "created": 2000, "completed": 3000 } }
        }),
    ];
    let blocks = latest_assistant_turn_blocks_internal_prompt(&messages);
    assert_eq!(blocks, true);
}

#[test]
fn test_completed_assistant_question_tool_no_real_user_answer_blocks() {
    let messages = vec![
        json!({
            "info": { "role": "user", "time": { "created": 1000 } },
            "parts": [{ "type": "text", "text": "start" }]
        }),
        json!({
            "info": { "role": "assistant", "finish": "tool-calls", "time": { "created": 2000, "completed": 3000 } },
            "parts": [{ "type": "tool_use", "name": "question", "state": { "status": "error" } }]
        }),
    ];
    assert_eq!(
        latest_assistant_turn_blocks_internal_prompt(&messages),
        true
    );
}

#[test]
fn test_internal_wake_follows_unanswered_question_does_not_count_as_answer() {
    let messages = vec![
        json!({
            "info": { "role": "assistant", "finish": "tool-calls", "time": { "created": 2000, "completed": 3000 } },
            "parts": [{ "type": "tool-invocation", "toolName": "question", "state": { "status": "error" } }]
        }),
        json!({
            "info": { "role": "user", "time": { "created": 4000 } },
            "parts": [{ "type": "text", "text": "wake\n<!-- OMO_INTERNAL_INITIATOR -->" }]
        }),
    ];
    assert_eq!(
        latest_assistant_turn_blocks_internal_prompt(&messages),
        true
    );
}

#[test]
fn test_opencode_question_tool_field_no_real_user_answer_blocks() {
    let messages = vec![
        json!({
            "info": { "role": "assistant", "finish": "tool-calls", "time": { "created": 2000, "completed": 3000 } },
            "parts": [{ "type": "tool", "tool": "question", "state": { "status": "error" } }]
        }),
        json!({
            "info": { "role": "user", "time": { "created": 4000 } },
            "parts": [{ "type": "text", "text": "wake\n<!-- OMO_INTERNAL_INITIATOR -->" }]
        }),
    ];
    assert_eq!(
        latest_assistant_turn_blocks_internal_prompt(&messages),
        true
    );
}

#[test]
fn test_opencode_ask_user_question_tool_field_no_real_user_answer_blocks() {
    let messages = vec![
        json!({
            "info": { "role": "assistant", "finish": "tool-calls", "time": { "created": 2000, "completed": 3000 } },
            "parts": [{ "type": "tool", "tool": "ask_user_question", "state": { "status": "error" } }]
        }),
        json!({
            "info": { "role": "user", "time": { "created": 4000 } },
            "parts": [{ "type": "text", "text": "wake\n<!-- OMO_INTERNAL_INITIATOR -->" }]
        }),
    ];
    assert_eq!(
        latest_assistant_turn_blocks_internal_prompt(&messages),
        true
    );
}

#[test]
fn test_answered_question_tool_completed_not_blocked() {
    let messages = vec![json!({
        "info": { "role": "assistant", "finish": "tool-calls", "time": { "created": 2000, "completed": 3000 } },
        "parts": [{
            "type": "tool",
            "tool": "question",
            "state": {
                "status": "completed",
                "output": "User has answered your questions: \"format\"=\"Flat codex:sess_abc\"."
            }
        }]
    })];
    assert_eq!(
        latest_assistant_turn_blocks_internal_prompt(&messages),
        false
    );
}

#[test]
fn test_real_user_answer_follows_question_not_blocked() {
    let messages = vec![
        json!({
            "info": { "role": "assistant", "finish": "tool-calls", "time": { "created": 2000, "completed": 3000 } },
            "parts": [{ "type": "tool_use", "name": "question", "state": { "status": "error" } }]
        }),
        json!({
            "info": { "role": "user", "time": { "created": 4000 } },
            "parts": [{ "type": "text", "text": "continue without the question tool" }]
        }),
    ];
    assert_eq!(
        latest_assistant_turn_blocks_internal_prompt(&messages),
        false
    );
}

#[test]
fn test_completed_assistant_followed_by_orphaned_reply_required_internal_wake_admitted() {
    let messages = vec![
        json!({
            "info": { "role": "assistant", "finish": "stop", "time": { "created": 1000, "completed": 2000 } },
            "parts": [{ "type": "text", "text": "working" }]
        }),
        json!({
            "info": { "role": "user", "time": { "created": 3000 } },
            "parts": [{ "type": "text", "text": "continue\n<!-- OMO_INTERNAL_INITIATOR -->", "synthetic": true }]
        }),
    ];
    assert_eq!(
        latest_assistant_turn_blocks_internal_prompt(&messages),
        false
    );
}

#[test]
fn test_internal_continuation_gets_only_empty_unknown_assistant_turn_blocks() {
    let messages = vec![
        json!({
            "info": { "role": "user", "time": { "created": 1000 } },
            "parts": [{ "type": "text", "text": "continue\n<!-- OMO_INTERNAL_INITIATOR -->", "synthetic": true }]
        }),
        json!({
            "info": { "role": "assistant", "finish": "unknown", "time": { "created": 2000, "completed": 3000 } },
            "parts": [
                { "type": "step-start" },
                { "type": "step-finish", "reason": "unknown" }
            ]
        }),
    ];
    assert_eq!(
        latest_assistant_turn_blocks_internal_prompt(&messages),
        true
    );
}

#[test]
fn test_internal_continuation_receives_assistant_text_not_blocked() {
    let messages = vec![
        json!({
            "info": { "role": "user", "time": { "created": 1000 } },
            "parts": [{ "type": "text", "text": "continue\n<!-- OMO_INTERNAL_INITIATOR -->", "synthetic": true }]
        }),
        json!({
            "info": { "role": "assistant", "finish": "unknown", "time": { "created": 2000, "completed": 3000 } },
            "parts": [
                { "type": "step-start" },
                { "type": "text", "text": "I will keep working." },
                { "type": "step-finish", "reason": "unknown" }
            ]
        }),
    ];
    assert_eq!(
        latest_assistant_turn_blocks_internal_prompt(&messages),
        false
    );
}

#[test]
fn test_completed_assistant_followed_by_noreply_tail_not_blocked() {
    let noreply_tail =
        format!("notification\n{OMO_INTERNAL_INITIATOR_MARKER}\n{OMO_INTERNAL_NOREPLY_MARKER}");
    let messages = vec![
        json!({
            "info": { "role": "assistant", "finish": "stop", "time": { "created": 1000, "completed": 2000 } },
            "parts": [{ "type": "text", "text": "done with the work" }]
        }),
        json!({
            "info": { "role": "user", "time": { "created": 3000 } },
            "parts": [{ "type": "text", "text": noreply_tail, "synthetic": true }]
        }),
    ];
    assert_eq!(
        latest_assistant_turn_blocks_internal_prompt(&messages),
        false
    );
}

#[test]
fn test_completed_assistant_followed_by_stacked_noreply_tails_not_blocked() {
    let noreply_tail =
        format!("notification\n{OMO_INTERNAL_INITIATOR_MARKER}\n{OMO_INTERNAL_NOREPLY_MARKER}");
    let messages = vec![
        json!({
            "info": { "role": "assistant", "finish": "stop", "time": { "created": 1000, "completed": 2000 } },
            "parts": [{ "type": "text", "text": "fired the background tasks" }]
        }),
        json!({
            "info": { "role": "user", "time": { "created": 3000 } },
            "parts": [{ "type": "text", "text": noreply_tail, "synthetic": true }]
        }),
        json!({
            "info": { "role": "user", "time": { "created": 4000 } },
            "parts": [{ "type": "text", "text": noreply_tail, "synthetic": true }]
        }),
    ];
    assert_eq!(
        latest_assistant_turn_blocks_internal_prompt(&messages),
        false
    );
}

#[test]
fn test_reply_expecting_internal_tail_behind_noreply_tail_admitted() {
    let noreply_tail =
        format!("notification\n{OMO_INTERNAL_INITIATOR_MARKER}\n{OMO_INTERNAL_NOREPLY_MARKER}");
    let reply_expecting = format!("continue\n{OMO_INTERNAL_INITIATOR_MARKER}");
    let messages = vec![
        json!({
            "info": { "role": "assistant", "finish": "stop", "time": { "created": 1000, "completed": 2000 } },
            "parts": [{ "type": "text", "text": "working" }]
        }),
        json!({
            "info": { "role": "user", "time": { "created": 3000 } },
            "parts": [{ "type": "text", "text": reply_expecting, "synthetic": true }]
        }),
        json!({
            "info": { "role": "user", "time": { "created": 4000 } },
            "parts": [{ "type": "text", "text": noreply_tail, "synthetic": true }]
        }),
    ];
    assert_eq!(
        latest_assistant_turn_blocks_internal_prompt(&messages),
        false
    );
}

#[test]
fn test_actively_waiting_assistant_behind_noreply_tail_blocks() {
    let noreply_tail =
        format!("notification\n{OMO_INTERNAL_INITIATOR_MARKER}\n{OMO_INTERNAL_NOREPLY_MARKER}");
    let messages = vec![
        json!({
            "info": { "role": "assistant", "finish": "tool-calls", "time": { "created": 1000 } },
            "parts": [{ "type": "tool", "tool": "bash", "state": { "status": "running" } }]
        }),
        json!({
            "info": { "role": "user", "time": { "created": 3000 } },
            "parts": [{ "type": "text", "text": noreply_tail, "synthetic": true }]
        }),
    ];
    assert_eq!(
        latest_assistant_turn_blocks_internal_prompt(&messages),
        true
    );
}
