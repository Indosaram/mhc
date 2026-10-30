//! Port of senpi `terminal/manifest-lease.ts` and `terminal/lease-file.ts` on the native v1 lease
//! record `{pid, startedAtMs}` (no token or boot fields; `startedAtMs` is the acquisition instant).
//!
//! A lease file is never visible half-written: it is written to a private temp file and published
//! with `hard_link` (exclusive) or `rename` (replace). Every removal by an acquire goes through
//! `reclaim_inspected`: under an exclusive `<lease>.lock`, and only while the file still holds
//! exactly the bytes that were judged stale, so a fresh lease another waiter just published is
//! never deleted and two concurrent reclaims cannot both acquire.
use crate::process_identity::{
    PROCESS_START_TOLERANCE_MS, own_process_started_at_ms, process_boot_at_ms, same_boot,
    same_process_start,
};
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// A reclaim lock that cannot be parsed was left by a crash mid-write; past this age it is abandoned.
const UNREADABLE_LOCK_STALE_MS: u128 = 30_000;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LeaseRecord {
    pub pid: f64,
    pub started_at_ms: f64,
}
#[derive(Debug, PartialEq)]
pub enum AcquireTerminalLeaseResult {
    Acquired { path: PathBuf, pid: f64 },
    Held { holder: LeaseRecord },
}

/// `LeaseClassification` without `self` (the native record carries no generation token).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LeaseClassification {
    Dead,
    Reused,
    LiveForeign,
}

fn read_text(path: &Path) -> std::io::Result<Option<Vec<u8>>> {
    match std::fs::read(path) {
        Ok(raw) => Ok(Some(raw)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

fn parse_record(raw: &[u8]) -> Option<LeaseRecord> {
    serde_json::from_slice::<LeaseRecord>(raw)
        .ok()
        .filter(|record| record.pid.is_finite() && record.started_at_ms.is_finite())
}

fn unlink_if_present(path: &Path) -> std::io::Result<()> {
    match std::fs::remove_file(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        result => result,
    }
}

fn unique_suffix() -> String {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.subsec_nanos());
    format!(
        "{}.{:08x}{}",
        std::process::id(),
        nanos,
        NEXT.fetch_add(1, Ordering::Relaxed)
    )
}

fn write_temp(path: &Path, content: &[u8]) -> std::io::Result<PathBuf> {
    let mut name = path.as_os_str().to_owned();
    name.push(format!(".{}.tmp", unique_suffix()));
    let temp = PathBuf::from(name);
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp)?;
    if let Err(error) = file.write_all(content) {
        drop(file);
        unlink_if_present(&temp)?;
        return Err(error);
    }
    Ok(temp)
}

/// `publishExclusive`: publish `content` only if nothing is at `path` (`AlreadyExists` otherwise).
pub fn publish_exclusive(path: &Path, content: &[u8]) -> std::io::Result<()> {
    let temp = write_temp(path, content)?;
    let linked = match std::fs::hard_link(&temp, path) {
        Err(error)
            if error.kind() == std::io::ErrorKind::Unsupported
                || error.kind() == std::io::ErrorKind::PermissionDenied =>
        {
            // A filesystem without hard links: exclusive create (briefly visible empty).
            std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(path)
                .and_then(|mut file| file.write_all(content))
        }
        result => result,
    };
    unlink_if_present(&temp)?;
    linked
}

/// `publishReplace`: atomically replace whatever is at `path`.
pub fn publish_replace(path: &Path, content: &[u8]) -> std::io::Result<()> {
    let temp = write_temp(path, content)?;
    std::fs::rename(&temp, path).inspect_err(|_| {
        let _ = std::fs::remove_file(&temp);
    })
}

/// Who holds a reclaim lock: pid plus boot and start instants, so a reused pid is never it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReclaimLockHolder {
    pub pid: f64,
    pub boot_at_ms: f64,
    pub process_started_at_ms: f64,
}

#[derive(Debug, PartialEq)]
pub enum LockState {
    Free,
    Held(Option<ReclaimLockHolder>),
    Stale(Vec<u8>),
}

/// Liveness and start-instant probes shared by the lease classifier and the reclaim lock.
pub struct LeaseProbes<'a> {
    pub is_process_alive: &'a mut dyn FnMut(f64) -> std::io::Result<bool>,
    pub read_process_start_ms: &'a mut dyn FnMut(f64) -> Option<f64>,
}

