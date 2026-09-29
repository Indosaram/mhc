//! Port of senpi packages/ai/src/utils/retry-profile/index.ts.

pub mod backoff;
pub mod classifiers;
pub mod failure;
pub mod planner;
pub mod profiles;
pub mod types;

pub use backoff::retry_backoff_delay_ms;
pub use classifiers::{classify_kimi_failure, classify_senpi_assistant_failure};
pub use failure::{RetryErrorShape, RetryFailureContext, normalize_anthropic_retry_failure};
pub use planner::{RetryPlanResult, plan_retry_delay};
pub use profiles::{KIMI_CODE_RETRY_PROFILE, SENPI_DEFAULT_RETRY_PROFILE};
pub use types::*;
