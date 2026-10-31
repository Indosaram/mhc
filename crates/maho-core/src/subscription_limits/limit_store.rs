//! Port of senpi packages/coding-agent/src/core/subscription-limits/limit-store.ts.
//!
//! The latest limit windows each subscription account reported, kept for the life of the process and
//! shared by every session on it. An account is named by its credential slot - the stored account's
//! name, `env` for an environment key, `default` for a single credential - never by key material.
//! Nothing reported yet means the account is absent.
//!
//! Requests feed the store through `with_limit_capture`, which wraps a request's `on_response` so the
//! response headers are read for limit windows (Codex, Claude unified) while the caller's own hook still
//! runs.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError};

use chrono::{DateTime, SecondsFormat, Utc};

use super::limit_windows::LimitWindow;

#[derive(Debug, Clone, PartialEq)]
pub struct AccountLimits {
    pub provider: String,
    pub account: String,
    pub windows: Vec<LimitWindow>,
    /// ISO time of the response that reported them.
    pub reported_at: String,
    /// The plan the provider's usage endpoint names (`pro`, `max`, ...), when it names one.
    pub plan: Option<String>,
    /// Why the last usage poll for this account failed (`sign-in needed`, `rate limited`, ...). The
    /// windows beside it are the last ones reported; the next successful report clears it (senpi#3684).
    pub error: Option<String>,
}

pub use super::usage_poller::PollReport;

pub type LimitListener = Arc<dyn Fn(&AccountLimits) + Send + Sync>;

#[derive(Default)]
struct StoreState {
    accounts: Vec<((String, String), AccountLimits)>,
    listeners: Vec<(u64, LimitListener)>,
    next_listener: u64,
}

impl StoreState {
    fn position(&self, key: &(String, String)) -> Option<usize> {
        self.accounts
            .iter()
            .position(|(existing, _)| existing == key)
    }
}

#[derive(Default)]
pub struct SubscriptionLimitStore {
    state: Mutex<StoreState>,
}

fn iso(at: DateTime<Utc>) -> String {
    at.to_rfc3339_opts(SecondsFormat::Millis, true)
}

fn iso_ms(ms: i64) -> String {
    iso(DateTime::from_timestamp_millis(ms).unwrap_or_default())
}

impl SubscriptionLimitStore {
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> MutexGuard<'_, StoreState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Keeps the windows one response reported; a window it did not mention keeps its last value.
    pub fn record(
        &self,
        provider: &str,
        account: &str,
        windows: &[LimitWindow],
        now: DateTime<Utc>,
    ) {
        if windows.is_empty() {
            return;
        }
        let key = (provider.to_owned(), account.to_owned());
        let previous = {
            let state = self.lock();
            state
                .position(&key)
                .map(|index| state.accounts[index].1.clone())
        };
        let mut merged: Vec<LimitWindow> = previous
            .as_ref()
            .map(|p| p.windows.clone())
            .unwrap_or_default();
        for window in windows {
            match merged.iter_mut().find(|existing| existing.id == window.id) {
                Some(existing) => *existing = window.clone(),
                None => merged.push(window.clone()),
            }
        }
        self.publish(
            key,
            AccountLimits {
                provider: provider.to_owned(),
                account: account.to_owned(),
                windows: merged,
                reported_at: iso(now),
                plan: previous.and_then(|p| p.plan),
                error: None,
            },
        );
    }

    /// Keeps what a usage-endpoint poll learned (senpi#3684). The endpoint answers with the account's
    /// whole window set, so it replaces the windows rather than merging; a failed poll keeps the last
    /// windows and says why beside them. An account with no windows and no prior record stays absent on
    /// success. A reading older than what the store already holds is ignored (newest report wins).
    pub fn report(&self, provider: &str, account: &str, report: PollReport) {
        let key = (provider.to_owned(), account.to_owned());
        let previous = {
            let state = self.lock();
            state
                .position(&key)
                .map(|index| state.accounts[index].1.clone())
        };
        let reported_at = match &report {
            PollReport::Windows { reported_at_ms, .. }
            | PollReport::Error { reported_at_ms, .. } => iso_ms(*reported_at_ms),
        };
        if previous
            .as_ref()
            .is_some_and(|p| reported_at < p.reported_at)
        {
            return;
        }
        match report {
            PollReport::Error { error, .. } => {
                if previous.as_ref().and_then(|p| p.error.as_deref()) == Some(error.as_str()) {
                    return;
                }
                let mut next = previous.unwrap_or_else(|| AccountLimits {
                    provider: provider.to_owned(),
                    account: account.to_owned(),
                    windows: Vec::new(),
                    reported_at,
                    plan: None,
                    error: None,
                });
                next.error = Some(error);
                self.publish(key, next);
            }
            PollReport::Windows { windows, plan, .. } => {
                if windows.is_empty() && previous.is_none() {
                    return;
                }
                self.publish(
                    key,
                    AccountLimits {
                        provider: provider.to_owned(),
                        account: account.to_owned(),
                        windows,
                        reported_at,
                        plan,
                        error: None,
                    },
                );
            }
        }
    }

