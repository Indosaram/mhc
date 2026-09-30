//! Port of senpi packages/coding-agent/src/core/session-title-generator.ts.

use std::sync::Arc;

use maho_ai::compat::complete_simple;
use maho_ai::model::Model;
use maho_ai::types::{AssistantMessage, CacheRetention, ContentBlock, Context, SimpleStreamOptions, StopReason, TextContent};
use regex::Regex;
use serde_json::Value;

pub const TITLE_SYSTEM_PROMPT: &str = "Generate a concise title for this coding-agent session.\n\nRules:\n- Use 3 to 6 words.\n- Prefer concrete nouns and verbs from the user's task.\n- Do not include quotes, punctuation at the end, markdown, or explanations.\n- If the input is only a greeting, acknowledgement, or too vague to title, return <title>none</title>.\n- Respond only as <title>Session Title</title>.";

pub const LOW_SIGNAL_PROMPTS: [&str; 14] = [
    "hi", "hello", "hey", "yo", "thanks", "thank you", "ok", "okay", "k", "yes", "no", "yep", "nope", "hmm",
];

pub const MAX_TITLE_RETRIES: u32 = 1;
pub const MAX_TITLE_RETRY_DELAY_MS: u64 = 2_000;

/// The user's retry budget narrowed for cosmetic background title generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TitleRetryPolicy {
    pub enabled: bool,
    pub max_retries: u32,
    pub base_delay_ms: u64,
}

pub fn session_title_retry_policy(settings: TitleRetryPolicy) -> TitleRetryPolicy {
    TitleRetryPolicy {
        enabled: settings.enabled,
        max_retries: settings.max_retries.min(MAX_TITLE_RETRIES),
        base_delay_ms: settings.base_delay_ms.min(MAX_TITLE_RETRY_DELAY_MS),
    }
}

pub fn should_skip_session_title(text: &str) -> bool {
    let normalized = text.split_whitespace().collect::<Vec<_>>().join(" ").trim().to_lowercase();
    if normalized.is_empty() {
        return true;
    }
    if normalized.starts_with('/') {
        return true;
    }
    LOW_SIGNAL_PROMPTS.contains(&normalized.as_str())
}

#[derive(Debug, Clone, Default)]
pub struct SessionTitleAuth {
    pub api_key: Option<String>,
    pub headers: Option<std::collections::BTreeMap<String, Option<String>>>,
    pub extra_body: Option<serde_json::Map<String, Value>>,
    pub env: Option<std::collections::BTreeMap<String, String>>,
}

pub struct GenerateSessionTitleOptions {
    pub first_prompt: String,
    pub model: Model,
    pub auth: SessionTitleAuth,
    pub session_id: String,
    pub base_options: Option<SimpleStreamOptions>,
    pub signal: Option<maho_ai::utils::abort::AbortSignal>,
}

pub fn build_title_context(first_prompt: &str) -> Context {
    Context {
        system_prompt: Some(TITLE_SYSTEM_PROMPT.to_owned()),
        messages: vec![maho_ai::types::Message::User(maho_ai::types::UserMessage {
            content: maho_ai::types::UserContent::Blocks(vec![ContentBlock::Text(TextContent {
                text: first_prompt.to_owned(),
                audience: None,
                text_signature: None,
            })]),
            timestamp: chrono::Utc::now().timestamp_millis(),
        })],
        ..Context::default()
    }
}

pub fn build_title_options(options: &GenerateSessionTitleOptions) -> SimpleStreamOptions {
    let mut title_options = options.base_options.clone().unwrap_or_default();
    title_options.stream.session_id = Some(options.session_id.clone());
    title_options.stream.cache_retention = Some(if options.model.cache_retention == Some(CacheRetention::None) {
        CacheRetention::None
    } else {
        CacheRetention::Short
    });
    title_options.stream.max_tokens = Some(64);
    if let Some(api_key) = &options.auth.api_key {
        title_options.stream.request.api_key = Some(api_key.clone());
    }
    if let Some(headers) = &options.auth.headers {
        title_options.stream.request.headers = Some(headers.clone());
    }
    if let Some(extra_body) = &options.auth.extra_body {
        title_options.stream.extra_body = Some(extra_body.clone());
    }
    if let Some(env) = &options.auth.env {
        title_options.stream.request.env = Some(env.clone());
    }
    if let Some(signal) = &options.signal {
        title_options.stream.request.signal = Some(signal.clone());
    }
    title_options
}

pub async fn complete_title(model: &Model, context: &Context, options: SimpleStreamOptions) -> Result<AssistantMessage, String> {
    complete_simple(model, context, Some(options)).await.map_err(|error| error.to_string())
}

pub fn parse_session_title(message: &AssistantMessage) -> Option<String> {
    let text: String = message
        .content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text(text) => Some(text.text.clone()),
            _ => None,
        })
        .collect::<Vec<String>>()
        .join("");
    let text = text.trim();
    let regex = Regex::new(r"(?is)<title>\s*([\s\S]*?)\s*</title>").expect("title regex");
    let raw_title = regex.captures(text)?.get(1)?.as_str();
    let title = sanitize_title(raw_title);
    if title.is_empty() || title.to_lowercase() == "none" {
        None
    } else {
        Some(title)
    }
}

