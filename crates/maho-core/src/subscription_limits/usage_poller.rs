//! Port of senpi packages/coding-agent/src/core/subscription-limits/usage-poller.ts.
//!
//! Polls each signed-in subscription account's usage endpoint into the process's limit store, so an
//! account's windows are known before its first turn. Every process on the machine shares one cache
//! file, so a machine makes one request per account per interval however many sessions are open.
//!
//! deviation: the store, the account listing and the abort signal are injected (`PollReport`,
//! `PollAccount`, `AbortSignal`) instead of imported from limit-store / usage-accounts / auth-lane, so
//! this module has no dependency on those ports; the host wires them in `UsagePollingOptions`.

use std::collections::{BTreeMap, HashMap};
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use super::limit_windows::LimitWindow;
use super::usage_cache::{
    CachedReading, UsageCacheFile, claimed_by, is_due, read_usage_cache, usage_key, with_claims,
    with_results, write_usage_cache,
};
use super::usage_endpoints::{UsageError, UsageFetch, UsageProvider, fetch_usage_json, read_usage};

/// Claude's numbers move in five-hour blocks and its endpoint is the stricter one, so it is read half as often.
pub const fn poll_interval_ms(provider: UsageProvider) -> i64 {
    match provider {
        UsageProvider::AnthropicSubscription => 300_000,
        UsageProvider::ChatgptSubscription => 150_000,
    }
}
const TICK: Duration = Duration::from_secs(30);
const MAX_BACKOFF_MS: i64 = 60 * 60_000;
/// Intervals to wait before asking again about a login only `/login` can fix.
const SIGNED_OUT_INTERVALS: i64 = 5;
/// Shown when a login only `/login` can fix is the reason a poll could not ask (usage-accounts.ts).
pub const SIGN_IN_NEEDED: &str = "auth stale - run /login";

/// Flips to `true` on shutdown.
pub type AbortSignal = tokio::sync::watch::Receiver<bool>;

/// `None`: nothing usable to ask with right now. `Err`: the sign-in message to show.
pub type AccessResult = Option<Result<String, String>>;
pub type AccessFuture = Pin<Box<dyn Future<Output = AccessResult> + Send>>;

#[derive(Clone)]
pub struct PollAccount {
    pub provider: UsageProvider,
    pub account: String,
    pub access: Arc<dyn Fn(AbortSignal) -> AccessFuture + Send + Sync>,
}

/// What one usage-endpoint poll learned about an account: its full window set, or why it could not ask.
#[derive(Debug, Clone, PartialEq)]
pub enum PollReport {
    Windows {
        windows: Vec<LimitWindow>,
        plan: Option<String>,
        reported_at_ms: i64,
    },
    Error {
        error: String,
        reported_at_ms: i64,
    },
}

pub type ReportSink = Arc<dyn Fn(&str, &str, PollReport) + Send + Sync>;
pub type ListAccountsFuture = Pin<Box<dyn Future<Output = Vec<PollAccount>> + Send>>;
pub type ListAccounts = Arc<dyn Fn() -> ListAccountsFuture + Send + Sync>;

#[derive(Clone)]
pub struct UsagePollerDeps {
    pub report: ReportSink,
    pub cache_path: PathBuf,
    pub list_accounts: ListAccounts,
    pub fetch: UsageFetch,
    pub now: Arc<dyn Fn() -> i64 + Send + Sync>,
}

pub struct UsagePoller {
    deps: UsagePollerDeps,
    /// The cached reading each account last handed the store, so an unchanged reading is not re-announced.
    adopted: Mutex<HashMap<String, i64>>,
    running: AtomicBool,
}

struct RunningGuard<'a>(&'a AtomicBool);
impl Drop for RunningGuard<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

fn key_of(account: &PollAccount) -> String {
    usage_key(account.provider.as_str(), &account.account)
}

impl UsagePoller {
    pub fn new(deps: UsagePollerDeps) -> Self {
        Self {
            deps,
            adopted: Mutex::new(HashMap::new()),
            running: AtomicBool::new(false),
        }
    }