fn alive(probes: &mut LeaseProbes<'_>, pid: f64) -> std::io::Result<bool> {
    match (probes.is_process_alive)(pid) {
        Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        result => result,
    }
}

/// `reclaimLockState`: a lock is held only by the very process that wrote it.
pub fn reclaim_lock_state(lock: &Path, probes: &mut LeaseProbes<'_>) -> std::io::Result<LockState> {
    let Some(raw) = read_text(lock)? else {
        return Ok(LockState::Free);
    };
    let Ok(holder) = serde_json::from_slice::<ReclaimLockHolder>(&raw) else {
        let age = std::fs::metadata(lock)
            .and_then(|meta| meta.modified())
            .ok()
            .and_then(|modified| modified.elapsed().ok())
            .map_or(0, |age| age.as_millis());
        return Ok(if age > UNREADABLE_LOCK_STALE_MS {
            LockState::Stale(raw)
        } else {
            LockState::Held(None)
        });
    };
    if !same_boot(holder.boot_at_ms, Some(process_boot_at_ms(None))) {
        return Ok(LockState::Stale(raw));
    }
    if holder.pid == f64::from(std::process::id()) {
        // Our own pid: held only by this very process, never by a crashed earlier one wearing it.
        return Ok(
            if same_process_start(
                Some(holder.process_started_at_ms),
                Some(own_process_started_at_ms(None)),
            ) {
                LockState::Held(Some(holder))
            } else {
                LockState::Stale(raw)
            },
        );
    }
    if !alive(probes, holder.pid)? {
        return Ok(LockState::Stale(raw));
    }
    match (probes.read_process_start_ms)(holder.pid) {
        Some(observed)
            if !same_process_start(Some(holder.process_started_at_ms), Some(observed)) =>
        {
            Ok(LockState::Stale(raw))
        }
        _ => Ok(LockState::Held(Some(holder))),
    }
}

/// `breakStaleLock`: remove a stale lock only if it still holds `raw`; a lock someone took
/// meanwhile is put back.
pub fn break_stale_lock(lock: &Path, raw: &[u8]) -> std::io::Result<()> {
    let mut aside = lock.as_os_str().to_owned();
    aside.push(format!(".{}.broken", unique_suffix()));
    let aside = PathBuf::from(aside);
    match std::fs::rename(lock, &aside) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        result => result?,
    }
    let moved = std::fs::read(&aside)?;
    if moved != raw {
        match publish_exclusive(lock, &moved) {
            Err(error) if error.kind() != std::io::ErrorKind::AlreadyExists => return Err(error),
            _ => {}
        }
    }
    unlink_if_present(&aside)
}

enum TakenLock {
    Mine(Vec<u8>),
    Busy(Option<ReclaimLockHolder>),
}

fn take_reclaim_lock(lock: &Path, probes: &mut LeaseProbes<'_>) -> std::io::Result<TakenLock> {
    let mine = serde_json::to_vec(&ReclaimLockHolder {
        pid: f64::from(std::process::id()),
        boot_at_ms: process_boot_at_ms(None),
        process_started_at_ms: own_process_started_at_ms(None),
    })
    .map_err(std::io::Error::other)?;
    for _ in 0..3 {
        match publish_exclusive(lock, &mine) {
            Ok(()) => return Ok(TakenLock::Mine(mine)),
            Err(error) if error.kind() != std::io::ErrorKind::AlreadyExists => return Err(error),
            Err(_) => {}
        }
        match reclaim_lock_state(lock, probes)? {
            LockState::Held(holder) => return Ok(TakenLock::Busy(holder)),
            LockState::Stale(raw) => break_stale_lock(lock, &raw)?,
            LockState::Free => {}
        }
    }
    Ok(TakenLock::Busy(None))
}

#[derive(Debug, PartialEq)]
pub enum ReclaimOutcome {
    Removed,
    Changed,
    Busy(Option<ReclaimLockHolder>),
}

fn remove_if_unchanged(path: &Path, inspected: &[u8]) -> std::io::Result<ReclaimOutcome> {
    if read_text(path)?.as_deref() != Some(inspected) {
        return Ok(ReclaimOutcome::Changed);
    }
    unlink_if_present(path)?;
    Ok(ReclaimOutcome::Removed)
}