    fn publish(&self, key: (String, String), limits: AccountLimits) {
        let listeners: Vec<LimitListener> = {
            let mut state = self.lock();
            match state.position(&key) {
                Some(index) => state.accounts[index].1 = limits.clone(),
                None => state.accounts.push((key, limits.clone())),
            }
            state
                .listeners
                .iter()
                .map(|(_, listener)| Arc::clone(listener))
                .collect()
        };
        // A listener is a broadcast to clients: its failure (a closed sink) must never reach the provider
        // response this runs inside, so it is reported and contained here.
        for listener in listeners {
            if catch_unwind(AssertUnwindSafe(|| listener(&limits))).is_err() {
                eprintln!("maho subscription limits: a listener failed");
            }
        }
    }

    pub fn snapshot(&self) -> Vec<AccountLimits> {
        self.lock()
            .accounts
            .iter()
            .map(|(_, limits)| limits.clone())
            .collect()
    }

    /// Registers a listener; the returned handle id is passed to `unsubscribe`.
    pub fn subscribe(&self, listener: LimitListener) -> u64 {
        let mut state = self.lock();
        state.next_listener += 1;
        let id = state.next_listener;
        state.listeners.push((id, listener));
        id
    }

    pub fn unsubscribe(&self, id: u64) {
        self.lock()
            .listeners
            .retain(|(existing, _)| *existing != id);
    }
}

/// The process's store.
pub fn subscription_limits() -> &'static SubscriptionLimitStore {
    static STORE: OnceLock<SubscriptionLimitStore> = OnceLock::new();
    STORE.get_or_init(SubscriptionLimitStore::new)
}

/// The account a request ran under, as the store names it.
pub fn limit_account_of(slot_name: Option<&str>, api_key: Option<&str>) -> String {
    match (slot_name, api_key) {
        (Some(name), _) => name.to_owned(),
        (None, Some(_)) => "env".to_owned(),
        (None, None) => "default".to_owned(),
    }
}

/// Request options whose `on_response` also records the windows the response headers carry, into the
/// process's store.
pub fn with_limit_capture(
    options: maho_ai::types::ProviderRequestOptions,
    provider: &str,
    account: &str,
) -> maho_ai::types::ProviderRequestOptions {
    with_limit_capture_in(options, provider, account, subscription_limits())
}

/// `with_limit_capture` against a chosen store (upstream's `store` parameter).
pub fn with_limit_capture_in(
    mut options: maho_ai::types::ProviderRequestOptions,
    provider: &str,
    account: &str,
    store: &'static SubscriptionLimitStore,
) -> maho_ai::types::ProviderRequestOptions {
    let inner = options.on_response.take();
    let (provider, account) = (provider.to_owned(), account.to_owned());
    options.on_response = Some(Arc::new(move |response, model| {
        let headers: std::collections::HashMap<String, String> = response
            .headers
            .iter()
            .map(|(name, value)| (name.clone(), value.clone()))
            .collect();
        let windows = [
            super::limit_windows::codex_windows_from_headers(&headers),
            super::limit_windows::claude_windows_from_headers(&headers),
        ]
        .concat();
        store.record(&provider, &account, &windows, Utc::now());
        if let Some(inner) = &inner {
            inner(response, model);
        }
    }));
    options
}

#[cfg(test)]
mod tests {
    use super::*;

    fn window(id: &str, used: f64) -> LimitWindow {
        LimitWindow {
            id: id.to_owned(),
            label: id.to_owned(),
            window_minutes: None,
            used_percent: used,
            resets_at: None,
        }
    }
    fn at(secs: i64) -> i64 {
        secs * 1000
    }

    #[test]
    fn report_replaces_windows_and_carries_plan_through_record() {
        let store = SubscriptionLimitStore::new();
        store.report(
            "p",
            "a",
            PollReport::Windows {
                windows: vec![window("x", 1.0), window("y", 2.0)],
                plan: Some("max".into()),
                reported_at_ms: at(100),
            },
        );
        store.report(
            "p",
            "a",
            PollReport::Windows {
                windows: vec![window("x", 5.0)],
                plan: Some("max".into()),
                reported_at_ms: at(200),
            },
        );
        assert_eq!(store.snapshot()[0].windows.len(), 1);
        store.record(
            "p",
            "a",
            &[window("z", 3.0)],
            DateTime::from_timestamp(300, 0).unwrap_or_default(),
        );
        let limits = &store.snapshot()[0];
        assert_eq!(limits.plan.as_deref(), Some("max"));
        assert_eq!(limits.windows.len(), 2);
    }

