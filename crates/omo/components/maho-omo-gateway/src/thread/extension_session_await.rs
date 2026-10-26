//! Waiting for a session op's completion without polling the store.
//!
//! Upstream counterpart: `thread/gateway/extension-session-await.ts`. The wake file is armed FIRST,
//! then `status` runs once (a completion that landed before the wait is caught here), then every
//! wake re-checks `status`. Only a final status ends the wait; at the deadline `expire` runs and then
//! `status` once more, so an open that completes during the expiry still returns as opened.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json::Value;

use super::store_extensions::{StoreExtensionRefusalCode, StoreExtensionResult};

/// How often the wake file's existence is rechecked while waiting: a dropped watch event must not
/// lose a completion, and the file is written only after the completion commits.
pub const WAKE_FILE_RECHECK_MS: u64 = 1_000;

pub struct SessionAwait<'a> {
    /// `<store dir>/<wakeDir>/<await_request_id>`: the connector touches it after its completion commits.
    pub file: PathBuf,
    pub timeout_ms: i64,
    pub status: &'a dyn Fn() -> StoreExtensionResult<Value>,
    pub expire: &'a dyn Fn() -> StoreExtensionResult<Value>,
    /// Called once, when the wait is armed and a status read answered not final.
    pub on_armed: Option<&'a dyn Fn()>,
}

/// A status or expire call that fails answers like a refusal, so it never ends the wait early.
fn settle(read: &dyn Fn() -> StoreExtensionResult<Value>) -> StoreExtensionResult<Value> {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(read)) {
        Ok(result) => result,
        Err(_) => StoreExtensionResult::refused(StoreExtensionRefusalCode::ExtensionOperationFailed, "the extension operation failed"),
    }
}

/// Only a status the op itself reports as not `pending` is final; a refused status call never is.
fn is_final(result: &StoreExtensionResult<Value>) -> bool {
    match result {
        StoreExtensionResult::Ok { value } => value.get("status").and_then(Value::as_str) != Some("pending"),
        StoreExtensionResult::Refused { .. } => false,
    }
}

fn unresolved(id: &str, why: &str) -> StoreExtensionResult<Value> {
    StoreExtensionResult::refused(
        StoreExtensionRefusalCode::AwaitUnresolved,
        format!("The request {id} has no final status: {why}. It may still complete; read its status again before retrying, so a retry does not repeat it."),
    )
}

fn request_id(file: &Path) -> String {
    file.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default()
}

/// The deadline: `expire`, then one final `status`.
fn expire_and_read(request: &SessionAwait<'_>) -> StoreExtensionResult<Value> {
    let id = request_id(&request.file);
    let expired = settle(request.expire);
    if let StoreExtensionResult::Refused { code, message } = &expired {
        return unresolved(&id, &format!("expiring it was refused ({}: {message})", code.as_str()));
    }
    let last = settle(request.status);
    if is_final(&last) {
        return last;
    }
    match &last {
        StoreExtensionResult::Ok { .. } => unresolved(&id, "its status still reads pending after it was expired"),
        StoreExtensionResult::Refused { code, message } => unresolved(&id, &format!("its status after expiry was refused ({}: {message})", code.as_str())),
    }
}

