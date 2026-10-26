//! Port of senpi packages/ai/src/tool-call-middleware/stream-message-metadata.ts.

use crate::types::{AssistantMessage, ContentBlock, Usage};
use crate::utils::diagnostics::AssistantMessageDiagnostic;

fn clone_usage(usage: &Usage) -> Usage {
    *usage
}

/// `preserveAll = false` clones only the legacy metadata fields the TS `cloneLegacyUsage`/
/// `cloneLegacyMessage` pair kept (usage minus `cacheWrite1h`/`reasoning`, no diagnostics carried
/// forward).
fn clone_legacy_message(source: &AssistantMessage, content: Vec<ContentBlock>) -> AssistantMessage {
    AssistantMessage {
        error_cause: None,
        prompt_cache_breakpoint: None,
        prompt_cache_diagnostics: None,
        thinking_level: None,
        provider_diagnostic: None,
        content,
        api: source.api.clone(),
        provider: source.provider.clone(),
        model: source.model.clone(),
        response_model: None,
        response_id: source.response_id.clone(),
        provider_thinking_level: None,
        diagnostics: None,
        usage: Usage {
            context_usage: source.usage.context_usage,
            cost_unknown: source.usage.cost_unknown,
            input: source.usage.input,
            output: source.usage.output,
            cache_read: source.usage.cache_read,
            cache_write: source.usage.cache_write,
            cache_write_1h: None,
            reasoning: None,
            total_tokens: source.usage.total_tokens,
            cost: source.usage.cost,
        },
        stop_reason: source.stop_reason,
        stop_details: None,
        deferred: None,
        error_message: source.error_message.clone(),
        abort_source: None,
        raw_stop_reason: None,
        end_turn: None,
        timestamp: source.timestamp,
        failure_source: None,
        timing: None,
    }
}

/// Field-by-field copy of every metadata field, so the source `content` is never deep-cloned only
/// to be replaced.
pub fn clone_message_with_content(
    source: &AssistantMessage,
    content: Vec<ContentBlock>,
) -> AssistantMessage {
    AssistantMessage {
        error_cause: source.error_cause,
        prompt_cache_breakpoint: source.prompt_cache_breakpoint,
        prompt_cache_diagnostics: source.prompt_cache_diagnostics.clone(),
        thinking_level: source.thinking_level,
        provider_diagnostic: source.provider_diagnostic.clone(),
        content,
        api: source.api.clone(),
        provider: source.provider.clone(),
        model: source.model.clone(),
        response_model: source.response_model.clone(),
        response_id: source.response_id.clone(),
        provider_thinking_level: source.provider_thinking_level.clone(),
        diagnostics: source.diagnostics.clone(),
        usage: clone_usage(&source.usage),
        stop_reason: source.stop_reason,
        stop_details: source.stop_details.clone(),
        deferred: source.deferred.clone(),
        error_message: source.error_message.clone(),
        abort_source: source.abort_source,
        raw_stop_reason: source.raw_stop_reason.clone(),
        end_turn: source.end_turn,
        timestamp: source.timestamp,
        failure_source: source.failure_source,
        timing: source.timing.clone(),
    }
}

pub fn clone_assistant_message_metadata(
    source: &AssistantMessage,
    content: Vec<ContentBlock>,
    projected_diagnostics: &[AssistantMessageDiagnostic],
    preserve_all: bool,
) -> AssistantMessage {
    if !preserve_all {
        return clone_legacy_message(source, content);
    }
    let mut message = clone_message_with_content(source, content);
    let mut diagnostics = message.diagnostics.take().unwrap_or_default();
    diagnostics.extend(projected_diagnostics.iter().cloned());
    message.diagnostics = if diagnostics.is_empty() {
        None
    } else {
        Some(diagnostics)
    };
    message
}

