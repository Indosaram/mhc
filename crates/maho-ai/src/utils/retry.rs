//! Port of senpi packages/ai/src/utils/retry.ts.

use crate::types::{AbortSource, AssistantMessage, AssistantStopDetails, StopReason};
use crate::utils::abort::AbortSignal;
use crate::utils::empty_response_errors::{FORWARDED_EMPTY_RESPONSE_ERROR, FORWARDED_EMPTY_TOOL_USE_ERROR};
use crate::utils::sleep::sleep;
use regex::{Regex, RegexBuilder};
use std::future::Future;
use std::sync::{Arc, LazyLock};

pub struct UsageLimitExhaustion {
    pub codes: &'static [&'static str],
    pub markers: &'static [&'static str],
}

pub const USAGE_LIMIT_EXHAUSTION: UsageLimitExhaustion = UsageLimitExhaustion {
    codes: &["usage_limit_reached", "usage_not_included"],
    markers: &["usage_limit_reached", "usage_not_included", "usage limit has been reached"],
};

fn provider_error_pattern(patterns: &[String]) -> Regex {
    RegexBuilder::new(&patterns.join("|")).case_insensitive(true).build().unwrap_or_else(|e| panic!("retry pattern: {e}"))
}

fn owned(patterns: &[&str]) -> Vec<String> {
    patterns.iter().map(|p| (*p).to_owned()).collect()
}

static NON_RETRYABLE_PROVIDER_ERROR_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    let mut patterns = owned(&[
        "GoUsageLimitError",
        "FreeUsageLimitError",
        "Monthly usage limit reached",
        "available balance",
        "insufficient_quota",
        "out of budget",
        "quota exceeded",
        "billing",
        "credits_required",
        "credits are required",
    ]);
    patterns.extend(owned(USAGE_LIMIT_EXHAUSTION.markers));
    patterns.extend(owned(&[
        r"invalid request: tools\.",
        r"invalid request: functions\.",
        r"tools\.[^ ]*function\.parameters",
        r"tools\.\d+\.function\.parameters",
        "invalid tool schema",
    ]));
    provider_error_pattern(&patterns)
});

static RETRYABLE_PROVIDER_ERROR_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    let mut patterns = owned(&[
        "Credential store is busy: lock",
        "overloaded",
        "rate.?limit",
        "too many requests",
        "429",
        "500",
        "502",
        "503",
        "504",
        "522",
        "524",
        "service.?unavailable",
        "server.?error",
        "internal.?error",
        "provider.?returned.?error",
        "exceeded request buffer limit while retrying upstream",
        "network.?error",
        "connection.?error",
        "connection.?refused",
        "connection.?lost",
        "other side closed",
        "fetch failed",
        "getaddrinfo",
        "ENOTFOUND",
        "EAI_AGAIN",
        "upstream.?connect",
        "upstream.?unavailable",
        "reset before headers",
        "socket hang up",
        "socket connection was closed",
        "timed? out",
        "timeout",
        "terminated",
        "websocket.?closed",
        "websocket.?error",
        "ended without",
        "stream ended before message_stop",
        "stream ended before a terminal response event",
        "http2 request did not get a response",
        "retry delay",
        "you can retry your request",
        "try your request again",
        "please retry your request",
        r"the model request was rejected\.\s*check the request and try again\.?",
        "was found without a corresponding `",
    ]);
    patterns.push(regex::escape(FORWARDED_EMPTY_RESPONSE_ERROR));
    patterns.push(regex::escape(FORWARDED_EMPTY_TOOL_USE_ERROR));
    patterns.extend(owned(&["ResourceExhausted", "Lock file is already being held"]));
    provider_error_pattern(&patterns)
});

pub type RandomSource = Arc<dyn Fn() -> f64 + Send + Sync>;

#[derive(Clone)]
pub struct RetryPolicy {
    pub enabled: bool,
    pub max_retries: u32,
    pub base_delay_ms: u64,
    pub max_agent_delay_ms: Option<u64>,
    pub random: Option<RandomSource>,
}

pub const DEFAULT_MAX_AGENT_RETRY_DELAY_MS: u64 = 60_000;
const MAX_SAFE_INTEGER: f64 = 9_007_199_254_740_991.0;

pub(crate) fn math_random() -> f64 {
    let bytes = *uuid::Uuid::new_v4().as_bytes();
    let bits = u64::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7]]);
    (bits >> 11) as f64 / (1u64 << 53) as f64
}

/// Exponential delay with +/-10% jitter, capped at the agent ceiling.
pub fn retry_delay_ms(policy: &RetryPolicy, attempt: u32) -> u64 {
    let scheduled = policy.base_delay_ms as f64 * 2f64.powi(attempt.saturating_sub(1) as i32);
    let sample = policy.random.as_ref().map_or_else(math_random, |random| random()).clamp(0.0, 1.0);
    let jittered = (scheduled * (0.9 + sample * 0.2)).round();
    let safe = if jittered <= MAX_SAFE_INTEGER { jittered } else { MAX_SAFE_INTEGER };
    (safe as u64).min(policy.max_agent_delay_ms.unwrap_or(DEFAULT_MAX_AGENT_RETRY_DELAY_MS))
}

#[derive(Default)]
pub struct RetryCallbacks<'a> {
    pub on_retry_scheduled: Option<OnRetryScheduled<'a>>,
    pub on_retry_attempt_start: Option<Box<dyn Fn() + Send + Sync + 'a>>,
    pub on_retry_finished: Option<OnRetryFinished<'a>>,
}

/// Signature of [`RetryCallbacks::on_retry_scheduled`]: attempt, max attempts, delay ms, error message.
type OnRetryScheduled<'a> = Box<dyn Fn(u32, u32, u64, &str) + Send + Sync + 'a>;
/// Signature of [`RetryCallbacks::on_retry_finished`]: success, attempt, final error message.
type OnRetryFinished<'a> = Box<dyn Fn(bool, u32, Option<&str>) + Send + Sync + 'a>;

impl RetryCallbacks<'_> {
    fn scheduled(&self, attempt: u32, max: u32, delay: u64, error: &str) {
        if let Some(callback) = &self.on_retry_scheduled {
            callback(attempt, max, delay, error);
        }
    }

    fn attempt_start(&self) {
        if let Some(callback) = &self.on_retry_attempt_start {
            callback();
        }
    }

    fn finished(&self, success: bool, attempt: u32, error: Option<&str>) {
        if let Some(callback) = &self.on_retry_finished {
            callback(success, attempt, error);
        }
    }
}

fn error_message_of(message: &str) -> String {
    if message.is_empty() { "Unknown error".to_owned() } else { message.to_owned() }
}

async fn policy_sleep(delay_ms: u64, signal: Option<&AbortSignal>) -> bool {
    match signal {
        Some(signal) => sleep(delay_ms, signal).await.is_ok(),
        None => {
            tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
            true
        }
    }
}