/// Reduce a raw provider error payload to a short human-readable line while keeping the error type
/// and request id for follow-up. Non-JSON and unrecognized payloads pass through unchanged.
pub fn humanize_provider_error(raw: &str) -> String {
    let trimmed = raw.trim();
    let status_prefix = Regex::new(r"^(?:.*?\((\d{3})\)|(\d{3}))\s*:\s*").expect("status regex");
    let capture = status_prefix.captures(trimmed);
    let status = capture.as_ref().and_then(|capture| capture.get(1).or_else(|| capture.get(2))).map(|matched| matched.as_str().to_owned());
    let body_text = match &capture {
        Some(capture) => trimmed[capture.get(0).map(|matched| matched.end()).unwrap_or(0)..].to_owned(),
        None => trimmed.to_owned(),
    };
    if !body_text.starts_with('{') {
        return raw.to_owned();
    }
    let Ok(parsed) = serde_json::from_str::<Value>(&body_text) else { return raw.to_owned() };
    let Some(body) = parsed.as_object() else { return raw.to_owned() };
    let nested = body.get("error").and_then(Value::as_object);
    let message = nested
        .and_then(|nested| non_empty_string(nested.get("message")))
        .or_else(|| non_empty_string(body.get("message")))
        .or_else(|| non_empty_string(body.get("error")));
    let Some(message) = message else { return raw.to_owned() };
    let mut details: Vec<String> = Vec::new();
    let error_type = nested.and_then(|nested| non_empty_string(nested.get("type"))).or_else(|| non_empty_string(body.get("type")));
    if let Some(error_type) = error_type.filter(|error_type| error_type != "error") {
        details.push(error_type);
    }
    if let Some(status) = status {
        details.push(format!("HTTP {status}"));
    }
    if let Some(request_id) = non_empty_string(body.get("request_id")) {
        details.push(format!("request {request_id}"));
    }
    if details.is_empty() { message } else { format!("{message} ({})", details.join(", ")) }
}

fn non_empty_string(value: Option<&Value>) -> Option<String> {
    let text = value?.as_str()?;
    if text.is_empty() { None } else { Some(text.to_owned()) }
}

pub fn sanitize_title(text: &str) -> String {
    let osc = Regex::new(r"\u{1b}\][^\u{7}]*(?:\u{7}|\u{1b}\\)").expect("osc");
    let csi = Regex::new(r"\u{1b}\[[0-?]*[ -/]*[@-~]").expect("csi");
    let two_char = Regex::new(r"\u{1b}[\u{20}-\u{2f}]*[\u{30}-\u{7e}]").expect("two char");
    let newlines = Regex::new(r"[\r\n]+").expect("newlines");
    let controls = Regex::new(r"[\u{0}-\u{1f}\u{7f}-\u{9f}]+").expect("controls");
    let spaces = Regex::new(r"\s+").expect("spaces");
    let edges = Regex::new("^[\"'`]+|[\"'`.!?]+$").expect("edges");
    let stripped = osc.replace_all(text, "");
    let stripped = csi.replace_all(&stripped, "");
    let stripped = two_char.replace_all(&stripped, "");
    let stripped = newlines.replace_all(&stripped, " ");
    let stripped = controls.replace_all(&stripped, " ");
    let stripped = spaces.replace_all(&stripped, " ");
    let stripped = edges.replace_all(&stripped, "");
    let trimmed = stripped.trim();
    let capped: String = trimmed.chars().take(80).collect();
    capped.trim().to_owned()
}

pub fn title_error_message(message: &AssistantMessage) -> Option<String> {
    if message.stop_reason == StopReason::Error {
        Some(humanize_provider_error(message.error_message.as_deref().unwrap_or("Session title generation failed")))
    } else {
        None
    }
}

pub type TitleStreamFn = Arc<dyn Fn(&Model, &Context, SimpleStreamOptions) -> Result<AssistantMessage, String> + Send + Sync>;

#[cfg(test)]
mod tests {
    use super::*;
    use maho_ai::types::Usage;

    fn test_model() -> Model {
        let mut model = maho_ai::models_generated::get_builtin_model("anthropic", "claude-sonnet-4-5")
            .expect("builtin anthropic model")
            .clone();
        model.cache_retention = Some(CacheRetention::Long);
        model
    }

    fn assistant_with_text(text: &str) -> AssistantMessage {
        AssistantMessage {
            content: vec![ContentBlock::Text(TextContent { text: text.to_owned(), audience: None, text_signature: None })],
            api: "anthropic".to_owned(),
            provider: "anthropic".to_owned(),
            model: "m".to_owned(),
            response_model: None,
            response_id: None,
            provider_thinking_level: None,
            diagnostics: None,
            usage: Usage::default(),
            stop_reason: StopReason::Stop,
            stop_details: None,
            deferred: None,
            error_message: None,
            abort_source: None,
            raw_stop_reason: None,
            end_turn: None,
            timestamp: 0,
        }
    }

