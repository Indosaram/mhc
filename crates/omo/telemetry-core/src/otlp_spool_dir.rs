pub type OnLockLeftCallback = Option<Arc<dyn Fn(&io::Error) + Send + Sync>>;
use crate::otlp_spool_file::{Signal, count_spool_records};
use crate::otlp_spool_lock::*;
pub use crate::otlp_spool_names::*;
use std::{
    fs, io,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock, PoisonError},
    time::UNIX_EPOCH,
};
pub const SPOOL_CAP_BYTES: u64 = 5 * 1024 * 1024;
pub const SPOOL_MAX_AGE_MS: u64 = 14 * 24 * 60 * 60 * 1000;
pub const CLAIM_LEASE_MS: u64 = 5 * 60 * 1000;
pub const DEAD_OWNER_IDLE_MS: u64 = 300_000;
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DropReason {
    Evicted,
    Expired,
    Rejected,
    Corrupt,
    WriteFailed,
}
#[derive(Clone)]
pub struct SpoolContext {
    pub dir: PathBuf,
    pub boot_id: String,
    pub pid: u32,
    pub now_ms: Arc<dyn Fn() -> u64 + Send + Sync>,
    pub is_pid_alive: Arc<dyn Fn(u32) -> bool + Send + Sync>,
    pub cap_bytes: u64,
    pub max_age_ms: u64,
    pub on_drop: Arc<dyn Fn(DropReason, usize) + Send + Sync>,
    pub on_lock_left: OnLockLeftCallback,
}
impl SpoolContext {
    fn owner(&self) -> LockOwner {
        LockOwner {
            pid: self.pid,
            boot_id: self.boot_id.clone(),
        }
    }
}
pub const BATCH_ID_COUNTER_LIMIT: u32 = 1 << 26;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BatchIdCounter {
    pub ms: u64,
    pub counter: u32,
    pub overflowed: bool,
}
pub fn next_batch_id_counter(last: BatchIdCounter, ms: u64, random: f64) -> BatchIdCounter {
    if ms == last.ms || (ms.checked_add(1) == Some(last.ms) && last.overflowed) {
        if last.counter + 1 >= BATCH_ID_COUNTER_LIMIT {
            BatchIdCounter {
                ms: last.ms + 1,
                counter: 0,
                overflowed: true,
            }
        } else {
            BatchIdCounter {
                counter: last.counter + 1,
                ..last
            }
        }
    } else {
        BatchIdCounter {
            ms,
            counter: (random * f64::from(BATCH_ID_COUNTER_LIMIT / 2)).floor() as u32,
            overflowed: false,
        }
    }
}
static LAST_ID: OnceLock<Mutex<Option<BatchIdCounter>>> = OnceLock::new();
pub fn create_batch_id(unix_ms: u64) -> String {
    let mut bytes = *uuid::Uuid::new_v4().as_bytes();
    let random = f64::from(u32::from_be_bytes([
        bytes[10], bytes[11], bytes[12], bytes[13],
    ])) / (f64::from(u32::MAX) + 1.0);
    let mut last = LAST_ID
        .get_or_init(|| Mutex::new(None))
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    let state = match *last {
        Some(previous) => next_batch_id_counter(previous, unix_ms, random),
        None => BatchIdCounter {
            ms: unix_ms,
            counter: (random * f64::from(BATCH_ID_COUNTER_LIMIT / 2)) as u32,
            overflowed: false,
        },
    };
    *last = Some(state);
    bytes[..6].copy_from_slice(&state.ms.to_be_bytes()[2..]);
    bytes[6] = 0x70 | ((state.counter >> 22) as u8 & 15);
    bytes[7] = (state.counter >> 14) as u8;
    bytes[8] = 0x80 | ((state.counter >> 8) as u8 & 63);
    bytes[9] = state.counter as u8;
    uuid::Uuid::from_bytes(bytes).to_string()
}
pub fn compare_batch_ids(a: &str, b: &str) -> std::cmp::Ordering {
    a.cmp(b)
}
pub fn batch_created_ms(id: &str) -> u64 {
    u64::from_str_radix(
        &id.chars()
            .take(13)
            .filter(|c| *c != '-')
            .collect::<String>(),
        16,
    )
    .unwrap_or(0)
}
fn try_rename(dir: &Path, from: &str, to: &str) -> io::Result<bool> {
    match fs::rename(dir.join(from), dir.join(to)) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e),
    }
}
fn locked_rename(ctx: &SpoolContext, from: &str, to: &str) -> io::Result<bool> {
    let Some(token) = try_lock_sync(&ctx.dir, from, &ctx.owner())? else {
        return Ok(false);
    };
    let result = (|| {
        if !holds_lock_sync(&ctx.dir, from, &token)? {
            return Ok(false);
        }
        try_rename(&ctx.dir, from, to).map_err(|e| throw_after_roll_back(&ctx.dir, from, to, e))
    })();
    unlock(&ctx.dir, from, &token, ctx.on_lock_left.as_deref());
    result
}
pub fn list_spool(dir: &Path) -> io::Result<Vec<SpoolEntry>> {
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    entries
        .map(|entry| entry.map(|e| parse_spool_name(&e.file_name().to_string_lossy())))
        .collect()
}
pub async fn claim_batch(
    ctx: &SpoolContext,
    signal: Signal,
    batch_id: &str,
) -> io::Result<Option<String>> {
    let source = batch_name(signal, batch_id);
    let Some(token) = try_lock(&ctx.dir, &source, &ctx.owner()).await? else {
        return Ok(None);
    };
    let result = async {
        if !holds_lock(&ctx.dir, &source, &token).await? {
            return Ok(None);
        }
        let claimed = sending_name(
            signal,
            batch_id,
            &ctx.boot_id,
            (ctx.now_ms)() + CLAIM_LEASE_MS,
        );
        match tokio::fs::rename(ctx.dir.join(&source), ctx.dir.join(&claimed)).await {
            Ok(()) => Ok(Some(claimed)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(throw_after_roll_back(&ctx.dir, &source, &claimed, e)),
        }
    }
    .await;
    unlock(&ctx.dir, &source, &token, ctx.on_lock_left.as_deref());
    result
}
pub fn renew_claim(ctx: &SpoolContext, claimed: &str) -> io::Result<Option<String>> {
    let SpoolEntry::Sending {
        signal, batch_id, ..
    } = parse_spool_name(claimed)
    else {
        return Ok(None);
    };
    let renewed = sending_name(
        signal,
        &batch_id,
        &ctx.boot_id,
        (ctx.now_ms)() + CLAIM_LEASE_MS,
    );
    Ok(locked_rename(ctx, claimed, &renewed)?.then_some(renewed))
}
pub fn release_claim(ctx: &SpoolContext, claimed: &str) -> io::Result<bool> {
    match parse_spool_name(claimed) {
        SpoolEntry::Sending {
            signal, batch_id, ..
        } => try_link_move(&ctx.dir, claimed, &batch_name(signal, &batch_id)),
        _ => Ok(false),
    }
}
pub fn delete_file(dir: &Path, name: &str) -> io::Result<bool> {
    match fs::remove_file(dir.join(name)) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e),
    }
}
fn try_link_move(dir: &Path, from: &str, to: &str) -> io::Result<bool> {
    match fs::hard_link(dir.join(from), dir.join(to)) {
        Ok(()) => {
            delete_file(dir, from)?;
            Ok(true)
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(e)
            if e.kind() == io::ErrorKind::Unsupported
                || matches!(
                    e.raw_os_error(),
                    Some(libc::EPERM | libc::ENOSYS | libc::EOPNOTSUPP)
                ) =>
        {
            try_rename(dir, from, to)
        }
        Err(e) => Err(e),
    }
}
pub fn publish_batch(dir: &Path, active: &str, sealed: &str) -> io::Result<bool> {
    try_link_move(dir, active, sealed)
}
pub fn drop_file(ctx: &SpoolContext, name: &str, reason: DropReason) -> io::Result<()> {
    let records = count_spool_records(&ctx.dir.join(name));
    if delete_file(&ctx.dir, name)? {
        (ctx.on_drop)(reason, records);
    }
    Ok(())
}
pub fn quarantine_file(ctx: &SpoolContext, name: &str) -> io::Result<()> {
    let records = count_spool_records(&ctx.dir.join(name));
    fs::create_dir_all(ctx.dir.join(QUARANTINE_DIR))?;
    if try_rename(&ctx.dir, name, &format!("{QUARANTINE_DIR}/{name}"))? {
        (ctx.on_drop)(DropReason::Corrupt, records);
    }
    Ok(())
}
pub struct QuarantinedFile {
    pub path: String,
    pub size: u64,
    pub mtime_ms: u64,
}
pub fn list_quarantine(ctx: &SpoolContext) -> io::Result<Vec<QuarantinedFile>> {
    let dir = ctx.dir.join(QUARANTINE_DIR);
    let mut files = Vec::new();
    for entry in list_spool(&dir)? {
        if let Some(stat) = file_stat(&dir, entry.name())? {
            files.push(QuarantinedFile {
                path: format!("{QUARANTINE_DIR}/{}", entry.name()),
                size: stat.size,
                mtime_ms: stat.mtime_ms,
            });
        }
    }
    files.sort_by_key(|f| f.mtime_ms);
    Ok(files)
}
pub struct FileStat {
    pub size: u64,
    pub mtime_ms: u64,
    pub birth_ms: u64,
    pub links: u64,
}
#[cfg(windows)]
fn windows_link_count(path: &Path) -> io::Result<u64> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, GetFileInformationByHandle,
    };
    let file = fs::File::open(path)?;
    // SAFETY: BY_HANDLE_FILE_INFORMATION is plain integers, so all-zero is valid.
    #[allow(unsafe_code)]
    let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
    // SAFETY: the handle is live for the call and `info` is a valid out pointer.
    #[allow(unsafe_code)]
    let ok = unsafe { GetFileInformationByHandle(file.as_raw_handle(), &mut info) };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(u64::from(info.nNumberOfLinks))
}
pub fn file_stat(dir: &Path, name: &str) -> io::Result<Option<FileStat>> {
    let stat = match fs::metadata(dir.join(name)) {
        Ok(s) => s,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    let ms = |t: std::time::SystemTime| {
        t.duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64
    };
    let mtime_ms = ms(stat.modified()?);
    let birth_ms = stat.created().map(ms).unwrap_or(mtime_ms);
    #[cfg(unix)]
    let links = {
        use std::os::unix::fs::MetadataExt;
        stat.nlink()
    };
    #[cfg(windows)]
    let links = match windows_link_count(&dir.join(name)) {
        Ok(n) => n,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    #[cfg(not(any(unix, windows)))]
    let links = 1;
    Ok(Some(FileStat {
        size: stat.len(),
        mtime_ms,
        birth_ms,
        links,
    }))
}
pub fn sweep_spool(ctx: &SpoolContext) -> io::Result<()> {
    let now = (ctx.now_ms)();
    for entry in list_spool(&ctx.dir)? {
        match entry {
            SpoolEntry::Sending {
                name,
                signal,
                batch_id,
                lease_expires_ms,
                ..
            } if lease_expires_ms <= now => {
                if file_stat(&ctx.dir, &name)?.is_some_and(|s| s.links > 1) {
                    delete_file(&ctx.dir, &name)?;
                } else {
                    locked_rename(ctx, &name, &batch_name(signal, &batch_id))?;
                }
            }
            SpoolEntry::Active {
                name,
                signal,
                pid,
                boot_id,
            } => {
                let dead = if pid == ctx.pid {
                    boot_id != ctx.boot_id
                } else {
                    !(ctx.is_pid_alive)(pid)
                };
                if let Some(stat) = file_stat(&ctx.dir, &name)? {
                    if dead {
                        if stat.links > 1 {
                            delete_file(&ctx.dir, &name)?;
                        } else if now.saturating_sub(stat.mtime_ms) >= DEAD_OWNER_IDLE_MS {
                            locked_rename(
                                ctx,
                                &name,
                                &batch_name(signal, &create_batch_id(stat.birth_ms)),
                            )?;
                        }
                    } else if pid != ctx.pid && now.saturating_sub(stat.mtime_ms) > ctx.max_age_ms {
                        drop_file(ctx, &name, DropReason::Expired)?;
                    }
                }
            }
            SpoolEntry::Lock { name, .. } => {
                if let Some(stat) = file_stat(&ctx.dir, &name)? {
                    let owner = ctx.owner();
                    let limits = LockLimits {
                        lease_ms: CLAIM_LEASE_MS,
                        max_age_ms: ctx.max_age_ms,
                        is_pid_alive: ctx.is_pid_alive.as_ref(),
                        owner: &owner,
                    };
                    if is_abandoned_lock(
                        &ctx.dir,
                        &name,
                        now.saturating_sub(stat.mtime_ms),
                        &limits,
                    )? {
                        delete_file(&ctx.dir, &name)?;
                    }
                }
            }
            _ => {}
        }
    }
    for entry in list_spool(&ctx.dir)? {
        if let SpoolEntry::Batch { name, batch_id, .. } = entry
            && now.saturating_sub(batch_created_ms(&batch_id)) > ctx.max_age_ms {
                drop_file(ctx, &name, DropReason::Expired)?;
            }
    }
    for file in list_quarantine(ctx)? {
        if now.saturating_sub(file.mtime_ms) > ctx.max_age_ms {
            delete_file(&ctx.dir, &file.path)?;
        }
    }
    Ok(())
}
pub fn oldest_sealed_batches(dir: &Path) -> io::Result<Vec<SpoolEntry>> {
    let mut entries: Vec<_> = list_spool(dir)?
        .into_iter()
        .filter(|e| matches!(e, SpoolEntry::Batch { .. }))
        .collect();
    entries.sort_by(|a, b| match (a, b) {
        (SpoolEntry::Batch { batch_id: a, .. }, SpoolEntry::Batch { batch_id: b, .. }) => a.cmp(b),
        _ => std::cmp::Ordering::Equal,
    });
    Ok(entries)
}