    /// One pass: adopt what siblings already read, fetch what is due and claimed by this process, publish.
    pub async fn poll_once(&self, signal: AbortSignal) {
        if self.running.swap(true, Ordering::SeqCst) {
            return;
        }
        let _guard = RunningGuard(&self.running);
        let accounts: BTreeMap<String, PollAccount> = (self.deps.list_accounts)()
            .await
            .into_iter()
            .map(|account| (key_of(&account), account))
            .collect();
        let path = self.deps.cache_path.as_path();
        let before = read_usage_cache(path);
        self.adopt(&before, &accounts);
        let now = (self.deps.now)();
        let due: Vec<String> = accounts
            .keys()
            .filter(|key| is_due(&before, key, now))
            .cloned()
            .collect();
        if due.is_empty() {
            return;
        }
        let token = uuid::Uuid::new_v4().to_string();
        write_usage_cache(path, &with_claims(&before, &due, now, &token));
        let owned = claimed_by(&read_usage_cache(path), &due, &token);
        let reads = owned.iter().filter_map(|key| {
            let account = accounts.get(key)?;
            let previous = before.accounts.get(key).cloned();
            let signal = signal.clone();
            let key = key.clone();
            Some(async move { (key, self.read(account, previous, signal).await) })
        });
        let results: HashMap<String, CachedReading> =
            futures::future::join_all(reads).await.into_iter().collect();
        let after = with_results(&read_usage_cache(path), &results, &token);
        write_usage_cache(path, &after);
        self.adopt(&after, &accounts);
    }

    fn adopt(&self, cache: &UsageCacheFile, accounts: &BTreeMap<String, PollAccount>) {
        let mut adopted = self
            .adopted
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for (key, account) in accounts {
            let Some(reading) = cache.accounts.get(key) else {
                continue;
            };
            if adopted.get(key) == Some(&reading.updated_at) {
                continue;
            }
            adopted.insert(key.clone(), reading.updated_at);
            let report = match &reading.error {
                Some(error) => PollReport::Error {
                    error: error.clone(),
                    reported_at_ms: reading.updated_at,
                },
                None => PollReport::Windows {
                    windows: reading.windows.clone(),
                    plan: reading.plan.clone(),
                    reported_at_ms: reading.updated_at,
                },
            };
            (self.deps.report)(account.provider.as_str(), &account.account, report);
        }
    }