    #[test]
    fn skips_greetings_slash_commands_and_empty_prompts() {
        assert!(should_skip_session_title(""));
        assert!(should_skip_session_title("   "));
        assert!(should_skip_session_title("Hi"));
        assert!(should_skip_session_title("  thank   you "));
        assert!(should_skip_session_title("/help"));
        assert!(!should_skip_session_title("port the session manager to Rust"));
    }

    #[test]
    fn narrows_the_retry_budget_without_inflating_it() {
        let narrowed = session_title_retry_policy(TitleRetryPolicy { enabled: true, max_retries: 5, base_delay_ms: 9_000 });
        assert_eq!(narrowed, TitleRetryPolicy { enabled: true, max_retries: 1, base_delay_ms: 2_000 });
        let already_small = session_title_retry_policy(TitleRetryPolicy { enabled: false, max_retries: 0, base_delay_ms: 10 });
        assert_eq!(already_small, TitleRetryPolicy { enabled: false, max_retries: 0, base_delay_ms: 10 });
    }

    #[test]
    fn parses_a_title_tag_and_rejects_none_or_missing_tags() {
        assert_eq!(parse_session_title(&assistant_with_text("<title>Port Session Manager</title>")).as_deref(), Some("Port Session Manager"));
        assert_eq!(parse_session_title(&assistant_with_text("<TITLE>  spaced  </TITLE>")).as_deref(), Some("spaced"));
        assert_eq!(parse_session_title(&assistant_with_text("<title>none</title>")), None);
        assert_eq!(parse_session_title(&assistant_with_text("<title>   </title>")), None);
        assert_eq!(parse_session_title(&assistant_with_text("no tags here")), None);
    }

    #[test]
    fn sanitizes_escapes_controls_and_edge_punctuation() {
        assert_eq!(sanitize_title("\u{1b}]0;title\u{7}Port Session"), "Port Session");
        assert_eq!(sanitize_title("\u{1b}[31mred\u{1b}[0m"), "red");
        assert_eq!(sanitize_title("line\nbreak"), "line break");
        assert_eq!(sanitize_title("\"quoted title\""), "quoted title");
        assert_eq!(sanitize_title("trailing..."), "trailing");
        assert_eq!(sanitize_title(&"a".repeat(200)).len(), 80);
    }

    #[test]
    fn humanizes_json_error_bodies_and_passes_others_through() {
        assert_eq!(humanize_provider_error("plain failure"), "plain failure");
        assert_eq!(
            humanize_provider_error("529: {\"type\":\"error\",\"error\":{\"type\":\"overloaded_error\",\"message\":\"Overloaded\"},\"request_id\":\"req_1\"}"),
            "Overloaded (overloaded_error, HTTP 529, request req_1)"
        );
        assert_eq!(
            humanize_provider_error("provider (429): {\"error\":{\"message\":\"slow down\"}}"),
            "slow down (HTTP 429)"
        );
        assert_eq!(humanize_provider_error("400: {\"error\":{\"type\":\"error\"}}"), "400: {\"error\":{\"type\":\"error\"}}");
        assert_eq!(humanize_provider_error("400: not json"), "400: not json");
        assert_eq!(humanize_provider_error("{\"error\":{\"message\":\"m\",\"type\":\"rate_limit_error\"}}"), "m (rate_limit_error)");
    }

    #[test]
    fn builds_the_title_context_and_options_from_the_auth_and_base_options() {
        let context = build_title_context("first prompt");
        assert_eq!(context.system_prompt.as_deref(), Some(TITLE_SYSTEM_PROMPT));
        assert_eq!(context.messages.len(), 1);

        let model = test_model();
        let options = GenerateSessionTitleOptions {
            first_prompt: "first".to_owned(),
            model,
            auth: SessionTitleAuth { api_key: Some("key".to_owned()), ..SessionTitleAuth::default() },
            session_id: "session-1".to_owned(),
            base_options: None,
            signal: None,
        };
        let title_options = build_title_options(&options);
        assert_eq!(title_options.stream.session_id.as_deref(), Some("session-1"));
        assert_eq!(title_options.stream.max_tokens, Some(64));
        assert_eq!(title_options.stream.request.api_key.as_deref(), Some("key"));
        assert_eq!(title_options.stream.cache_retention, Some(CacheRetention::Short));
    }

    #[test]
    fn an_error_stop_reason_produces_the_humanized_message() {
        let mut message = assistant_with_text("");
        message.stop_reason = StopReason::Error;
        message.error_message = Some("{\"error\":{\"message\":\"bad key\",\"type\":\"authentication_error\"}}".to_owned());
        assert_eq!(title_error_message(&message).as_deref(), Some("bad key (authentication_error)"));
        assert!(title_error_message(&assistant_with_text("<title>t</title>")).is_none());
    }
}