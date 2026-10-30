//! Port of senpi packages/coding-agent/src/core/retry-fallback/controller-types.ts.
//!
//! `ActiveFallbackState` and `FallbackReason` live in the drift-owned `controller.rs` and are re-exported
//! here; the remaining declarations are new.

use std::collections::BTreeMap;

use maho_ai::types::UsageLimitScope;

pub use super::controller::{ActiveFallbackState, FallbackReason};

impl FallbackReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Transient => "transient",
            Self::Refusal => "refusal",
            Self::HardError => "hard-error",
            Self::Billing => "billing",
        }
    }
}

/// Absent: the original recovered after its cooldown. `FallbackUnusable`: the fallback itself could not serve.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FallbackRevertCause {
    FallbackUnusable,
}

impl FallbackRevertCause {
    pub fn as_str(self) -> &'static str {
        "fallback-unusable"
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct CircuitFailure {
    pub error_message: Option<String>,
    pub retry_after_ms: Option<f64>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FallbackSettings {
    pub model_fallback: bool,
    pub chains: BTreeMap<String, Vec<String>>,
}

/// The events a fallback controller emits (upstream `RetryFallbackControllerDeps.emit`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RetryFallbackControllerEvent {
    Applied {
        from: String,
        to: String,
        chain_key: String,
        reason: FallbackReason,
        limit: Option<UsageLimitScope>,
    },
    Reverted {
        from: String,
        to: String,
        cause: Option<FallbackRevertCause>,
    },
}

impl RetryFallbackControllerEvent {
    pub fn event_type(&self) -> &'static str {
        match self {
            Self::Applied { .. } => "retry_fallback_applied",
            Self::Reverted { .. } => "retry_fallback_reverted",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_reasons_causes_and_events_with_upstream_wire_values() {
        let reasons = [
            FallbackReason::Transient,
            FallbackReason::Refusal,
            FallbackReason::HardError,
            FallbackReason::Billing,
        ];
        assert_eq!(
            reasons.map(FallbackReason::as_str),
            ["transient", "refusal", "hard-error", "billing"]
        );
        assert_eq!(
            FallbackRevertCause::FallbackUnusable.as_str(),
            "fallback-unusable"
        );
        let applied = RetryFallbackControllerEvent::Applied {
            from: "p/a".into(),
            to: "p/b".into(),
            chain_key: "p/a".into(),
            reason: FallbackReason::Billing,
            limit: Some(UsageLimitScope::Account),
        };
        assert_eq!(applied.event_type(), "retry_fallback_applied");
        let reverted = RetryFallbackControllerEvent::Reverted {
            from: "p/b".into(),
            to: "p/a".into(),
            cause: None,
        };
        assert_eq!(reverted.event_type(), "retry_fallback_reverted");
    }
}
