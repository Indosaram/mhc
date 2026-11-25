//! Port of senpi packages/coding-agent/src/core/retry-fallback/unusable.ts.

use std::collections::HashMap;
use std::sync::LazyLock;

use maho_ai::types::{Model, UsageLimitScope};
use regex::{Regex, RegexBuilder};

use super::billing::is_billing_error_message;
use super::chains::format_selector;
use super::circuit_probes::is_health_exhaustion_failure;
use super::cooldown::{SelectorCooldowns, SelectorFailure};
use super::usage_limit::usage_limit_scope_text;
use crate::credential_pool::model_scope::rate_limit_model_family;
use crate::subscription_limits::limit_store::AccountLimits;
use crate::subscription_limits::usage_capacity::accounts_with_capacity;

static WEEKLY_LIMIT: LazyLock<Option<Regex>> = LazyLock::new(|| {
    RegexBuilder::new(r"\bweekly\b[^.]*\blimit\b")
        .case_insensitive(true)
        .build()
        .ok()
});

static MODEL_FAMILY: LazyLock<Option<Regex>> = LazyLock::new(|| {
    RegexBuilder::new(r"(?:^|[^a-z])(opus|sonnet|haiku|fable|mythos)(?:[^a-z]|$)")
        .case_insensitive(true)
        .build()
        .ok()
});

fn model_family_of(model_id: &str) -> Option<String> {
    MODEL_FAMILY
        .as_ref()?
        .captures(model_id)?
        .get(1)
        .map(|m| m.as_str().to_ascii_lowercase())
}

/// A once-per-provider check of a limit that named no family (senpi#5596).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsageLimitScopeProbe {
    pub provider: String,
    pub held_family: String,
    pub probe_family: String,
    pub outcome: UsageLimitScopeProbeOutcome,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UsageLimitScopeProbeOutcome {
    Served,
    Widened,
}

impl UsageLimitScopeProbeOutcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Served => "served",
            Self::Widened => "widened",
        }
    }
}

#[derive(Debug, Default)]
pub struct UnusableExtraState {
    pub account_held_since: HashMap<String, f64>,
    pub pending_probes: HashMap<String, String>,
    pub widened_families: HashMap<String, Vec<String>>,
    pub family_errors: HashMap<String, String>,
}

/// Chain entries this session has seen refuse to serve for a reason retrying cannot fix within the
/// cooldown: an account-wide usage limit or billing failure (every model on that provider is out) or a
/// model-scoped limit/exhaustion (that entry is out). Backed by the session's selector cooldowns under
/// namespaced keys, so it is runtime-only and expires on the same injected clock.
pub struct UnusableEntries<'a> {
    cooldowns: &'a mut SelectorCooldowns,
    account_errors: &'a mut HashMap<String, String>,
    extra: Option<&'a mut UnusableExtraState>,
}

impl<'a> UnusableEntries<'a> {
    pub fn new(
        cooldowns: &'a mut SelectorCooldowns,
        account_errors: &'a mut HashMap<String, String>,
    ) -> Self {
        Self {
            cooldowns,
            account_errors,
            extra: None,
        }
    }

    pub fn with_state(
        cooldowns: &'a mut SelectorCooldowns,
        account_errors: &'a mut HashMap<String, String>,
        extra: &'a mut UnusableExtraState,
    ) -> Self {
        Self {
            cooldowns,
            account_errors,
            extra: Some(extra),
        }
    }

    pub fn note(&mut self, model: &Model, failure: SelectorFailure<'_>) {
        let _ = self.note_with_probe(model, failure);
    }

