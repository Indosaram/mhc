//! Port of senpi `packages/coding-agent/src/core/retry-fallback/credential-recovery.ts`.

use std::collections::HashMap;

use maho_ai::types::Model;

use super::chains::format_selector;
use super::controller::{
    ActiveFallbackState, EpisodeRecoveryMeta, FallbackLogLevel, RetryFallbackDeps,
};
use super::cooldown::SelectorCooldowns;
use super::unusable::{UnusableEntries, UnusableExtraState};

/// A billing pin assumes the original's account cannot recover by retrying it. A sign-in changes
/// that account, so when the original provider's login id moved since the episode began, the
/// billing pin and the original's cooldown, unusable mark and circuit are released and the
/// existing revert path tries it once (senpi#4099).
pub fn release_on_credential_change<D: RetryFallbackDeps>(
    state: &mut ActiveFallbackState,
    meta: &mut EpisodeRecoveryMeta,
    original: &Model,
    deps: &mut D,
    cooldowns: &mut SelectorCooldowns,
    account_errors: &mut HashMap<String, String>,
    unusable_state: &mut UnusableExtraState,
) {
    if !state.pinned_by_billing {
        return;
    }
    let Some(recorded) = meta.recovery_revision.as_deref() else {
        return;
    };
    let Some(revision) = deps.recovery_revision(&original.provider) else {
        return;
    };
    if revision.is_empty() || revision == recorded {
        return;
    }
    meta.recovery_revision = Some(revision);
    state.pinned_by_billing = false;
    state.pinned = state.pinned_by_refusal;
    let selector = format_selector(original, None);
    cooldowns.clear_model(&selector);
    UnusableEntries::with_state(cooldowns, account_errors, unusable_state).clear(original);
    deps.close_circuit(&selector);
    deps.log(
        FallbackLogLevel::Info,
        "credential_recovery_released",
        &[
            ("chainKey", Some(state.chain_key.as_str())),
            ("originalSelector", Some(state.original_selector.as_str())),
            ("trigger", Some("sign-in")),
        ],
    );
}

/// A billing pin a usage limit caused holds only until the reset that limit stated: the account
/// serves again then, so the pin and the original's cooldown, unusable mark and circuit are
/// released and the existing revert path tries it once, with no re-login (senpi#4085).
pub fn release_on_limit_reset<D: RetryFallbackDeps>(
    state: &mut ActiveFallbackState,
    meta: &mut EpisodeRecoveryMeta,
    original: &Model,
    deps: &mut D,
    cooldowns: &mut SelectorCooldowns,
    account_errors: &mut HashMap<String, String>,
    unusable_state: &mut UnusableExtraState,
) {
    if !state.pinned_by_billing {
        return;
    }
    let Some(until) = meta.pinned_until else {
        return;
    };
    if cooldowns.wall_clock() < until {
        return;
    }
    state.pinned_by_billing = false;
    meta.pinned_until = None;
    state.pinned = state.pinned_by_refusal;
    let selector = format_selector(original, None);
    cooldowns.clear_model(&selector);
    UnusableEntries::with_state(cooldowns, account_errors, unusable_state).clear(original);
    deps.close_circuit(&selector);
    deps.log(
        FallbackLogLevel::Info,
        "credential_recovery_released",
        &[
            ("chainKey", Some(state.chain_key.as_str())),
            ("originalSelector", Some(state.original_selector.as_str())),
            ("trigger", Some("limit-reset")),
        ],
    );
}
