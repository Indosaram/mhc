//! Port of senpi packages/ai/src/api/context-room.ts.
//!
//! Node-private copy: `src/api/context_room.rs` is owned by node 12-misc. `simple-options.ts`
//! (also ported here) clamps the request budget through this module.

use crate::model::Model;
use crate::types::Context;
use crate::utils::estimate::estimate_context_tokens;

pub const CONTEXT_SAFETY_TOKENS: u64 = 4096;
pub const MIN_ANSWER_TOKENS: u64 = 1024;
const MIN_MAX_TOKENS: u64 = 1;
pub const CONTEXT_GUARD_MIN_WINDOW: u64 = CONTEXT_SAFETY_TOKENS + MIN_ANSWER_TOKENS;

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error(
    "Context window exhausted: the conversation is estimated at {estimated_tokens} of {context_window} tokens, \
     leaving fewer than 1024 tokens for a response. \
     Compact the conversation, enable auto-compaction, or start a new session before retrying."
)]
pub struct ContextWindowExhaustedError {
    pub estimated_tokens: u64,
    pub context_window: u64,
}

pub fn clamp_max_tokens_to_context(
    model: &Model,
    context: &Context,
    max_tokens: u64,
) -> Result<u64, ContextWindowExhaustedError> {
    if model.context_window == 0 {
        return Ok(if max_tokens > 0 { MIN_MAX_TOKENS.max(max_tokens) } else { MIN_MAX_TOKENS });
    }
    let estimated_tokens = estimate_context_tokens(context).tokens;
    let available = model.context_window as i128 - estimated_tokens as i128 - CONTEXT_SAFETY_TOKENS as i128;    if model.context_window >= CONTEXT_GUARD_MIN_WINDOW && available < MIN_ANSWER_TOKENS as i128 {
        return Err(ContextWindowExhaustedError { estimated_tokens, context_window: model.context_window });
    }
    let safe_available = (MIN_MAX_TOKENS as i128).max(available);
    let requested = if max_tokens > 0 { max_tokens as i128 } else { safe_available };
    Ok(requested.min(safe_available) as u64)
}
