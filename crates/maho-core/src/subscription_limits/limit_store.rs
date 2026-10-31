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

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountLimits {
    pub provider: String,
    pub account: String,
    pub windows: Vec<LimitWindow>,
    /// ISO time of the response that reported them.
    pub reported_at: String,
    /// The plan the provider's usage endpoint names (`pro`, `max`, ...), when it names one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan: Option<String>,
    /// Why the last usage poll for this account failed (`sign-in needed`, `rate limited`, ...). The
    /// windows beside it are the last ones reported; the next successful report clears it (senpi#3684).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// A record as the stores of one process's isolates mirror it to each other (senpi#3695). `updated_at` is when
/// the record last really changed, set only when that differs from `reported_at` (a failed poll adds an error
/// beside the last windows, which keep their time). With `revision` it orders mirrored copies; neither leaves the
/// mirror: `snapshot()` and `subscribe()` hand out the public shape. Wire shape: upstream camelCase.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MirroredLimits {
    #[serde(flatten)]
    pub limits: AccountLimits,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
    /// How many local writes landed at the same `updated_at ?? reported_at` instant before this one, so a
    /// same-instant write still orders after the record it replaces. Absent means 0. The time stays the real
    /// change time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision: Option<u64>,
}

pub use super::usage_poller::PollReport;

pub type LimitListener = Arc<dyn Fn(&AccountLimits) + Send + Sync>;
pub type MirrorListener = Arc<dyn Fn(&MirroredLimits) + Send + Sync>;

