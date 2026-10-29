//! Cross-process per-session admission lease (`lifecycle/admission-lease.ts`).
//!
//! The lease file is created with a hard link (atomic create-if-absent); a stale lease is taken
//! over by a compare-and-swap under the record lock, and a holder thread renews `renewed_at`.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::store::{StoreError, with_task_record_lock};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AdmissionLeaseTiming {
    pub renew_ms: u64,
    pub stale_ms: u64,
    pub acquire_timeout_ms: u64,
    pub retry_ms: u64,
}

/// Partial timing overrides; unset fields take defaults (`stale_ms` defaults to 3x renew).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct AdmissionLeaseTimingOverrides {
    pub renew_ms: Option<u64>,
    pub stale_ms: Option<u64>,
    pub acquire_timeout_ms: Option<u64>,
    pub retry_ms: Option<u64>,
}

const DEFAULT_ACQUIRE_TIMEOUT_MS: u64 = 5_000;
const DEFAULT_RETRY_MS: u64 = 25;

pub fn resolve_admission_lease_timing(
    overrides: AdmissionLeaseTimingOverrides,
) -> AdmissionLeaseTiming {
    let renew_ms = overrides.renew_ms.unwrap_or(1_000);
    AdmissionLeaseTiming {
        renew_ms,
        stale_ms: overrides.stale_ms.unwrap_or(renew_ms * 3),
        acquire_timeout_ms: overrides
            .acquire_timeout_ms
            .unwrap_or(DEFAULT_ACQUIRE_TIMEOUT_MS),
        retry_ms: overrides.retry_ms.unwrap_or(DEFAULT_RETRY_MS),
    }
}

pub fn admission_lease_path(state_dir: &Path, parent_session_id: &str) -> PathBuf {
    state_dir
        .join("locks")
        .join(format!("session-{parent_session_id}.lock"))
}

/// A held admission lease: ownership is re-read from disk on every check.
pub trait SessionAdmissionLease: Send + Sync {
    fn token(&self) -> &str;
    fn path(&self) -> &Path;
    fn is_owner(&self) -> bool;
    fn release(&self);
}

