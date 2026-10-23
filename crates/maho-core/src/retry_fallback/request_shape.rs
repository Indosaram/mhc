//! Port of senpi packages/coding-agent/src/core/retry-fallback/request-shape.ts.

use maho_agent::types::AgentMessage;
use maho_ai::types::{AssistantMessage, ContentBlock, Message, StopReason};
use regex::Regex;
use std::sync::OnceLock;

use super::billing::is_billing_error_message;

pub const REQUEST_SHAPE_RECOVERY_ENTRY_TYPE: &str = "request-shape-recovery";

pub const REQUEST_SHAPE_RECOVERY_NOTE: &str = "Retried without web search replay data";

pub const REQUEST_SHAPE_FAILURE_TEXT: &str = "Anthropic rejected this conversation's request format, even after retrying without web search replay data. Run /compact or start a new session to continue, and report it if it happens again.";

/// Gateways speak the same wire but answer their own 400s; only Anthropic's own rejection is a replay-shape problem.
const ANTHROPIC_PROVIDER: &str = "anthropic";

fn invalid_request_matches(message: &str) -> bool {
    static PATTERN: OnceLock<Option<Regex>> = OnceLock::new();
    PATTERN
        .get_or_init(|| Regex::new(r"\binvalid_request_error\b").ok())
        .as_ref()
        .is_some_and(|pattern| pattern.is_match(message))
}

/// A request-shape 400 is ours to fix, not the model's: it never walks the fallback chain (senpi#3727).
pub fn is_anthropic_request_shape_rejection(
    message: &AssistantMessage,
    context_window: u64,
) -> bool {
    message.stop_reason == StopReason::Error
        && message.api == "anthropic-messages"
        && message.provider == ANTHROPIC_PROVIDER
        && invalid_request_matches(message.error_message.as_deref().unwrap_or_default())
        && !is_billing_error_message(message.error_message.as_deref())
        && !maho_ai::utils::overflow::is_context_overflow(message, Some(context_window))
}

#[derive(Debug, Clone, PartialEq)]
pub struct ProviderNativeStripResult {
    pub messages: Vec<AgentMessage>,
    pub stripped_blocks: usize,
}

/// Only assistant `providerNative` blocks at or before `cutoff` go; user text and client tool pairs never do.
pub fn strip_provider_native_replay(
    messages: &[AgentMessage],
    cutoff: i64,
) -> ProviderNativeStripResult {
    let mut stripped_blocks = 0usize;
    let stripped = messages
        .iter()
        .map(|message| {
            let AgentMessage::Llm(Message::Assistant(assistant)) = message else {
                return message.clone();
            };
            if assistant.timestamp > cutoff {
                return message.clone();
            }
            let content: Vec<ContentBlock> = assistant
                .content
                .iter()
                .filter(|block| !matches!(block, ContentBlock::ProviderNative(_)))
                .cloned()
                .collect();
            if content.len() == assistant.content.len() {
                return message.clone();
            }
            stripped_blocks += assistant.content.len() - content.len();
            let mut next = assistant.clone();
            next.content = content;
            AgentMessage::Llm(Message::Assistant(next))
        })
        .collect();
    ProviderNativeStripResult {
        messages: stripped,
        stripped_blocks,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestShapeDecision {
    Recover,
    GiveUp,
    NotRequestShape,
}

/// One same-model recovery per failing streak; older history stays stripped because the bad block is still in it.
#[derive(Debug, Clone, Default)]
pub struct RequestShapeRecovery {
    cutoff: Option<i64>,
    attempted: bool,
}

impl RequestShapeRecovery {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn decide(&self, message: &AssistantMessage, context_window: u64) -> RequestShapeDecision {
        if !is_anthropic_request_shape_rejection(message, context_window) {
            return RequestShapeDecision::NotRequestShape;
        }
        if self.attempted {
            RequestShapeDecision::GiveUp
        } else {
            RequestShapeDecision::Recover
        }
    }

    pub fn begin(&mut self, failed_at: i64) {
        self.attempted = true;
        self.cutoff = Some(failed_at);
    }

    /// Any non-request-shape outcome (success, another error, a new prompt) ends the streak.
    pub fn end_streak(&mut self) {
        self.attempted = false;
    }

    pub fn apply(&self, messages: Vec<AgentMessage>) -> Vec<AgentMessage> {
        match self.cutoff {
            None => messages,
            Some(cutoff) => strip_provider_native_replay(&messages, cutoff).messages,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn failure(error: &str) -> AssistantMessage {
        let mut message = maho_ai::providers::faux::faux_assistant_message("", Default::default());
        message.stop_reason = StopReason::Error;
        message.api = "anthropic-messages".into();
        message.provider = "anthropic".into();
        message.error_message = Some(error.into());
        message
    }

    #[test]
    fn only_anthropics_own_invalid_request_error_is_a_request_shape_rejection() {
        let rejected = failure(r#"400 {"type":"invalid_request_error","message":"bad block"}"#);
        assert!(is_anthropic_request_shape_rejection(&rejected, 200_000));
        let mut gateway = rejected.clone();
        gateway.provider = "openrouter".into();
        assert!(!is_anthropic_request_shape_rejection(&gateway, 200_000));
        assert!(!is_anthropic_request_shape_rejection(
            &failure("overloaded"),
            200_000
        ));
        assert!(!is_anthropic_request_shape_rejection(
            &failure("invalid_request_error: credit balance is too low"),
            200_000
        ));
    }

    #[test]
    fn recovery_is_offered_once_per_streak() {
        let rejected = failure("invalid_request_error");
        let mut recovery = RequestShapeRecovery::new();
        assert_eq!(
            recovery.decide(&rejected, 200_000),
            RequestShapeDecision::Recover
        );
        recovery.begin(10);
        assert_eq!(
            recovery.decide(&rejected, 200_000),
            RequestShapeDecision::GiveUp
        );
        assert_eq!(
            recovery.decide(&failure("overloaded"), 200_000),
            RequestShapeDecision::NotRequestShape
        );
        recovery.end_streak();
        assert_eq!(
            recovery.decide(&rejected, 200_000),
            RequestShapeDecision::Recover
        );
    }

    #[test]
    fn stripping_leaves_messages_after_the_cutoff_alone() {
        let mut message = maho_ai::providers::faux::faux_assistant_message("", Default::default());
        message.timestamp = 20;
        let messages = vec![AgentMessage::Llm(Message::Assistant(Box::new(message)))];
        let result = strip_provider_native_replay(&messages, 10);
        assert_eq!(result.stripped_blocks, 0);
        assert_eq!(result.messages, messages);
        let mut recovery = RequestShapeRecovery::new();
        assert_eq!(recovery.apply(messages.clone()), messages);
        recovery.begin(10);
        assert_eq!(recovery.apply(messages.clone()), messages);
    }
}