    pub fn note_with_probe(
        &mut self,
        model: &Model,
        failure: SelectorFailure<'_>,
    ) -> Option<UsageLimitScopeProbe> {
        let scope = usage_limit_scope_text(failure.error_message);
        let family = model_family_of(&model.id);
        if scope == Some(UsageLimitScope::Account)
            && let Some(family) = family
            && let Some(error_message) = failure.error_message
            && WEEKLY_LIMIT
                .as_ref()
                .is_some_and(|re| re.is_match(error_message))
            && !is_billing_error_message(Some(error_message))
            && rate_limit_model_family(error_message).is_none()
            && let Some(extra) = self.extra.as_deref_mut()
        {
            let held = extra.pending_probes.get(&model.provider).cloned();
            if held.is_none() || held.as_deref() == Some(family.as_str()) {
                let key = family_key(&model.provider, &family);
                extra.pending_probes.insert(model.provider.clone(), family);
                self.cooldowns.note(&key, failure);
                extra.family_errors.insert(key, error_message.to_owned());
                return None;
            }
            if let Some(held) = held {
                extra.pending_probes.remove(&model.provider);
                extra
                    .widened_families
                    .insert(model.provider.clone(), vec![held.clone(), family.clone()]);
                self.note_account(&model.provider, failure);
                self.account_errors
                    .insert(model.provider.clone(), error_message.to_owned());
                return Some(UsageLimitScopeProbe {
                    provider: model.provider.clone(),
                    held_family: held,
                    probe_family: family,
                    outcome: UsageLimitScopeProbeOutcome::Widened,
                });
            }
        }
        match scope {
            Some(UsageLimitScope::Account) => {
                self.note_account(&model.provider, failure);
                if let Some(message) = failure.error_message.filter(|message| !message.is_empty()) {
                    self.account_errors
                        .insert(model.provider.clone(), message.to_owned());
                }
            }
            Some(UsageLimitScope::Model) => self.cooldowns.note(&entry_key(model), failure),
            None if is_health_exhaustion_failure(failure.error_message) => {
                self.cooldowns.note(&entry_key(model), failure);
            }
            None => {}
        }
        None
    }

    fn note_account(&mut self, provider: &str, failure: SelectorFailure<'_>) {
        self.cooldowns.note(&account_key(provider), failure);
        let wall_now = self.cooldowns.wall_clock();
        if let Some(extra) = self.extra.as_deref_mut() {
            extra
                .account_held_since
                .insert(provider.to_owned(), wall_now);
        }
    }

    /// A response served on `model`: a pending family probe on another family resolves as served.
    pub fn note_served(&mut self, provider: &str, model_id: &str) -> Option<UsageLimitScopeProbe> {
        let extra = self.extra.as_deref_mut()?;
        let held = extra.pending_probes.get(provider)?.clone();
        let family = model_family_of(model_id)?;
        if family == held {
            return None;
        }
        extra.pending_probes.remove(provider);
        Some(UsageLimitScopeProbe {
            provider: provider.to_owned(),
            held_family: held,
            probe_family: family,
            outcome: UsageLimitScopeProbeOutcome::Served,
        })
    }

    /// The weekly-limit family hold on `model`, while it lasts (senpi#5596).
    pub fn held_family(&mut self, model: &Model) -> Option<(String, String)> {
        let family = model_family_of(&model.id)?;
        let key = family_key(&model.provider, &family);
        let error_message = self
            .extra
            .as_deref()
            .and_then(|extra| extra.family_errors.get(&key).cloned())?;
        self.cooldowns
            .is_suppressed(&key)
            .then_some((family, error_message))
    }

    /// The two families a no-family limit was confirmed on before it widened to the account.
    pub fn widened_families_of(&mut self, provider: &str) -> Option<Vec<String>> {
        if !self.is_provider_spent(provider) {
            return None;
        }
        self.extra
            .as_deref()
            .and_then(|extra| extra.widened_families.get(provider).cloned())
    }

    pub fn is_provider_spent(&mut self, provider: &str) -> bool {
        self.cooldowns.is_suppressed(&account_key(provider))
    }

    pub fn account_error(&mut self, provider: &str) -> Option<String> {
        if self.is_provider_spent(provider) {
            self.account_errors.get(provider).cloned()
        } else {
            None
        }
    }

    /// Wall-clock time the provider said its account limit resets; `None` when the hold is a guessed default.
    pub fn account_reset_at(&mut self, provider: &str) -> Option<f64> {
        self.cooldowns.stated_reset_at(&account_key(provider))
    }

