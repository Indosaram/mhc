//! Port of senpi packages/coding-agent/src/core/retry-fallback/unusable.ts.

use std::collections::HashMap;

use maho_ai::types::{Model, UsageLimitScope};

use super::chains::format_selector;
use super::circuit_probes::is_health_exhaustion_failure;
use super::cooldown::{SelectorCooldowns, SelectorFailure};
use super::usage_limit::usage_limit_scope_text;

/// Chain entries this session has seen refuse to serve for a reason retrying cannot fix within the
/// cooldown: an account-wide usage limit or billing failure (every model on that provider is out) or a
/// model-scoped limit/exhaustion (that entry is out). Backed by the session's selector cooldowns under
/// namespaced keys, so it is runtime-only and expires on the same injected clock.
pub struct UnusableEntries<'a> {
    cooldowns: &'a mut SelectorCooldowns,
    account_errors: &'a mut HashMap<String, String>,
}

impl<'a> UnusableEntries<'a> {
    pub fn new(
        cooldowns: &'a mut SelectorCooldowns,
        account_errors: &'a mut HashMap<String, String>,
    ) -> Self {
        Self {
            cooldowns,
            account_errors,
        }
    }

    pub fn note(&mut self, model: &Model, failure: SelectorFailure<'_>) {
        match usage_limit_scope_text(failure.error_message) {
            Some(UsageLimitScope::Account) => {
                self.cooldowns.note(&account_key(&model.provider), failure);
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

    pub fn is_unusable(&mut self, model: &Model) -> bool {
        self.is_provider_spent(&model.provider) || self.cooldowns.is_suppressed(&entry_key(model))
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