pub fn await_session_request(request: SessionAwait<'_>) -> StoreExtensionResult<Value> {
    let Some(directory) = request.file.parent() else {
        let current = settle(request.status);
        return if is_final(&current) { current } else { expire_and_read(&request) };
    };
    if !directory.exists() {
        // No connector has started, so nothing can complete the request: it expires at once.
        let current = settle(request.status);
        return if is_final(&current) { current } else { expire_and_read(&request) };
    }
    let deadline = Instant::now() + Duration::from_millis(request.timeout_ms.max(0) as u64);
    let mut saw_wake_file = false;
    let mut announced = false;
    loop {
        let current = settle(request.status);
        if is_final(&current) {
            return current;
        }
        if !saw_wake_file && request.file.exists() {
            saw_wake_file = true;
        }
        let now = Instant::now();
        if now >= deadline {
            break;
        }
        if !announced {
            announced = true;
            if let Some(on_armed) = request.on_armed {
                on_armed();
            }
        }
        let remaining = deadline.saturating_duration_since(now);
        // Upstream watches the wake file and its directory; with no watcher in this workspace the
        // documented recheck interval is the only mechanism, so a completion is seen within it.
        std::thread::sleep(remaining.min(Duration::from_millis(WAKE_FILE_RECHECK_MS)));
    }
    expire_and_read(&request)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn ok(value: Value) -> StoreExtensionResult<Value> {
        StoreExtensionResult::ok(value)
    }

    #[test]
    fn a_final_status_ends_the_wait_without_a_deadline() {
        let directory = tempfile::tempdir().expect("temp dir");
        let file = directory.path().join("alpha").join("tor_0123456789abcdef0123456789abcdef");
        std::fs::create_dir_all(directory.path().join("alpha")).expect("wake dir");
        let status = || ok(serde_json::json!({ "status": "opened", "value": 1 }));
        let expire = || panic!("expire must not run");
        let result = await_session_request(SessionAwait { file, timeout_ms: 60_000, status: &status, expire: &expire, on_armed: None });
        assert_eq!(result, ok(serde_json::json!({ "status": "opened", "value": 1 })));
    }

    #[test]
    fn a_wake_file_rechecks_the_status_and_ends_on_its_final_value() {
        let directory = tempfile::tempdir().expect("temp dir");
        let wake_dir = directory.path().join("alpha");
        std::fs::create_dir_all(&wake_dir).expect("wake dir");
        let file = wake_dir.join("tor_0123456789abcdef0123456789abcdef");
        std::fs::write(&file, b"{}").expect("wake file");
        let reads = AtomicUsize::new(0);
        let status = || {
            if reads.fetch_add(1, Ordering::SeqCst) == 0 {
                ok(serde_json::json!({ "status": "pending" }))
            } else {
                ok(serde_json::json!({ "status": "opened" }))
            }
        };
        let expire = || panic!("expire must not run");
        let result = await_session_request(SessionAwait { file, timeout_ms: 60_000, status: &status, expire: &expire, on_armed: None });
        assert_eq!(result, ok(serde_json::json!({ "status": "opened" })));
    }

    #[test]
    fn a_missing_wake_directory_expires_at_once() {
        let directory = tempfile::tempdir().expect("temp dir");
        let file = directory.path().join("missing").join("tor_0123456789abcdef0123456789abcdef");
        let expired = std::cell::Cell::new(false);
        let status = || ok(serde_json::json!({ "status": if expired.get() { "expired" } else { "pending" } }));
        let expire = || {
            expired.set(true);
            ok(serde_json::json!({ "expired": true }))
        };
        let result = await_session_request(SessionAwait { file, timeout_ms: 60_000, status: &status, expire: &expire, on_armed: None });
        assert_eq!(result, ok(serde_json::json!({ "status": "expired" })));
    }

    #[test]
    fn an_expired_request_that_stays_pending_is_unresolved_not_failed() {
        let directory = tempfile::tempdir().expect("temp dir");
        let wake_dir = directory.path().join("alpha");
        std::fs::create_dir_all(&wake_dir).expect("wake dir");
        let file = wake_dir.join("tor_0123456789abcdef0123456789abcdef");
        let status = || ok(serde_json::json!({ "status": "pending" }));
        let expire = || ok(serde_json::json!({ "status": "pending" }));
        let result = await_session_request(SessionAwait { file, timeout_ms: 1, status: &status, expire: &expire, on_armed: None });
        assert!(matches!(result, StoreExtensionResult::Refused { code, .. } if code == StoreExtensionRefusalCode::AwaitUnresolved));
    }
}
