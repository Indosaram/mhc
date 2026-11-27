//! Port of senpi `packages/agent/src/durable/testing/store-harness.ts`.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use maho_ai::types::BoxFuture;
use serde::Serialize;
use serde_json::Value;

use crate::durable::store::{
    CommitResult, DurableStore, DurableStoreError, DurableStoreErrorCode, DurableStoreOptions,
    StoreOperation, StoreSnapshot, StoreWritePoint,
};

/// What a host provides to the STORE and KILL-STORE cases.
pub trait StoreConformanceAdapter: Send + Sync {
    /// A new, empty durable medium; its name opens it again.
    fn create_medium(&self) -> BoxFuture<'_, Result<String, DurableStoreError>>;
    /// A store instance over `medium`. Instances opened on one medium share its durable state.
    fn open(
        &self,
        medium: &str,
        options: DurableStoreOptions,
    ) -> BoxFuture<'_, Result<Arc<dyn DurableStore>, DurableStoreError>>;
}

#[derive(Clone)]
pub struct StoreCaseAdapters {
    pub store: Arc<dyn StoreConformanceAdapter>,
}

pub fn is_cut_signal(error: &DurableStoreError) -> bool {
    error.is_cut()
}

fn same_point(a: StoreWritePoint, b: StoreWritePoint) -> bool {
    match (a, b) {
        (StoreWritePoint::Before { op: op_a }, StoreWritePoint::Before { op: op_b }) => {
            op_a == op_b
        }
        (
            StoreWritePoint::Write {
                op: op_a,
                index: ia,
            },
            StoreWritePoint::Write {
                op: op_b,
                index: ib,
            },
        ) => op_a == op_b && ia == ib,
        (StoreWritePoint::Durable { op: op_a }, StoreWritePoint::Durable { op: op_b }) => {
            op_a == op_b
        }
        _ => false,
    }
}

pub fn cut_once_at(target: StoreWritePoint) -> DurableStoreOptions {
    let armed = Arc::new(AtomicBool::new(true));
    DurableStoreOptions {
        on_write_point: Some(Arc::new(move |point| {
            if same_point(point, target) && armed.swap(false, Ordering::SeqCst) {
                return Err(DurableStoreError::Cut(format!(
                    "cut at {}",
                    serde_json::to_string(&point).unwrap_or_default()
                )));
            }
            Ok(())
        })),
    }
}

pub fn cut_op_at(op: StoreOperation, at: &str) -> StoreWritePoint {
    match at {
        "before" => StoreWritePoint::Before { op },
        _ => StoreWritePoint::Durable { op },
    }
}

pub fn canonical_value(value: &Value) -> String {
    match value {
        Value::Null => "null".to_owned(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        Value::String(s) => serde_json::to_string(s).unwrap_or_else(|_| "\"\"".to_owned()),
        Value::Array(items) => {
            let parts: Vec<String> = items.iter().map(canonical_value).collect();
            format!("[{}]", parts.join(","))
        }
        Value::Object(map) => {
            let mut entries: Vec<(&String, &Value)> = map.iter().collect();
            entries.sort_by_key(|(a, _)| *a);
            let parts: Vec<String> = entries
                .into_iter()
                .map(|(k, v)| {
                    format!(
                        "{}:{}",
                        serde_json::to_string(k).unwrap_or_else(|_| "\"\"".to_owned()),
                        canonical_value(v)
                    )
                })
                .collect();
            format!("{{{}}}", parts.join(","))
        }
    }
}

pub fn expect_equal<T: Serialize, U: Serialize>(
    actual: &T,
    expected: &U,
    what: &str,
) -> Result<(), String> {
    let actual_val = serde_json::to_value(actual).map_err(|e| e.to_string())?;
    let expected_val = serde_json::to_value(expected).map_err(|e| e.to_string())?;
    let a = canonical_value(&actual_val);
    let e = canonical_value(&expected_val);
    if a != e {
        return Err(format!("{what}: expected {e}, got {a}"));
    }
    Ok(())
}

pub fn expect_store_error<T>(
    result: Result<T, DurableStoreError>,
    code: DurableStoreErrorCode,
    what: &str,
) -> Result<(), String> {
    match result {
        Err(ref err) if err.code() == Some(code) => Ok(()),
        Err(err) => Err(format!(
            "{what}: expected DurableStoreError {code:?}, got {err}"
        )),
        Ok(_) => Err(format!(
            "{what}: expected DurableStoreError {code:?}, but it resolved"
        )),
    }
}

pub fn expect_cut<T>(result: Result<T, DurableStoreError>, what: &str) -> Result<(), String> {
    match result {
        Err(ref err) if is_cut_signal(err) => Ok(()),
        Err(err) => Err(format!("{what}: expected the cut to surface, got {err}")),
        Ok(_) => Err(format!(
            "{what}: expected the cut to surface, but it resolved"
        )),
    }
}

pub async fn reopen_snapshot(
    adapter: &dyn StoreConformanceAdapter,
    medium: &str,
) -> Result<StoreSnapshot, String> {
    let store = adapter
        .open(medium, DurableStoreOptions::default())
        .await
        .map_err(|e| e.to_string())?;
    store.export_snapshot().await.map_err(|e| e.to_string())
}

#[derive(Clone, Default)]
pub struct AdoptLog {
    calls: Arc<Mutex<Vec<String>>>,
}

impl AdoptLog {
    pub fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap_or_else(|p| p.into_inner()).clone()
    }

    pub fn record(&self, result: &CommitResult) {
        let joined: Vec<String> = result.seqs.iter().map(ToString::to_string).collect();
        self.calls
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .push(format!("adopt:{}", joined.join(",")));
    }

    pub fn callback(&self) -> Box<dyn FnOnce(&CommitResult) + Send> {
        let log = self.clone();
        Box::new(move |result| log.record(result))
    }
}

pub fn adopt_log() -> AdoptLog {
    AdoptLog::default()
}
