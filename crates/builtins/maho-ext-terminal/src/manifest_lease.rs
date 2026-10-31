//! Port of senpi `terminal/manifest-lease.ts` and `terminal/lease-file.ts`, including the v2 lease
//! record (`LEASE_RECORD_VERSION`): a per-acquire generation token plus the holder's boot and
//! process-start instants. `startedAtMs` duplicates `acquiredAtMs` so a v1 reader (`{pid,
//! startedAtMs}`) still parses the file; a v1 record found on disk is classified by the v1 rule.
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
use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};

/// Upstream `LEASE_RECORD_VERSION`.
pub const LEASE_RECORD_VERSION: f64 = 2.0;

/// A reclaim lock that cannot be parsed was left by a crash mid-write; past this age it is abandoned.
const UNREADABLE_LOCK_STALE_MS: u128 = 30_000;

/// Upstream `LeaseRecord`: every field but `pid`/`startedAtMs` is optional (a v1 record has none).
/// As a holder (`LeaseHolder`) only `pid`, `startedAtMs` and `bootAtMs` are set.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LeaseRecord {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub v: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
    pub pid: f64,
    pub started_at_ms: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub boot_at_ms: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub process_started_at_ms: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub acquired_at_ms: Option<f64>,
}
impl LeaseRecord {
    /// A legacy v1 record `{pid, startedAtMs}`.
    pub fn v1(pid: f64, started_at_ms: f64) -> Self {
        Self {
            pid,
            started_at_ms,
            ..Self::default()
        }
    }
    /// Upstream `holderOf`.
    fn holder(&self) -> Self {
        Self {
            pid: self.pid,
            started_at_ms: self.started_at_ms,
            boot_at_ms: self.boot_at_ms,
            ..Self::default()
        }
    }
}

/// Upstream `LeaseSelfIdentity`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LeaseSelfIdentity {
    pub pid: f64,
    pub boot_at_ms: f64,
    pub process_started_at_ms: f64,
}
impl LeaseSelfIdentity {
    /// This process, as upstream's default `self` (`processBootAtMs`, `ownProcessStartedAtMs`).
    pub fn own(pid: f64) -> Self {
        Self {
            pid,
            boot_at_ms: process_boot_at_ms(None),
            process_started_at_ms: own_process_started_at_ms(None),
        }
    }
}

#[derive(Debug, PartialEq)]
pub enum AcquireTerminalLeaseResult {
    Acquired {
        path: PathBuf,
        pid: f64,
        token: String,
    },
    Held {
        holder: LeaseRecord,
    },
}

/// Upstream `LeaseClassification` (`"self"` is `SelfOwned`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LeaseClassification {
    SelfOwned,
    Dead,
    Reused,
    LiveForeign,
}

#[derive(Default)]
struct Tokens {
    /// Upstream `generationTokens`: encoded session id -> the token this process minted for it.
    generation: HashMap<String, String>,
    /// Upstream `liveTokens`: tokens held by generations of THIS process that are still running. A
    /// same-pid lease is a live holder only when its token is here; any other token was minted by
    /// a generation that has shut down (even one whose release never ran), so it is ours to re-enter.
    live: HashSet<String>,
}

fn tokens() -> std::sync::MutexGuard<'static, Tokens> {
    static TOKENS: OnceLock<Mutex<Tokens>> = OnceLock::new();
    TOKENS
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Upstream `currentLeaseToken`.
pub fn current_lease_token(encoded_session_id: &str) -> Option<String> {
    tokens().generation.get(encoded_session_id).cloned()
}

/// Upstream `retireLeaseToken`: mark a generation's token dead the moment its shutdown starts,
/// before any slow flush.
pub fn retire_lease_token(token: &str) {
    tokens().live.remove(token);
}

/// A fresh, unguessable-enough generation token (upstream `randomUUID()`; only uniqueness within
/// one host matters, so pid, wall-clock nanos and a process-wide counter are hashed).
fn mint_token() -> String {
    use sha2::{Digest, Sha256};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let mut hasher = Sha256::new();
    hasher.update(std::process::id().to_le_bytes());
    hasher.update(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_nanos())
            .to_le_bytes(),
    );
    hasher.update(NEXT.fetch_add(1, Ordering::Relaxed).to_le_bytes());
    let digest = hasher.finalize();
    let hex: String = digest[..16]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    format!(
        "{}-{}-{}-{}-{}",
        &hex[..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    )
}