/// `reclaimInspected`: remove the lease at `path` only if it still holds `inspected`.
pub fn reclaim_inspected(
    path: &Path,
    inspected: &[u8],
    probes: &mut LeaseProbes<'_>,
) -> std::io::Result<ReclaimOutcome> {
    let mut lock = path.as_os_str().to_owned();
    lock.push(".lock");
    let lock = PathBuf::from(lock);
    let mine = match take_reclaim_lock(&lock, probes)? {
        TakenLock::Mine(mine) => mine,
        TakenLock::Busy(holder) => return Ok(ReclaimOutcome::Busy(holder)),
    };
    let outcome = remove_if_unchanged(path, inspected);
    // Release only our own lock: one broken while we held it may now belong to someone else.
    if read_text(&lock)?.as_deref() == Some(mine.as_slice()) {
        unlink_if_present(&lock)?;
    }
    outcome
}

/// `classifyLease` for a v1 record: alive only when the pid answers AND the process now wearing it
/// started no later than the recorded acquisition instant (plus tolerance). An unknown start is
/// trusted as the holder.
pub fn classify_lease(
    existing: &LeaseRecord,
    probes: &mut LeaseProbes<'_>,
) -> std::io::Result<LeaseClassification> {
    if !alive(probes, existing.pid)? {
        return Ok(LeaseClassification::Dead);
    }
    Ok(match (probes.read_process_start_ms)(existing.pid) {
        Some(observed) if observed > existing.started_at_ms + PROCESS_START_TOLERANCE_MS => {
            LeaseClassification::Reused
        }
        _ => LeaseClassification::LiveForeign,
    })
}

/// Pid-liveness-only acquire (no start-instant probe): every live pid is the holder.
pub fn acquire_terminal_lease(
    dir: &Path,
    encoded_session_id: &str,
    pid: f64,
    started_at_ms: f64,
    mut is_process_alive: impl FnMut(f64) -> std::io::Result<bool>,
) -> std::io::Result<AcquireTerminalLeaseResult> {
    acquire_terminal_lease_with(
        dir,
        encoded_session_id,
        pid,
        started_at_ms,
        &mut LeaseProbes {
            is_process_alive: &mut is_process_alive,
            read_process_start_ms: &mut |_| None,
        },
    )
}

/// `acquireTerminalLease`: exclusive publish; a dead, reused or unparseable record is removed only
/// through `reclaim_inspected`, so a racer's fresh lease makes the next try lose cleanly.
pub fn acquire_terminal_lease_with(
    dir: &Path,
    encoded_session_id: &str,
    pid: f64,
    started_at_ms: f64,
    probes: &mut LeaseProbes<'_>,
) -> std::io::Result<AcquireTerminalLeaseResult> {
    std::fs::create_dir_all(dir)?;
    let path = dir.join(format!("{encoded_session_id}.lease"));
    let content =
        serde_json::to_vec(&LeaseRecord { pid, started_at_ms }).map_err(std::io::Error::other)?;
    for _ in 0..3 {
        match publish_exclusive(&path, &content) {
            Ok(()) => return Ok(AcquireTerminalLeaseResult::Acquired { path, pid }),
            Err(error) if error.kind() != std::io::ErrorKind::AlreadyExists => return Err(error),
            Err(_) => {}
        }
        let Some(raw) = read_text(&path)? else {
            continue;
        };
        if let Some(existing) = parse_record(&raw)
            && classify_lease(&existing, probes)? == LeaseClassification::LiveForeign
        {
            return Ok(AcquireTerminalLeaseResult::Held { holder: existing });
        }
        // Someone is reclaiming right now: wait on that live process, never on the stale record.
        if let ReclaimOutcome::Busy(Some(holder)) = reclaim_inspected(&path, &raw, probes)? {
            return Ok(AcquireTerminalLeaseResult::Held {
                holder: LeaseRecord {
                    pid: holder.pid,
                    started_at_ms: holder.process_started_at_ms,
                },
            });
        }
    }
    if let Some(last) = read_text(&path)?.as_deref().and_then(parse_record)
        && classify_lease(&last, probes)? == LeaseClassification::LiveForeign
    {
        return Ok(AcquireTerminalLeaseResult::Held { holder: last });
    }
    Err(std::io::Error::other(format!(
        "terminal lease {} kept changing while it was being acquired",
        path.display()
    )))
}

