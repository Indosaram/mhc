//! Port of senpi packages/ai/src/utils/retry-profile/types.ts.

use std::sync::Arc;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetryFailureKind {
    Abort,
    Connection,
    Timeout,
    EmptyResponse,
    QuotaExhausted,
    HttpStatus,
    ImageFormat,
    Provider,
    Refusal,
    Sensitive,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetryFailure {
    pub origin: String,
    pub kind: RetryFailureKind,
    pub message: String,
    pub status_code: Option<u16>,
    pub provider_codes: Option<Vec<String>>,
    pub finish_reason: Option<String>,
    pub retry_after_ms: Option<u64>,
    pub should_retry: Option<bool>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetryClassification {
    Transient,
    RateLimited,
    Terminal,
}

pub type RetryClassifier = fn(&RetryFailure) -> RetryClassification;
pub type RetryHintExtractor = fn(&RetryFailure) -> Option<u64>;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum RetryJitterPolicy {
    None,
    Additive { ratio: f64 },
    Subtractive { ratio: f64 },
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RetryBackoffPolicy {
    pub base_delay_ms: f64,
    pub growth_factor: f64,
    pub per_attempt_cap_ms: Option<f64>,
    pub jitter: RetryJitterPolicy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetryHintExceeded {
    ErrorWithMarker,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetryHintCeiling {
    pub max_delay_ms: Option<u64>,
    pub on_exceeded: RetryHintExceeded,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RetryTieredHintDecision {
    pub tier: String,
    pub delay_ms: f64,
}

pub type RetryTieredHintStrategy =
    Arc<dyn Fn(&RetryFailure, u32, &RetryBackoffPolicy, i64) -> Result<RetryTieredHintDecision, String> + Send + Sync>;

#[derive(Clone)]
pub enum RetryServerHintPolicy {
    Override { accept_zero: bool, ceiling: RetryHintCeiling },
    Tiered { strategy: RetryTieredHintStrategy },
}

#[derive(Clone)]
pub struct RetryStagePolicy {
    pub enabled: bool,
    pub max_retries: u32,
    pub backoff: RetryBackoffPolicy,
    pub extract_server_hint: RetryHintExtractor,
    pub server_hint: RetryServerHintPolicy,
    pub classify: RetryClassifier,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FallbackTerminal {
    ImmediateIfEligible,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FallbackTransient {
    AfterTurnBudget,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FallbackRateLimited {
    Tiered,
    AfterTurnBudget,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetryFallbackPolicy {
    pub terminal: FallbackTerminal,
    pub transient: FallbackTransient,
    pub rate_limited: FallbackRateLimited,
    /// Always `true` in TS (a literal type).
    pub reset_budget_on_model_change: bool,
}

#[derive(Clone)]
pub struct RetryPolicyProfile {
    pub id: &'static str,
    pub provider_request: RetryStagePolicy,
    pub turn: RetryStagePolicy,
    pub fallback: RetryFallbackPolicy,
}