/// Upstream `readLeaseRecord`: `None` is `"unparseable"`. `pid` and `startedAtMs` must be finite
/// numbers; an optional field of the wrong type is dropped, never fatal.
pub fn read_lease_record(raw: &[u8]) -> Option<LeaseRecord> {
    let parsed: serde_json::Value = serde_json::from_slice(raw).ok()?;
    let object = parsed.as_object()?;
    let finite = |key: &str| {
        object
            .get(key)
            .and_then(serde_json::Value::as_f64)
            .filter(|value| value.is_finite())
    };
    Some(LeaseRecord {
        pid: finite("pid")?,
        started_at_ms: finite("startedAtMs")?,
        v: finite("v"),
        token: object
            .get("token")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned),
        boot_at_ms: finite("bootAtMs"),
        process_started_at_ms: finite("processStartedAtMs"),
        acquired_at_ms: finite("acquiredAtMs"),
    })
}

fn read_text(path: &Path) -> std::io::Result<Option<Vec<u8>>> {
    match std::fs::read(path) {
        Ok(raw) => Ok(Some(raw)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

fn parse_record(raw: &[u8]) -> Option<LeaseRecord> {
    read_lease_record(raw)
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

/// `classifyLease`: a pid alone is never trusted. Our own pid is `SelfOwned` unless the record
/// carries a token another live generation of this process holds (a multi-session host holds many
/// sessions under one pid). A foreign holder is alive only when its pid answers AND its recorded
/// start instant matches the process now wearing that pid; an unknown start is trusted. A v1
/// record's `startedAtMs` is when the lease was taken, so its holder started no later than that.
pub fn classify_lease(
    existing: &LeaseRecord,
    self_pid: f64,
    probes: &mut LeaseProbes<'_>,
) -> std::io::Result<LeaseClassification> {
    if existing.pid == self_pid {
        let live = existing
            .token
            .as_ref()
            .is_some_and(|token| tokens().live.contains(token));
        return Ok(if live {
            LeaseClassification::LiveForeign
        } else {
            LeaseClassification::SelfOwned
        });
    }
    if !alive(probes, existing.pid)? {
        return Ok(LeaseClassification::Dead);
    }
    let Some(observed) = (probes.read_process_start_ms)(existing.pid) else {
        return Ok(LeaseClassification::LiveForeign);
    };
    let live = match existing.process_started_at_ms {
        None => observed <= existing.started_at_ms + PROCESS_START_TOLERANCE_MS,
        Some(started) => same_process_start(Some(started), Some(observed)),
    };
    Ok(if live {
        LeaseClassification::LiveForeign
    } else {
        LeaseClassification::Reused
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

/// `acquireTerminalLease` as this process (`self` defaults to its own boot and start instants).
pub fn acquire_terminal_lease_with(
    dir: &Path,
    encoded_session_id: &str,
    pid: f64,
    now: f64,
    probes: &mut LeaseProbes<'_>,
) -> std::io::Result<AcquireTerminalLeaseResult> {
    acquire_terminal_lease_as(
        dir,
        encoded_session_id,
        now,
        &LeaseSelfIdentity::own(pid),
        probes,
    )
}

/// `acquireTerminalLease`: exclusive publish of a v2 record with a fresh token; a same-pid record
/// whose token no live generation holds is re-entered, and a dead, reused or unparseable record is
/// removed only through `reclaim_inspected`, so a racer's fresh lease makes the next try lose cleanly.
pub fn acquire_terminal_lease_as(
    dir: &Path,
    encoded_session_id: &str,
    now: f64,
    identity: &LeaseSelfIdentity,
    probes: &mut LeaseProbes<'_>,
) -> std::io::Result<AcquireTerminalLeaseResult> {
    std::fs::create_dir_all(dir)?;
    let path = dir.join(format!("{encoded_session_id}.lease"));
    let pid = identity.pid;
    let token = mint_token();
    let content = serde_json::to_vec(&LeaseRecord {
        v: Some(LEASE_RECORD_VERSION),
        token: Some(token.clone()),
        pid,
        started_at_ms: now,
        boot_at_ms: Some(identity.boot_at_ms),
        process_started_at_ms: Some(identity.process_started_at_ms),
        acquired_at_ms: Some(now),
    })
    .map_err(std::io::Error::other)?;
    let acquired = |path: PathBuf| {
        tokens()
            .generation
            .insert(encoded_session_id.to_owned(), token.clone());
        AcquireTerminalLeaseResult::Acquired {
            path,
            pid,
            token: token.clone(),
        }
    };
    // Native threads (unlike upstream's single event loop) can read a just-published file before
    // the publisher returns, so the token is live BEFORE it is published: a same-pid racer always
    // sees the winner as a live holder. It is retired again on every path that does not acquire.
    tokens().live.insert(token.clone());
    let result = acquire_published(&path, &content, pid, probes, acquired);
    if !matches!(result, Ok(AcquireTerminalLeaseResult::Acquired { .. })) {
        retire_lease_token(&token);
    }
    result
}

fn acquire_published(
    path: &Path,
    content: &[u8],
    pid: f64,
    probes: &mut LeaseProbes<'_>,
    acquired: impl Fn(PathBuf) -> AcquireTerminalLeaseResult,
) -> std::io::Result<AcquireTerminalLeaseResult> {
    let path = path.to_path_buf();
    for _ in 0..3 {
        match publish_exclusive(&path, content) {
            Ok(()) => return Ok(acquired(path)),
            Err(error) if error.kind() != std::io::ErrorKind::AlreadyExists => return Err(error),
            Err(_) => {}
        }
        let Some(raw) = read_text(&path)? else {
            continue;
        };
        let existing = parse_record(&raw);
        let verdict = match &existing {
            None => LeaseClassification::Dead,
            Some(existing) => classify_lease(existing, pid, probes)?,
        };
        if verdict == LeaseClassification::SelfOwned {
            publish_replace(&path, content)?;
            return Ok(acquired(path));
        }
        if verdict == LeaseClassification::LiveForeign
            && let Some(existing) = existing
        {
            return Ok(AcquireTerminalLeaseResult::Held {
                holder: existing.holder(),
            });
        }
        // Someone is reclaiming right now: wait on that live process, never on the stale record.
        if let ReclaimOutcome::Busy(Some(holder)) = reclaim_inspected(&path, &raw, probes)? {
            return Ok(AcquireTerminalLeaseResult::Held {
                holder: LeaseRecord {
                    pid: holder.pid,
                    started_at_ms: holder.process_started_at_ms,
                    boot_at_ms: Some(holder.boot_at_ms),
                    ..LeaseRecord::default()
                },
            });
        }
    }
    if let Some(last) = read_text(&path)?.as_deref().and_then(parse_record)
        && classify_lease(&last, pid, probes)? == LeaseClassification::LiveForeign
    {
        return Ok(AcquireTerminalLeaseResult::Held {
            holder: last.holder(),
        });
    }
    Err(std::io::Error::other(format!(
        "terminal lease {} kept changing while it was being acquired",
        path.display()
    )))
}

/// `releaseTerminalLease`: release only the generation that acquired. A record naming another pid,
/// or (when both carry one) another token, is a later generation's lease and is left alone.
pub fn release_terminal_lease(path: &Path, pid: f64, token: Option<&str>) -> std::io::Result<()> {
    let Some(existing) = read_text(path)?.as_deref().and_then(parse_record) else {
        return Ok(());
    };
    if existing.pid != pid {
        return Ok(());
    }
    if let (Some(token), Some(existing)) = (token, existing.token.as_deref())
        && token != existing
    {
        return Ok(());
    }
    unlink_if_present(path)?;
    if let Some(existing) = existing.token {
        let mut tokens = tokens();
        tokens.live.remove(&existing);
        tokens.generation.retain(|_, token| *token != existing);
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
    use crate::process_identity::BOOT_INSTANT_TOLERANCE_MS;
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
        let AcquireTerminalLeaseResult::Acquired {
            path: acquired,
            pid,
            token,
        } = acquire_terminal_lease(&dir.path().join("nested"), "s", 12.0, 99.0, |_| Ok(true))?
        else {
            panic!("first acquire must succeed");
        };
        assert_eq!((acquired, pid), (path.clone(), 12.0));
        let written = record(&path).expect("lease record");
        assert_eq!((written.pid, written.started_at_ms), (12.0, 99.0));
        assert_eq!(written.token.as_deref(), Some(token.as_str()));
        Ok(())
    }
    #[test]
    fn second_acquire_retains_live_holder() -> std::io::Result<()> {
        let dir = tempfile::tempdir()?;
        acquire_terminal_lease(dir.path(), "s", 12.0, 99.0, |_| Ok(true))?;
        let AcquireTerminalLeaseResult::Held { holder } =
            acquire_terminal_lease(dir.path(), "s", 13.0, 100.0, |_| Ok(true))?
        else {
            panic!("a live holder is kept");
        };
        assert_eq!((holder.pid, holder.started_at_ms), (12.0, 99.0));
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
        release_terminal_lease(&path, 13.0, None)?;
        assert!(path.exists());
        release_terminal_lease(&path, 12.0, None)?;
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
        // A legacy v1 record: `startedAtMs` is the acquisition instant.
        std::fs::write(
            dir.path().join("s.lease"),
            br#"{"pid":12,"startedAtMs":10000}"#,
        )?;
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
        // A legacy v1 record: `startedAtMs` is the acquisition instant.
        std::fs::write(
            dir.path().join("s.lease"),
            br#"{"pid":12,"startedAtMs":10000}"#,
        )?;
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
                    started_at_ms: holder.process_started_at_ms,
                    boot_at_ms: Some(holder.boot_at_ms),
                    ..LeaseRecord::default()
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
        let AcquireTerminalLeaseResult::Held { holder } = second else {
            panic!("the second waiter waits on the first");
        };
        assert_eq!((holder.pid, holder.started_at_ms), (13.0, 2.0));
        assert_eq!(
            record(&dir.path().join("s.lease")).map(|record| record.pid),
            Some(13.0)
        );
        Ok(())
    }

    // terminal-manifest-lease-identity.test.ts (v2). Each test uses its own encoded session id:
    // the generation token map is process-wide.
    const BOOT: f64 = 1_790_000_000_000.0;
    const NOW: f64 = BOOT + 3_600_000.0;
    const HOLDER_START: f64 = NOW - 600_000.0;

    fn v2_record(pid: f64, token: &str) -> serde_json::Value {
        serde_json::json!({
            "v": LEASE_RECORD_VERSION,
            "token": token,
            "pid": pid,
            "startedAtMs": HOLDER_START,
            "bootAtMs": BOOT,
            "processStartedAtMs": HOLDER_START,
            "acquiredAtMs": HOLDER_START,
        })
    }
    fn self_identity(pid: f64) -> LeaseSelfIdentity {
        LeaseSelfIdentity {
            pid,
            boot_at_ms: BOOT,
            process_started_at_ms: NOW - 5_000.0,
        }
    }
    fn own_pid() -> f64 {
        f64::from(std::process::id())
    }
    fn acquire_as<'a>(
        dir: &Path,
        session: &str,
        identity: &LeaseSelfIdentity,
        alive: &'a mut dyn FnMut(f64) -> std::io::Result<bool>,
        start: &'a mut dyn FnMut(f64) -> Option<f64>,
    ) -> std::io::Result<AcquireTerminalLeaseResult> {
        acquire_terminal_lease_as(
            dir,
            session,
            NOW,
            identity,
            &mut LeaseProbes {
                is_process_alive: alive,
                read_process_start_ms: start,
            },
        )
    }
    fn write_json(path: &Path, value: &serde_json::Value) {
        std::fs::write(path, value.to_string()).unwrap();
    }

    #[test]
    fn read_lease_record_keeps_typed_fields_and_drops_mistyped_optionals() {
        assert_eq!(read_lease_record(b"nope"), None);
        assert_eq!(read_lease_record(b"[1]"), None);
        assert_eq!(read_lease_record(br#"{"pid":"1","startedAtMs":2}"#), None);
        assert_eq!(read_lease_record(br#"{"pid":1}"#), None);
        assert_eq!(
            read_lease_record(br#"{"pid":1,"startedAtMs":2,"v":"2","token":7,"bootAtMs":3}"#),
            Some(LeaseRecord {
                pid: 1.0,
                started_at_ms: 2.0,
                boot_at_ms: Some(3.0),
                ..LeaseRecord::default()
            })
        );
        let full = v2_record(5.0, "t").to_string();
        assert_eq!(
            read_lease_record(full.as_bytes()),
            Some(LeaseRecord {
                v: Some(2.0),
                token: Some("t".into()),
                pid: 5.0,
                started_at_ms: HOLDER_START,
                boot_at_ms: Some(BOOT),
                process_started_at_ms: Some(HOLDER_START),
                acquired_at_ms: Some(HOLDER_START),
            })
        );
    }

    #[test]
    fn writes_a_v2_record_that_the_legacy_v1_reader_still_parses() {
        let dir = tempfile::tempdir().unwrap();
        let session = "identity-v2-record";
        let result = acquire_as(
            dir.path(),
            session,
            &self_identity(own_pid()),
            &mut |_| Ok(true),
            &mut |_| None,
        )
        .unwrap();
        let AcquireTerminalLeaseResult::Acquired { token, .. } = result else {
            panic!("acquired");
        };
        let raw: serde_json::Value = serde_json::from_slice(
            &std::fs::read(dir.path().join(format!("{session}.lease"))).unwrap(),
        )
        .unwrap();
        assert_eq!(raw["v"], LEASE_RECORD_VERSION);
        assert_eq!(raw["pid"], own_pid());
        assert_eq!(raw["startedAtMs"], NOW);
        assert_eq!(raw["acquiredAtMs"], NOW);
        assert_eq!(raw["bootAtMs"], BOOT);
        assert_eq!(raw["processStartedAtMs"], NOW - 5_000.0);
        assert_eq!(raw["token"], token.as_str());
        // The v1 reader shape: `{pid, startedAtMs}` parses as numbers.
        #[derive(serde::Deserialize)]
        #[serde(rename_all = "camelCase")]
        #[allow(dead_code)]
        struct V1 {
            pid: f64,
            started_at_ms: f64,
        }
        assert!(serde_json::from_value::<V1>(raw).is_ok());
        assert_eq!(current_lease_token(session), Some(token));
    }

    #[test]
    fn waits_on_a_live_reclaimer_and_breaks_a_lock_whose_holder_is_gone_or_whose_pid_was_reused() {
        let dir = tempfile::tempdir().unwrap();
        let session = "identity-reclaimer";
        let path = dir.path().join(format!("{session}.lease"));
        let lock = dir.path().join(format!("{session}.lease.lock"));
        let stale = v2_record(2_147_000_002.0, "dead-holder").to_string();
        let reclaimer = ReclaimLockHolder {
            pid: 3_100_001.0,
            boot_at_ms: process_boot_at_ms(None),
            process_started_at_ms: NOW - 60_000.0,
        };
        let reclaimer_raw = serde_json::to_vec(&reclaimer).unwrap();
        let acquire = |reclaimer_start: f64, reclaimer_alive: bool| {
            acquire_as(
                dir.path(),
                session,
                &self_identity(own_pid()),
                &mut |pid| {
                    Ok(if pid == 3_100_001.0 {
                        reclaimer_alive
                    } else {
                        pid != 2_147_000_002.0
                    })
                },
                &mut |pid| {
                    Some(if pid == 3_100_001.0 {
                        reclaimer_start
                    } else {
                        NOW - 5_000.0
                    })
                },
            )
            .unwrap()
        };
        std::fs::write(&path, &stale).unwrap();
        std::fs::write(&lock, &reclaimer_raw).unwrap();
        let AcquireTerminalLeaseResult::Held { holder } =
            acquire(reclaimer.process_started_at_ms, true)
        else {
            panic!("a live reclaimer is waited on");
        };
        assert_eq!(holder.pid, reclaimer.pid);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), stale);
        // The reclaimer's pid now belongs to a later process: the lock is broken.
        assert!(matches!(
            acquire(NOW - 1_000.0, true),
            AcquireTerminalLeaseResult::Acquired { .. }
        ));
        assert!(!lock.exists());
        release_terminal_lease(&path, own_pid(), None).unwrap();
        // Same for a reclaimer that is simply gone.
        std::fs::write(&path, &stale).unwrap();
        std::fs::write(&lock, &reclaimer_raw).unwrap();
        assert!(matches!(
            acquire(reclaimer.process_started_at_ms, false),
            AcquireTerminalLeaseResult::Acquired { .. }
        ));
    }

    #[test]
    fn breaks_a_lock_left_under_this_process_pid_by_an_earlier_process_that_wore_it() {
        let dir = tempfile::tempdir().unwrap();
        let session = "identity-own-pid-lock";
        let path = dir.path().join(format!("{session}.lease"));
        write_json(&path, &v2_record(2_147_000_005.0, "dead-holder"));
        std::fs::write(
            dir.path().join(format!("{session}.lease.lock")),
            serde_json::to_vec(&ReclaimLockHolder {
                pid: own_pid(),
                boot_at_ms: process_boot_at_ms(None),
                process_started_at_ms: own_process_started_at_ms(None) - 3_600_000.0,
            })
            .unwrap(),
        )
        .unwrap();
        let result = acquire_as(
            dir.path(),
            session,
            &self_identity(own_pid()),
            &mut |pid| Ok(pid != 2_147_000_005.0),
            &mut |_| Some(NOW - 5_000.0),
        )
        .unwrap();
        let AcquireTerminalLeaseResult::Acquired { path, pid, token } = result else {
            panic!("acquired");
        };
        release_terminal_lease(&path, pid, Some(token.as_str())).unwrap();
    }

    #[test]
    fn treats_an_unreadable_lock_as_held_until_it_is_old_enough_to_have_been_abandoned() {
        let dir = tempfile::tempdir().unwrap();
        let session = "identity-unreadable-lock";
        let path = dir.path().join(format!("{session}.lease"));
        let lock = dir.path().join(format!("{session}.lease.lock"));
        let stale = v2_record(2_147_000_004.0, "dead-holder").to_string();
        let acquire = || {
            acquire_as(
                dir.path(),
                session,
                &self_identity(own_pid()),
                &mut |pid| Ok(pid != 2_147_000_004.0),
                &mut |_| Some(NOW - 5_000.0),
            )
        };
        std::fs::write(&path, &stale).unwrap();
        std::fs::write(&lock, "").unwrap();
        let error = acquire().expect_err("an unreadable fresh lock blocks the reclaim");
        assert!(error.to_string().contains("kept changing"), "{error}");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), stale);
        std::fs::File::options()
            .write(true)
            .open(&lock)
            .unwrap()
            .set_modified(std::time::SystemTime::now() - Duration::from_secs(60))
            .unwrap();
        assert!(matches!(
            acquire().unwrap(),
            AcquireTerminalLeaseResult::Acquired { .. }
        ));
    }

    /// The stale holder's pid answers but belongs to a later process (pid reuse), so both racers
    /// probe it; racer A is held inside that probe until racer B has fully acquired. Every
    /// handshake is a bounded `recv_timeout`.
    #[test]
    fn a_slow_reclaimer_never_deletes_the_fresh_lease_a_faster_one_took_from_the_same_stale_file() {
        let dir = tempfile::tempdir().unwrap();
        let session = "identity-slow-reclaimer";
        let path = dir.path().join(format!("{session}.lease"));
        write_json(&path, &v2_record(2_147_000_001.0, "reused-holder"));
        let bound = Duration::from_secs(10);
        let (entered, wait_entered) = mpsc::channel::<()>();
        let (resume, wait_resume) = mpsc::channel::<()>();
        let slow_dir = dir.path().to_path_buf();
        let slow = std::thread::spawn(move || {
            let mut first = true;
            acquire_as(
                &slow_dir,
                session,
                &self_identity(1_001.0),
                &mut |_| Ok(true),
                &mut |_| {
                    if first {
                        first = false;
                        entered.send(()).ok()?;
                        wait_resume.recv_timeout(bound).ok()?;
                    }
                    Some(NOW - 5_000.0)
                },
            )
        });
        wait_entered
            .recv_timeout(bound)
            .expect("slow racer probing");
        let fast = acquire_as(
            dir.path(),
            session,
            &self_identity(1_002.0),
            &mut |_| Ok(true),
            &mut |_| Some(NOW - 5_000.0),
        )
        .unwrap();
        let AcquireTerminalLeaseResult::Acquired { token, .. } = fast else {
            panic!("the fast racer acquires");
        };
        resume.send(()).unwrap();
        let slow = slow.join().expect("slow racer").unwrap();
        let AcquireTerminalLeaseResult::Held { holder } = slow else {
            panic!("the slow racer waits on the fast one");
        };
        assert_eq!(holder.pid, 1_002.0);
        assert_eq!(record(&path).and_then(|record| record.token), Some(token));
    }

    #[test]
    fn reclaims_a_live_looking_pid_whose_start_time_does_not_match() {
        let dir = tempfile::tempdir().unwrap();
        let session = "identity-reused";
        let path = dir.path().join(format!("{session}.lease"));
        write_json(&path, &v2_record(424_242.0, "holder-token"));
        let mut probed = Vec::new();
        let result = acquire_as(
            dir.path(),
            session,
            &self_identity(own_pid()),
            &mut |_| Ok(true),
            &mut |pid| {
                probed.push(pid);
                Some(HOLDER_START + 10_000.0)
            },
        )
        .unwrap();
        assert!(matches!(
            result,
            AcquireTerminalLeaseResult::Acquired { .. }
        ));
        assert!(probed.contains(&424_242.0));
        assert_eq!(record(&path).map(|record| record.pid), Some(own_pid()));
    }

    #[test]
    fn reclaims_a_pre_boot_lease_with_a_dead_pid_without_consulting_the_start_probe() {
        let dir = tempfile::tempdir().unwrap();
        let session = "identity-pre-boot-dead";
        let mut record = v2_record(424_242.0, "holder-token");
        record["bootAtMs"] = serde_json::json!(BOOT - BOOT_INSTANT_TOLERANCE_MS - 3_600_000.0);
        write_json(&dir.path().join(format!("{session}.lease")), &record);
        let mut probes = 0;
        let result = acquire_as(
            dir.path(),
            session,
            &self_identity(own_pid()),
            &mut |_| Ok(false),
            &mut |_| {
                probes += 1;
                Some(HOLDER_START)
            },
        )
        .unwrap();
        assert!(matches!(
            result,
            AcquireTerminalLeaseResult::Acquired { .. }
        ));
        assert_eq!(probes, 0);
    }

    #[test]
    fn keeps_a_pre_boot_lease_whose_pid_is_alive_and_whose_start_time_is_confirmed() {
        let dir = tempfile::tempdir().unwrap();
        let session = "identity-pre-boot-live";
        let mut record = v2_record(424_242.0, "holder-token");
        record["bootAtMs"] = serde_json::json!(BOOT - BOOT_INSTANT_TOLERANCE_MS - 3_600_000.0);
        write_json(&dir.path().join(format!("{session}.lease")), &record);
        let result = acquire_as(
            dir.path(),
            session,
            &self_identity(own_pid()),
            &mut |_| Ok(true),
            &mut |_| Some(HOLDER_START + 1_000.0),
        )
        .unwrap();
        let AcquireTerminalLeaseResult::Held { holder } = result else {
            panic!("held");
        };
        assert_eq!(
            (holder.pid, holder.started_at_ms),
            (424_242.0, HOLDER_START)
        );
    }

    #[test]
    fn re_enters_its_own_lease_with_a_fresh_token_and_ignores_a_stale_release() {
        let dir = tempfile::tempdir().unwrap();
        let session = "identity-re-enter";
        let path = dir.path().join(format!("{session}.lease"));
        let previous = acquire_terminal_lease_as(
            dir.path(),
            session,
            NOW - 60_000.0,
            &self_identity(own_pid()),
            &mut LeaseProbes {
                is_process_alive: &mut |_| Ok(true),
                read_process_start_ms: &mut |_| None,
            },
        )
        .unwrap();
        let AcquireTerminalLeaseResult::Acquired {
            token: stale_token, ..
        } = previous
        else {
            panic!("acquired");
        };
        // The previous generation's shutdown started (its token is retired) but never released.
        retire_lease_token(&stale_token);
        let result = acquire_as(
            dir.path(),
            session,
            &self_identity(own_pid()),
            &mut |_| Ok(true),
            &mut |_| panic!("self re-entry must not probe"),
        )
        .unwrap();
        assert!(matches!(
            result,
            AcquireTerminalLeaseResult::Acquired { .. }
        ));
        let fresh = record(&path).and_then(|record| record.token).unwrap();
        assert_ne!(fresh, stale_token);
        release_terminal_lease(&path, own_pid(), Some(stale_token.as_str())).unwrap();
        assert!(path.exists());
        release_terminal_lease(&path, own_pid(), Some(fresh.as_str())).unwrap();
        assert!(!path.exists());
        assert_eq!(current_lease_token(session), None);
    }

    #[test]
    fn treats_another_live_generation_of_this_very_process_as_a_live_holder_not_a_re_entry() {
        let dir = tempfile::tempdir().unwrap();
        let session = "identity-sibling";
        let acquire = || {
            acquire_as(
                dir.path(),
                session,
                &self_identity(own_pid()),
                &mut |_| Ok(true),
                &mut |_| panic!("same-pid classification must not probe"),
            )
            .unwrap()
        };
        let sibling = acquire();
        let AcquireTerminalLeaseResult::Acquired { path, pid, token } = sibling else {
            panic!("sibling acquired");
        };
        let result = acquire();
        let AcquireTerminalLeaseResult::Held { holder } = result else {
            panic!("a live sibling generation is a holder");
        };
        assert_eq!(holder.pid, own_pid());
        release_terminal_lease(&path, pid, Some(token.as_str())).unwrap();
    }

    #[test]
    fn re_enters_a_same_pid_lease_whose_generation_shut_down_without_releasing_it() {
        let dir = tempfile::tempdir().unwrap();
        let session = "identity-shut-down";
        write_json(
            &dir.path().join(format!("{session}.lease")),
            &v2_record(own_pid(), "generation-that-already-shut-down"),
        );
        let result = acquire_as(
            dir.path(),
            session,
            &self_identity(own_pid()),
            &mut |_| Ok(true),
            &mut |_| panic!("same-pid classification must not probe"),
        )
        .unwrap();
        let AcquireTerminalLeaseResult::Acquired { path, pid, token } = result else {
            panic!("re-entered");
        };
        release_terminal_lease(&path, pid, Some(token.as_str())).unwrap();
    }

    #[test]
    fn reports_a_confirmed_live_foreign_holder_as_attached_elsewhere() {
        let dir = tempfile::tempdir().unwrap();
        let session = "identity-live-foreign";
        write_json(
            &dir.path().join(format!("{session}.lease")),
            &v2_record(424_242.0, "holder-token"),
        );
        let result = acquire_as(
            dir.path(),
            session,
            &self_identity(own_pid()),
            &mut |_| Ok(true),
            &mut |_| Some(HOLDER_START + 2_000.0),
        )
        .unwrap();
        assert_eq!(
            result,
            AcquireTerminalLeaseResult::Held {
                holder: LeaseRecord {
                    pid: 424_242.0,
                    started_at_ms: HOLDER_START,
                    boot_at_ms: Some(BOOT),
                    ..LeaseRecord::default()
                }
            }
        );
    }

    #[test]
    fn treats_a_legacy_v1_record_by_liveness_plus_start_time_confirmation() {
        let dir = tempfile::tempdir().unwrap();
        let session = "identity-legacy";
        let path = dir.path().join(format!("{session}.lease"));
        let v1 = serde_json::json!({"pid": 424_242.0, "startedAtMs": HOLDER_START});
        write_json(&path, &v1);
        assert!(matches!(
            acquire_as(
                dir.path(),
                session,
                &self_identity(own_pid()),
                &mut |_| Ok(false),
                &mut |_| None
            )
            .unwrap(),
            AcquireTerminalLeaseResult::Acquired { .. }
        ));
        write_json(&path, &v1);
        assert!(matches!(
            acquire_as(
                dir.path(),
                session,
                &self_identity(own_pid()),
                &mut |_| Ok(true),
                &mut |_| Some(HOLDER_START + 60_000.0)
            )
            .unwrap(),
            AcquireTerminalLeaseResult::Acquired { .. }
        ));
        write_json(&path, &v1);
        assert_eq!(
            acquire_as(
                dir.path(),
                session,
                &self_identity(own_pid()),
                &mut |_| Ok(true),
                &mut |_| Some(HOLDER_START)
            )
            .unwrap(),
            AcquireTerminalLeaseResult::Held {
                holder: LeaseRecord::v1(424_242.0, HOLDER_START)
            }
        );
    }

    #[test]
    fn treats_an_unconfirmable_start_time_as_a_live_holder() {
        let dir = tempfile::tempdir().unwrap();
        let session = "identity-unconfirmable";
        write_json(
            &dir.path().join(format!("{session}.lease")),
            &v2_record(424_242.0, "holder-token"),
        );
        assert!(matches!(
            acquire_as(
                dir.path(),
                session,
                &self_identity(own_pid()),
                &mut |_| Ok(true),
                &mut |_| None
            )
            .unwrap(),
            AcquireTerminalLeaseResult::Held { .. }
        ));
    }
}
