#[cfg(test)]
pub type ListenerCallback = Arc<Mutex<Option<Arc<dyn Fn() + Send + Sync>>>>;
use crate::{
    constants::{CLAUDE_PROVIDERS, CODEX_PROVIDERS, MIN_ACCESS_TOKEN_LENGTH},
    native::now,
    native_readers::read_json,
    runtime::UsageSource,
    types::{PanelColor, PanelRow, Stop},
};
use maho_ext_api::{AccountLimits, LimitWindow};
use serde_json::{Value, json};
use std::{
    path::PathBuf,
    sync::{
        Arc, Mutex, Weak,
        atomic::{AtomicBool, Ordering},
    },
};

pub type LimitsGetter = Arc<dyn Fn() -> Vec<AccountLimits> + Send + Sync>;
pub type LimitsSubscriber = Arc<dyn Fn(Arc<dyn Fn() + Send + Sync>) -> Option<Stop> + Send + Sync>;

/// Reads host-owned subscription reports; this component never fetches provider endpoints.
pub struct NativeUsage {
    home: PathBuf,
    get: LimitsGetter,
    subscribe: LimitsSubscriber,
    changed: Arc<dyn Fn() + Send + Sync>,
    snapshot: Mutex<Value>,
    unsubscribe: Mutex<Option<Stop>>,
    stopped: AtomicBool,
    weak: Weak<Self>,
}
impl NativeUsage {
    pub fn create(
        home: PathBuf,
        get: LimitsGetter,
        subscribe: LimitsSubscriber,
        changed: Arc<dyn Fn() + Send + Sync>,
    ) -> Arc<dyn UsageSource> {
        Arc::new_cyclic(|weak| Self {
            home,
            get,
            subscribe,
            changed,
            snapshot: Mutex::new(json!({})),
            unsubscribe: Mutex::default(),
            stopped: AtomicBool::new(true),
            weak: weak.clone(),
        })
    }
    fn rebuild(&self) {
        if self.stopped.load(Ordering::Acquire) {
            return;
        }
        let auth = read_json(&self.home.join("auth.json")).unwrap_or(Value::Null);
        let pool = read_json(&self.home.join("credential-pool-state.json")).unwrap_or(Value::Null);
        let value = snapshot_of(&(self.get)(), &auth, &pool, now());
        *self
            .snapshot
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = value;
        (self.changed)();
    }
}
impl UsageSource for NativeUsage {
    fn start(&self) {
        if !self.stopped.swap(false, Ordering::AcqRel) {
            return;
        }
        let weak = self.weak.clone();
        *self
            .unsubscribe
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) =
            (self.subscribe)(Arc::new(move || {
                if let Some(source) = weak.upgrade() {
                    source.rebuild();
                }
            }));
        self.rebuild();
    }
    fn refresh(&self) {
        self.rebuild();
    }
    fn stop(&self) {
        self.stopped.store(true, Ordering::Release);
        if let Some(unsubscribe) = self
            .unsubscribe
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
        {
            unsubscribe();
        }
    }
    fn rows(&self, at: u64, width: usize) -> Vec<PanelRow> {
        let snapshot = self
            .snapshot
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut rows = Vec::new();
        for key in ["claude", "codex"] {
            let Some(entry) = snapshot.get(key).filter(|entry| entry.is_object()) else {
                continue;
            };
            rows.push(PanelRow::colored(
                format!(
                    "{}  {}",
                    key.to_uppercase(),
                    entry.get("account").and_then(Value::as_str).unwrap_or("")
                ),
                PanelColor::Accent,
            ));
            if let Some(pinned) = entry.get("pinnedAccount").and_then(Value::as_str) {
                rows.push(PanelRow::colored(
                    format!("failover from {pinned}"),
                    PanelColor::Warning,
                ));
            }
            if let Some(windows) = entry.get("windows").and_then(Value::as_array) {
                for window in windows.iter().take(4) {
                    let Some(percent) = window.get("percent").and_then(Value::as_f64) else {
                        continue;
                    };
                    let label = window
                        .get("label")
                        .and_then(Value::as_str)
                        .unwrap_or("window");
                    let cells = width.saturating_sub(14).min(24);
                    let marker = window
                        .get("windowMs")
                        .and_then(Value::as_f64)
                        .zip(window.get("resetsAt").and_then(Value::as_f64))
                        .and_then(|(ms, reset)| {
                            let remaining = reset - at as f64;
                            (ms > 0.0 && remaining > 0.0 && remaining <= ms)
                                .then_some(1.0 - remaining / ms)
                        });
                    let bar = usage_bar(percent / 100.0, cells, marker);
                    rows.push(PanelRow::colored(
                        format!("{label:<4} {bar} {percent:.0}%"),
                        if percent >= 90.0 {
                            PanelColor::Warning
                        } else {
                            PanelColor::Muted
                        },
                    ));
                    if let Some(reset) = window.get("resetsAt").and_then(Value::as_u64) {
                        rows.push(PanelRow::colored(
                            format!("reset in {}m", reset.saturating_sub(at) / 60000),
                            PanelColor::Dim,
                        ));
                    }
                }
            }
            if let Some(error) = entry.get("error").and_then(Value::as_str) {
                rows.push(PanelRow::colored(error.to_owned(), PanelColor::Warning));
            }
            if let Some(state) = entry
                .get("accountState")
                .and_then(Value::as_str)
                .filter(|state| *state != "ok")
            {
                rows.push(PanelRow::colored(
                    format!("account {state}"),
                    PanelColor::Warning,
                ));
            }
            if let Some(updated) = entry.get("updatedAt").and_then(Value::as_u64) {
                rows.push(PanelRow::colored(
                    format!("updated {}m ago", at.saturating_sub(updated) / 60000),
                    PanelColor::Dim,
                ));
            }
        }
        if !rows.is_empty() {
            rows.insert(0, PanelRow::colored("USAGE", PanelColor::Accent));
        }
        rows
    }
}
struct Credential {
    account: Option<String>,
    pinned: Option<String>,
    state: &'static str,
}

