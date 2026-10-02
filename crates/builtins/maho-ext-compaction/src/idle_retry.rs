//! Bounded retry eligibility for transient idle warm-up failures.
pub const MAX_IDLE_WARMUP_RETRIES: u32 = 2;
pub const IDLE_WARMUP_RETRY_DELAY_MS: u64 = 15_000;
pub struct IdleWarmupRetryDecision {
    pub attempt: u32,
    pub transient: bool,
    pub is_idle: bool,
    pub breaker_tripped: bool,
    pub still_warm_eligible: bool,
}
pub fn should_retry_idle_warmup(decision: &IdleWarmupRetryDecision) -> bool {
    decision.transient && decision.is_idle && !decision.breaker_tripped
        && decision.still_warm_eligible && decision.attempt < MAX_IDLE_WARMUP_RETRIES
}