/// Retries `produce` while `is_retryable` accepts the error; an abort during the backoff sleep
/// surfaces the last call error.
pub async fn retry_transient_call<T, E, F, Fut>(
    mut produce: F,
    is_retryable: impl Fn(&E) -> bool,
    policy: Option<&RetryPolicy>,
    signal: Option<&AbortSignal>,
    callbacks: Option<&RetryCallbacks<'_>>,
) -> Result<T, E>
where
    E: std::fmt::Display,
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T, E>>,
{
    let default_callbacks = RetryCallbacks::default();
    let callbacks = callbacks.unwrap_or(&default_callbacks);
    let max_attempts = policy.filter(|p| p.enabled).map_or(0, |p| p.max_retries);
    let mut attempt = 0;
    let mut last_retry: Option<u32> = None;
    loop {
        match produce().await {
            Ok(value) => {
                if let Some(last) = last_retry {
                    callbacks.finished(true, last, None);
                }
                return Ok(value);
            }
            Err(error) => {
                let Some(policy) = policy.filter(|_| attempt < max_attempts && is_retryable(&error)) else {
                    if let Some(last) = last_retry {
                        callbacks.finished(false, last, Some(&error_message_of(&error.to_string())));
                    }
                    return Err(error);
                };
                attempt += 1;
                last_retry = Some(attempt);
                let message = error_message_of(&error.to_string());
                let delay = retry_delay_ms(policy, attempt);
                callbacks.scheduled(attempt, max_attempts, delay, &message);
                if !policy_sleep(delay, signal).await {
                    callbacks.finished(false, attempt, Some(&message));
                    return Err(error);
                }
                callbacks.attempt_start();
            }
        }
    }
}

/// Retries error-terminated assistant responses; an abort during backoff returns the response
/// as `aborted` without its error message.
pub async fn retry_assistant_call<F, Fut>(
    mut produce: F,
    policy: Option<&RetryPolicy>,
    signal: Option<&AbortSignal>,
    callbacks: Option<&RetryCallbacks<'_>>,
) -> AssistantMessage
where
    F: FnMut() -> Fut,
    Fut: Future<Output = AssistantMessage>,
{
    let default_callbacks = RetryCallbacks::default();
    let callbacks = callbacks.unwrap_or(&default_callbacks);
    let max_attempts = policy.filter(|p| p.enabled).map_or(0, |p| p.max_retries);
    let mut attempt = 0;
    let mut last_retry: Option<u32> = None;
    loop {
        let mut response = produce().await;
        if response.stop_reason == StopReason::Aborted {
            if let Some(last) = last_retry {
                callbacks.finished(false, last, None);
            }
            return response;
        }
        if response.stop_reason != StopReason::Error {
            if let Some(last) = last_retry {
                callbacks.finished(true, last, None);
            }
            return response;
        }
        let Some(policy) = policy.filter(|_| attempt < max_attempts && is_retryable_assistant_error(&response)) else {
            if let Some(last) = last_retry {
                callbacks.finished(false, last, response.error_message.as_deref());
            }
            return response;
        };
        attempt += 1;
        last_retry = Some(attempt);
        let message = error_message_of(response.error_message.as_deref().unwrap_or_default());
        let delay = retry_delay_ms(policy, attempt);
        callbacks.scheduled(attempt, max_attempts, delay, &message);
        if !policy_sleep(delay, signal).await {
            callbacks.finished(false, attempt, Some(&message));
            response.error_message = None;
            response.stop_reason = StopReason::Aborted;
            return response;
        }
        callbacks.attempt_start();
    }
}

pub fn is_retryable_assistant_error(message: &AssistantMessage) -> bool {
    if message.stop_reason != StopReason::Error
        || matches!(message.stop_details, Some(AssistantStopDetails::Refusal { .. } | AssistantStopDetails::Sensitive))
    {
        return false;
    }
    match message.error_message.as_deref() {
        Some(error) if !error.is_empty() => is_retryable_error_message(error),
        _ => false,
    }
}

static PROVIDER_STREAM_STALL_ERROR_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    RegexBuilder::new(r"^(?:Idle timeout waiting for provider stream after \d+ms|Provider stream start timed out after \d+ms(?: \([^)]*\))?|WebSocket liveness timeout after \d+ms \(\d+ pings unanswered\)|Provider stream stalled after the last output item: response\.completed timed out after \d+ms)$")
        .case_insensitive(true)
        .build()
        .unwrap_or_else(|e| panic!("stall pattern: {e}"))
});

static PROVIDER_TRANSPORT_TIMEOUT_ERROR_PATTERN: LazyLock<Regex> =
    LazyLock::new(|| RegexBuilder::new(r"^Request timed out\.?$").case_insensitive(true).build().unwrap_or_else(|e| panic!("{e}")));

pub fn is_provider_stream_stall_error(message: &AssistantMessage) -> bool {
    message.stop_reason == StopReason::Error
        && PROVIDER_STREAM_STALL_ERROR_PATTERN.is_match(message.error_message.as_deref().unwrap_or_default())
}

struct StallPhase {
    pattern: Regex,
    symptom: &'static str,
    setting: Option<&'static str>,
}

static PROVIDER_STALL_PHASES: LazyLock<Vec<StallPhase>> = LazyLock::new(|| {
    let phase = |pattern: &str, symptom, setting| StallPhase {
        pattern: RegexBuilder::new(pattern).case_insensitive(true).build().unwrap_or_else(|e| panic!("{e}")),
        symptom,
        setting,
    };
    vec![
        phase(
            r"^Provider stream start timed out after (\d+)ms",
            "accepted the request but never started sending a response",
            Some("retry.provider.streamStartTimeoutMs"),
        ),
        phase(r"^Idle timeout waiting for provider stream after (\d+)ms", "started the response and then went silent", Some("retry.provider.timeoutMs")),
        phase(r"^WebSocket liveness timeout after (\d+)ms", "stopped answering connection health checks", None),
        phase(
            r"^Provider stream stalled after the last output item: response\.completed timed out after (\d+)ms",
            "finished its output but never sent the end-of-response event",
            None,
        ),
    ]
});

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StallRecovery {
    NoFallbackConfigured,
    ChainExhausted,
}

#[derive(Debug, Clone, Default)]
pub struct ProviderStallDescriptionOptions {
    pub attempts: Option<u32>,
    pub model: Option<String>,
    pub recovery: Option<StallRecovery>,
}

/// JS number-to-string for the rounded one-decimal values produced here.
fn js_decimal(value: f64) -> String {
    if value.fract() == 0.0 { format!("{}", value as i64) } else { format!("{value}") }
}

fn js_round(value: f64) -> f64 {
    (value + 0.5).floor()
}

pub fn format_stall_duration(timeout_ms: u64) -> String {
    let ms = timeout_ms as f64;
    if timeout_ms < 1000 {
        return format!("{timeout_ms}ms");
    }
    if timeout_ms < 120_000 {
        return format!("{}s", js_decimal(js_round(ms / 100.0) / 10.0));
    }
    format!("{}m", js_decimal(js_round(ms / 6000.0) / 10.0))
}

pub(crate) fn retried_sentence(attempts: u32) -> String {
    format!("Retried {attempts} time{} on the same model with the same result.", if attempts == 1 { "" } else { "s" })
}

