//! Port of senpi `packages/coding-agent/src/core/retry-fallback/pin.ts`.

use maho_ai::types::UsageLimitScope;

use super::controller::FallbackReason;

/// `Pick<ActiveFallbackState, "originalSelector" | "pinnedByRefusal" | "pinnedByBilling" | "pinned">`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PinProvenance {
    pub original_selector: String,
    pub pinned_by_refusal: bool,
    pub pinned_by_billing: bool,
    pub pinned: bool,
}

pub struct FallbackSwitch<'a> {
    pub from: &'a str,
    pub from_provider: &'a str,
    pub reason: FallbackReason,
    pub limit: Option<UsageLimitScope>,
}

/// Pin provenance after one fallback switch away from `from`. A refusal pins (the same context refuses
/// again). Billing pins only when the ORIGINAL cannot recover: the failure hit that model, or the whole
/// account the original lives on. Billing on a fallback target from another account says nothing about
/// the original, so it must not hold the session there (senpi#2376).
pub fn pin_after_switch(
    prior: Option<&PinProvenance>,
    switch: &FallbackSwitch<'_>,
) -> PinProvenance {
    let original_selector =
        prior.map_or_else(|| switch.from.to_owned(), |p| p.original_selector.clone());
    let billing_hits_original = switch.reason == FallbackReason::Billing
        && (switch.from == original_selector
            || (switch.limit == Some(UsageLimitScope::Account)
                && original_selector.starts_with(&format!("{}/", switch.from_provider))));
    let pinned_by_refusal =
        prior.is_some_and(|p| p.pinned_by_refusal) || switch.reason == FallbackReason::Refusal;
    let pinned_by_billing = prior.is_some_and(|p| p.pinned_by_billing) || billing_hits_original;
    PinProvenance {
        original_selector,
        pinned_by_refusal,
        pinned_by_billing,
        pinned: pinned_by_refusal || pinned_by_billing,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn switch(
        from: &str,
        reason: FallbackReason,
        limit: Option<UsageLimitScope>,
    ) -> FallbackSwitch<'_> {
        FallbackSwitch {
            from,
            from_provider: from.split('/').next().unwrap_or_default(),
            reason,
            limit,
        }
    }

    #[test]
    fn billing_on_the_original_pins() {
        let pin = pin_after_switch(None, &switch("a/m1", FallbackReason::Billing, None));
        assert!(pin.pinned_by_billing && pin.pinned);
        assert_eq!(pin.original_selector, "a/m1");
    }

    #[test]
    fn billing_on_a_fallback_from_another_account_does_not_pin() {
        let prior = pin_after_switch(None, &switch("a/m1", FallbackReason::Transient, None));
        assert!(!prior.pinned);
        let next = pin_after_switch(
            Some(&prior),
            &switch(
                "b/m2",
                FallbackReason::Billing,
                Some(UsageLimitScope::Account),
            ),
        );
        assert!(!next.pinned_by_billing && !next.pinned);
        assert_eq!(next.original_selector, "a/m1");
    }

    #[test]
    fn account_billing_on_the_original_provider_pins() {
        let prior = pin_after_switch(None, &switch("a/m1", FallbackReason::Transient, None));
        let next = pin_after_switch(
            Some(&prior),
            &switch(
                "a/m2",
                FallbackReason::Billing,
                Some(UsageLimitScope::Account),
            ),
        );
        assert!(next.pinned_by_billing);
        let model_only = pin_after_switch(
            Some(&prior),
            &switch(
                "a/m2",
                FallbackReason::Billing,
                Some(UsageLimitScope::Model),
            ),
        );
        assert!(!model_only.pinned_by_billing);
    }

    #[test]
    fn refusal_pins_and_provenance_accumulates() {
        let refusal = pin_after_switch(None, &switch("a/m1", FallbackReason::Refusal, None));
        assert!(refusal.pinned_by_refusal && refusal.pinned);
        let next = pin_after_switch(
            Some(&refusal),
            &switch("b/m2", FallbackReason::HardError, None),
        );
        assert!(next.pinned_by_refusal && next.pinned);
    }
}
