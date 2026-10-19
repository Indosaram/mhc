//! Mutation state lock for ulw-loop.

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::runtime::UlwLoopError;

pub const ULW_LOOP_LOCK_TIMEOUT_CODE: &str = "ULW_LOOP_LOCK_TIMEOUT";
pub const DEFAULT_TIMEOUT_MS: u64 = 10_000;
pub const DEFAULT_STALE_MS: u64 = 60_000;
pub const DEFAULT_LEASE_MS: u64 = 30_000;

#[derive(Clone, Debug)]
pub struct StateLockOptions {
    pub timeout_ms: Option<u64>,
    pub stale_ms: Option<u64>,
    pub lease_ms: Option<u64>,
    pub heartbeat_ms: Option<u64>,
}

impl Default for StateLockOptions {
    fn default() -> Self {
        Self {
            timeout_ms: Some(DEFAULT_TIMEOUT_MS),
            stale_ms: Some(DEFAULT_STALE_MS),
            lease_ms: Some(DEFAULT_LEASE_MS),
            heartbeat_ms: None,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LockRecord {
    pub pid: u32,
    #[serde(rename = "createdAt")]
    pub created_at: String,
    pub token: String,
    #[serde(rename = "leaseUntil", default, skip_serializing_if = "Option::is_none")]
    pub lease_until: Option<u64>,
}

fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn is_pid_alive(pid: u32) -> bool {
    #[cfg(target_os = "linux")]
    {
        Path::new(&format!("/proc/{pid}")).exists()
    }
    // Other unix kernels have no /proc: `kill(pid, 0)` answers liveness without signalling, and
    // EPERM means the pid exists under another user. A pid that is not a valid positive pid_t is
    // never probed (`kill(-1, 0)` would ask about every process).
    #[cfg(all(unix, not(target_os = "linux")))]
    {
        let Some(pid) = i32::try_from(pid).ok().and_then(rustix::process::Pid::from_raw) else {
            return false;
        };
        match rustix::process::test_kill_process(pid) {
            Ok(()) => true,
            Err(error) => error == rustix::io::Errno::PERM,
        }
    }
    #[cfg(windows)]
    {
        use windows_sys::Win32::Foundation::{CloseHandle, STILL_ACTIVE};
        use windows_sys::Win32::System::Threading::{
            GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
        };

        // SAFETY: OpenProcess takes plain integers; a null handle is checked before any use.
        let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
        if handle.is_null() {
            return false;
        }
        let mut exit_code = 0u32;
        // SAFETY: `handle` is a live process handle and `exit_code` outlives the call.
        let queried = unsafe { GetExitCodeProcess(handle, &mut exit_code) };
        // SAFETY: `handle` came from OpenProcess and is closed exactly once.
        unsafe { CloseHandle(handle) };
        queried != 0 && exit_code == STILL_ACTIVE as u32
    }
    #[cfg(not(any(unix, windows)))]
    {
        true
    }
}

pub fn is_state_lock_timeout(error: &UlwLoopError) -> bool {
    error.code == ULW_LOOP_LOCK_TIMEOUT_CODE
}

pub fn with_state_lock_sync<T>(
    lock_path: impl AsRef<Path>,
    options: Option<&StateLockOptions>,
    f: impl FnOnce(&str) -> Result<T, UlwLoopError>,
) -> Result<T, UlwLoopError> {
    let default_opts = StateLockOptions::default();
    let opts = options.unwrap_or(&default_opts);
    let timeout_ms = opts.timeout_ms.unwrap_or(DEFAULT_TIMEOUT_MS);
    let stale_ms = opts.stale_ms.unwrap_or(DEFAULT_STALE_MS);
    let lease_ms = opts.lease_ms.unwrap_or(DEFAULT_LEASE_MS);

    let path = lock_path.as_ref();
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }

    let token = uuid::Uuid::new_v4().to_string();
    let start = Instant::now();
    let timeout = Duration::from_millis(timeout_ms);

    loop {
        let record = LockRecord {
            pid: std::process::id(),
            created_at: crate::runtime::iso(),
            token: token.clone(),
            lease_until: Some(now_millis() + lease_ms),
        };
        let content = serde_json::to_string(&record).unwrap_or_default();

        match OpenOptions::new().write(true).create_new(true).open(path) {
            Ok(mut file) => {
                let _ = file.write_all(content.as_bytes());
                let _ = file.flush();
                break;
            }
            Err(_) => {
                // Check if existing lock is stale or expired
                let mut can_reclaim = false;
                if let Ok(existing_content) = fs::read_to_string(path) {
                    if let Ok(existing_record) = serde_json::from_str::<LockRecord>(&existing_content) {
                        let now = now_millis();
                        if let Some(lease_until) = existing_record.lease_until {
                            if now > lease_until {
                                can_reclaim = true;
                            }
                        } else if !is_pid_alive(existing_record.pid) {
                            can_reclaim = true;
                        } else if let Ok(metadata) = fs::metadata(path) {
                            if let Ok(modified) = metadata.modified() {
                                if let Ok(elapsed) = modified.elapsed() {
                                    if elapsed.as_millis() as u64 > stale_ms {
                                        can_reclaim = true;
                                    }
                                }
                            }
                        }
                    } else if let Ok(metadata) = fs::metadata(path) {
                        if let Ok(modified) = metadata.modified() {
                            if let Ok(elapsed) = modified.elapsed() {
                                can_reclaim = elapsed.as_millis() as u64 > stale_ms;
                            }
                        }
                    }
                } else {
                    can_reclaim = false;
                }

                if can_reclaim {
                    let _ = fs::remove_file(path);
                    continue;
                }

                if start.elapsed() >= timeout {
                    return Err(UlwLoopError::new(
                        format!("Timed out acquiring ulw-loop state lock at {}", path.display()),
                        ULW_LOOP_LOCK_TIMEOUT_CODE,
                    ));
                }

                std::thread::sleep(Duration::from_millis(50));
            }
        }
    }

    let result = f(&token);

    // Release lock if we still own it
    if let Ok(current_content) = fs::read_to_string(path) {
        if let Ok(current_record) = serde_json::from_str::<LockRecord>(&current_content) {
            if current_record.token == token {
                let _ = fs::remove_file(path);
            }
        }
    }

    result
}

pub fn with_state_lock<T>(
    lock_path: impl AsRef<Path>,
    options: Option<&StateLockOptions>,
    f: impl FnOnce(&str) -> Result<T, UlwLoopError>,
) -> Result<T, UlwLoopError> {
    with_state_lock_sync(lock_path, options, f)
}
