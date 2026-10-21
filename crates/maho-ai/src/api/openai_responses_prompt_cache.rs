//! Port of senpi packages/ai/src/api/openai-responses-prompt-cache.ts.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::types::{Message, Model, StopReason};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpenAiPromptCacheOptionsPayload {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ttl: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prewarm: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub comparison_response_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PromptCacheDiagnostics {
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_missed_tokens: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub comparison_reusable_tokens: Option<f64>,
}

pub fn is_openai_responses_prompt_cache_model(model: &Model) -> bool {
    model.api == "openai-responses"
        && model.compat.as_ref().is_some_and(|compat| {
            compat
                .openai_responses()
                .supports_explicit_prompt_cache_mode
                == Some(true)
        })
        && reqwest::Url::parse(if model.base_url.is_empty() {
            "https://api.openai.com/v1"
        } else {
            &model.base_url
        })
        .is_ok_and(|url| url.host_str() == Some("api.openai.com"))
}

pub fn find_prompt_cache_comparison_response_id<'a>(
    model: &Model,
    messages: &'a [Message],
) -> Option<&'a str> {
    for message in messages.iter().rev() {
        let Message::Assistant(message) = message else {
            continue;
        };
        if message.api != model.api
            || message.provider != model.provider
            || message.model != model.id
            || matches!(message.stop_reason, StopReason::Error | StopReason::Aborted)
        {
            continue;
        }
        if let Some(id) = message.response_id.as_deref().filter(|id| !id.is_empty()) {
            return id.starts_with("resp").then_some(id);
        }
    }
    None
}

pub fn with_prompt_cache_comparison(
    options: Option<OpenAiPromptCacheOptionsPayload>,
    comparison_response_id: Option<&str>,
) -> Option<OpenAiPromptCacheOptionsPayload> {
    match comparison_response_id {
        None => options,
        Some(id) => Some(OpenAiPromptCacheOptionsPayload {
            comparison_response_id: Some(id.to_owned()),
            ..options.unwrap_or_default()
        }),
    }
}

pub fn parse_prompt_cache_diagnostics(raw: &Value) -> Option<PromptCacheDiagnostics> {
    let record = raw.as_object()?;
    let kind = record
        .get("type")?
        .as_str()
        .filter(|kind| !kind.is_empty())?;
    let finite_count = |key| {
        record
            .get(key)
            .and_then(Value::as_f64)
            .filter(|value| value.is_finite() && *value >= 0.0)
    };
    Some(PromptCacheDiagnostics {
        kind: kind.to_owned(),
        reason: record
            .get("reason")
            .and_then(Value::as_str)
            .map(str::to_owned),
        cache_missed_tokens: finite_count("cache_missed_tokens"),
        comparison_reusable_tokens: finite_count("comparison_reusable_tokens"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn comparison_stops_at_the_latest_successful_nonempty_id() {
        let model: Model = serde_json::from_value(json!({
            "id":"test", "name":"test", "api":"openai-responses", "provider":"openai",
            "baseUrl":"", "reasoning":false,"input":["text"],
            "cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0},
            "contextWindow":1000,"maxTokens":100
        }))
        .expect("model");
        for (id, stop, expected) in [
            ("resp_new", "stop", Some("resp_new")),
            ("foreign", "stop", None),
            ("resp_failed", "error", Some("resp_old")),
            ("resp_aborted", "aborted", Some("resp_old")),
            ("", "stop", Some("resp_old")),
        ] {
            let messages: Vec<Message> = [("resp_old", "stop"), (id, stop)].into_iter()
                .map(|(id, stop)| serde_json::from_value(json!({
                    "role":"assistant", "content":[], "api":model.api,"provider":model.provider,
                    "model":model.id,"responseId":id,"stopReason":stop,"usage":crate::types::Usage::default(),"timestamp":0
                })).expect("message")).collect();
            assert_eq!(
                find_prompt_cache_comparison_response_id(&model, &messages),
                expected
            );
        }
    }

    #[test]
    fn comparison_preserves_options_and_can_create_comparison_only_payload() {
        let options = OpenAiPromptCacheOptionsPayload {
            mode: Some("explicit".into()),
            ..Default::default()
        };
        let updated =
            with_prompt_cache_comparison(Some(options.clone()), Some("resp_1")).expect("options");
        assert_eq!(updated.mode, options.mode);
        assert_eq!(updated.comparison_response_id.as_deref(), Some("resp_1"));
        assert_eq!(
            with_prompt_cache_comparison(None, Some("resp_2"))
                .expect("options")
                .comparison_response_id
                .as_deref(),
            Some("resp_2")
        );
        assert_eq!(
            with_prompt_cache_comparison(Some(options.clone()), None),
            Some(options)
        );
    }

    #[test]
    fn diagnostics_drop_invalid_counts_but_keep_valid_fractional_counts() {
        // Given provider diagnostics with one invalid count.
        let raw = json!({"type":"miss", "reason":"changed", "cache_missed_tokens":-1,
            "comparison_reusable_tokens":1.5});
        // When parsing at the wire boundary.
        let parsed = parse_prompt_cache_diagnostics(&raw).expect("diagnostics");
        // Then only finite nonnegative counts survive.
        assert_eq!(parsed.cache_missed_tokens, None);
        assert_eq!(parsed.comparison_reusable_tokens, Some(1.5));
        assert_eq!(
            serde_json::to_value(parsed).expect("serialize"),
            json!({"type":"miss","reason":"changed","comparisonReusableTokens":1.5})
        );
    }

    #[test]
    fn diagnostics_require_a_nonempty_type() {
        // Given malformed diagnostics; when parsed; then no diagnostics are produced.
        for raw in [
            Value::Null,
            json!([]),
            json!({}),
            json!({"type":""}),
            json!({"type":1}),
        ] {
            assert_eq!(parse_prompt_cache_diagnostics(&raw), None);
        }
    }
}