pub fn release_terminal_lease(path: &Path, pid: f64) -> std::io::Result<()> {
    if let Some(record) = read_text(path)?.as_deref().and_then(parse_record)
        && record.pid == pid
    {
        unlink_if_present(path)?;
    }
    Ok(())
}

/// Synchronous `readProcessStartMs`: Linux reads `/proc/<pid>/stat`.
#[cfg(target_os = "linux")]
pub fn read_process_start_ms(pid: f64) -> Option<f64> {
    if !pid.is_finite() || pid <= 0.0 || pid > f64::from(u32::MAX) {
        return None;
    }
    let stat = std::fs::read_to_string(format!("/proc/{}/stat", pid as u32)).ok()?;
    let ticks = stat.rsplit_once(") ")?.1.split_whitespace().nth(19)?;
    maho_tool_child::group_ownership::process_start_time_ms(ticks)
}
/// Elsewhere the start is unknown (`None`), which `classify_lease` trusts as the holder.
#[cfg(not(target_os = "linux"))]
pub fn read_process_start_ms(_pid: f64) -> Option<f64> {
    None
}

#[cfg(unix)]
pub fn probe_alive(pid: f64) -> std::io::Result<bool> {
    match nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid as i32), None) {
        Ok(()) | Err(nix::errno::Errno::EPERM) => Ok(true),
        Err(nix::errno::Errno::ESRCH) => Ok(false),
        Err(error) => Err(std::io::Error::from_raw_os_error(error as i32)),
    }
}
#[cfg(windows)]
pub fn probe_alive(pid: f64) -> std::io::Result<bool> {
    if !pid.is_finite() || pid <= 0.0 || pid > f64::from(u32::MAX) {
        return Ok(false);
    }
    Ok(maho_pty::process_alive(pid as u32))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::Duration;

    fn record(path: &Path) -> Option<LeaseRecord> {
        read_text(path).unwrap().as_deref().and_then(parse_record)
    }

    #[test]
    fn default_probe_observes_current_process() -> std::io::Result<()> {
        assert!(probe_alive(f64::from(std::process::id()))?);
        Ok(())
    }
    #[test]
    fn first_acquire_writes_pid() -> std::io::Result<()> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("nested/s.lease");
        assert_eq!(
            acquire_terminal_lease(&dir.path().join("nested"), "s", 12.0, 99.0, |_| Ok(true))?,
            AcquireTerminalLeaseResult::Acquired {
                path: path.clone(),
                pid: 12.0
            }
        );
        assert_eq!(
            record(&path),
            Some(LeaseRecord {
                pid: 12.0,
                started_at_ms: 99.0
            })
        );
        Ok(())
    }
    #[test]
    fn second_acquire_retains_live_holder() -> std::io::Result<()> {
        let dir = tempfile::tempdir()?;
        acquire_terminal_lease(dir.path(), "s", 12.0, 99.0, |_| Ok(true))?;
        assert_eq!(
            acquire_terminal_lease(dir.path(), "s", 13.0, 100.0, |_| Ok(true))?,
            AcquireTerminalLeaseResult::Held {
                holder: LeaseRecord {
                    pid: 12.0,
                    started_at_ms: 99.0
                }
            }
        );
        Ok(())
    }
    #[test]
    fn reclaims_dead_pid() -> std::io::Result<()> {
        let dir = tempfile::tempdir()?;
        acquire_terminal_lease(dir.path(), "s", 12.0, 99.0, |_| Ok(true))?;
        assert!(matches!(
            acquire_terminal_lease(dir.path(), "s", 13.0, 100.0, |_| Ok(false))?,
            AcquireTerminalLeaseResult::Acquired { pid: 13.0, .. }
        ));
        assert!(!dir.path().join("s.lease.lock").exists());
        Ok(())
    }
    #[test]
    fn old_live_holder_is_not_reclaimed() -> std::io::Result<()> {
        let dir = tempfile::tempdir()?;
        acquire_terminal_lease(dir.path(), "s", 12.0, 1.0, |_| Ok(true))?;
        assert!(matches!(
            acquire_terminal_lease(dir.path(), "s", 13.0, 660001.0, |_| Ok(true))?,
            AcquireTerminalLeaseResult::Held { .. }
        ));
        Ok(())
    }
    #[test]
    fn eperm_is_alive() -> std::io::Result<()> {
        let dir = tempfile::tempdir()?;
        acquire_terminal_lease(dir.path(), "s", 12.0, 1.0, |_| Ok(true))?;
        assert!(matches!(
            acquire_terminal_lease(dir.path(), "s", 13.0, 2.0, |_| Err(
                std::io::ErrorKind::PermissionDenied.into()
            ))?,
            AcquireTerminalLeaseResult::Held { .. }
        ));
        Ok(())
    }
    #[test]
    fn release_matches_pid_only() -> std::io::Result<()> {
        let dir = tempfile::tempdir()?;
        acquire_terminal_lease(dir.path(), "s", 12.0, 1.0, |_| Ok(true))?;
        let path = dir.path().join("s.lease");
        release_terminal_lease(&path, 13.0)?;
        assert!(path.exists());
        release_terminal_lease(&path, 12.0)?;
        assert!(!path.exists());
        Ok(())
    }
    #[test]
    fn corrupt_lease_reclaimed() -> std::io::Result<()> {
        let dir = tempfile::tempdir()?;
        std::fs::write(dir.path().join("s.lease"), "not-json{{{")?;
        assert!(matches!(
            acquire_terminal_lease(dir.path(), "s", 13.0, 2.0, |_| Ok(true))?,
            AcquireTerminalLeaseResult::Acquired { .. }
        ));
        Ok(())
    }

    #[test]
    fn reclaims_a_live_pid_whose_process_started_after_the_lease_was_taken() -> std::io::Result<()>
    {
        let dir = tempfile::tempdir()?;
        acquire_terminal_lease(dir.path(), "s", 12.0, 10_000.0, |_| Ok(true))?;
        let result = acquire_terminal_lease_with(
            dir.path(),
            "s",
            13.0,
            20_000.0,
            &mut LeaseProbes {
                is_process_alive: &mut |_| Ok(true),
                read_process_start_ms: &mut |_| Some(15_000.0),
            },
        )?;
        assert!(matches!(
            result,
            AcquireTerminalLeaseResult::Acquired { pid: 13.0, .. }
        ));
        Ok(())
    }

    #[test]
    fn keeps_a_live_holder_that_started_before_its_lease_or_has_no_known_start()
    -> std::io::Result<()> {
        let dir = tempfile::tempdir()?;
        acquire_terminal_lease(dir.path(), "s", 12.0, 10_000.0, |_| Ok(true))?;
        for start in [Some(1_000.0), Some(12_999.0), None] {
            let result = acquire_terminal_lease_with(
                dir.path(),
                "s",
                13.0,
                20_000.0,
                &mut LeaseProbes {
                    is_process_alive: &mut |_| Ok(true),
                    read_process_start_ms: &mut |_| start,
                },
            )?;
            assert!(
                matches!(result, AcquireTerminalLeaseResult::Held { .. }),
                "{start:?}"
            );
        }
        Ok(())
    }

    #[test]
    fn reclaim_leaves_a_lease_that_changed_since_it_was_inspected() -> std::io::Result<()> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("s.lease");
        std::fs::write(&path, br#"{"pid":12,"startedAtMs":1}"#)?;
        let stale = std::fs::read(&path)?;
        let mut probes = LeaseProbes {
            is_process_alive: &mut |_| Ok(false),
            read_process_start_ms: &mut |_| None,
        };
        assert_eq!(
            reclaim_inspected(&path, &stale, &mut probes)?,
            ReclaimOutcome::Removed
        );
        publish_exclusive(&path, br#"{"pid":13,"startedAtMs":2}"#)?;
        assert_eq!(
            reclaim_inspected(&path, &stale, &mut probes)?,
            ReclaimOutcome::Changed
        );
        assert_eq!(record(&path).map(|record| record.pid), Some(13.0));
        assert!(!dir.path().join("s.lease.lock").exists());
        Ok(())
    }

    #[test]
    fn a_live_reclaim_lock_makes_the_acquire_wait_on_its_holder() -> std::io::Result<()> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("s.lease");
        std::fs::write(&path, br#"{"pid":12,"startedAtMs":1}"#)?;
        let holder = ReclaimLockHolder {
            pid: f64::from(std::process::id()),
            boot_at_ms: process_boot_at_ms(None),
            process_started_at_ms: own_process_started_at_ms(None),
        };
        std::fs::write(
            dir.path().join("s.lease.lock"),
            serde_json::to_vec(&holder).map_err(std::io::Error::other)?,
        )?;
        let result = acquire_terminal_lease_with(
            dir.path(),
            "s",
            13.0,
            2.0,
            &mut LeaseProbes {
                is_process_alive: &mut |_| Ok(false),
                read_process_start_ms: &mut |_| None,
            },
        )?;
        assert_eq!(
            result,
            AcquireTerminalLeaseResult::Held {
                holder: LeaseRecord {
                    pid: holder.pid,
                    started_at_ms: holder.process_started_at_ms
                }
            }
        );
        assert_eq!(record(&path).map(|record| record.pid), Some(12.0));
        Ok(())
    }

    #[test]
    fn a_stale_reclaim_lock_from_a_dead_pid_is_broken() -> std::io::Result<()> {
        let dir = tempfile::tempdir()?;
        let lock = dir.path().join("s.lease.lock");
        let dead = ReclaimLockHolder {
            pid: 999_999.0,
            boot_at_ms: process_boot_at_ms(None),
            process_started_at_ms: 1.0,
        };
        let raw = serde_json::to_vec(&dead).map_err(std::io::Error::other)?;
        std::fs::write(&lock, &raw)?;
        let mut probes = LeaseProbes {
            is_process_alive: &mut |_| Ok(false),
            read_process_start_ms: &mut |_| None,
        };
        assert_eq!(
            reclaim_lock_state(&lock, &mut probes)?,
            LockState::Stale(raw.clone())
        );
        break_stale_lock(&lock, &raw)?;
        assert!(!lock.exists());
        assert!(std::fs::read_dir(dir.path())?.next().is_none());
        Ok(())
    }

    /// Two waiters inspect the same dead record; the first finishes its acquire before the second
    /// acts on its stale view. Without the inspected reclaim the second unlinks the first's fresh
    /// lease and both acquire. Every handshake is a bounded `recv_timeout`, so a waiter that fails
    /// early makes the test fail instead of hang.
    #[test]
    fn concurrent_reclaim_of_one_dead_record_acquires_exactly_once() -> std::io::Result<()> {
        let dir = tempfile::tempdir()?;
        std::fs::write(dir.path().join("s.lease"), br#"{"pid":12,"startedAtMs":1}"#)?;
        let bound = Duration::from_secs(10);
        let (inspected, wait_inspected) = mpsc::channel::<f64>();
        let spawn = |me: f64| {
            let dir = dir.path().to_path_buf();
            let inspected = inspected.clone();
            let (go, wait_go) = mpsc::channel::<()>();
            let handle = std::thread::spawn(move || {
                let mut seen_dead = false;
                let mut is_process_alive = |pid: f64| -> std::io::Result<bool> {
                    if pid != 12.0 {
                        return Ok(true);
                    }
                    if !seen_dead {
                        seen_dead = true;
                        inspected.send(me).map_err(std::io::Error::other)?;
                        wait_go.recv_timeout(bound).map_err(std::io::Error::other)?;
                    }
                    Ok(false)
                };
                acquire_terminal_lease_with(
                    &dir,
                    "s",
                    me,
                    2.0,
                    &mut LeaseProbes {
                        is_process_alive: &mut is_process_alive,
                        read_process_start_ms: &mut |_| None,
                    },
                )
            });
            (go, handle)
        };
        let (go_first, first) = spawn(13.0);
        let (go_second, second) = spawn(14.0);
        let mut arrived = [
            wait_inspected
                .recv_timeout(bound)
                .map_err(std::io::Error::other)?,
            wait_inspected
                .recv_timeout(bound)
                .map_err(std::io::Error::other)?,
        ];
        arrived.sort_by(f64::total_cmp);
        assert_eq!(arrived, [13.0, 14.0]);
        go_first.send(()).map_err(std::io::Error::other)?;
        let first = first
            .join()
            .map_err(|_| std::io::Error::other("first waiter panicked"))??;
        go_second.send(()).map_err(std::io::Error::other)?;
        let second = second
            .join()
            .map_err(|_| std::io::Error::other("second waiter panicked"))??;
        assert!(matches!(
            first,
            AcquireTerminalLeaseResult::Acquired { pid: 13.0, .. }
        ));
        assert_eq!(
            second,
            AcquireTerminalLeaseResult::Held {
                holder: LeaseRecord {
                    pid: 13.0,
                    started_at_ms: 2.0
                }
            }
        );
        assert_eq!(
            record(&dir.path().join("s.lease")).map(|record| record.pid),
            Some(13.0)
        );
        Ok(())
    }
}