    /// Drops an account hold whose length was a guessed default, so the next request asks the provider
    /// again (senpi#5568). Returns whether a hold was dropped.
    pub fn release_guessed_account_hold(&mut self, provider: &str) -> bool {
        if self.account_reset_at(provider).is_some() {
            return false;
        }
        self.release_account_hold(provider)
    }

    /// Drops the provider's account hold when usage reported after the hold began shows it can serve
    /// again (senpi#6536). Returns whether one was dropped.
    pub fn release_account_hold_shown_servable(
        &mut self,
        provider: &str,
        usage: &[AccountLimits],
    ) -> bool {
        let since = self
            .extra
            .as_deref()
            .and_then(|extra| extra.account_held_since.get(provider).copied());
        let Some(since) = since else {
            return false;
        };
        if !self.is_provider_spent(provider) {
            return false;
        }
        let wall_now = self.cooldowns.wall_clock() as i64;
        if accounts_with_capacity(usage, provider, since as i64, wall_now).is_empty() {
            return false;
        }
        self.release_account_hold(provider)
    }

    /// Drops the provider's account hold, whatever its length; returns whether one was held (senpi#5590).
    pub fn release_account_hold(&mut self, provider: &str) -> bool {
        if !self.is_provider_spent(provider) {
            return false;
        }
        self.cooldowns.clear(&account_key(provider));
        self.account_errors.remove(provider);
        if let Some(extra) = self.extra.as_deref_mut() {
            extra.account_held_since.remove(provider);
        }
        self.clear_family_holds(provider);
        true
    }

    pub fn is_unusable(&mut self, model: &Model) -> bool {
        let family_suppressed = model_family_of(&model.id).is_some_and(|family| {
            self.cooldowns
                .is_suppressed(&family_key(&model.provider, &family))
        });
        self.is_provider_spent(&model.provider)
            || self.cooldowns.is_suppressed(&entry_key(model))
            || family_suppressed
    }

    /// Every account noted spent, with the deadline its cooldown lifts at and the raw error that spent it.
    /// An account-scoped note always carries a non-empty error, so `account_errors` names every spent account.
    pub fn spent_accounts(&self) -> SpentAccounts {
        SpentAccounts {
            by_provider: self
                .account_errors
                .iter()
                .filter_map(|(provider, error)| {
                    self.cooldowns
                        .expires_at(&account_key(provider))
                        .map(|expires_at| (provider.clone(), (expires_at, error.clone())))
                })
                .collect(),
        }
    }

    /// A deliberate model selection allows the user to probe after reauth/reset.
    pub fn clear(&mut self, model: &Model) {
        self.cooldowns.clear(&entry_key(model));
        self.cooldowns.clear(&account_key(&model.provider));
        self.account_errors.remove(&model.provider);
        if let Some(extra) = self.extra.as_deref_mut() {
            extra.account_held_since.remove(&model.provider);
        }
        self.clear_family_holds(&model.provider);
    }

    /// The provider's accounts changed (a login, an added or re-authorized account): every hold it earned is stale.
    pub fn clear_provider(&mut self, provider: &str, models: &[Model]) {
        self.cooldowns.clear(&account_key(provider));
        self.account_errors.remove(provider);
        if let Some(extra) = self.extra.as_deref_mut() {
            extra.account_held_since.remove(provider);
        }
        for model in models {
            if model.provider == provider {
                self.cooldowns.clear(&entry_key(model));
            }
        }
        self.clear_family_holds(provider);
    }

    fn clear_family_holds(&mut self, provider: &str) {
        let Some(extra) = self.extra.as_deref_mut() else {
            return;
        };
        let prefix = family_key(provider, "");
        let keys: Vec<String> = extra
            .family_errors
            .keys()
            .filter(|k| k.starts_with(&prefix))
            .cloned()
            .collect();
        for key in keys {
            self.cooldowns.clear(&key);
            extra.family_errors.remove(&key);
        }
        extra.pending_probes.remove(provider);
        extra.widened_families.remove(provider);
    }
}