pub fn describe_provider_stall_for_user(error_message: Option<&str>, options: &ProviderStallDescriptionOptions) -> Option<String> {
    let error_message = error_message.filter(|m| !m.is_empty())?;
    for phase in PROVIDER_STALL_PHASES.iter() {
        let Some(captures) = phase.pattern.captures(error_message) else { continue };
        let subject = options.model.as_ref().map_or_else(|| "The provider".to_owned(), |m| format!("The provider for {m}"));
        let duration = format_stall_duration(captures[1].parse().unwrap_or(u64::MAX));
        let mut sentences = vec![format!("{subject} {} within {duration}, so the request was cancelled.", phase.symptom)];
        let attempts = options.attempts.unwrap_or(0);
        if attempts > 0 {
            sentences.push(retried_sentence(attempts));
        }
        if let Some(recovery) = options.recovery {
            let raise = phase.setting.map(|s| format!(", or raise {s} in settings (0 disables the bound)")).unwrap_or_default();
            let lead = match recovery {
                StallRecovery::NoFallbackConfigured => {
                    "No fallback model is configured for it, so nothing could take the turn over: run /fallback to add one"
                }
                StallRecovery::ChainExhausted => "Every model in its fallback chain was tried as well: run /fallback to review the chain",
            };
            sentences.push(format!("{lead}, send the message again{raise}."));
        }
        return Some(sentences.join(" "));
    }
    None
}

pub fn is_provider_timeout_error(message: &AssistantMessage) -> bool {
    if message.abort_source == Some(AbortSource::Provider) || is_provider_stream_stall_error(message) {
        return true;
    }
    if !matches!(message.stop_reason, StopReason::Error | StopReason::Aborted) {
        return false;
    }
    PROVIDER_TRANSPORT_TIMEOUT_ERROR_PATTERN.is_match(message.error_message.as_deref().unwrap_or_default())
}

pub fn is_retryable_error_message(error_message: &str) -> bool {
    classify_error_message(error_message) == ErrorMessageClass::Retryable
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorMessageClass {
    NonRetryable,
    Retryable,
    Unknown,
}

impl ErrorMessageClass {
    pub fn as_str(self) -> &'static str {
        match self {
            ErrorMessageClass::NonRetryable => "non-retryable",
            ErrorMessageClass::Retryable => "retryable",
            ErrorMessageClass::Unknown => "unknown",
        }
    }
}