pub fn sync_assistant_message_metadata(
    target: &mut AssistantMessage,
    source: &AssistantMessage,
    projected_diagnostics: &[AssistantMessageDiagnostic],
    preserve_all: bool,
) {
    if !preserve_all {
        let legacy = clone_legacy_message(source, std::mem::take(&mut target.content));
        *target = legacy;
        return;
    }
    let content = std::mem::take(&mut target.content);
    *target = clone_assistant_message_metadata(source, content, projected_diagnostics, true);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{StopReason, UsageCost};
    use crate::utils::diagnostics::{
        DiagnosticErrorInfo, Thrown, create_assistant_message_diagnostic,
    };

    fn usage(reasoning: Option<u64>) -> Usage {
        Usage {
            context_usage: None,
            cost_unknown: None,
            input: 1,
            output: 2,
            cache_read: 3,
            cache_write: 4,
            cache_write_1h: Some(5),
            reasoning,
            total_tokens: 6,
            cost: UsageCost {
                input: 0.1,
                output: 0.2,
                cache_read: 0.3,
                cache_write: 0.4,
                total: 1.0,
            },
        }
    }

    fn message() -> AssistantMessage {
        AssistantMessage {
            error_cause: None,
            prompt_cache_diagnostics: None,
            thinking_level: None,
            provider_diagnostic: None,
            content: vec![ContentBlock::text("hi")],
            api: "openai-completions".into(),
            provider: "openai".into(),
            model: "m".into(),
            response_model: Some("rm".into()),
            response_id: Some("rid".into()),
            provider_thinking_level: Some("high".into()),
            diagnostics: None,
            usage: usage(Some(9)),
            stop_reason: StopReason::Stop,
            stop_details: None,
            deferred: None,
            error_message: None,
            abort_source: None,
            raw_stop_reason: Some("stop".into()),
            end_turn: Some(true),
            prompt_cache_breakpoint: Some(true),
            timestamp: 42,
            failure_source: None,
            timing: None,
        }
    }

    #[test]
    fn legacy_clone_drops_cache_write_1h_and_reasoning_and_extra_metadata() {
        let mut source = message();
        source.usage.cost_unknown = Some(true);
        let cloned = clone_assistant_message_metadata(&source, vec![], &[], false);
        assert_eq!(cloned.usage.cache_write_1h, None);
        assert_eq!(cloned.usage.reasoning, None);
        assert_eq!(cloned.usage.cost_unknown, Some(true));
        assert_eq!(cloned.response_model, None);
        assert_eq!(cloned.provider_thinking_level, None);
        assert_eq!(cloned.diagnostics, None);
        assert_eq!(cloned.prompt_cache_breakpoint, None);
        assert_eq!(cloned.response_id, source.response_id);
        assert_eq!(cloned.stop_reason, source.stop_reason);
    }

    #[test]
    fn preserve_all_clone_carries_full_usage_and_appends_diagnostics() {
        let mut source = message();
        source.failure_source = Some(crate::types::AssistantFailureSource::Local);
        source.timing = Some(crate::types::AssistantMessageTiming {
            requested_at: 1,
            first_token_at: Some(2),
            last_token_at: Some(3),
            ended_at: 4,
            generation_ms: Some(1),
        });
        let diagnostic =
            create_assistant_message_diagnostic("retry", &Thrown::error("E", "boom"), None);
        let cloned = clone_assistant_message_metadata(
            &source,
            vec![],
            std::slice::from_ref(&diagnostic),
            true,
        );
        assert_eq!(cloned.usage.cache_write_1h, Some(5));
        assert_eq!(cloned.usage.reasoning, Some(9));
        assert_eq!(cloned.response_model, source.response_model);
        assert_eq!(cloned.failure_source, source.failure_source);
        assert_eq!(cloned.timing, source.timing);
        assert_eq!(cloned.diagnostics.as_ref().map(Vec::len), Some(1));
        let _: Option<DiagnosticErrorInfo> = cloned.diagnostics.unwrap()[0].error.clone();
    }

    #[test]
    fn preserve_all_clears_diagnostics_when_none_accumulated() {
        let source = message();
        let cloned = clone_assistant_message_metadata(&source, vec![], &[], true);
        assert_eq!(cloned.diagnostics, None);
    }

    #[test]
    fn sync_legacy_replaces_metadata_but_keeps_content() {
        let mut target = message();
        target.content = vec![ContentBlock::text("outer")];
        let source = AssistantMessage {
            model: "other".into(),
            timestamp: 99,
            ..message()
        };
        sync_assistant_message_metadata(&mut target, &source, &[], false);
        assert_eq!(target.model, "other");
        assert_eq!(target.timestamp, 99);
        assert_eq!(target.content, vec![ContentBlock::text("outer")]);
    }

    #[test]
    fn sync_preserve_all_keeps_content_and_appends_diagnostics() {
        let mut target = message();
        target.content = vec![ContentBlock::text("outer")];
        let diagnostic =
            create_assistant_message_diagnostic("retry", &Thrown::error("E", "boom"), None);
        sync_assistant_message_metadata(
            &mut target,
            &message(),
            std::slice::from_ref(&diagnostic),
            true,
        );
        assert_eq!(target.content, vec![ContentBlock::text("outer")]);
        assert_eq!(target.diagnostics.as_ref().map(Vec::len), Some(1));
    }
}
