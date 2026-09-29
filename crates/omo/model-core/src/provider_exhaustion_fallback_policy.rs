use serde_json::Value;

use crate::runtime_fallback_error_classifier::RuntimeFallbackErrorType;
use crate::runtime_fallback_error_classifier::classify_runtime_fallback_error;

/// Only quota exhaustion qualifies as a provider-exhaustion fallback signal.
#[must_use]
pub fn classify_provider_exhaustion_fallback_signal(
    error: Option<&Value>,
) -> Option<RuntimeFallbackErrorType> {
    classify_runtime_fallback_error(error)
        .filter(|kind| *kind == RuntimeFallbackErrorType::QuotaExceeded)
}

#[must_use]
pub fn is_provider_exhaustion_fallback_eligible(error: Option<&Value>) -> bool {
    classify_provider_exhaustion_fallback_signal(error).is_some()
}
