use crate::{
    constants::*,
    native::now,
    native_readers::read_json,
    runtime::UsagePoller,
    types::{PanelColor, PanelRow},
};
use base64::Engine;
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, Weak,
        atomic::{AtomicBool, Ordering},
    },
};
static NEXT_CLAIM: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

pub struct NativeUsage {
    home: PathBuf,
    cache: PathBuf,
    ms: u64,
    changed: Arc<dyn Fn() + Send + Sync>,
    snapshot: Mutex<Value>,
    running: AtomicBool,
    stopped: AtomicBool,
    timer: Mutex<Option<tokio::task::JoinHandle<()>>>,
    weak: Weak<Self>,
}
impl NativeUsage {
    pub fn create(
        home: PathBuf,
        ms: u64,
        changed: Arc<dyn Fn() + Send + Sync>,
    ) -> Arc<dyn UsagePoller> {
        let base = std::env::var_os("XDG_CACHE_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                std::env::var_os("HOME")
                    .map(PathBuf::from)
                    .unwrap_or_default()
                    .join(".cache")
            });
        Arc::new_cyclic(|weak| Self {
            home,
            cache: base.join("omo-senpi/side-panel-usage.json"),
            ms,
            changed,
            snapshot: Mutex::new(json!({})),
            running: AtomicBool::new(false),
            stopped: AtomicBool::new(true),
            timer: Mutex::default(),
            weak: weak.clone(),
        })
    }
    fn publish(&self, value: Value) {
        if self.stopped.load(Ordering::Acquire) {
            return;
        }
        let mut snapshot = self
            .snapshot
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if *snapshot != value {
            *snapshot = value;
            drop(snapshot);
            (self.changed)();
        }
    }
    async fn poll(&self) {
        let mut cache = read_cache(&self.cache);
        self.publish(cache.clone());
        let auth = read_json(&self.home.join("auth.json")).unwrap_or(Value::Null);
        let pool = read_json(&self.home.join("credential-pool-state.json")).unwrap_or(Value::Null);
        for (key, providers, url, ttl) in [
            (
                "claude",
                CLAUDE_PROVIDERS,
                CLAUDE_USAGE_URL,
                USAGE_TTL_MS.claude,
            ),
            (
                "codex",
                CODEX_PROVIDERS,
                CODEX_USAGE_URL,
                USAGE_TTL_MS.codex,
            ),
        ] {
            if self.stopped.load(Ordering::Acquire) {
                return;
            }
            let credential = credential(&auth, &pool, providers, now());
            if credential.is_none() {
                continue;
            }
            let account = credential
                .as_ref()
                .and_then(|credential| credential.account.as_deref());
            let at = now();
            let entry = &cache[key];
            if entry.get("account").and_then(Value::as_str) == account {
                if entry
                    .get("retryAt")
                    .and_then(Value::as_u64)
                    .is_some_and(|time| time > at)
                {
                    continue;
                }
                if entry
                    .get("updatedAt")
                    .and_then(Value::as_u64)
                    .is_some_and(|time| at.saturating_sub(time) < ttl.max(self.ms))
                {
                    continue;
                }
            }
            let claim = &cache["fetching"][key];
            if claim.get("account").and_then(Value::as_str) == account
                && claim
                    .get("claimedAt")
                    .and_then(Value::as_u64)
                    .is_some_and(|time| at.saturating_sub(time) < USAGE_CLAIM_MS)
            {
                continue;
            }
            let token = format!(
                "{}-{at}-{}-{key}",
                std::process::id(),
                NEXT_CLAIM.fetch_add(1, Ordering::Relaxed)
            );
            if !cache["fetching"].is_object() {
                cache["fetching"] = json!({});
            }
            cache["fetching"][key] = json!({"claimedAt":at,"token":token,"account":account});
            if let Err(error) = write_cache(&self.cache, &cache) {
                eprintln!("side panel: usage cache: {error}");
                continue;
            }
            cache = read_cache(&self.cache);
            if cache["fetching"][key]["token"].as_str() != Some(&token) {
                continue;
            }
            let mut next = if let Some(credential) = &credential {
                match fetch(url, key, &credential.access).await {
                    Ok(value) => value,
                    Err((message, retry)) => {
                        let delay = retry
                            .unwrap_or(if message == "rate limited" {
                                self.ms
                            } else {
                                self.ms * 2
                            })
                            .min(USAGE_MAX_BACKOFF_MS);
                        json!({"error":message,"retryAt":now()+delay})
                    }
                }
            } else {
                json!({"error":"auth stale - run /login","retryAt":now()+self.ms*5})
            };
            if self.stopped.load(Ordering::Acquire) {
                let mut latest = read_cache(&self.cache);
                if latest["fetching"][key]["token"].as_str() == Some(&token) {
                    if let Some(fetching) = latest["fetching"].as_object_mut() {
                        fetching.remove(key);
                    }
                    if let Err(error) = write_cache(&self.cache, &latest) {
                        eprintln!("side panel: usage cache: {error}");
                    }
                }
                return;
            }
            if let Some(credential) = &credential {
                next["account"] = json!(credential.account);
                next["pinnedAccount"] = json!(credential.pinned);
                next["accountState"] = json!(credential.state);
            }
            cache = read_cache(&self.cache);
            if cache["fetching"][key]["token"].as_str() != Some(&token) {
                continue;
            }
            if next.get("error").is_some()
                && cache[key].get("account").and_then(Value::as_str) == account
            {
                let mut previous = cache[key].as_object().cloned().unwrap_or_default();
                if let Some(next) = next.as_object() {
                    previous.extend(next.clone());
                }
                next = Value::Object(previous);
            }
            cache[key] = next;
            if let Some(fetching) = cache["fetching"].as_object_mut() {
                fetching.remove(key);
            }
            if let Err(error) = write_cache(&self.cache, &cache) {
                eprintln!("side panel: usage cache: {error}");
            }
            self.publish(cache.clone());
        }
    }
}
impl UsagePoller for NativeUsage {
    fn start(&self) {
        if !self.stopped.swap(false, Ordering::AcqRel) {
            return;
        }
        self.poll_once();
        let weak = self.weak.clone();
        let ms = self.ms;
        let task = tokio::spawn(async move {
            let mut timer = tokio::time::interval(std::time::Duration::from_millis(ms));
            timer.tick().await;
            loop {
                timer.tick().await;
                let Some(this) = weak.upgrade() else {
                    break;
                };
                if this.stopped.load(Ordering::Acquire) {
                    break;
                }
                this.poll_once();
            }
        });
        *self
            .timer
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(task);
    }
    fn stop(&self) {
        self.stopped.store(true, Ordering::Release);
        if let Some(task) = self
            .timer
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
        {
            task.abort();
        }
    }
    fn poll_once(&self) {
        if self.stopped.load(Ordering::Acquire) || self.running.swap(true, Ordering::AcqRel) {
            return;
        }
        if let Some(this) = self.weak.upgrade() {
            tokio::spawn(async move {
                this.poll().await;
                this.running.store(false, Ordering::Release);
            });
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
                let retry = entry
                    .get("retryAt")
                    .and_then(Value::as_u64)
                    .map_or_else(String::new, |retry| {
                        format!(" · retry {}m", retry.saturating_sub(at) / 60000)
                    });
                rows.push(PanelRow::colored(
                    format!("{error}{retry}"),
                    PanelColor::Warning,
                ));
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
    access: String,
    account: Option<String>,
    pinned: Option<String>,
    state: &'static str,
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
                    access: serving["access"].as_str()?.into(),
                    account,
                    pinned,
                    state: health(serving),
                });
            }
        }
        if usable(node) {
            return Some(Credential {
                access: node["access"].as_str()?.into(),
                account: None,
                pinned: None,
                state: health(node),
            });
        }
    }
    None
}
async fn fetch(url: &str, key: &str, token: &str) -> Result<Value, (String, Option<u64>)> {
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(std::time::Duration::from_millis(USAGE_TIMEOUT_MS))
        .build()
        .map_err(|error| (error.to_string(), None))?;
    let mut request = client
        .get(url)
        .bearer_auth(token)
        .header("Accept", "application/json");
    if key == "codex" {
        if let Some(claims) = token
            .split('.')
            .nth(1)
            .and_then(|part| {
                base64::engine::general_purpose::URL_SAFE_NO_PAD
                    .decode(part)
                    .ok()
            })
            .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
        {
            if let Some(id) = claims
                .pointer("/https:~1~1api.openai.com~1auth/chatgpt_account_id")
                .or_else(|| claims.pointer("/https:~1~1api.openai.com~1auth/account_id"))
                .and_then(Value::as_str)
            {
                request = request.header("ChatGPT-Account-Id", id);
            }
        }
    }
    let response = request.send().await.map_err(|error| {
        (
            if error.is_timeout() {
                "timed out".into()
            } else {
                error.to_string()
            },
            None,
        )
    })?;
    if !response.status().is_success() {
        let retry = response
            .headers()
            .get("retry-after")
            .and_then(|value| value.to_str().ok())
            .and_then(|value| {
                value
                    .parse::<u64>()
                    .ok()
                    .map(|seconds| seconds * 1000)
                    .or_else(|| {
                        chrono::DateTime::parse_from_rfc2822(value)
                            .ok()
                            .and_then(|time| u64::try_from(time.timestamp_millis()).ok())
                            .map(|time| time.saturating_sub(now()))
                    })
            })
            .filter(|delay| *delay > 0);
        return Err((
            match response.status().as_u16() {
                401 | 403 => "auth stale - run /login".into(),
                429 => "rate limited".into(),
                status => format!("HTTP {status}"),
            },
            retry,
        ));
    }
    let payload: Value = response
        .json()
        .await
        .map_err(|error| (error.to_string(), None))?;
    let mut windows = Vec::new();
    if key == "claude" {
        if let Some(limits) = payload.get("limits").and_then(Value::as_array) {
            for limit in limits {
                if let Some(percent) = limit.get("percent").and_then(Value::as_f64) {
                    let kind = limit.get("kind").and_then(Value::as_str).unwrap_or("");
                    let label = match kind {
                        "session" => "5h",
                        "weekly_all" => "7d",
                        _ => limit
                            .pointer("/scope/model/display_name")
                            .and_then(Value::as_str)
                            .unwrap_or("7d*"),
                    };
                    windows.push(json!({"label":label,"percent":percent,"windowMs":if kind=="session"{FIVE_HOUR_MS}else{WEEK_MS},"scoped":kind=="weekly_scoped","resetsAt":limit.get("resets_at").and_then(Value::as_str).and_then(|value|chrono::DateTime::parse_from_rfc3339(value).ok()).map(|time|time.timestamp_millis())}));
                }
            }
        }
        if windows.is_empty() {
            for (field, label, ms) in [
                ("five_hour", "5h", FIVE_HOUR_MS),
                ("seven_day", "7d", WEEK_MS),
            ] {
                if let Some(percent) = payload[field]["utilization"].as_f64() {
                    windows.push(json!({"label":label,"percent":percent,"windowMs":ms,"resetsAt":payload[field]["resets_at"].as_str().and_then(|value|chrono::DateTime::parse_from_rfc3339(value).ok()).map(|time|time.timestamp_millis())}));
                }
            }
        }
    } else {
        for field in ["primary_window", "secondary_window"] {
            let window = &payload["rate_limit"][field];
            if let Some(percent) = window["used_percent"].as_f64() {
                let seconds = window["limit_window_seconds"].as_u64();
                let label: String = match seconds {
                    Some(18000) => "5h".into(),
                    Some(604800) => "7d".into(),
                    Some(seconds) => format!("{}h", seconds / 3600),
                    None => "window".into(),
                };
                windows.push(json!({"label":label,"percent":percent,"windowMs":seconds.map(|seconds|seconds*1000),"resetsAt":window["reset_at"].as_u64().map(|seconds|seconds*1000)}));
            }
        }
    }
    Ok(json!({"windows":windows,"updatedAt":now(),"plan":payload.get("plan_type")}))
}
fn read_cache(path: &Path) -> Value {
    let raw = read_json(path).unwrap_or(Value::Null);
    let mut cache = json!({"fetching":{}});
    for key in ["claude", "codex"] {
        if let Some(entry) = raw.get(key).and_then(Value::as_object) {
            let mut clean = serde_json::Map::new();
            for field in ["plan", "account", "pinnedAccount", "accountState", "error"] {
                if let Some(text) = entry
                    .get(field)
                    .and_then(Value::as_str)
                    .filter(|text| !text.is_empty())
                {
                    clean.insert(field.into(), json!(text));
                }
            }
            let mut windows = Vec::new();
            if let Some(raw_windows) = entry.get("windows").and_then(Value::as_array) {
                for window in raw_windows {
                    if let (Some(label), Some(percent)) = (
                        window.get("label").and_then(Value::as_str),
                        window.get("percent").and_then(Value::as_f64),
                    ) {
                        let mut clean_window = json!({"label":label,"percent":percent});
                        for field in ["resetsAt", "windowMs"] {
                            if let Some(value) = window.get(field).and_then(Value::as_u64) {
                                clean_window[field] = json!(value);
                            }
                        }
                        if window.get("scoped").and_then(Value::as_bool) == Some(true) {
                            clean_window["scoped"] = json!(true);
                        }
                        windows.push(clean_window);
                    }
                }
            }
            let invalid = entry
                .get("windows")
                .and_then(Value::as_array)
                .is_some_and(|windows| !windows.is_empty())
                && windows.is_empty();
            if !windows.is_empty() {
                clean.insert("windows".into(), json!(windows));
            }
            for field in ["updatedAt", "retryAt"] {
                if !(invalid && field == "updatedAt")
                    && let Some(time) = entry.get(field).and_then(Value::as_u64)
                {
                    clean.insert(field.into(), json!(time));
                }
            }
            cache[key] = Value::Object(clean);
        }
        let claim = &raw["fetching"][key];
        if let Some(at) = claim.as_u64() {
            cache["fetching"][key] = json!({"claimedAt":at,"token":"legacy"});
        } else if let (Some(at), Some(token)) = (
            claim.get("claimedAt").and_then(Value::as_u64),
            claim.get("token").and_then(Value::as_str),
        ) {
            cache["fetching"][key] = json!({"claimedAt":at,"token":token,"account":claim.get("account").and_then(Value::as_str)});
        }
    }
    cache
}
fn write_cache(path: &Path, value: &Value) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let temporary = path.with_extension(format!(
        "{}-{}.tmp",
        std::process::id(),
        NEXT_CLAIM.fetch_add(1, Ordering::Relaxed)
    ));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options.open(&temporary)?;
    serde_json::to_writer(file, value)?;
    std::fs::rename(temporary, path)
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
        assert_eq!(selected.access, healthy);
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
}
