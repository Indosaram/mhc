//! Single-flight cold-wake session opener (`thread/wake/session-waker.ts`).

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use serde_json::Value;

use super::super::store_api::GatewayStoreApi;
use super::super::woken_types::{WakeRetryOutcome, WakeStartOutcome};
use super::legacy_open::{WakeOpen, WakeOpenReply, WakeOpenRequest};
use super::session_target::{WakeSessionKind, WakeTarget, resolve_wake_target};

pub type BoxFuture<T> = Pin<Box<dyn Future<Output = T> + Send>>;

#[derive(Debug, Clone)]
pub enum WakeOutcome {
    Live {
        session_id: String,
        state: Value,
        generation: i64,
    },
    Gone,
    Ambiguous {
        session_paths: Vec<String>,
    },
    NotWakeable {
        session_kind: WakeSessionKind,
    },
    NotYet {
        next_attempt_at: i64,
    },
    Busy {
        code: String,
        retry_after_ms: Option<u64>,
    },
    Deferred {
        attempts: i64,
        next_attempt_at: i64,
    },
    Failed {
        attempts: i64,
        error: String,
    },
}

pub struct SessionWakerOptions {
    pub store: Arc<dyn GatewayStoreApi>,
    pub sessions_directory: PathBuf,
    pub ensure_host: Arc<dyn Fn() -> BoxFuture<()> + Send + Sync>,
    pub open: WakeOpen,
}

fn held_elsewhere(code: &str) -> bool {
    matches!(code, "session_held" | "session_path_in_use")
}

fn definitive(code: &str) -> bool {
    matches!(code, "invalid_path" | "unknown_session")
}

async fn settle_open(
    store: &dyn GatewayStoreApi,
    durable_id: &str,
    generation: i64,
    reply: WakeOpenReply,
) -> WakeOutcome {
    match reply {
        WakeOpenReply::Opened {
            session_id, state, ..
        } => WakeOutcome::Live {
            session_id,
            state,
            generation,
        },
        WakeOpenReply::Refused {
            ref code,
            retry_after_ms,
        } if held_elsewhere(code) || definitive(code) => {
            let _ = store.woken_end(durable_id, generation);
            if definitive(code) {
                WakeOutcome::Gone
            } else {
                WakeOutcome::Busy {
                    code: code.clone(),
                    retry_after_ms,
                }
            }
        }
        other => match store.woken_retry_later(durable_id, generation) {
            Ok(WakeRetryOutcome::LostLease) => WakeOutcome::Busy {
                code: "wake_in_progress".to_owned(),
                retry_after_ms: None,
            },
            Ok(WakeRetryOutcome::Failed { attempts }) => WakeOutcome::Failed {
                attempts,
                error: match other {
                    WakeOpenReply::Refused { code, .. } => code,
                    WakeOpenReply::Unreachable { message } => message,
                    WakeOpenReply::Opened { .. } => String::new(),
                },
            },
            Ok(WakeRetryOutcome::Deferred { row }) => WakeOutcome::Deferred {
                attempts: row.attempts,
                next_attempt_at: row.next_attempt_at.unwrap_or(0),
            },
            Err(err) => WakeOutcome::Failed {
                attempts: 0,
                error: err.to_string(),
            },
        },
    }
}

async fn wake_once(options: &SessionWakerOptions, durable_id: &str) -> WakeOutcome {
    let target = resolve_wake_target(&options.sessions_directory, durable_id);
    let (session_path, cwd) = match target {
        WakeTarget::Gone => return WakeOutcome::Gone,
        WakeTarget::Ambiguous { session_paths } => {
            return WakeOutcome::Ambiguous { session_paths };
        }
        WakeTarget::Found {
            session_path,
            cwd,
            session_kind,
            ..
        } => {
            if session_kind != WakeSessionKind::Interactive {
                return WakeOutcome::NotWakeable { session_kind };
            }
            (session_path, cwd)
        }
    };

    if let Err(err) = options.store.woken_request(durable_id, &session_path) {
        return WakeOutcome::Failed {
            attempts: 0,
            error: err.to_string(),
        };
    }
    let started = match options.store.woken_start(durable_id) {
        Ok(s) => s,
        Err(err) => {
            return WakeOutcome::Failed {
                attempts: 0,
                error: err.to_string(),
            };
        }
    };
    let row = match started {
        WakeStartOutcome::NotYet { next_attempt_at } => {
            return WakeOutcome::NotYet { next_attempt_at };
        }
        WakeStartOutcome::Started { row } => row,
        _ => {
            return WakeOutcome::Busy {
                code: "wake_in_progress".to_owned(),
                retry_after_ms: None,
            };
        }
    };
    (options.ensure_host)().await;
    let reply = (options.open)(WakeOpenRequest { session_path, cwd }).await;
    settle_open(&*options.store, durable_id, row.generation, reply).await
}

pub struct SessionWaker {
    options: Arc<SessionWakerOptions>,
    in_flight: Mutex<HashMap<String, Arc<tokio::sync::Notify>>>,
    results: Mutex<HashMap<String, WakeOutcome>>,
    _markers: Mutex<HashSet<String>>,
}

impl SessionWaker {
    pub fn new(options: SessionWakerOptions) -> Self {
        Self {
            options: Arc::new(options),
            in_flight: Mutex::new(HashMap::new()),
            results: Mutex::new(HashMap::new()),
            _markers: Mutex::new(HashSet::new()),
        }
    }

    pub async fn ensure_session_running(&self, durable_id: &str) -> WakeOutcome {
        let key = format!(
            "{}\0{durable_id}",
            self.options.sessions_directory.display()
        );
        let existing = {
            let mut guard = self
                .in_flight
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if let Some(notify) = guard.get(&key) {
                Some(notify.clone())
            } else {
                guard.insert(key.clone(), Arc::new(tokio::sync::Notify::new()));
                None
            }
        };
        if let Some(notify) = existing {
            notify.notified().await;
            return self
                .results
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .get(&key)
                .cloned()
                .unwrap_or(WakeOutcome::Busy {
                    code: "wake_in_progress".to_owned(),
                    retry_after_ms: None,
                });
        }

        let outcome = wake_once(&self.options, durable_id).await;
        self.results
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(key.clone(), outcome.clone());
        if let Some(notify) = self
            .in_flight
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&key)
        {
            notify.notify_waiters();
        }
        outcome
    }
}