#[derive(Default)]
struct StoreState {
    accounts: Vec<((String, String), MirroredLimits)>,
    listeners: Vec<(u64, LimitListener)>,
    mirror_listeners: Vec<(u64, MirrorListener)>,
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
            .map(|p| p.limits.windows.clone())
            .unwrap_or_default();
        for window in windows {
            match merged.iter_mut().find(|existing| existing.id == window.id) {
                Some(existing) => *existing = window.clone(),
                None => merged.push(window.clone()),
            }
        }
        let plan = previous.as_ref().and_then(|p| p.limits.plan.clone());
        let limits = AccountLimits {
            provider: provider.to_owned(),
            account: account.to_owned(),
            windows: merged,
            reported_at: iso(now),
            plan,
            error: None,
        };
        let changed_at = limits.reported_at.clone();
        self.write(key, previous.as_ref(), limits, changed_at);
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
            .is_some_and(|p| reported_at.as_str() < changed_at_of(p))
        {
            return;
        }
        match report {
            PollReport::Error { error, .. } => {
                if previous.as_ref().and_then(|p| p.limits.error.as_deref()) == Some(error.as_str())
                {
                    return;
                }
                let mut next = previous
                    .as_ref()
                    .map(|p| p.limits.clone())
                    .unwrap_or_else(|| AccountLimits {
                        provider: provider.to_owned(),
                        account: account.to_owned(),
                        windows: Vec::new(),
                        reported_at: reported_at.clone(),
                        plan: None,
                        error: None,
                    });
                next.error = Some(error);
                self.write(key, previous.as_ref(), next, reported_at);
            }
            PollReport::Windows { windows, plan, .. } => {
                if windows.is_empty() && previous.is_none() {
                    return;
                }
                let limits = AccountLimits {
                    provider: provider.to_owned(),
                    account: account.to_owned(),
                    windows,
                    reported_at: reported_at.clone(),
                    plan,
                    error: None,
                };
                self.write(key, previous.as_ref(), limits, reported_at);
            }
        }
    }

    /// Takes another isolate's record as it stands (senpi#3695: the host and its session workers each hold a
    /// store and mirror each other). It goes through the same total order as every local write, so whatever
    /// order the copies cross in, every store keeps the same record and the echo ends when a copy comes back.
    pub fn adopt(&self, limits: MirroredLimits) {
        let key = (
            limits.limits.provider.clone(),
            limits.limits.account.clone(),
        );
        self.offer(key, limits);
    }

    /// A write from this store's own turn or poll always lands: it orders strictly after the held record. A later
    /// change time does that by time; at the same instant (or a clock that reads earlier) the time stays the held
    /// record's real change time and `revision` counts up, so a mirrored store takes it by the same total order.
    fn write(
        &self,
        key: (String, String),
        previous: Option<&MirroredLimits>,
        limits: AccountLimits,
        changed_at: String,
    ) {
        let held = previous.map(|p| changed_at_of(p).to_owned());
        let later = held
            .as_ref()
            .is_none_or(|held| changed_at.as_str() > held.as_str());
        let time = if later {
            changed_at
        } else {
            held.unwrap_or_default()
        };
        let revision = if later {
            0
        } else {
            previous.and_then(|p| p.revision).unwrap_or(0) + 1
        };
        let updated_at = (time != limits.reported_at).then_some(time);
        self.publish(
            key,
            MirroredLimits {
                limits,
                updated_at,
                revision: (revision != 0).then_some(revision),
            },
        );
    }

    /// The mirror's rule (senpi#3695): a copy replaces the held record only when it is strictly later in the
    /// total order (change time, then revision, then content), so two stores that see the same records keep the
    /// same one.
    fn offer(&self, key: (String, String), limits: MirroredLimits) {
        let previous = {
            let state = self.lock();
            state
                .position(&key)
                .map(|index| state.accounts[index].1.clone())
        };
        let Some(previous) = previous else {
            self.publish(key, limits);
            return;
        };
        // Two isolates that each wrote locally from the same held record at the same instant carry the same
        // stamp. Neither update may be lost, so the two are combined symmetrically: both stores reach the same
        // record.
        if canonical(&limits) == canonical(&previous) {
            return;
        }
        if changed_at_of(&limits) == changed_at_of(&previous)
            && limits.revision.unwrap_or(0) == previous.revision.unwrap_or(0)
        {
            let combined = combine_concurrent(&previous, &limits);
            if canonical(&combined) != canonical(&previous) {
                self.publish(key, combined);
            }
            return;
        }
        if compare_records(&limits, &previous) != std::cmp::Ordering::Greater {
            return;
        }
        self.publish(key, limits);
    }

    fn publish(&self, key: (String, String), limits: MirroredLimits) {
        let (mirror_listeners, listeners): (Vec<MirrorListener>, Vec<LimitListener>) = {
            let mut state = self.lock();
            match state.position(&key) {
                Some(index) => state.accounts[index].1 = limits.clone(),
                None => state.accounts.push((key, limits.clone())),
            }
            (
                state
                    .mirror_listeners
                    .iter()
                    .map(|(_, listener)| Arc::clone(listener))
                    .collect(),
                state
                    .listeners
                    .iter()
                    .map(|(_, listener)| Arc::clone(listener))
                    .collect(),
            )
        };
        // A listener (a client broadcast or the isolate mirror) failing must never reach the provider response
        // this runs inside, so each failure is reported and contained here.
        for listener in mirror_listeners {
            if catch_unwind(AssertUnwindSafe(|| listener(&limits))).is_err() {
                eprintln!("maho subscription limits: a listener failed");
            }
        }
        let shown = limits.limits;
        for listener in listeners {
            if catch_unwind(AssertUnwindSafe(|| listener(&shown))).is_err() {
                eprintln!("maho subscription limits: a listener failed");
            }
        }
    }

    pub fn snapshot(&self) -> Vec<AccountLimits> {
        self.lock()
            .accounts
            .iter()
            .map(|(_, limits)| limits.limits.clone())
            .collect()
    }

    /// The records with their ordering key, for mirroring into another isolate's store.
    pub fn mirror_snapshot(&self) -> Vec<MirroredLimits> {
        self.lock()
            .accounts
            .iter()
            .map(|(_, limits)| limits.clone())
            .collect()
    }

    /// Hears every change with its ordering key, for mirroring into another isolate's store; the returned id
    /// is passed to `unsubscribe_mirror`.
    pub fn subscribe_mirror(&self, listener: MirrorListener) -> u64 {
        let mut state = self.lock();
        state.next_listener += 1;
        let id = state.next_listener;
        state.mirror_listeners.push((id, listener));
        id
    }

    pub fn unsubscribe_mirror(&self, id: u64) {
        self.lock()
            .mirror_listeners
            .retain(|(existing, _)| *existing != id);
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

/// When the record last really changed: never bumped, so a poll compares against a real time.
fn changed_at_of(limits: &MirroredLimits) -> &str {
    limits
        .updated_at
        .as_deref()
        .unwrap_or(&limits.limits.reported_at)
}

/// The total order every store applies: change time, then revision, then key-sorted content.
fn compare_records(a: &MirroredLimits, b: &MirroredLimits) -> std::cmp::Ordering {
    changed_at_of(a)
        .cmp(changed_at_of(b))
        .then_with(|| a.revision.unwrap_or(0).cmp(&b.revision.unwrap_or(0)))
        .then_with(|| canonical(a).cmp(&canonical(b)))
}

/// Combines two different records that changed at the same stamp (senpi#3695). Symmetric, so both isolates
/// land on the same record.
///
/// Windows and plan come from the side whose `reported_at` is later: that is when its windows were really
/// reported. Only when `reported_at` also ties are the windows the union by id (on the same id the window that
/// sorts later wins), and the plan the value that sorts later. The error is kept when either side set it (the
/// value that sorts later when both did): a success at the same stamp does not clear the other side's error.
fn combine_concurrent(a: &MirroredLimits, b: &MirroredLimits) -> MirroredLimits {
    let error = later(a.limits.error.as_ref(), b.limits.error.as_ref()).cloned();
    if a.limits.reported_at != b.limits.reported_at {
        let newer = if a.limits.reported_at < b.limits.reported_at {
            b
        } else {
            a
        };
        return concurrent_record(
            a,
            newer.limits.windows.clone(),
            newer.limits.plan.clone(),
            error,
            newer.limits.reported_at.clone(),
        );
    }
    let mut windows: Vec<LimitWindow> = a.limits.windows.clone();
    for window in &b.limits.windows {
        match windows.iter_mut().find(|existing| existing.id == window.id) {
            Some(existing) => {
                if let Some(chosen) = later(Some(&*existing), Some(window)) {
                    *existing = chosen.clone();
                }
            }
            None => windows.push(window.clone()),
        }
    }
    let plan = later(a.limits.plan.as_ref(), b.limits.plan.as_ref()).cloned();
    concurrent_record(a, windows, plan, error, a.limits.reported_at.clone())
}

/// `later(x, y)`: the present one, or the one whose canonical rendering sorts later (ties keep `x`).
fn later<'a, T: serde::Serialize>(x: Option<&'a T>, y: Option<&'a T>) -> Option<&'a T> {
    match (x, y) {
        (None, y) => y,
        (x, None) => x,
        (Some(x), Some(y)) => Some(if canonical(x) >= canonical(y) { x } else { y }),
    }
}

/// The combined record at `a`'s stamp (both sides share it), reported at the later `reported_at` of the two.
fn concurrent_record(
    a: &MirroredLimits,
    mut windows: Vec<LimitWindow>,
    plan: Option<String>,
    error: Option<String>,
    reported_at: String,
) -> MirroredLimits {
    let updated_at = changed_at_of(a).to_owned();
    windows.sort_by(|x, y| x.id.cmp(&y.id));
    MirroredLimits {
        updated_at: (updated_at != reported_at).then_some(updated_at),
        revision: a.revision.filter(|revision| *revision != 0),
        limits: AccountLimits {
            provider: a.limits.provider.clone(),
            account: a.limits.account.clone(),
            windows,
            reported_at,
            plan,
            error,
        },
    }
}

/// A key-order-independent rendering of a record, so equal records compare equal however they were built.
/// Rust string order is byte order where upstream compares UTF-16 units; they agree for the ASCII ids, ISO times
/// and plan names compared here. Every mirrored store is this implementation, so all sides apply one order.
fn canonical<T: serde::Serialize>(value: &T) -> String {
    fn sorted(value: serde_json::Value) -> serde_json::Value {
        match value {
            serde_json::Value::Object(map) => {
                let mut entries: Vec<_> = map.into_iter().collect();
                entries.sort_by(|(a, _), (b, _)| a.cmp(b));
                serde_json::Value::Object(
                    entries
                        .into_iter()
                        .map(|(key, value)| (key, sorted(value)))
                        .collect(),
                )
            }
            serde_json::Value::Array(items) => {
                serde_json::Value::Array(items.into_iter().map(sorted).collect())
            }
            other => other,
        }
    }
    serde_json::to_value(value)
        .map(sorted)
        .map(|value| value.to_string())
        .unwrap_or_default()
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

    fn limits(reported_at: &str, windows: Vec<LimitWindow>, error: Option<&str>) -> MirroredLimits {
        MirroredLimits {
            limits: AccountLimits {
                provider: "p".into(),
                account: "a".into(),
                windows,
                reported_at: reported_at.into(),
                plan: None,
                error: error.map(str::to_owned),
            },
            updated_at: None,
            revision: None,
        }
    }

    /// senpi test/suite/regressions/3695-limit-mirror.test.ts: the wire shape is upstream camelCase.
    #[test]
    fn mirrored_limits_carry_the_camel_case_wire_shape() {
        let mut record = limits("2026-01-01T00:00:00.000Z", vec![window("x", 1.0)], None);
        record.updated_at = Some("2026-01-01T00:00:01.000Z".into());
        record.revision = Some(2);
        let wire = serde_json::to_value(&record).expect("wire");
        assert_eq!(wire["reportedAt"], "2026-01-01T00:00:00.000Z");
        assert_eq!(wire["updatedAt"], "2026-01-01T00:00:01.000Z");
        assert_eq!(wire["revision"], 2);
        assert_eq!(wire["windows"][0]["usedPercent"], 1.0);
        assert!(wire.get("plan").is_none());
        let back: MirroredLimits = serde_json::from_value(wire).expect("parse");
        assert_eq!(back, record);
    }

    /// A same-instant local write orders after the held record; a mirrored store takes it, and the copy that
    /// comes straight back is a no-op.
    #[test]
    fn same_instant_writes_bump_the_revision_and_echoes_end() {
        let host = SubscriptionLimitStore::new();
        let worker = SubscriptionLimitStore::new();
        let echoes = Arc::new(Mutex::new(0));
        let seen = Arc::clone(&echoes);
        worker.subscribe_mirror(Arc::new(move |_| {
            *seen.lock().unwrap_or_else(PoisonError::into_inner) += 1
        }));
        let now = DateTime::from_timestamp(100, 0).unwrap_or_default();
        host.record("p", "a", &[window("x", 1.0)], now);
        host.record("p", "a", &[window("x", 2.0)], now);
        let held = host.mirror_snapshot().remove(0);
        assert_eq!(held.revision, Some(1));
        worker.adopt(held.clone());
        worker.adopt(held.clone());
        assert_eq!(*echoes.lock().unwrap_or_else(PoisonError::into_inner), 1);
        host.adopt(worker.mirror_snapshot().remove(0));
        assert_eq!(host.mirror_snapshot()[0], held);
        // An older copy never replaces a newer one.
        worker.adopt(limits(
            "1970-01-01T00:00:00.000Z",
            vec![window("x", 9.0)],
            None,
        ));
        assert_eq!(worker.snapshot()[0].windows[0].used_percent, 2.0);
    }

    /// Two concurrent writes at the same stamp combine symmetrically: both stores reach the same record.
    #[test]
    fn concurrent_same_stamp_records_combine_symmetrically() {
        let a = limits(
            "2026-01-01T00:00:00.000Z",
            vec![window("x", 1.0)],
            Some("rate limited"),
        );
        let b = limits("2026-01-01T00:00:00.000Z", vec![window("y", 2.0)], None);
        let left = SubscriptionLimitStore::new();
        left.adopt(a.clone());
        left.adopt(b.clone());
        let right = SubscriptionLimitStore::new();
        right.adopt(b);
        right.adopt(a);
        assert_eq!(left.mirror_snapshot(), right.mirror_snapshot());
        let combined = &left.snapshot()[0];
        assert_eq!(combined.windows.len(), 2);
        assert_eq!(combined.error.as_deref(), Some("rate limited"));
    }

    /// A failed poll keeps the windows' time and stamps its own change time as `updatedAt`.
    #[test]
    fn an_error_poll_keeps_reported_at_and_sets_updated_at() {
        let store = SubscriptionLimitStore::new();
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
        let held = store.mirror_snapshot().remove(0);
        assert_eq!(held.limits.reported_at, iso_ms(at(100)));
        assert_eq!(held.updated_at, Some(iso_ms(at(200))));
        // A poll older than the change time is ignored.
        store.report(
            "p",
            "a",
            PollReport::Windows {
                windows: vec![window("x", 1.0)],
                plan: None,
                reported_at_ms: at(150),
            },
        );
        assert_eq!(store.snapshot()[0].windows[0].used_percent, 9.0);
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
