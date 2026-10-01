use maho_ai::types::{ThinkingBudgets, ThinkingLevel};
use serde_json::{json, Value};

use super::harness::*;

fn vllm_model(compat: Value) -> maho_ai::types::Model {
    model(&[
        ("id", json!("zai-org/glm-5.2")),
        ("name", json!("GLM 5.2 (local vLLM)")),
        ("provider", json!("local-vllm")),
        ("baseUrl", json!("http://localhost:8000/v1")),
        ("reasoning", json!(true)),
        ("contextWindow", json!(262144)),
        ("maxTokens", json!(16384)),
        ("compat", compat),
    ])
}

fn default_compat() -> Value {
    json!({ "thinkingFormat": "zai", "supportsThinkingTokenBudget": true })
}

fn budgets(medium: Option<u64>, high: Option<u64>) -> ThinkingBudgets {
    ThinkingBudgets { minimal: None, low: None, medium, high, max: None }
}

async fn capture(
    model: &maho_ai::types::Model,
    reasoning: Option<ThinkingLevel>,
    thinking_budgets: Option<ThinkingBudgets>,
    max_tokens: Option<u64>,
) -> Value {
    let transport = ScriptedTransport::success([json!({
        "choices": [{ "delta": {}, "finish_reason": "stop" }],
        "usage": { "prompt_tokens": 1, "completion_tokens": 1 },
    })]);
    let mut options = simple_options("test");
    options.reasoning = reasoning;
    options.thinking_budgets = thinking_budgets;
    options.stream.max_tokens = max_tokens;
    let captured = std::sync::Arc::new(std::sync::Mutex::new(None));
    let sink = captured.clone();
    options.stream.request.on_payload = Some(std::sync::Arc::new(move |payload: &Value, _model, _meta| {
        *sink.lock().expect("capture lock") = Some(payload.clone());
        None
    }));
    let _ = finish(&run_simple(model, &context(vec![user_message("Hi")], None), options, Some(transport.clone()))).await;
    let payload = captured.lock().expect("capture lock").clone();
    payload.unwrap_or_else(|| transport.last_request().body)
}

#[tokio::test]
async fn sends_the_configured_budget_for_the_requested_level() {
    let params = capture(
        &vllm_model(default_compat()),
        Some(ThinkingLevel::Medium),
        Some(budgets(Some(4096), None)),
        None,
    )
    .await;

    assert_eq!(params.get("thinking_token_budget").and_then(Value::as_u64), Some(4096));
}

#[tokio::test]
async fn omits_the_budget_when_neither_the_field_nor_the_alias_is_set() {
    let params = capture(
        &vllm_model(json!({ "thinkingFormat": "zai" })),
        Some(ThinkingLevel::Medium),
        Some(budgets(Some(4096), None)),
        None,
    )
    .await;

    assert_eq!(params.get("thinking_token_budget"), None);
    assert_eq!(params.get("thinking_budget"), None);
    assert_eq!(params.get("thinking_budget_tokens"), None);
}

#[tokio::test]
async fn omits_the_budget_when_thinking_is_off() {
    let params =
        capture(&vllm_model(default_compat()), None, Some(budgets(None, Some(8192))), None).await;

    assert_eq!(params.get("thinking_token_budget"), None);
}

#[tokio::test]
async fn clamps_xhigh_and_max_to_the_high_budget() {
    let xhigh = capture(
        &vllm_model(default_compat()),
        Some(ThinkingLevel::Xhigh),
        Some(budgets(None, Some(8192))),
        None,
    )
    .await;
    let max = capture(
        &vllm_model(default_compat()),
        Some(ThinkingLevel::Max),
        Some(budgets(None, Some(8192))),
        None,
    )
    .await;

    assert_eq!(xhigh.get("thinking_token_budget").and_then(Value::as_u64), Some(8192));
    assert_eq!(max.get("thinking_token_budget").and_then(Value::as_u64), Some(8192));
}

#[tokio::test]
async fn leaves_room_for_the_answer_when_the_budget_meets_the_response_ceiling() {
    let params = capture(&vllm_model(default_compat()), Some(ThinkingLevel::High), None, None).await;

    assert_eq!(params.get("thinking_token_budget").and_then(Value::as_u64), Some(16384 - 1024));
}

#[tokio::test]
async fn uses_the_caller_max_tokens_as_the_ceiling_when_it_is_lower_than_the_model_cap() {
    let params = capture(
        &vllm_model(default_compat()),
        Some(ThinkingLevel::High),
        Some(budgets(None, Some(8192))),
        Some(4096),
    )
    .await;

    assert_eq!(params.get("thinking_token_budget").and_then(Value::as_u64), Some(4096 - 1024));
}

#[tokio::test]
async fn sends_thinking_budget_when_thinking_token_budget_field_is_set() {
    let params = capture(
        &vllm_model(json!({ "thinkingFormat": "qwen", "thinkingTokenBudgetField": "thinking_budget" })),
        Some(ThinkingLevel::Medium),
        Some(budgets(Some(4096), None)),
        None,
    )
    .await;

    assert_eq!(params.get("thinking_budget").and_then(Value::as_u64), Some(4096));
    assert_eq!(params.get("thinking_token_budget"), None);
}

#[tokio::test]
async fn sends_thinking_budget_tokens_when_thinking_token_budget_field_is_set() {
    let params = capture(
        &vllm_model(json!({ "thinkingFormat": "qwen", "thinkingTokenBudgetField": "thinking_budget_tokens" })),
        Some(ThinkingLevel::Medium),
        Some(budgets(Some(4096), None)),
        None,
    )
    .await;

    assert_eq!(params.get("thinking_budget_tokens").and_then(Value::as_u64), Some(4096));
    assert_eq!(params.get("thinking_token_budget"), None);
}

#[tokio::test]
async fn lets_thinking_token_budget_field_win_over_the_boolean_alias() {
    let params = capture(
        &vllm_model(json!({
            "thinkingFormat": "zai",
            "supportsThinkingTokenBudget": true,
            "thinkingTokenBudgetField": "thinking_budget",
        })),
        Some(ThinkingLevel::Medium),
        Some(budgets(Some(4096), None)),
        None,
    )
    .await;

    assert_eq!(params.get("thinking_budget").and_then(Value::as_u64), Some(4096));
    assert_eq!(params.get("thinking_token_budget"), None);
}

#[tokio::test]
async fn puts_the_clamped_budget_in_chat_template_kwargs_when_var_is_thinking_budget() {
    let params = capture(
        &vllm_model(json!({
            "thinkingFormat": "chat-template",
            "chatTemplateKwargs": {
                "enable_thinking": { "$var": "thinking.enabled" },
                "thinking_budget": { "$var": "thinking.budget" },
            },
        })),
        Some(ThinkingLevel::High),
        None,
        None,
    )
    .await;

    assert_eq!(
        params.get("chat_template_kwargs"),
        Some(&json!({ "enable_thinking": true, "thinking_budget": 16384 - 1024 }))
    );
    assert_eq!(params.get("thinking_token_budget"), None);
}

#[tokio::test]
async fn omits_thinking_budget_from_chat_template_kwargs_when_thinking_is_off() {
    let params = capture(
        &vllm_model(json!({
            "thinkingFormat": "chat-template",
            "chatTemplateKwargs": {
                "enable_thinking": { "$var": "thinking.enabled" },
                "thinking_budget": { "$var": "thinking.budget" },
            },
        })),
        None,
        None,
        None,
    )
    .await;

    assert_eq!(params.get("chat_template_kwargs"), Some(&json!({ "enable_thinking": false })));
}