fn snapshot_of(records: &[AccountLimits], auth: &Value, pool: &Value, at: u64) -> Value {
    let mut snapshot = json!({});
    for (key, providers) in [("claude", CLAUDE_PROVIDERS), ("codex", CODEX_PROVIDERS)] {
        let selected = credential(auth, pool, providers, at);
        if let Some(record) = serving_record(records, providers, selected.as_ref()) {
            snapshot[key] = entry_of(record, selected.as_ref());
        }
    }
    snapshot
}
fn serving_record<'a>(
    records: &'a [AccountLimits],
    providers: &[&str],
    credential: Option<&Credential>,
) -> Option<&'a AccountLimits> {
    let own: Vec<_> = records
        .iter()
        .filter(|record| providers.contains(&record.provider.as_str()))
        .collect();
    if let Some(credential) = credential {
        own.iter()
            .copied()
            .find(|record| record.account == credential.account.as_deref().unwrap_or("default"))
            .or_else(|| own.iter().copied().find(|record| record.account == "env"))
    } else {
        own.into_iter().reduce(|latest, record| {
            if record.reported_at > latest.reported_at {
                record
            } else {
                latest
            }
        })
    }
}
fn timestamp(value: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|date| date.timestamp_millis())
}
fn entry_of(record: &AccountLimits, credential: Option<&Credential>) -> Value {
    let mut entry = json!({"account":record.account,"windows":record.windows.iter().map(window_of).collect::<Vec<_>>()});
    if let Some(at) = timestamp(&record.reported_at) {
        entry["updatedAt"] = json!(at);
    }
    if let Some(plan) = &record.plan {
        entry["plan"] = json!(plan);
    }
    if let Some(error) = &record.error {
        entry["error"] = json!(error);
    }
    if let Some(credential) = credential
        .filter(|selected| selected.account.as_deref().unwrap_or("default") == record.account)
    {
        entry["accountState"] = json!(credential.state);
        if let Some(pinned) = &credential.pinned {
            entry["pinnedAccount"] = json!(pinned);
        }
    }
    entry
}
fn window_of(window: &LimitWindow) -> Value {
    let mut value = json!({"label":window.label,"percent":window.used_percent});
    if let Some(at) = window.resets_at.as_deref().and_then(timestamp) {
        value["resetsAt"] = json!(at);
    }
    if let Some(minutes) = window.window_minutes {
        value["windowMs"] = json!(minutes * 60_000);
    }
    if window.id.starts_with("claude:7d_") {
        value["scoped"] = json!(true);
    }
    value
}
fn usage_bar(ratio: f64, width: usize, marker: Option<f64>) -> String {
    let exact = ratio.clamp(0.0, 1.0) * width as f64;
    let mut cells = Vec::with_capacity(width);
    for index in 0..width {
        let eighths = ((exact - index as f64) * 8.0).round().clamp(0.0, 8.0) as usize;
        cells.push(['░', '▏', '▎', '▍', '▌', '▋', '▊', '▉', '█'][eighths]);
    }
    if ratio > 0.0
        && cells.iter().all(|cell| *cell == '░')
        && let Some(first) = cells.first_mut()
    {
        *first = '▏';
    }
    if let Some(marker) = marker
        && width > 0
    {
        cells[((marker.clamp(0.0, 1.0) * width as f64) as usize).min(width - 1)] = '┊';
    }
    cells.into_iter().collect()
}
fn credential(auth: &Value, pool: &Value, providers: &[&str], at: u64) -> Option<Credential> {
    let health = |account: &Value| {
        let name = account.get("name").and_then(Value::as_str).unwrap_or("");
        let blocked = providers.iter().any(|provider| {
            ["blockedUntil", "cooldownUntil"].iter().any(|field| {
                pool["providers"][*provider]["lanes"]["stored"]["slots"][name][*field]
                    .as_u64()
                    .is_some_and(|until| until > at)
            })
        });
        if blocked
            || account
                .get("blockedUntil")
                .and_then(Value::as_u64)
                .is_some_and(|until| until > at)
        {
            "cooldown"
        } else if account
            .get("expires")
            .and_then(Value::as_u64)
            .is_some_and(|until| until <= at)
        {
            "stale"
        } else {
            "ok"
        }
    };
    let usable = |account: &Value| {
        account
            .get("access")
            .and_then(Value::as_str)
            .is_some_and(|token| token.len() >= MIN_ACCESS_TOKEN_LENGTH)
    };
    for provider in providers {
        let Some(node) = auth.get(*provider) else {
            continue;
        };
        if let Some(accounts) = node
            .get("accounts")
            .and_then(Value::as_array)
            .filter(|accounts| !accounts.is_empty())
        {
            let pinned = accounts
                .iter()
                .find(|account| account.get("name") == node.get("pinned"))
                .or_else(|| accounts.first())?;
            let serving = if health(pinned) == "ok" && usable(pinned) {
                Some(pinned)
            } else {
                accounts
                    .iter()
                    .find(|account| health(account) == "ok" && usable(account))
                    .or_else(|| (health(pinned) == "stale" && usable(pinned)).then_some(pinned))
            };
            if let Some(serving) = serving {
                let account = serving
                    .get("name")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                let pinned = pinned
                    .get("name")
                    .and_then(Value::as_str)
                    .filter(|name| Some(*name) != account.as_deref())
                    .map(str::to_owned);
                return Some(Credential {
                    account,
                    pinned,
                    state: health(serving),
                });
            }
        }
        if usable(node) {
            return Some(Credential {
                account: None,
                pinned: None,
                state: health(node),
            });
        }
    }
    None
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn pool_failover_keeps_token_and_label_on_the_same_account() {
        let pinned = "p".repeat(50);
        let healthy = "h".repeat(50);
        let auth = json!({"anthropic-subscription":{"pinned":"primary","accounts":[{"name":"primary","access":pinned},{"name":"backup","access":healthy}]}});
        let pool = json!({"providers":{"claude-sdk-oauth":{"lanes":{"stored":{"slots":{"primary":{"cooldownUntil":2000}}}}}}});
        let selected = credential(&auth, &pool, CLAUDE_PROVIDERS, 1000).expect("healthy backup");
        assert_eq!(selected.account.as_deref(), Some("backup"));
        assert_eq!(selected.pinned.as_deref(), Some("primary"));
        assert_eq!(selected.state, "ok");
        assert!(
            credential(
                &json!({"chatgpt-subscription":{"key":"k".repeat(50)}}),
                &Value::Null,
                CODEX_PROVIDERS,
                1000
            )
            .is_none()
        );
    }
    #[test]
    fn quota_bar_marks_even_burn_without_losing_fractional_usage() {
        assert_eq!(usage_bar(0.001, 10, None), "▏░░░░░░░░░");
        assert_eq!(usage_bar(0.5, 4, Some(0.75)), "██░┊");
        assert_eq!(usage_bar(1.0, 0, Some(0.5)), "");
    }
    fn record(account: &str, percent: f64) -> AccountLimits {
        AccountLimits {
            provider: "anthropic-subscription".into(),
            account: account.into(),
            reported_at: "2026-10-19T03:59:00.000Z".into(),
            plan: None,
            error: None,
            windows: vec![
                LimitWindow {
                    id: "claude:5h".into(),
                    label: "5h".into(),
                    window_minutes: Some(300),
                    used_percent: percent,
                    resets_at: Some("2026-10-19T09:00:00.000Z".into()),
                },
                LimitWindow {
                    id: "claude:7d_opus".into(),
                    label: "7d opus".into(),
                    window_minutes: Some(10080),
                    used_percent: 3.0,
                    resets_at: None,
                },
            ],
        }
    }
    fn auth(pinned: &str) -> Value {
        json!({"anthropic-subscription":{"pinned":pinned,"accounts":[{"name":"work","access":"t".repeat(50)},{"name":"home","access":"t".repeat(50)}]}})
    }
    #[test]
    fn engine_selects_serving_account_and_normalizes_windows() {
        let snapshot = snapshot_of(
            &[record("work", 41.0), record("home", 90.0)],
            &auth("work"),
            &Value::Null,
            0,
        );
        assert_eq!(
            snapshot,
            json!({"claude":{"account":"work","accountState":"ok","updatedAt":1792382340000_i64,"windows":[{"label":"5h","percent":41.0,"resetsAt":1792400400000_i64,"windowMs":18000000},{"label":"7d opus","percent":3.0,"windowMs":604800000,"scoped":true}]}})
        );
    }
    #[test]
    fn ambient_uses_latest_but_named_login_only_falls_back_to_env() {
        let mut env = record("env", 55.0);
        env.reported_at = "2026-10-19T03:00:00.000Z".into();
        let records = [env, record("ambient", 20.0)];
        let ambient = snapshot_of(&records, &json!({}), &Value::Null, 0);
        assert_eq!(ambient["claude"]["account"], "ambient");
        assert!(ambient["claude"].get("accountState").is_none());
        let fallback = snapshot_of(&records, &auth("work"), &Value::Null, 0);
        assert_eq!(fallback["claude"]["account"], "env");
        assert!(fallback["claude"].get("accountState").is_none());
        let own = snapshot_of(
            &[record("env", 55.0), record("work", 41.0)],
            &auth("work"),
            &Value::Null,
            0,
        );
        assert_eq!(own["claude"]["windows"][0]["percent"], 41.0);
    }
    #[test]
    fn rotation_never_keeps_previous_accounts_numbers() {
        let records = [record("work", 41.0), record("home", 7.0)];
        assert_eq!(
            snapshot_of(&records, &auth("work"), &Value::Null, 0)["claude"]["account"],
            "work"
        );
        assert_eq!(
            snapshot_of(&records, &auth("home"), &Value::Null, 0)["claude"]["account"],
            "home"
        );
        assert!(
            snapshot_of(&records[..1], &auth("home"), &Value::Null, 0)
                .get("claude")
                .is_none()
        );
    }
    #[test]
    fn engine_reports_repaint_and_stop_unsubscribes() {
        use std::sync::atomic::AtomicUsize;
        let records = Arc::new(Mutex::new(Vec::new()));
        let listener: ListenerCallback = Arc::default();
        let repaints = Arc::new(AtomicUsize::new(0));
        let source = NativeUsage::create(
            PathBuf::from("/nonexistent-side-panel-test"),
            {
                let records = records.clone();
                Arc::new(move || records.lock().expect("records").clone())
            },
            {
                let listener = listener.clone();
                Arc::new(move |callback| {
                    *listener.lock().expect("listener") = Some(callback);
                    let listener = listener.clone();
                    Some(Box::new(move || {
                        listener.lock().expect("listener").take();
                    }))
                })
            },
            {
                let repaints = repaints.clone();
                Arc::new(move || {
                    repaints.fetch_add(1, Ordering::SeqCst);
                })
            },
        );
        source.start();
        assert_eq!(repaints.load(Ordering::SeqCst), 1);
        let mut report = record("work", 12.0);
        report.error = Some("rate limited".into());
        *records.lock().expect("records") = vec![report];
        let notify = listener
            .lock()
            .expect("listener")
            .clone()
            .expect("subscribed");
        notify();
        assert_eq!(repaints.load(Ordering::SeqCst), 2);
        let rows = source.rows(0, 40);
        assert!(rows.iter().any(|row| row.text == "rate limited"));
        assert!(rows.iter().all(|row| !row.text.contains("retry")));
        source.refresh();
        assert_eq!(repaints.load(Ordering::SeqCst), 3);
        source.stop();
        assert!(listener.lock().expect("listener").is_none());
        notify();
        assert_eq!(repaints.load(Ordering::SeqCst), 3);
    }
}
