//! Port of senpi packages/ai/src/api/context-room.ts.

use crate::types::{Context, Model};
use crate::utils::estimate::estimate_context_tokens;

pub const CONTEXT_SAFETY_TOKENS: u64 = 4096;
/// Tokens always left for the answer when a thinking budget shares the response ceiling.
pub const MIN_ANSWER_TOKENS: u64 = 1024;
const MIN_MAX_TOKENS: u64 = 1;
/// Windows too small to hold the safety margin plus one answer keep the legacy one-token floor.
pub const CONTEXT_GUARD_MIN_WINDOW: u64 = CONTEXT_SAFETY_TOKENS + MIN_ANSWER_TOKENS;

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error(
    "Context window exhausted: the conversation is estimated at {estimated_tokens} of {context_window} tokens, \
     leaving fewer than {min_answer_tokens} tokens for a response. \
     Compact the conversation, enable auto-compaction, or start a new session before retrying."
)]
pub struct ContextWindowExhaustedError {
    pub estimated_tokens: u64,
    pub context_window: u64,
    pub min_answer_tokens: u64,
}

impl ContextWindowExhaustedError {
    pub fn new(estimated_tokens: u64, context_window: u64) -> Self {
        Self { estimated_tokens, context_window, min_answer_tokens: MIN_ANSWER_TOKENS }
    }
}

/// Fit the requested output budget into the room the context leaves in the model window.
/// For windows of at least [`CONTEXT_GUARD_MIN_WINDOW`] tokens this returns an error instead of
/// shrinking the budget below [`MIN_ANSWER_TOKENS`]: such a request can only return a truncated
/// tool call or an empty "length" stop while still billing the whole prompt.
pub fn clamp_max_tokens_to_context(
    model: &Model,
    context: &Context,
    max_tokens: u64,
) -> Result<u64, ContextWindowExhaustedError> {
    if model.context_window == 0 {
        return Ok(if max_tokens > 0 { max_tokens.max(MIN_MAX_TOKENS) } else { MIN_MAX_TOKENS });
    }
    let estimated_tokens = estimate_context_tokens(context).tokens;
    let available = model.context_window as i64 - estimated_tokens as i64 - CONTEXT_SAFETY_TOKENS as i64;
    if model.context_window >= CONTEXT_GUARD_MIN_WINDOW && available < MIN_ANSWER_TOKENS as i64 {
        return Err(ContextWindowExhaustedError::new(estimated_tokens, model.context_window));
    }
    let safe_available = available.max(MIN_MAX_TOKENS as i64) as u64;
    let requested = if max_tokens > 0 { max_tokens } else { safe_available };
    Ok(requested.min(safe_available))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Message, UserMessage, UserContent};

    fn any_model() -> Model {
        crate::models_generated::get_builtin_provider_models("anthropic").expect("models")[0].clone()
    }

    fn model_with_window(context_window: u64) -> Model {
        let mut model = any_model();
        model.context_window = context_window;
        model
    }

    fn context_with_text(chars: usize) -> Context {
        Context {
            system_prompt: None,
            messages: vec![Message::User(UserMessage { content: UserContent::Text("x".repeat(chars)), timestamp: 0 })],
            tools: None,
        }
    }

    #[test]
    fn zero_window_clamps_to_the_one_token_floor() {
        let model = model_with_window(0);
        assert_eq!(clamp_max_tokens_to_context(&model, &Context::default(), 0), Ok(1));
        assert_eq!(clamp_max_tokens_to_context(&model, &Context::default(), 500), Ok(500));
    }

    #[test]
    fn tiny_window_shrinks_the_budget_instead_of_failing() {
        let model = model_with_window(100);
        assert_eq!(clamp_max_tokens_to_context(&model, &Context::default(), 8192), Ok(1));
    }

    #[test]
    fn exhausted_window_reports_the_estimate_and_window() {
        let model = model_with_window(CONTEXT_GUARD_MIN_WINDOW);
        let context = context_with_text(CONTEXT_SAFETY_TOKENS as usize * 4 + 4);
        let error = clamp_max_tokens_to_context(&model, &context, 8192).expect_err("exhausted");
        assert_eq!(error.context_window, CONTEXT_GUARD_MIN_WINDOW);
        assert!(error.estimated_tokens >= MIN_ANSWER_TOKENS);
        assert!(error.to_string().starts_with("Context window exhausted: the conversation is estimated at "));
    }

    #[test]
    fn ample_window_keeps_the_requested_budget() {
        let model = model_with_window(200_000);
        assert_eq!(clamp_max_tokens_to_context(&model, &context_with_text(40), 8192), Ok(8192));
    }
}