pub fn classify_error_message(error_message: &str) -> ErrorMessageClass {
    if error_message.is_empty() {
        ErrorMessageClass::Unknown
    } else if NON_RETRYABLE_PROVIDER_ERROR_PATTERN.is_match(error_message) {
        ErrorMessageClass::NonRetryable
    } else if RETRYABLE_PROVIDER_ERROR_PATTERN.is_match(error_message) {
        ErrorMessageClass::Retryable
    } else {
        ErrorMessageClass::Unknown
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::abort::AbortController;
    use crate::utils::empty_response_errors::{EMPTY_RESPONSE_ERROR, EMPTY_TOOL_USE_ERROR};
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicU32, Ordering};

    /// Mirrors senpi's `test/retry.test.ts` / `src/providers/faux.ts` `fauxAssistantMessage`:
    /// an assistant message with `stopReason: "stop"` by default, overridden per case.
    struct FauxOptions {
        stop_reason: StopReason,
        error_message: Option<String>,
        abort_source: Option<AbortSource>,
        stop_details: Option<AssistantStopDetails>,
    }

    impl Default for FauxOptions {
        fn default() -> Self {
            FauxOptions { stop_reason: StopReason::Stop, error_message: None, abort_source: None, stop_details: None }
        }
    }

    fn faux(options: FauxOptions) -> AssistantMessage {
        let mut model = crate::models_generated::get_builtin_model("anthropic", "claude-opus-4-8").expect("model").clone();
        model.id = "m".into();
        AssistantMessage {
            content: Vec::new(),
            api: model.api,
            provider: model.provider,
            model: model.id,
            response_model: None,
            response_id: None,
            provider_thinking_level: None,
            diagnostics: None,
            usage: crate::types::Usage::default(),
            stop_reason: options.stop_reason,
            stop_details: options.stop_details,
            deferred: None,
            error_message: options.error_message,
            abort_source: options.abort_source,
            raw_stop_reason: None,
            end_turn: None,
            timestamp: crate::utils::diagnostics::now_ms(),
        }
    }

    /// `fauxAssistantMessage("", { stopReason: "error", errorMessage })`
    fn error_message(text: &str) -> AssistantMessage {
        faux(FauxOptions { stop_reason: StopReason::Error, error_message: Some(text.to_owned()), ..Default::default() })
    }

    /// `fauxAssistantMessage("ok")` / `fauxAssistantMessage("recovered")`
    fn ok_message() -> AssistantMessage {
        faux(FauxOptions::default())
    }

    fn policy(max_retries: u32) -> RetryPolicy {
        RetryPolicy { enabled: true, max_retries, base_delay_ms: 1000, max_agent_delay_ms: None, random: Some(Arc::new(|| 0.5)) }
    }

    /// A policy whose jittered delay is always 0, so retry-loop tests under `start_paused = true`
    /// settle without advancing the paused clock.
    fn zero_delay_policy(enabled: bool, max_retries: u32) -> RetryPolicy {
        RetryPolicy { enabled, max_retries, base_delay_ms: 0, max_agent_delay_ms: None, random: Some(Arc::new(|| 0.0)) }
    }

    const OPENAI_EXPLICIT_RETRY_MESSAGE: &str = "An error occurred while processing your request. You can retry your request, or contact us through our help center at help.openai.com if the error persists. Please include the request ID req_******** in your message.";
    const OPENAI_SERVER_ERROR_MESSAGE: &str = "Error: Error Code server_error: An error occurred while processing your request. You can retry your request, or contact us through our help center at help.openai.com if the error persists. Please include the request ID e4026cfc-c6b6-414a-8a21-c03a6adf0336 in your message.";
    const BEDROCK_EXPLICIT_RETRY_MESSAGE: &str =
        "{\"message\":\"The system encountered an unexpected error during processing. Try your request again.\"}";
    const NVIDIA_NIM_RESOURCE_EXHAUSTED_MESSAGE: &str = "ResourceExhausted: Worker local total request limit reached (288/48)";
    const BUN_FETCH_SOCKET_CLOSED_MESSAGE: &str =
        "The socket connection was closed unexpectedly. For more information, pass `verbose: true` in the second argument to fetch()";
    const OPENAI_RESPONSES_EARLY_EOF_MESSAGE: &str = "OpenAI Responses stream ended before a terminal response event";
    const WRAPPED_DNS_LOOKUP_ERROR: &str =
        "The pending stream has been canceled (caused by: getaddrinfo ENOTFOUND bedrock-runtime.us-east-1.amazonaws.com)";
    const CODEX_UPSTREAM_UNAVAILABLE_MESSAGE: &str =
        "Error: upstream_unavailable: Codex upstream websocket send failed via proxy endpoint unknown: ConnectionClosedOK";
    const ANTHROPIC_ORPHAN_SERVER_TOOL_MESSAGE: &str = "400 {\"type\":\"error\",\"error\":{\"type\":\"invalid_request_error\",\"message\":\"messages.1: `web_search` tool use with id `srvtoolu_01Gchdhqw1UaCNUuVq2LhMH9` was found without a corresponding `web_search_tool_result` block\"},\"request_id\":\"req_011CdQL9JsEk5NWJxWQX4NiG\"}";
    const ANTHROPIC_INVALID_MAX_TOKENS_MESSAGE: &str =
        "400 {\"type\":\"error\",\"error\":{\"type\":\"invalid_request_error\",\"message\":\"max_tokens: must be greater than or equal to 1\"}}";
    const APITOPIA_TOOL_SCHEMA_REJECTION_MESSAGE: &str = "500 data: {\"error\":{\"message\":\"500 server_error: Invalid request: tools.function.parameters.type is required and must be \\\"object\\\"\",\"type\":\"server_error\",\"code\":500,\"status\":500,\"statusCode\":500,\"isRetryable\":true}}\n\ndata:[DONE]\n\n";
    const MOONSHOT_TOOL_SCHEMA_REJECTION_MESSAGE: &str = "500 server_error: Invalid request: tools.0.function.parameters: invalid tool schema";
    const GATEWAY_MODEL_REQUEST_REJECTED_MESSAGE: &str = "Error: The model request was rejected. Check the request and try again.";

    /// "applies bounded injectable Codex-style jitter"
    #[test]
    fn applies_bounded_injectable_codex_style_jitter() {
        assert_eq!(retry_delay_ms(&RetryPolicy { random: Some(Arc::new(|| 0.0)), ..policy(3) }, 1), 900);
        assert_eq!(retry_delay_ms(&RetryPolicy { random: Some(Arc::new(|| 1.0)), ..policy(3) }, 1), 1_100);
    }

    /// "keeps provider retry hints above the jittered schedule"
    #[test]
    fn keeps_provider_retry_hints_above_the_jittered_schedule() {
        let hinted: u64 = 1_050;
        let jittered = retry_delay_ms(&RetryPolicy { random: Some(Arc::new(|| 0.0)), ..policy(3) }, 1);
        assert_eq!(hinted.max(jittered), hinted);
    }

    /// "matches explicit provider retry guidance"
    #[test]
    fn matches_explicit_provider_retry_guidance() {
        assert!(is_retryable_assistant_error(&error_message(OPENAI_EXPLICIT_RETRY_MESSAGE)));
        assert!(is_retryable_assistant_error(&error_message(BEDROCK_EXPLICIT_RETRY_MESSAGE)));
        assert!(is_retryable_assistant_error(&error_message(NVIDIA_NIM_RESOURCE_EXHAUSTED_MESSAGE)));
    }

    /// "classifies credential-store lock exhaustion as retryable infrastructure"
    #[test]
    fn classifies_credential_store_lock_exhaustion_as_retryable() {
        assert!(is_retryable_assistant_error(&error_message(
            "Credential store is busy: lock /tmp/auth.json was held for 1234ms. Another process may be refreshing credentials"
        )));
    }

    /// "classifies only explicitly provider-owned aborts as provider timeouts"
    #[test]
    fn classifies_only_explicitly_provider_owned_aborts_as_provider_timeouts() {
        let provider_aborted = faux(FauxOptions {
            stop_reason: StopReason::Aborted,
            error_message: Some("Request was aborted".into()),
            abort_source: Some(AbortSource::Provider),
            ..Default::default()
        });
        assert!(is_provider_timeout_error(&provider_aborted));
        let plain_aborted =
            faux(FauxOptions { stop_reason: StopReason::Aborted, error_message: Some("Request was aborted".into()), ..Default::default() });
        assert!(!is_provider_timeout_error(&plain_aborted));
    }

    /// "classifies agent-loop stream timeout errors as retryable"
    #[test]
    fn classifies_agent_loop_stream_timeout_errors_as_retryable() {
        assert!(is_retryable_assistant_error(&error_message("Provider stream start timed out after 90000ms")));
        assert!(is_retryable_assistant_error(&error_message("Idle timeout waiting for provider stream after 300000ms")));
        assert!(is_retryable_assistant_error(&error_message("WebSocket liveness timeout after 70000ms (2 pings unanswered)")));
        assert!(!is_retryable_assistant_error(&error_message("Provider stream never started")));
    }

    /// "classifies provider timeout provenance without matching incidental text: Idle timeout
    /// waiting for provider stream after 300000ms"
    #[test]
    fn classifies_provider_timeout_provenance_idle_timeout_waiting_for_provider_stream_after_300000ms() {
        let message = error_message("Idle timeout waiting for provider stream after 300000ms");
        assert!(is_provider_stream_stall_error(&message));
        assert!(is_provider_timeout_error(&message));
    }

    /// "classifies provider timeout provenance without matching incidental text: Provider stream
    /// start timed out after 90000ms"
    #[test]
    fn classifies_provider_timeout_provenance_provider_stream_start_timed_out_after_90000ms() {
        let message = error_message("Provider stream start timed out after 90000ms");
        assert!(is_provider_stream_stall_error(&message));
        assert!(is_provider_timeout_error(&message));
    }

    /// "classifies provider timeout provenance without matching incidental text: Provider stream
    /// start timed out after 90000ms (raise streamStartTimeoutMs — retry.provider.streamStartTimeoutMs
    /// in senpi settings; 0 disables)"
    #[test]
    fn classifies_provider_timeout_provenance_provider_stream_start_timed_out_with_raise_hint() {
        let message = error_message(
            "Provider stream start timed out after 90000ms (raise streamStartTimeoutMs \u{2014} retry.provider.streamStartTimeoutMs in senpi settings; 0 disables)",
        );
        assert!(is_provider_stream_stall_error(&message));
        assert!(is_provider_timeout_error(&message));
    }

    /// "classifies provider timeout provenance without matching incidental text: Idle timeout
    /// waiting for provider stream after 5ms (x)"
    #[test]
    fn classifies_provider_timeout_provenance_idle_timeout_waiting_for_provider_stream_after_5ms_with_suffix() {
        let message = error_message("Idle timeout waiting for provider stream after 5ms (x)");
        assert!(!is_provider_stream_stall_error(&message));
        assert!(!is_provider_timeout_error(&message));
    }

    /// "classifies provider timeout provenance without matching incidental text: WebSocket
    /// liveness timeout after 70000ms (2 pings unanswered)"
    #[test]
    fn classifies_provider_timeout_provenance_websocket_liveness_timeout_with_ping_count() {
        let message = error_message("WebSocket liveness timeout after 70000ms (2 pings unanswered)");
        assert!(is_provider_stream_stall_error(&message));
        assert!(is_provider_timeout_error(&message));
    }

    /// "classifies provider timeout provenance without matching incidental text: WebSocket
    /// liveness timeout after 70000ms (2 pings unanswered) extra"
    #[test]
    fn classifies_provider_timeout_provenance_websocket_liveness_timeout_with_trailing_text() {
        let message = error_message("WebSocket liveness timeout after 70000ms (2 pings unanswered) extra");
        assert!(!is_provider_stream_stall_error(&message));
        assert!(!is_provider_timeout_error(&message));
    }

    /// "classifies provider timeout provenance without matching incidental text: Request timed
    /// out."
    #[test]
    fn classifies_provider_timeout_provenance_request_timed_out_with_period() {
        let message = error_message("Request timed out.");
        assert!(!is_provider_stream_stall_error(&message));
        assert!(is_provider_timeout_error(&message));
    }

    /// "classifies provider timeout provenance without matching incidental text: Request timed
    /// out"
    #[test]
    fn classifies_provider_timeout_provenance_request_timed_out_without_period() {
        let message = error_message("Request timed out");
        assert!(!is_provider_stream_stall_error(&message));
        assert!(is_provider_timeout_error(&message));
    }

    /// "classifies provider timeout provenance without matching incidental text: Command timed
    /// out after 30000ms"
    #[test]
    fn classifies_provider_timeout_provenance_command_timed_out_after_30000ms() {
        let message = error_message("Command timed out after 30000ms");
        assert!(!is_provider_stream_stall_error(&message));
        assert!(!is_provider_timeout_error(&message));
    }

    /// "classifies provider timeout provenance without matching incidental text: MCP server
    /// example timed out"
    #[test]
    fn classifies_provider_timeout_provenance_mcp_server_example_timed_out() {
        let message = error_message("MCP server example timed out");
        assert!(!is_provider_stream_stall_error(&message));
        assert!(!is_provider_timeout_error(&message));
    }

    /// "classifies provider timeout provenance without matching incidental text: extension timed
    /// out"
    #[test]
    fn classifies_provider_timeout_provenance_extension_timed_out() {
        let message = error_message("extension timed out");
        assert!(!is_provider_stream_stall_error(&message));
        assert!(!is_provider_timeout_error(&message));
    }

    /// "recognizes aborted transport timeouts but not unrelated aborted work"
    #[test]
    fn recognizes_aborted_transport_timeouts_but_not_unrelated_aborted_work() {
        let transport_timeout =
            faux(FauxOptions { stop_reason: StopReason::Aborted, error_message: Some("Request timed out.".into()), ..Default::default() });
        assert!(is_provider_timeout_error(&transport_timeout));
        let unrelated = faux(FauxOptions {
            stop_reason: StopReason::Aborted,
            error_message: Some("Command timed out after 30000ms".into()),
            ..Default::default()
        });
        assert!(!is_provider_timeout_error(&unrelated));
        let stall_but_aborted = faux(FauxOptions {
            stop_reason: StopReason::Aborted,
            error_message: Some("Idle timeout waiting for provider stream after 300000ms".into()),
            ..Default::default()
        });
        assert!(!is_provider_stream_stall_error(&stall_but_aborted));
    }

    /// "classifies the observed OpenAI server_error as retryable"
    #[test]
    fn classifies_the_observed_openai_server_error_as_retryable() {
        assert!(is_retryable_assistant_error(&error_message(OPENAI_SERVER_ERROR_MESSAGE)));
    }

    /// "classifies Cloudflare 522 connection timeouts as retryable"
    #[test]
    fn classifies_cloudflare_522_connection_timeouts_as_retryable() {
        assert!(is_retryable_assistant_error(&error_message("Error: error code: 522")));
        assert!(is_retryable_assistant_error(&error_message("522: Connection timed out")));
    }

    /// "matches Bun fetch socket drop wording"
    #[test]
    fn matches_bun_fetch_socket_drop_wording() {
        assert!(is_retryable_assistant_error(&error_message(BUN_FETCH_SOCKET_CLOSED_MESSAGE)));
    }

    /// "matches Codex upstream websocket unavailability"
    #[test]
    fn matches_codex_upstream_websocket_unavailability() {
        assert!(is_retryable_assistant_error(&error_message(CODEX_UPSTREAM_UNAVAILABLE_MESSAGE)));
    }

    /// "classifies zero-event stream idle timeouts as provider stream stalls"
    #[test]
    fn classifies_zero_event_stream_idle_timeouts_as_provider_stream_stalls() {
        assert!(is_provider_stream_stall_error(&error_message("Idle timeout waiting for provider stream after 300000ms")));
        assert!(is_provider_stream_stall_error(&error_message(
            "Provider stream start timed out after 90000ms (raise streamStartTimeoutMs \u{2014} retry.provider.streamStartTimeoutMs in senpi settings; 0 disables)"
        )));
        assert!(!is_provider_stream_stall_error(&error_message("Request timed out.")));
        let aborted = faux(FauxOptions {
            stop_reason: StopReason::Aborted,
            error_message: Some("Idle timeout waiting for provider stream after 300000ms".into()),
            ..Default::default()
        });
        assert!(!is_provider_stream_stall_error(&aborted));
    }

    /// "classifies agent-loop stream idle timeouts as retryable"
    #[test]
    fn classifies_agent_loop_stream_idle_timeouts_as_retryable() {
        assert!(is_retryable_assistant_error(&error_message("Idle timeout waiting for provider stream after 300000ms")));
    }

    /// "matches Claude Agent SDK session lock contention"
    #[test]
    fn matches_claude_agent_sdk_session_lock_contention() {
        assert!(is_retryable_assistant_error(&error_message("Lock file is already being held")));
    }

    /// "matches upstream request buffer exhaustion wording"
    #[test]
    fn matches_upstream_request_buffer_exhaustion_wording() {
        assert!(is_retryable_assistant_error(&error_message("Error: exceeded request buffer limit while retrying upstream")));
    }

    /// "matches DNS transport failure wording: The pending stream has been canceled (caused by:
    /// getaddrinfo ENOTFOUND bedrock-runtime.us-east-1.amazonaws.com)"
    #[test]
    fn matches_dns_transport_failure_wording_wrapped_stream_cancellation() {
        assert!(is_retryable_assistant_error(&error_message(WRAPPED_DNS_LOOKUP_ERROR)));
    }

    /// "matches DNS transport failure wording: connect ENOTFOUND api.example.com"
    #[test]
    fn matches_dns_transport_failure_wording_connect_enotfound() {
        assert!(is_retryable_assistant_error(&error_message("connect ENOTFOUND api.example.com")));
    }

    /// "matches DNS transport failure wording: EAI_AGAIN api.example.com"
    #[test]
    fn matches_dns_transport_failure_wording_eai_again() {
        assert!(is_retryable_assistant_error(&error_message("EAI_AGAIN api.example.com")));
    }

    /// "matches DNS transport failure wording: getaddrinfo failed for api.example.com"
    #[test]
    fn matches_dns_transport_failure_wording_getaddrinfo_failed() {
        assert!(is_retryable_assistant_error(&error_message("getaddrinfo failed for api.example.com")));
    }

    /// "matches OpenAI Responses streams that end before terminal events"
    #[test]
    fn matches_openai_responses_streams_that_end_before_terminal_events() {
        assert!(is_retryable_assistant_error(&error_message(OPENAI_RESPONSES_EARLY_EOF_MESSAGE)));
    }

    /// "classifies Anthropic server-tool pairing 400s as retryable"
    #[test]
    fn classifies_anthropic_server_tool_pairing_400s_as_retryable() {
        assert!(is_retryable_assistant_error(&error_message(ANTHROPIC_ORPHAN_SERVER_TOOL_MESSAGE)));
    }

    /// "retries an empty outcome only when its reasoning was already forwarded live"
    #[test]
    fn retries_an_empty_outcome_only_when_its_reasoning_was_already_forwarded_live() {
        for message in [FORWARDED_EMPTY_RESPONSE_ERROR, FORWARDED_EMPTY_TOOL_USE_ERROR] {
            assert!(is_retryable_assistant_error(&error_message(message)), "{message}");
        }
        for message in [EMPTY_RESPONSE_ERROR, EMPTY_TOOL_USE_ERROR] {
            assert!(!is_retryable_assistant_error(&error_message(message)), "{message}");
        }
    }

    /// "keeps unrelated invalid_request errors non-retryable"
    #[test]
    fn keeps_unrelated_invalid_request_errors_non_retryable() {
        assert!(!is_retryable_assistant_error(&error_message(ANTHROPIC_INVALID_MAX_TOKENS_MESSAGE)));
    }

    /// "keeps gateway-wrapped tool-schema rejections non-retryable"
    #[test]
    fn keeps_gateway_wrapped_tool_schema_rejections_non_retryable() {
        assert!(!is_retryable_assistant_error(&error_message(APITOPIA_TOOL_SCHEMA_REJECTION_MESSAGE)));
        assert!(!is_retryable_assistant_error(&error_message(MOONSHOT_TOOL_SCHEMA_REJECTION_MESSAGE)));
    }

    /// "keeps genuine transient server errors retryable alongside schema rejections"
    #[test]
    fn keeps_genuine_transient_server_errors_retryable_alongside_schema_rejections() {
        assert!(is_retryable_assistant_error(&error_message(OPENAI_SERVER_ERROR_MESSAGE)));
        assert!(is_retryable_assistant_error(&error_message("500 server_error: internal error, please retry")));
    }

    /// "matches the canonical gateway model-request rejection as transient"
    #[test]
    fn matches_the_canonical_gateway_model_request_rejection_as_transient() {
        assert!(is_retryable_assistant_error(&error_message(GATEWAY_MODEL_REQUEST_REJECTED_MESSAGE)));
    }

    /// "keeps non-canonical model-request rejections terminal: Error: The model request was
    /// rejected because this API key does not have permission to use it."
    #[test]
    fn keeps_non_canonical_model_request_rejection_without_permission_terminal() {
        assert!(!is_retryable_assistant_error(&error_message(
            "Error: The model request was rejected because this API key does not have permission to use it."
        )));
    }

    /// "keeps non-canonical model-request rejections terminal: Error: The model request was
    /// rejected because max_tokens must be greater than or equal to 1."
    #[test]
    fn keeps_non_canonical_model_request_rejection_for_max_tokens_terminal() {
        assert!(!is_retryable_assistant_error(&error_message(
            "Error: The model request was rejected because max_tokens must be greater than or equal to 1."
        )));
    }

    /// "keeps non-canonical model-request rejections terminal: Error: The model request was
    /// rejected by the safety classifier."
    #[test]
    fn keeps_non_canonical_model_request_rejection_by_safety_classifier_terminal() {
        assert!(!is_retryable_assistant_error(&error_message(
            "Error: The model request was rejected by the safety classifier."
        )));
    }

    /// "keeps typed refusal messages terminal even with the canonical retry wording"
    #[test]
    fn keeps_typed_refusal_messages_terminal_even_with_the_canonical_retry_wording() {
        let message = faux(FauxOptions {
            stop_reason: StopReason::Error,
            error_message: Some(GATEWAY_MODEL_REQUEST_REJECTED_MESSAGE.into()),
            stop_details: Some(AssistantStopDetails::Refusal { explanation: None }),
            ..Default::default()
        });
        assert!(!is_retryable_assistant_error(&message));
    }

    /// "keeps typed sensitive messages terminal even with the canonical retry wording"
    #[test]
    fn keeps_typed_sensitive_messages_terminal_even_with_the_canonical_retry_wording() {
        let message = faux(FauxOptions {
            stop_reason: StopReason::Error,
            error_message: Some(GATEWAY_MODEL_REQUEST_REJECTED_MESSAGE.into()),
            stop_details: Some(AssistantStopDetails::Sensitive),
            ..Default::default()
        });
        assert!(!is_retryable_assistant_error(&message));
    }

    /// "keeps non-retryable overlap precedence over the canonical retry wording"
    #[test]
    fn keeps_non_retryable_overlap_precedence_over_the_canonical_retry_wording() {
        assert!(!is_retryable_assistant_error(&error_message(&format!("{GATEWAY_MODEL_REQUEST_REJECTED_MESSAGE} quota exceeded"))));
        assert!(!is_retryable_assistant_error(&error_message(&format!(
            "{GATEWAY_MODEL_REQUEST_REJECTED_MESSAGE} Invalid request: tools.0.function.parameters.type is required"
        ))));
    }

    /// "keeps provider limit errors non-retryable"
    #[test]
    fn keeps_provider_limit_errors_non_retryable() {
        assert!(!is_retryable_assistant_error(&error_message("429 quota exceeded")));
    }

    /// "keeps anthropic credits_required errors non-retryable"
    #[test]
    fn keeps_anthropic_credits_required_errors_non_retryable() {
        // Verbatim 429 from a real session (2026-07-29, anthropic claude-fable-5): a
        // billing-dead account must not burn same-model retries before the fallback chain
        // takes over.
        let credits_required = "429 event: error\ndata: {\"type\":\"error\",\"error\":{\"type\":\"rate_limit_error\",\"message\":\"Usage credits are required for this model.\",\"details\":{\"error_code\":\"credits_required\",\"model\":\"claude-fable-5\"}},\"request_id\":\"req_011CdW2nFxprAx6KQ9JhnAvq\"}";
        assert!(!is_retryable_assistant_error(&error_message(credits_required)));
    }

    /// "classifies assistant error messages"
    #[test]
    fn classifies_assistant_error_messages() {
        assert!(is_retryable_assistant_error(&error_message("overloaded_error")));
        assert!(is_retryable_assistant_error(&error_message("524 status code (no body)")));
        assert!(!is_retryable_assistant_error(&faux(FauxOptions { stop_reason: StopReason::Stop, ..Default::default() })));
    }

    /// "caps agent retry delay"
    #[test]
    fn caps_agent_retry_delay() {
        // Regression for #8826. The fork jitters the scheduled delay by +/-10% before the cap
        // applies, so the jitter source is pinned here instead of relying on the exact
        // unjittered product.
        let hi = RetryPolicy { enabled: true, max_retries: 3, base_delay_ms: 2000, max_agent_delay_ms: None, random: Some(Arc::new(|| 1.0)) };
        assert_eq!(retry_delay_ms(&hi, 6), 60_000);
        let capped_5000 =
            RetryPolicy { enabled: true, max_retries: 3, base_delay_ms: 2000, max_agent_delay_ms: Some(5000), random: Some(Arc::new(|| 1.0)) };
        assert_eq!(retry_delay_ms(&capped_5000, 5), 5000);
        let capped_0 =
            RetryPolicy { enabled: true, max_retries: 3, base_delay_ms: 2000, max_agent_delay_ms: Some(0), random: Some(Arc::new(|| 1.0)) };
        assert_eq!(retry_delay_ms(&capped_0, 5), 0);
    }

    /// "clamps an exponentially overflowed delay to the cap"
    #[test]
    fn clamps_an_exponentially_overflowed_delay_to_the_cap() {
        let policy_a = RetryPolicy {
            enabled: true,
            max_retries: 3,
            base_delay_ms: 2000,
            max_agent_delay_ms: Some(60_000),
            random: Some(Arc::new(|| 1.0)),
        };
        assert_eq!(retry_delay_ms(&policy_a, 20), 60_000);
        let policy_b = RetryPolicy {
            enabled: true,
            max_retries: 3,
            base_delay_ms: 2000,
            max_agent_delay_ms: Some(60_000),
            random: Some(Arc::new(|| 0.0)),
        };
        assert_eq!(retry_delay_ms(&policy_b, 2000), 60_000);
    }

    /// "never exceeds the default cap for any jitter sample"
    #[test]
    fn never_exceeds_the_default_cap_for_any_jitter_sample() {
        for sample in [0.0, 0.25, 0.5, 0.75, 1.0] {
            let p = RetryPolicy {
                enabled: true,
                max_retries: 3,
                base_delay_ms: 2000,
                max_agent_delay_ms: None,
                random: Some(Arc::new(move || sample)),
            };
            assert_eq!(retry_delay_ms(&p, 8), 60_000);
        }
    }

    /// "returns a successful response immediately without retrying"
    #[tokio::test]
    async fn returns_a_successful_response_immediately_without_retrying() {
        let calls = AtomicU32::new(0);
        let response = retry_assistant_call(
            || {
                calls.fetch_add(1, Ordering::SeqCst);
                async { ok_message() }
            },
            Some(&policy(3)),
            None,
            None,
        )
        .await;
        assert_eq!(response.stop_reason, StopReason::Stop);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    /// "does not retry an aborted message"
    #[tokio::test]
    async fn does_not_retry_an_aborted_message() {
        let calls = AtomicU32::new(0);
        let scheduled = AtomicU32::new(0);
        let callbacks = RetryCallbacks {
            on_retry_scheduled: Some(Box::new(|_, _, _, _| {
                scheduled.fetch_add(1, Ordering::SeqCst);
            })),
            ..Default::default()
        };
        let response = retry_assistant_call(
            || {
                calls.fetch_add(1, Ordering::SeqCst);
                async { faux(FauxOptions { stop_reason: StopReason::Aborted, ..Default::default() }) }
            },
            Some(&policy(3)),
            None,
            Some(&callbacks),
        )
        .await;
        assert_eq!(response.stop_reason, StopReason::Aborted);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(scheduled.load(Ordering::SeqCst), 0);
    }

    /// "does not retry a non-retryable error (quota/billing)"
    #[tokio::test]
    async fn does_not_retry_a_non_retryable_error() {
        let calls = AtomicU32::new(0);
        let scheduled = AtomicU32::new(0);
        let finished = AtomicU32::new(0);
        let callbacks = RetryCallbacks {
            on_retry_scheduled: Some(Box::new(|_, _, _, _| {
                scheduled.fetch_add(1, Ordering::SeqCst);
            })),
            on_retry_finished: Some(Box::new(|_, _, _| {
                finished.fetch_add(1, Ordering::SeqCst);
            })),
            ..Default::default()
        };
        let response = retry_assistant_call(
            || {
                calls.fetch_add(1, Ordering::SeqCst);
                async { error_message("insufficient_quota") }
            },
            Some(&policy(3)),
            None,
            Some(&callbacks),
        )
        .await;
        assert_eq!(response.stop_reason, StopReason::Error);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(scheduled.load(Ordering::SeqCst), 0);
        assert_eq!(finished.load(Ordering::SeqCst), 0);
    }

    /// "retries a transient error up to maxRetries then returns the final error"
    #[tokio::test(start_paused = true)]
    async fn retries_a_transient_error_up_to_max_retries_then_returns_the_final_error() {
        let calls = AtomicU32::new(0);
        let scheduled = AtomicU32::new(0);
        let finished_args = Mutex::new(Vec::new());
        let callbacks = RetryCallbacks {
            on_retry_scheduled: Some(Box::new(|_, _, _, _| {
                scheduled.fetch_add(1, Ordering::SeqCst);
            })),
            on_retry_finished: Some(Box::new(|success, attempt, error| {
                finished_args.lock().expect("events").push((success, attempt, error.map(str::to_owned)));
            })),
            ..Default::default()
        };
        let response = retry_assistant_call(
            || {
                calls.fetch_add(1, Ordering::SeqCst);
                async { error_message("terminated") }
            },
            Some(&zero_delay_policy(true, 3)),
            None,
            Some(&callbacks),
        )
        .await;
        assert_eq!(response.stop_reason, StopReason::Error);
        assert_eq!(calls.load(Ordering::SeqCst), 4); // 1 initial + 3 retries
        assert_eq!(scheduled.load(Ordering::SeqCst), 3);
        assert_eq!(*finished_args.lock().expect("events"), [(false, 3, Some("terminated".to_owned()))]);
    }

    /// "reports capped retry delays"
    #[tokio::test(start_paused = true)]
    async fn reports_capped_retry_delays() {
        // Regression for #8826. The fork jitters the scheduled delay by +/-10% before the cap
        // applies, so pin the jitter source to its neutral midpoint (multiplier exactly 1.0)
        // instead of letting the random source perturb the schedule.
        let policy = RetryPolicy { enabled: true, max_retries: 4, base_delay_ms: 10, max_agent_delay_ms: Some(15), random: Some(Arc::new(|| 0.5)) };
        let n = AtomicU32::new(0);
        let delays = Mutex::new(Vec::new());
        let callbacks = RetryCallbacks {
            on_retry_scheduled: Some(Box::new(|_, _, delay, _| {
                delays.lock().expect("events").push(delay);
            })),
            ..Default::default()
        };
        retry_assistant_call(
            || {
                let value = n.fetch_add(1, Ordering::SeqCst) + 1;
                async move { if value < 5 { error_message("terminated") } else { ok_message() } }
            },
            Some(&policy),
            None,
            Some(&callbacks),
        )
        .await;
        assert_eq!(*delays.lock().expect("events"), [10, 15, 15, 15]);
    }

    /// "stops retrying once a call succeeds"
    #[tokio::test(start_paused = true)]
    async fn stops_retrying_once_a_call_succeeds() {
        let n = AtomicU32::new(0);
        let finished_args = Mutex::new(Vec::new());
        let callbacks = RetryCallbacks {
            on_retry_finished: Some(Box::new(|success, attempt, _| {
                finished_args.lock().expect("events").push((success, attempt));
            })),
            ..Default::default()
        };
        let response = retry_assistant_call(
            || {
                let value = n.fetch_add(1, Ordering::SeqCst) + 1;
                async move { if value < 3 { error_message("terminated") } else { ok_message() } }
            },
            Some(&zero_delay_policy(true, 3)),
            None,
            Some(&callbacks),
        )
        .await;
        assert_eq!(response.stop_reason, StopReason::Stop);
        assert_eq!(n.load(Ordering::SeqCst), 3);
        assert_eq!(*finished_args.lock().expect("events"), [(true, 2)]);
    }

    /// "retries a model-request rejection once then returns the recovered response"
    #[tokio::test(start_paused = true)]
    async fn retries_a_model_request_rejection_once_then_returns_the_recovered_response() {
        let n = AtomicU32::new(0);
        let scheduled = AtomicU32::new(0);
        let callbacks = RetryCallbacks {
            on_retry_scheduled: Some(Box::new(|_, _, _, _| {
                scheduled.fetch_add(1, Ordering::SeqCst);
            })),
            ..Default::default()
        };
        let response = retry_assistant_call(
            || {
                let value = n.fetch_add(1, Ordering::SeqCst) + 1;
                async move { if value < 2 { error_message(GATEWAY_MODEL_REQUEST_REJECTED_MESSAGE) } else { ok_message() } }
            },
            Some(&zero_delay_policy(true, 3)),
            None,
            Some(&callbacks),
        )
        .await;
        assert_eq!(response.stop_reason, StopReason::Stop);
        assert_eq!(n.load(Ordering::SeqCst), 2);
        assert_eq!(scheduled.load(Ordering::SeqCst), 1);
    }

    /// "reports an aborted retried call as unsuccessful"
    #[tokio::test(start_paused = true)]
    async fn reports_an_aborted_retried_call_as_unsuccessful() {
        let n = AtomicU32::new(0);
        let finished_args = Mutex::new(Vec::new());
        let callbacks = RetryCallbacks {
            on_retry_finished: Some(Box::new(|success, attempt, _| {
                finished_args.lock().expect("events").push((success, attempt));
            })),
            ..Default::default()
        };
        let response = retry_assistant_call(
            || {
                let value = n.fetch_add(1, Ordering::SeqCst) + 1;
                async move {
                    if value == 1 {
                        error_message("terminated")
                    } else {
                        faux(FauxOptions { stop_reason: StopReason::Aborted, ..Default::default() })
                    }
                }
            },
            Some(&zero_delay_policy(true, 3)),
            None,
            Some(&callbacks),
        )
        .await;
        assert_eq!(response.stop_reason, StopReason::Aborted);
        assert_eq!(n.load(Ordering::SeqCst), 2);
        assert_eq!(*finished_args.lock().expect("events"), [(false, 1)]);
    }

    /// "does not retry when policy is disabled"
    #[tokio::test]
    async fn does_not_retry_when_policy_is_disabled() {
        let calls = AtomicU32::new(0);
        let scheduled = AtomicU32::new(0);
        let finished = AtomicU32::new(0);
        let callbacks = RetryCallbacks {
            on_retry_scheduled: Some(Box::new(|_, _, _, _| {
                scheduled.fetch_add(1, Ordering::SeqCst);
            })),
            on_retry_finished: Some(Box::new(|_, _, _| {
                finished.fetch_add(1, Ordering::SeqCst);
            })),
            ..Default::default()
        };
        let disabled = RetryPolicy { enabled: false, max_retries: 3, base_delay_ms: 0, max_agent_delay_ms: None, random: None };
        let response = retry_assistant_call(
            || {
                calls.fetch_add(1, Ordering::SeqCst);
                async { error_message("terminated") }
            },
            Some(&disabled),
            None,
            Some(&callbacks),
        )
        .await;
        assert_eq!(response.stop_reason, StopReason::Error);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(scheduled.load(Ordering::SeqCst), 0);
        assert_eq!(finished.load(Ordering::SeqCst), 0);
    }

    /// "emits onRetryAttemptStart after backoff before each retried call"
    #[tokio::test(start_paused = true)]
    async fn emits_on_retry_attempt_start_after_backoff_before_each_retried_call() {
        let events = Mutex::new(Vec::<String>::new());
        let n = AtomicU32::new(0);
        let callbacks = RetryCallbacks {
            on_retry_scheduled: Some(Box::new(|attempt, _, _, _| {
                events.lock().expect("events").push(format!("retry:{attempt}"));
            })),
            on_retry_attempt_start: Some(Box::new(|| {
                events.lock().expect("events").push("attempt-start".to_owned());
            })),
            ..Default::default()
        };
        let response = retry_assistant_call(
            || {
                let value = n.fetch_add(1, Ordering::SeqCst);
                events.lock().expect("events").push(format!("produce:{value}"));
                async move { if value < 2 { error_message("terminated") } else { ok_message() } }
            },
            Some(&zero_delay_policy(true, 3)),
            None,
            Some(&callbacks),
        )
        .await;
        assert_eq!(response.stop_reason, StopReason::Stop);
        assert_eq!(
            *events.lock().expect("events"),
            ["produce:0", "retry:1", "attempt-start", "produce:1", "retry:2", "attempt-start", "produce:2"]
        );
    }

    /// "aborts backoff sleep via signal, returns an aborted message, and emits onRetryFinished(false)"
    #[tokio::test]
    async fn aborts_backoff_sleep_via_signal_returns_an_aborted_message_and_emits_on_retry_finished_false() {
        let controller = AbortController::new();
        let calls = Arc::new(AtomicU32::new(0));
        let finished_args = Arc::new(Mutex::new(Vec::new()));
        let finished_args_cb = Arc::clone(&finished_args);
        let callbacks = RetryCallbacks {
            on_retry_finished: Some(Box::new(move |success, attempt, error| {
                finished_args_cb.lock().expect("events").push((success, attempt, error.map(str::to_owned)));
            })),
            ..Default::default()
        };
        let policy = RetryPolicy { enabled: true, max_retries: 5, base_delay_ms: 10_000, max_agent_delay_ms: None, random: None };
        let signal = controller.signal();
        let calls_produce = Arc::clone(&calls);
        let response_future = retry_assistant_call(
            move || {
                calls_produce.fetch_add(1, Ordering::SeqCst);
                async { error_message("terminated") }
            },
            Some(&policy),
            Some(&signal),
            Some(&callbacks),
        );
        tokio::pin!(response_future);

        // Let one error call resolve and the first backoff sleep start, then abort — mirrors
        // `vi.waitFor(() => expect(produce).toHaveBeenCalled())` before `controller.abort()`.
        while calls.load(Ordering::SeqCst) == 0 {
            tokio::select! {
                biased;
                () = tokio::task::yield_now() => {}
                _ = &mut response_future => unreachable!("the retry future must not resolve before its first call"),
            }
        }
        controller.abort(None);
        let response = response_future.await;
        assert_eq!(response.stop_reason, StopReason::Aborted);
        assert_eq!(response.error_message, None);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(*finished_args.lock().expect("events"), [(false, 1, Some("terminated".to_owned()))]);
    }

    /// "classifies provider retry classification" describe-block regression coverage for the
    /// classifier surface itself (not an individual TS `it`, kept from the pre-existing suite).
    #[test]
    fn classifies_provider_error_messages() {
        assert_eq!(classify_error_message("429 Too Many Requests"), ErrorMessageClass::Retryable);
        assert_eq!(classify_error_message("insufficient_quota: 429"), ErrorMessageClass::NonRetryable);
        assert_eq!(classify_error_message("usage limit has been reached"), ErrorMessageClass::NonRetryable);
        assert_eq!(classify_error_message("invalid request: tools.0 bad"), ErrorMessageClass::NonRetryable);
        assert_eq!(classify_error_message(FORWARDED_EMPTY_RESPONSE_ERROR), ErrorMessageClass::Retryable);
        assert_eq!(classify_error_message("bad input"), ErrorMessageClass::Unknown);
        assert_eq!(classify_error_message(""), ErrorMessageClass::Unknown);
    }

    #[test]
    fn stall_descriptions_and_timeouts() {
        let options = ProviderStallDescriptionOptions {
            attempts: Some(2),
            model: Some("m1".into()),
            recovery: Some(StallRecovery::NoFallbackConfigured),
        };
        assert_eq!(
            describe_provider_stall_for_user(Some("Idle timeout waiting for provider stream after 90000ms"), &options).as_deref(),
            Some("The provider for m1 started the response and then went silent within 90s, so the request was cancelled. Retried 2 times on the same model with the same result. No fallback model is configured for it, so nothing could take the turn over: run /fallback to add one, send the message again, or raise retry.provider.timeoutMs in settings (0 disables the bound).")
        );
        assert_eq!(format_stall_duration(1250), "1.3s");
        assert_eq!(format_stall_duration(300_000), "5m");
        assert_eq!(format_stall_duration(999), "999ms");
        assert!(is_provider_stream_stall_error(&error_message("WebSocket liveness timeout after 30000ms (3 pings unanswered)")));
        assert!(is_provider_timeout_error(&error_message("Request timed out.")));
        assert!(!is_provider_timeout_error(&error_message("boom")));
    }
}
