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
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicU32, Ordering};

    fn error_message(text: &str) -> AssistantMessage {
        let mut model = crate::models_generated::get_builtin_model("anthropic", "claude-opus-4-8").expect("model").clone();
        model.id = "m".into();
        crate::utils::lazy::setup_error_message(&model, text)
    }

    fn policy(max_retries: u32) -> RetryPolicy {
        RetryPolicy { enabled: true, max_retries, base_delay_ms: 1000, max_agent_delay_ms: None, random: Some(Arc::new(|| 0.5)) }
    }

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
    fn retry_delay_is_exponential_jittered_and_capped() {
        assert_eq!(retry_delay_ms(&policy(3), 1), 1000);
        assert_eq!(retry_delay_ms(&policy(3), 3), 4000);
        let low = RetryPolicy { random: Some(Arc::new(|| -3.0)), ..policy(3) };
        assert_eq!(retry_delay_ms(&low, 1), 900);
        assert_eq!(retry_delay_ms(&policy(3), 40), DEFAULT_MAX_AGENT_RETRY_DELAY_MS);
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

    #[tokio::test(start_paused = true)]
    async fn retry_assistant_call_retries_retryable_errors_then_succeeds() {
        let calls = AtomicU32::new(0);
        let events = Mutex::new(Vec::new());
        let callbacks = RetryCallbacks {
            on_retry_scheduled: Some(Box::new(|attempt, max, delay, error: &str| {
                events.lock().expect("events").push(format!("scheduled {attempt}/{max} {delay} {error}"));
            })),
            on_retry_attempt_start: None,
            on_retry_finished: Some(Box::new(|success, attempt, _| events.lock().expect("events").push(format!("finished {success} {attempt}")))),
        };
        let response = retry_assistant_call(
            || {
                let n = calls.fetch_add(1, Ordering::SeqCst);
                async move {
                    let mut message = error_message("503 overloaded");
                    if n > 0 {
                        message.stop_reason = StopReason::Stop;
                        message.error_message = None;
                    }
                    message
                }
            },
            Some(&policy(2)),
            None,
            Some(&callbacks),
        )
        .await;
        assert_eq!(response.stop_reason, StopReason::Stop);
        assert_eq!(*events.lock().expect("events"), ["scheduled 1/2 1000 503 overloaded", "finished true 1"]);
    }

    #[tokio::test]
    async fn retry_assistant_call_abort_during_backoff_returns_aborted() {
        let controller = AbortController::new();
        controller.abort(None);
        let response =
            retry_assistant_call(|| async { error_message("fetch failed") }, Some(&policy(3)), Some(&controller.signal()), None).await;
        assert_eq!(response.stop_reason, StopReason::Aborted);
        assert_eq!(response.error_message, None);
        let refusal = AssistantMessage { stop_details: Some(AssistantStopDetails::Sensitive), ..error_message("429") };
        assert!(!is_retryable_assistant_error(&refusal));
    }

    #[tokio::test(start_paused = true)]
    async fn retry_transient_call_stops_on_non_retryable_errors() {
        let calls = AtomicU32::new(0);
        let result: Result<(), String> = retry_transient_call(
            || {
                calls.fetch_add(1, Ordering::SeqCst);
                async { Err("fatal".to_owned()) }
            },
            |error: &String| error != "fatal",
            Some(&policy(3)),
            None,
            None,
        )
        .await;
        assert_eq!(result, Err("fatal".into()));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }
}