    #[test]
    fn older_report_is_ignored_and_empty_first_report_stays_absent() {
        let store = SubscriptionLimitStore::new();
        store.report(
            "p",
            "a",
            PollReport::Windows {
                windows: vec![],
                plan: None,
                reported_at_ms: at(100),
            },
        );
        assert!(store.snapshot().is_empty());
        store.report(
            "p",
            "a",
            PollReport::Windows {
                windows: vec![window("x", 9.0)],
                plan: None,
                reported_at_ms: at(200),
            },
        );
        store.report(
            "p",
            "a",
            PollReport::Windows {
                windows: vec![window("x", 1.0)],
                plan: None,
                reported_at_ms: at(100),
            },
        );
        assert_eq!(store.snapshot()[0].windows[0].used_percent, 9.0);
    }

    #[test]
    fn error_report_keeps_windows_and_dedupes_the_same_error() {
        let store = SubscriptionLimitStore::new();
        let calls = Arc::new(Mutex::new(0));
        let seen = Arc::clone(&calls);
        store.subscribe(Arc::new(move |_| {
            *seen.lock().unwrap_or_else(PoisonError::into_inner) += 1
        }));
        store.report(
            "p",
            "a",
            PollReport::Windows {
                windows: vec![window("x", 9.0)],
                plan: None,
                reported_at_ms: at(100),
            },
        );
        store.report(
            "p",
            "a",
            PollReport::Error {
                error: "sign-in needed".into(),
                reported_at_ms: at(200),
            },
        );
        store.report(
            "p",
            "a",
            PollReport::Error {
                error: "sign-in needed".into(),
                reported_at_ms: at(300),
            },
        );
        let limits = &store.snapshot()[0];
        assert_eq!(limits.error.as_deref(), Some("sign-in needed"));
        assert_eq!(limits.windows.len(), 1);
        assert_eq!(*calls.lock().unwrap_or_else(PoisonError::into_inner), 2);
    }

    #[test]
    fn a_panicking_listener_is_contained() {
        let store = SubscriptionLimitStore::new();
        store.subscribe(Arc::new(|_| panic!("closed sink")));
        store.record(
            "p",
            "a",
            &[window("x", 1.0)],
            DateTime::from_timestamp(1, 0).unwrap_or_default(),
        );
        assert_eq!(store.snapshot().len(), 1);
    }

    fn codex_fixture_headers() -> std::collections::BTreeMap<String, String> {
        // senpi test/fixtures/subscription-limits/codex-response-headers.json.
        [
            ("x-codex-credits-balance", "1000.0"),
            ("x-codex-credits-has-credits", "True"),
            ("x-codex-credits-unlimited", "False"),
            ("x-codex-primary-over-secondary-limit-percent", "0"),
            ("x-codex-primary-reset-after-seconds", "523382"),
            ("x-codex-primary-reset-at", "1792555751"),
            ("x-codex-primary-used-percent", "99"),
            ("x-codex-primary-window-minutes", "10080"),
            ("x-codex-secondary-reset-after-seconds", "0"),
            ("x-codex-secondary-reset-at", ""),
            ("x-codex-secondary-used-percent", "0"),
            ("x-codex-secondary-window-minutes", "0"),
        ]
        .into_iter()
        .map(|(name, value)| (name.to_owned(), value.to_owned()))
        .collect()
    }

    /// senpi#3370: the windows a response reports are kept per provider and slot, and the caller's own
    /// `on_response` still runs.
    #[test]
    fn limit_capture_records_the_response_windows_and_runs_the_callers_hook() {
        static STORE: OnceLock<SubscriptionLimitStore> = OnceLock::new();
        let store = STORE.get_or_init(SubscriptionLimitStore::new);
        let seen = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&seen);
        let options = with_limit_capture_in(
            maho_ai::types::ProviderRequestOptions {
                on_response: Some(Arc::new(move |response, _| {
                    sink.lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .push(response.status);
                })),
                ..Default::default()
            },
            "chatgpt-subscription",
            &limit_account_of(Some("work"), None),
            store,
        );
        let model = maho_ai::providers::faux::faux_provider(Default::default())
            .expect("faux provider")
            .get_model(None)
            .expect("model");
        options.on_response.as_ref().expect("hook")(
            &maho_ai::types::ProviderResponse {
                status: 200,
                headers: codex_fixture_headers(),
            },
            &model,
        );
        assert_eq!(
            *seen.lock().unwrap_or_else(PoisonError::into_inner),
            vec![200]
        );
        let snapshot = store.snapshot();
        assert_eq!(snapshot.len(), 1);
        assert_eq!(
            (snapshot[0].provider.as_str(), snapshot[0].account.as_str()),
            ("chatgpt-subscription", "work")
        );
        assert_eq!(snapshot[0].windows.len(), 1);
        let weekly = &snapshot[0].windows[0];
        assert_eq!(
            (
                weekly.id.as_str(),
                weekly.label.as_str(),
                weekly.window_minutes,
                weekly.used_percent
            ),
            ("codex:primary", "7d", Some(10080), 99.0)
        );
        assert_eq!(
            [
                limit_account_of(None, None),
                limit_account_of(None, Some("k"))
            ],
            ["default".to_owned(), "env".to_owned()]
        );
    }
}