pub enum AcquireAdmissionLeaseResult {
    Acquired(Arc<dyn SessionAdmissionLease>),
    Contended,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct LeaseBody {
    pid: i64,
    token: String,
    renewed_at: i64,
}

enum Observed {
    Missing,
    Corrupt,
    Body(LeaseBody),
}

fn now_ms() -> i64 {
    i64::try_from(crate::state::system_now_ms()).unwrap_or(i64::MAX)
}

fn random_hex(bytes: usize) -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let mut out = String::with_capacity(bytes * 2);
    while out.len() < bytes * 2 {
        let seed = COUNTER.fetch_add(1, Ordering::Relaxed);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_nanos());
        let mut state = (nanos as u64) ^ seed.rotate_left(17) ^ u64::from(std::process::id()) << 32;
        // splitmix64 finaliser: tokens only need to be unique, not unpredictable.
        state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        state = (state ^ (state >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        state = (state ^ (state >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        state ^= state >> 31;
        out.push_str(&format!("{state:016x}"));
    }
    out.truncate(bytes * 2);
    out
}

fn body_json(body: &LeaseBody) -> String {
    json!({ "pid": body.pid, "token": body.token, "renewed_at": body.renewed_at }).to_string()
}

fn own_body(token: &str) -> LeaseBody {
    LeaseBody {
        pid: i64::from(std::process::id()),
        token: token.to_string(),
        renewed_at: now_ms(),
    }
}

fn tmp_path(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(format!(".{}.{}.tmp", std::process::id(), random_hex(6)));
    PathBuf::from(name)
}

fn read_lease_body(path: &Path) -> Observed {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Observed::Missing,
        Err(_) => return Observed::Corrupt,
    };
    let Ok(Value::Object(map)) = serde_json::from_str::<Value>(&text) else {
        return Observed::Corrupt;
    };
    match (
        map.get("pid").and_then(Value::as_f64),
        map.get("token").and_then(Value::as_str),
        map.get("renewed_at").and_then(Value::as_f64),
    ) {
        (Some(pid), Some(token), Some(renewed_at)) => Observed::Body(LeaseBody {
            pid: pid as i64,
            token: token.to_string(),
            renewed_at: renewed_at as i64,
        }),
        _ => Observed::Corrupt,
    }
}

fn holds_token(path: &Path, token: &str) -> bool {
    matches!(read_lease_body(path), Observed::Body(body) if body.token == token)
}

fn try_create_lease(path: &Path, body: &LeaseBody) -> Result<bool, StoreError> {
    let tmp = tmp_path(path);
    fs::write(&tmp, body_json(body))?;
    let linked = fs::hard_link(&tmp, path);
    let _ = fs::remove_file(&tmp);
    match linked {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Ok(false),
        Err(error) => Err(error.into()),
    }
}

fn write_lease_atomic(path: &Path, body: &LeaseBody) -> Result<(), StoreError> {
    let tmp = tmp_path(path);
    fs::write(&tmp, body_json(body))?;
    fs::rename(&tmp, path)?;
    Ok(())
}

fn try_takeover(path: &Path, observed: &Observed, token: &str, stale_ms: u64) -> bool {
    let stale = i64::try_from(stale_ms).unwrap_or(i64::MAX);
    with_task_record_lock(path, || {
        let fresh = read_lease_body(path);
        let won = match (observed, &fresh) {
            (_, Observed::Missing) => false,
            (Observed::Corrupt, Observed::Corrupt) => true,
            (Observed::Corrupt, _) | (_, Observed::Corrupt) => false,
            (Observed::Body(seen), Observed::Body(current)) => {
                // Another waiter won the CAS first, or the holder renewed meanwhile.
                current.token == seen.token && now_ms() - current.renewed_at > stale
            }
            (Observed::Missing, Observed::Body(_)) => false,
        };
        if won {
            write_lease_atomic(path, &own_body(token))?;
        }
        Ok(won)
    })
    // Mutex contention timeout: the acquire loop retries or yields contended.
    .unwrap_or(false)
}

pub fn acquire_session_admission_lease(
    state_dir: &Path,
    parent_session_id: &str,
    overrides: AdmissionLeaseTimingOverrides,
) -> Result<AcquireAdmissionLeaseResult, StoreError> {
    let timing = resolve_admission_lease_timing(overrides);
    let path = admission_lease_path(state_dir, parent_session_id);
    fs::create_dir_all(state_dir.join("locks"))?;
    let token = random_hex(16);
    let started = Instant::now();
    loop {
        if try_create_lease(&path, &own_body(&token))? {
            return Ok(AcquireAdmissionLeaseResult::Acquired(start_holder(
                path, token, timing,
            )));
        }
        let observed = read_lease_body(&path);
        let stale = match &observed {
            // Released between our create attempt and this read.
            Observed::Missing => continue,
            Observed::Corrupt => true,
            Observed::Body(body) => {
                now_ms() - body.renewed_at > i64::try_from(timing.stale_ms).unwrap_or(i64::MAX)
            }
        };
        if stale && try_takeover(&path, &observed, &token, timing.stale_ms) {
            return Ok(AcquireAdmissionLeaseResult::Acquired(start_holder(
                path, token, timing,
            )));
        }
        if started.elapsed() >= Duration::from_millis(timing.acquire_timeout_ms) {
            return Ok(AcquireAdmissionLeaseResult::Contended);
        }
        std::thread::sleep(Duration::from_millis(timing.retry_ms));
    }
}

struct RenewStop {
    stopped: Mutex<bool>,
    wake: Condvar,
}

struct HeldLease {
    token: String,
    path: PathBuf,
    stop: Arc<RenewStop>,
    released: AtomicBool,
}

fn start_holder(
    path: PathBuf,
    token: String,
    timing: AdmissionLeaseTiming,
) -> Arc<dyn SessionAdmissionLease> {
    let stop = Arc::new(RenewStop {
        stopped: Mutex::new(false),
        wake: Condvar::new(),
    });
    let renew_stop = Arc::clone(&stop);
    let renew_path = path.clone();
    let renew_token = token.clone();
    let interval = Duration::from_millis(timing.renew_ms.max(1));
    std::thread::spawn(move || renew_loop(&renew_stop, &renew_path, &renew_token, interval));
    Arc::new(HeldLease {
        token,
        path,
        stop,
        released: AtomicBool::new(false),
    })
}

fn renew_loop(stop: &RenewStop, path: &Path, token: &str, interval: Duration) {
    let mut stopped = stop.stopped.lock().unwrap_or_else(PoisonError::into_inner);
    loop {
        let (guard, _) = stop
            .wake
            .wait_timeout(stopped, interval)
            .unwrap_or_else(PoisonError::into_inner);
        stopped = guard;
        if *stopped {
            return;
        }
        let still_held = with_task_record_lock(path, || {
            if !holds_token(path, token) {
                return Ok(false);
            }
            write_lease_atomic(path, &own_body(token))?;
            Ok(true)
        });
        // A displaced holder stops renewing; lock contention just retries next tick.
        if matches!(still_held, Ok(false)) {
            return;
        }
    }
}

impl HeldLease {
    fn stop_renewal(&self) {
        *self
            .stop
            .stopped
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = true;
        self.stop.wake.notify_all();
    }
}

impl SessionAdmissionLease for HeldLease {
    fn token(&self) -> &str {
        &self.token
    }

    fn path(&self) -> &Path {
        &self.path
    }

    fn is_owner(&self) -> bool {
        holds_token(&self.path, &self.token)
    }

    fn release(&self) {
        self.stop_renewal();
        if self.released.swap(true, Ordering::SeqCst) {
            return;
        }
        let _ = with_task_record_lock(&self.path, || {
            if holds_token(&self.path, &self.token) {
                match fs::remove_file(&self.path) {
                    Ok(()) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error) => return Err(error.into()),
                }
            }
            Ok(())
        });
    }
}

impl Drop for HeldLease {
    fn drop(&mut self) {
        self.stop_renewal();
    }
}
