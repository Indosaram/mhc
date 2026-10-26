//! Setup-error streams shared by registries and the models collection (the error envelope
//! senpi's api/lazy.ts builds; the lazy loader itself is ported with the wire APIs).

use crate::types::{
    AssistantMessage, AssistantMessageEvent, ErrorReason, Model, StopReason, Usage,
};
use crate::utils::diagnostics::now_ms;
use crate::utils::event_stream::AssistantMessageEventStream;

pub fn setup_error_message(model: &Model, message: &str) -> AssistantMessage {
    AssistantMessage {
        error_cause: None,
        prompt_cache_breakpoint: None,
        prompt_cache_diagnostics: None,
        thinking_level: None,
        provider_diagnostic: None,
        content: Vec::new(),
        api: model.api.clone(),
        provider: model.provider.clone(),
        model: model.id.clone(),
        response_model: None,
        response_id: None,
        provider_thinking_level: None,
        diagnostics: None,
        usage: Usage::default(),
        stop_reason: StopReason::Error,
        stop_details: None,
        deferred: None,
        error_message: Some(message.to_owned()),
        abort_source: None,
        raw_stop_reason: None,
        end_turn: None,
        timestamp: now_ms(),
        failure_source: None,
        timing: None,
    }
}

pub fn setup_stream_error_message(
    model: &Model,
    error: &crate::utils::event_stream::StreamError,
) -> AssistantMessage {
    let mut message = setup_error_message(model, &error.message);
    for prefix in [
        "Cannot find module '",
        "Cannot find module \"",
        "ENOENT reading '",
        "ENOENT reading \"",
    ] {
        if let Some(rest) = error.message.strip_prefix(prefix) {
            let quote = prefix.as_bytes()[prefix.len() - 1] as char;
            if let Some((missing, _)) = rest.split_once(quote) {
                let path_like = missing.starts_with('.')
                    || missing.starts_with('/')
                    || missing.starts_with('\\')
                    || missing.starts_with("file:")
                    || missing.as_bytes().get(1) == Some(&b':');
                if path_like && (missing.ends_with(".js") || missing.ends_with(".mjs")) {
                    let filename = missing.rsplit(['/', '\\']).next().unwrap_or(missing);
                    message.error_message = Some(format!(
                        "{}The installed package changed while this session was running, so {filename} can no longer be loaded. Restart and resume this session to continue.",
                        crate::utils::provider_failure_description::TURN_RETRY_SUPPRESSION_PREFIX
                    ));
                    break;
                }
            }
        }
    }
    if error.kind == crate::utils::event_stream::StreamErrorKind::OAuthRefreshUnavailable {
        message.diagnostics = Some(vec![
            crate::utils::diagnostics::AssistantMessageDiagnostic {
                kind: crate::utils::retry::OAUTH_REFRESH_UNAVAILABLE_DIAGNOSTIC.to_owned(),
                timestamp: now_ms(),
                error: None,
                details: Some(serde_json::Map::from_iter([(
                    "provider".to_owned(),
                    serde_json::json!(model.provider),
                )])),
            },
        ]);
    }
    message
}

/// A stream that immediately terminates with an error event for `model`.
pub fn error_stream(model: &Model, message: &str) -> AssistantMessageEventStream {
    let stream = AssistantMessageEventStream::assistant();
    let error = setup_error_message(model, message);
    stream.push(AssistantMessageEvent::Error {
        reason: ErrorReason::Error,
        error: error.clone(),
    });
    stream.end(Some(error));
    stream
}