    async fn read(
        &self,
        account: &PollAccount,
        previous: Option<CachedReading>,
        signal: AbortSignal,
    ) -> CachedReading {
        let interval = poll_interval_ms(account.provider);
        let now = (self.deps.now)();
        let Some(credential) = (account.access)(signal).await else {
            // Nothing to ask with yet: keep what is known and look again next interval.
            let kept = previous.unwrap_or(CachedReading {
                windows: vec![],
                plan: None,
                error: None,
                updated_at: now,
                next_at: now,
            });
            return CachedReading {
                next_at: now + interval,
                ..kept
            };
        };
        let access = match credential {
            Ok(access) => access,
            Err(error) => {
                return CachedReading {
                    windows: vec![],
                    plan: None,
                    error: Some(error),
                    updated_at: now,
                    next_at: now + SIGNED_OUT_INTERVALS * interval,
                };
            }
        };
        match read_usage(account.provider, &access, &self.deps.fetch).await {
            Ok(reading) => CachedReading {
                windows: reading.windows,
                plan: reading.plan,
                error: None,
                updated_at: now,
                next_at: now + interval,
            },
            Err(error) => {
                let failure = describe_failure(&error, interval);
                CachedReading {
                    windows: vec![],
                    plan: None,
                    error: Some(failure.error),
                    updated_at: now,
                    next_at: now + failure.wait_ms.min(MAX_BACKOFF_MS),
                }
            }
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct Failure {
    pub error: String,
    pub wait_ms: i64,
}

/// A 429 with `retry-after: 0` means the window is about to roll, so only a positive header earns a long wait.
pub fn describe_failure(error: &UsageError, interval: i64) -> Failure {
    match error {
        UsageError::Http(http) => match http.status {
            401 | 403 => Failure {
                error: SIGN_IN_NEEDED.into(),
                wait_ms: SIGNED_OUT_INTERVALS * interval,
            },
            429 => Failure {
                error: "rate limited".into(),
                wait_ms: http.retry_after_ms.unwrap_or(interval),
            },
            status => Failure {
                error: format!("HTTP {status}"),
                wait_ms: http.retry_after_ms.unwrap_or(2 * interval),
            },
        },
        UsageError::Other(message) => {
            let lower = message.to_lowercase();
            let timed_out =
                lower.contains("abort") || lower.contains("timeout") || lower.contains("timed out");
            Failure {
                error: if timed_out {
                    "timed out"
                } else {
                    "unreachable"
                }
                .into(),
                wait_ms: 2 * interval,
            }
        }
    }
}

pub struct UsagePollingOptions {
    pub agent_dir: PathBuf,
    pub report: Arc<dyn Fn(String) + Send + Sync>,
    /// Where polled readings go (the process's subscription limit store).
    pub report_limits: ReportSink,
    /// The signed-in accounts a poll can speak for (usage-accounts.ts `listPollAccounts`).
    pub list_accounts: ListAccounts,
}

/// Stops the poller started by [`start_subscription_usage_polling`].
#[derive(Clone)]
pub struct UsagePollingHandle {
    stop: tokio::sync::watch::Sender<bool>,
}

impl UsagePollingHandle {
    pub fn stop(&self) {
        let _ = self.stop.send(true);
        if let Some(slot) = RUNNING.get() {
            *slot
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
        }
    }
}

static RUNNING: OnceLock<Mutex<Option<UsagePollingHandle>>> = OnceLock::new();

/// Starts the process's poller once (interactive and RPC modes; never print mode or `--offline`).
/// Must run inside a tokio runtime; `report` receives a pass that failed as a whole.
pub fn start_subscription_usage_polling(options: UsagePollingOptions) -> UsagePollingHandle {
    let slot = RUNNING.get_or_init(|| Mutex::new(None));
    let mut running = slot
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(handle) = running.as_ref() {
        return handle.clone();
    }
    let poller = Arc::new(UsagePoller::new(UsagePollerDeps {
        report: options.report_limits,
        cache_path: options.agent_dir.join("subscription-usage.json"),
        list_accounts: options.list_accounts,
        fetch: fetch_usage_json(),
        now: Arc::new(|| chrono::Utc::now().timestamp_millis()),
    }));
    let (stop, signal) = tokio::sync::watch::channel(false);
    let report = options.report;
    let mut stopped = signal.clone();
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(TICK);
        loop {
            tokio::select! {
                _ = stopped.changed() => break,
                _ = ticker.tick() => {
                    let poller = Arc::clone(&poller);
                    let signal = signal.clone();
                    let report = Arc::clone(&report);
                    // A panicking pass is contained in its own task and surfaced like a failed one.
                    tokio::spawn(async move {
                        if let Err(error) = tokio::spawn(async move { poller.poll_once(signal).await }).await {
                            report(format!("maho subscription limits: usage poll failed: {error}"));
                        }
                    });
                }
            }
        }
    });
    let handle = UsagePollingHandle { stop };
    *running = Some(handle.clone());
    handle
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::subscription_limits::usage_endpoints::UsageHttpError;
    use serde_json::json;

    const INTERVAL: i64 = 1000;

    fn http(status: u16, retry_after_ms: Option<i64>) -> UsageError {
        UsageHttpError {
            status,
            retry_after_ms,
        }
        .into()
    }

    #[test]
    fn failures_are_described_with_their_waits() {
        assert_eq!(
            describe_failure(&http(401, None), INTERVAL),
            Failure {
                error: SIGN_IN_NEEDED.into(),
                wait_ms: 5000
            }
        );
        assert_eq!(describe_failure(&http(429, None), INTERVAL).wait_ms, 1000);
        assert_eq!(
            describe_failure(&http(429, Some(7)), INTERVAL),
            Failure {
                error: "rate limited".into(),
                wait_ms: 7
            }
        );
        assert_eq!(
            describe_failure(&http(500, None), INTERVAL),
            Failure {
                error: "HTTP 500".into(),
                wait_ms: 2000
            }
        );
        assert_eq!(
            describe_failure(&UsageError::Other("request timed out".into()), INTERVAL).error,
            "timed out"
        );
        assert_eq!(
            describe_failure(&UsageError::Other("dns".into()), INTERVAL).error,
            "unreachable"
        );
    }

    #[tokio::test]
    async fn a_pass_claims_fetches_reports_and_then_waits() {
        let directory = tempfile::tempdir().expect("tempdir");
        let reports: Arc<Mutex<Vec<PollReport>>> = Arc::default();
        let sink = Arc::clone(&reports);
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counted = Arc::clone(&calls);
        let fetch: UsageFetch = Arc::new(move |_url, _headers| {
            counted.fetch_add(1, Ordering::SeqCst);
            Box::pin(async { Ok(json!({"limits": [{"kind": "session", "percent": 5}]})) })
        });
        let account = PollAccount {
            provider: UsageProvider::AnthropicSubscription,
            account: "default".into(),
            access: Arc::new(|_| Box::pin(async { Some(Ok("token".to_owned())) })),
        };
        let poller = UsagePoller::new(UsagePollerDeps {
            report: Arc::new(move |_, _, report| sink.lock().expect("lock").push(report)),
            cache_path: directory.path().join("subscription-usage.json"),
            list_accounts: Arc::new(move || {
                let account = account.clone();
                Box::pin(async move { vec![account] })
            }),
            fetch,
            now: Arc::new(|| 1_000),
        });
        let (_stop, signal) = tokio::sync::watch::channel(false);
        poller.poll_once(signal.clone()).await;
        poller.poll_once(signal).await;
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        let reports = reports.lock().expect("lock");
        assert_eq!(reports.len(), 1);
        assert!(
            matches!(&reports[0], PollReport::Windows { windows, .. } if windows[0].id == "claude:5h")
        );
    }
}