/// A copy of the spent accounts taken where the controller records or clears them, so a reader that cannot
/// await the controller lock still sees every spent account. Deadlines are on the session's cooldown clock.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SpentAccounts {
    by_provider: HashMap<String, (f64, String)>,
}

impl SpentAccounts {
    /// senpi `isProviderSpent` at `now`.
    pub fn is_spent(&self, provider: &str, now: f64) -> bool {
        self.by_provider
            .get(provider)
            .is_some_and(|(expires_at, _)| now < *expires_at)
    }

    /// senpi `accountError` at `now`: the raw error that spent a still-spent provider.
    pub fn account_error(&self, provider: &str, now: f64) -> Option<String> {
        self.by_provider
            .get(provider)
            .filter(|(expires_at, _)| now < *expires_at)
            .map(|(_, error)| error.clone())
    }
}

/// A provider-name approximation, not resolved credential/account identity.
fn account_key(provider: &str) -> String {
    format!("account:{provider}")
}

fn entry_key(model: &Model) -> String {
    format!("exhausted:{}", format_selector(model, None))
}

fn family_key(provider: &str, family: &str) -> String {
    format!("family:{provider}:{family}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn model(provider: &str, id: &str) -> Model {
        let mut model = maho_ai::providers::all::get_builtin_models("openai").remove(0);
        model.provider = provider.into();
        model.id = id.into();
        model
    }

    fn failure(message: &str) -> SelectorFailure<'_> {
        SelectorFailure {
            retry_after_ms: None,
            error_message: Some(message),
        }
    }

    #[test]
    fn spends_the_whole_provider_on_an_account_limit_and_one_entry_on_a_model_limit() {
        let clock = Arc::new(AtomicU64::new(0));
        let now = clock.clone();
        let Ok(mut cooldowns) =
            SelectorCooldowns::new(move || now.load(Ordering::SeqCst) as f64, || 0.0)
        else {
            panic!("cooldowns")
        };
        let mut errors = HashMap::new();
        let mut unusable = UnusableEntries::new(&mut cooldowns, &mut errors);
        let (a, b, other) = (model("p", "a"), model("p", "b"), model("q", "c"));
        unusable.note(&a, failure("You've hit your session limit"));
        assert!(unusable.is_unusable(&b));
        assert_eq!(
            unusable.account_error("p").as_deref(),
            Some("You've hit your session limit")
        );
        assert!(!unusable.is_unusable(&other));
        unusable.clear(&a);
        assert!(!unusable.is_unusable(&b));
        unusable.note(
            &other,
            failure("Premium models are not included: usage limit reached"),
        );
        assert!(unusable.is_unusable(&other));
        assert!(!unusable.is_provider_spent("q"));
        unusable.note(&a, failure("429 rate limit exceeded"));
        assert!(!unusable.is_unusable(&a));
    }

    #[test]
    fn spent_accounts_snapshot_names_each_spent_account_until_its_cooldown_lifts() {
        let Ok(mut cooldowns) = SelectorCooldowns::new(|| 0.0, || 0.0) else {
            panic!("cooldowns")
        };
        let mut errors = HashMap::new();
        let mut unusable = UnusableEntries::new(&mut cooldowns, &mut errors);
        let (a, other) = (model("p", "a"), model("q", "c"));
        unusable.note(&a, failure("You've hit your session limit"));
        unusable.note(
            &other,
            failure("Premium models are not included: usage limit reached"),
        );
        let spent = unusable.spent_accounts();
        assert!(spent.is_spent("p", 0.0));
        assert_eq!(
            spent.account_error("p", 0.0).as_deref(),
            Some("You've hit your session limit")
        );
        assert!(!spent.is_spent("q", 0.0));
        assert!(!spent.is_spent("p", 1_800_000.0));
        assert_eq!(spent.account_error("p", 1_800_000.0), None);
        unusable.clear(&a);
        assert_eq!(unusable.spent_accounts(), SpentAccounts::default());
    }
}
