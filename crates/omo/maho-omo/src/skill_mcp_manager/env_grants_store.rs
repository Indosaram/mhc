use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
};
use tokio::sync::watch;
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct GrantLabel {
    pub project: String,
    pub skill: String,
    pub server: String,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Grant {
    pub label: GrantLabel,
    pub names: Vec<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct GrantSnapshot {
    pub version: u8,
    pub generation: u64,
    pub grants: BTreeMap<String, Grant>,
}
impl Default for GrantSnapshot {
    fn default() -> Self {
        Self {
            version: 1,
            generation: 0,
            grants: BTreeMap::new(),
        }
    }
}
#[derive(Clone, Debug)]
pub struct GrantObservation {
    pub snapshot: GrantSnapshot,
    pub conservative: bool,
}
#[derive(Clone, PartialEq, Eq)]
struct StoreState {
    snapshot: GrantSnapshot,
    inode: Option<String>,
    valid: bool,
}
pub fn resolve_grants_store_path(config_dir: Option<&Path>) -> PathBuf {
    let root = config_dir.map(Path::to_path_buf).unwrap_or_else(|| {
        std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                std::env::var_os("HOME")
                    .map(PathBuf::from)
                    .unwrap_or_default()
                    .join(".config")
            })
            .join("omo")
    });
    let root = if root.is_absolute() {
        root
    } else {
        std::env::current_dir().unwrap_or_default().join(root)
    };
    root.join("mcp-env-grants.json")
}
fn valid_name(name: &str) -> bool {
    !name.is_empty() && !name.contains(['=', '\0'])
}
fn valid_digest(digest: &str) -> bool {
    digest.len() == 64
        && digest
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn parse(value: Value) -> Option<GrantSnapshot> {
    let mut snapshot: GrantSnapshot = serde_json::from_value(value).ok()?;
    if snapshot.version != 1 || snapshot.generation > 9_007_199_254_740_991 {
        return None;
    }
    for (digest, grant) in &mut snapshot.grants {
        if !valid_digest(digest) || !grant.names.iter().all(|n| valid_name(n)) {
            return None;
        }
        grant.names.sort();
        grant.names.dedup();
    }
    Some(snapshot)
}
pub fn revision(metadata: &fs::Metadata) -> String {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        format!("{}:{}", metadata.dev(), metadata.ino())
    }
    #[cfg(not(unix))]
    {
        format!("{:?}:{}", metadata.created(), metadata.len())
    }
}
/// Identity of the lock directory itself. Windows metadata exposes no file id on stable Rust, and a
/// replacement directory created right after a rename can share the creation time and zero length
/// of the one it replaced, so the volume serial and file index are read from an opened handle.
#[cfg(unix)]
fn lock_revision(path: &Path) -> std::io::Result<String> {
    fs::metadata(path).map(|metadata| revision(&metadata))
}
#[cfg(windows)]
fn open_lock_directory(path: &Path) -> std::io::Result<File> {
    use std::os::windows::fs::OpenOptionsExt;
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_FLAG_BACKUP_SEMANTICS, FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE, FILE_SHARE_READ,
        FILE_SHARE_WRITE, FILE_WRITE_ATTRIBUTES,
    };
    OpenOptions::new()
        .access_mode(FILE_READ_ATTRIBUTES | FILE_WRITE_ATTRIBUTES)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
        .open(path)
}
#[cfg(not(windows))]
fn open_lock_directory(path: &Path) -> std::io::Result<File> {
    File::open(path)
}
#[cfg(windows)]
fn lock_revision(path: &Path) -> std::io::Result<String> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, GetFileInformationByHandle,
    };
    let file = open_lock_directory(path)?;
    let mut info = BY_HANDLE_FILE_INFORMATION::default();
    #[allow(unsafe_code)]
    // SAFETY: the handle is owned by `file`, which outlives the call; `info` is a valid out pointer.
    let ok = unsafe { GetFileInformationByHandle(file.as_raw_handle(), &mut info) };
    if ok == 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(format!(
        "{}:{}:{}",
        info.dwVolumeSerialNumber, info.nFileIndexHigh, info.nFileIndexLow
    ))
}
fn state(path: &Path) -> StoreState {
    let result = (|| {
        let mut file = File::open(path).ok()?;
        let inode = revision(&file.metadata().ok()?);
        let mut text = String::new();
        file.read_to_string(&mut text).ok()?;
        let snapshot = parse(serde_json::from_str(&text).ok()?);
        Some(StoreState {
            valid: snapshot.is_some(),
            snapshot: snapshot.unwrap_or_default(),
            inode: Some(inode),
        })
    })();
    result.unwrap_or(StoreState {
        snapshot: GrantSnapshot::default(),
        inode: None,
        valid: false,
    })
}
pub fn read(path: &Path) -> GrantSnapshot {
    state(path).snapshot
}
fn canonical(value: &Value) -> Value {
    match value {
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(k, v)| (k.clone(), canonical(v)))
                .collect::<BTreeMap<_, _>>()
                .into_iter()
                .collect(),
        ),
        Value::Array(array) => Value::Array(array.iter().map(canonical).collect()),
        _ => value.clone(),
    }
}
pub fn declaration_digest(identity: &Value) -> String {
    format!(
        "{:x}",
        Sha256::digest(canonical(identity).to_string().as_bytes())
    )
}
fn acquire_lock(path: &Path) -> super::Result<GrantsLock> {
    let lock_path = path.with_extension("json.lock");
    for attempt in 0..=100 {
        match fs::create_dir(&lock_path) {
            Ok(()) => {
                let inode = lock_revision(&lock_path).map_err(|e| e.to_string())?;
                let lost = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
                let closed = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
                let thread_path = lock_path.clone();
                let expected = inode.clone();
                let compromised = lost.clone();
                let stopped = closed.clone();
                let heartbeat = std::thread::spawn(move || {
                    while !stopped.load(std::sync::atomic::Ordering::Acquire) {
                        std::thread::park_timeout(std::time::Duration::from_secs(10));
                        if stopped.load(std::sync::atomic::Ordering::Acquire) {
                            break;
                        }
                        let result = (|| -> std::io::Result<()> {
                            if lock_revision(&thread_path)? != expected {
                                return Err(std::io::Error::other("MCP grants lock reclaimed"));
                            }
                            let file = open_lock_directory(&thread_path)?;
                            file.set_times(
                                fs::FileTimes::new().set_modified(std::time::SystemTime::now()),
                            )
                        })();
                        if result.is_err() {
                            compromised.store(true, std::sync::atomic::Ordering::Release);
                            break;
                        }
                    }
                });
                return Ok(GrantsLock {
                    path: lock_path,
                    inode,
                    lost,
                    closed,
                    heartbeat: Some(heartbeat),
                });
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                if fs::metadata(&lock_path)
                    .and_then(|m| m.modified())
                    .ok()
                    .and_then(|t| t.elapsed().ok())
                    .is_some_and(|elapsed| elapsed > std::time::Duration::from_secs(30))
                {
                    match fs::remove_dir(&lock_path) {
                        Ok(()) => continue,
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                        Err(error) => return Err(error.to_string()),
                    }
                }
                if attempt == 100 {
                    return Err("MCP grants lock retries exhausted".into());
                }
                std::thread::sleep(std::time::Duration::from_millis(
                    (20.0 * 1.2f64.powi(attempt)).min(200.0) as u64,
                ));
            }
            Err(error) => return Err(error.to_string()),
        }
    }
    Err("MCP grants lock retries exhausted".into())
}
struct GrantsLock {
    path: PathBuf,
    inode: String,
    lost: std::sync::Arc<std::sync::atomic::AtomicBool>,
    closed: std::sync::Arc<std::sync::atomic::AtomicBool>,
    heartbeat: Option<std::thread::JoinHandle<()>>,
}
impl GrantsLock {
    fn ensure_owned(&self) -> super::Result<()> {
        if self.lost.load(std::sync::atomic::Ordering::Acquire)
            || lock_revision(&self.path)
                .map(|revision| revision != self.inode)
                .unwrap_or(true)
        {
            self.lost.store(true, std::sync::atomic::Ordering::Release);
            return Err("ELOCKLOST: MCP grants lock was lost before the write committed; the write was skipped.".into());
        }
        Ok(())
    }
}
impl Drop for GrantsLock {
    fn drop(&mut self) {
        self.closed
            .store(true, std::sync::atomic::Ordering::Release);
        if let Some(thread) = self.heartbeat.take() {
            thread.thread().unpark();
            if thread.join().is_err() {
                eprintln!("MCP grants lock heartbeat failed");
            }
        }
        if self.ensure_owned().is_ok()
            && let Err(error) = fs::remove_dir(&self.path)
        {
            eprintln!("MCP grants lock release: {error}");
        }
    }
}
fn replace_with_retry(
    windows: bool,
    mut ensure_owned: impl FnMut() -> super::Result<()>,
    mut rename: impl FnMut() -> std::io::Result<()>,
    mut delay: impl FnMut(std::time::Duration),
) -> super::Result<()> {
    for attempt in 0..=5 {
        ensure_owned()?;
        match rename() {
            Ok(()) => return Ok(()),
            Err(error)
                if windows && attempt < 5 && matches!(error.raw_os_error(), Some(5 | 32 | 33)) =>
            {
                delay(std::time::Duration::from_millis(50u64 << attempt));
            }
            Err(error) => return Err(error.to_string()),
        }
    }
    unreachable!("last rename attempt always returns")
}
fn update(
    path: &Path,
    mutate: impl FnOnce(&mut BTreeMap<String, Grant>),
) -> super::Result<GrantSnapshot> {
    let parent = path.parent().ok_or("Grants path has no parent")?;
    let mut directory = fs::DirBuilder::new();
    directory.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        directory.mode(0o700);
    }
    directory.create(parent).map_err(|e| e.to_string())?;
    let lock = acquire_lock(path)?;
    let mut next = read(path);
    mutate(&mut next.grants);
    next.generation = next
        .generation
        .checked_add(1)
        .filter(|n| *n <= 9_007_199_254_740_991)
        .ok_or("MCP grant generation exhausted")?;
    let temporary = parent.join(format!(".mcp-env-grants.{}.tmp", uuid::Uuid::new_v4()));
    let result = (|| {
        let mut options = OpenOptions::new();
        options.create_new(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&temporary).map_err(|e| e.to_string())?;
        writeln!(
            file,
            "{}",
            serde_json::to_string(&next).map_err(|e| e.to_string())?
        )
        .map_err(|e| e.to_string())?;
        file.sync_all().map_err(|e| e.to_string())?;
        drop(file);
        replace_with_retry(
            cfg!(windows),
            || lock.ensure_owned(),
            || fs::rename(&temporary, path),
            std::thread::sleep,
        )?;
        #[cfg(unix)]
        File::open(parent)
            .and_then(|f| f.sync_all())
            .map_err(|e| e.to_string())?;
        Ok(next)
    })();
    if let Err(error) = fs::remove_file(&temporary)
        && error.kind() != std::io::ErrorKind::NotFound
    {
        eprintln!("MCP grants temporary cleanup: {error}");
    }
    result
}
pub fn add_grant(
    path: &Path,
    digest: &str,
    label: GrantLabel,
    names: &[String],
) -> super::Result<GrantSnapshot> {
    if !valid_digest(digest) || !names.iter().all(|n| valid_name(n)) {
        return Err("Invalid MCP grant identity or variable name".into());
    }
    update(path, |grants| {
        let names = grants
            .get(digest)
            .into_iter()
            .flat_map(|g| g.names.iter())
            .chain(names)
            .cloned()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        grants.insert(digest.into(), Grant { label, names });
    })
}
pub fn revoke(path: &Path, digest: &str, names: Option<&[String]>) -> super::Result<GrantSnapshot> {
    update(path, |grants| {
        if let Some(entry) = grants.get_mut(digest) {
            entry
                .names
                .retain(|name| names.is_some_and(|names| !names.contains(name)));
            if entry.names.is_empty() {
                grants.remove(digest);
            }
        }
    })
}
pub fn list(path: &Path) -> BTreeMap<String, Grant> {
    read(path).grants
}
pub struct Observation {
    stop: watch::Sender<bool>,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for Observation {
    fn drop(&mut self) {
        self.stop.send_replace(true);
        self.task.abort();
    }
}
#[cfg(target_os = "linux")]
fn directory_watch(path: &Path) -> std::io::Result<tokio::io::unix::AsyncFd<std::os::fd::OwnedFd>> {
    use std::os::{fd::FromRawFd, unix::ffi::OsStrExt};
    let mut directory = path.parent().unwrap_or(Path::new("/"));
    while !directory.is_dir() {
        directory = directory.parent().unwrap_or(Path::new("/"));
    }
    let name = std::ffi::CString::new(directory.as_os_str().as_bytes())?;
    #[allow(unsafe_code)]
    // SAFETY: inotify returns an owned descriptor; the CString lives through add_watch.
    let fd = unsafe {
        let raw = libc::inotify_init1(libc::IN_NONBLOCK | libc::IN_CLOEXEC);
        if raw < 0 {
            return Err(std::io::Error::last_os_error());
        }
        let fd = std::os::fd::OwnedFd::from_raw_fd(raw);
        if libc::inotify_add_watch(
            raw,
            name.as_ptr(),
            libc::IN_CREATE
                | libc::IN_MOVED_TO
                | libc::IN_DELETE
                | libc::IN_CLOSE_WRITE
                | libc::IN_ATTRIB
                | libc::IN_DELETE_SELF
                | libc::IN_MOVE_SELF,
        ) < 0
        {
            return Err(std::io::Error::last_os_error());
        }
        fd
    };
    tokio::io::unix::AsyncFd::new(fd)
}
#[cfg(target_os = "linux")]
async fn watch_event(watcher: &Option<tokio::io::unix::AsyncFd<std::os::fd::OwnedFd>>) {
    use std::os::fd::AsRawFd;
    let Some(watcher) = watcher else {
        std::future::pending::<()>().await;
        return;
    };
    if let Ok(mut ready) = watcher.readable().await {
        let mut buffer = [0u8; 8192];
        #[allow(unsafe_code)]
        // SAFETY: buffer is writable for its full length and the watcher owns the descriptor.
        let count = unsafe {
            libc::read(
                watcher.get_ref().as_raw_fd(),
                buffer.as_mut_ptr().cast(),
                buffer.len(),
            )
        };
        if count < 0 && std::io::Error::last_os_error().kind() != std::io::ErrorKind::WouldBlock {
            eprintln!("MCP grants watch read: {}", std::io::Error::last_os_error());
        }
        ready.clear_ready();
    }
}
pub fn observe(path: PathBuf, listener: impl Fn(GrantObservation) + Send + 'static) -> Observation {
    let (stop, mut stopped) = watch::channel(false);
    let task = tokio::spawn(async move {
        let mut previous = state(&path);
        #[cfg(target_os = "linux")]
        let mut watcher = directory_watch(&path)
            .map_err(|error| eprintln!("MCP grants watch unavailable: {error}"))
            .ok();
        listener(GrantObservation {
            snapshot: previous.snapshot.clone(),
            conservative: false,
        });
        let mut timer = tokio::time::interval(std::time::Duration::from_secs(5));
        loop {
            #[cfg(target_os = "linux")]
            {
                tokio::select! {_=stopped.changed()=>return,_=timer.tick()=>{},_=watch_event(&watcher)=>{}}
                watcher = directory_watch(&path)
                    .map_err(|error| eprintln!("MCP grants watch unavailable: {error}"))
                    .ok();
            }
            #[cfg(not(target_os = "linux"))]
            tokio::select! {_=stopped.changed()=>return,_=timer.tick()=>{}}
            let current = state(&path);
            if current == previous {
                continue;
            }
            let conservative = !current.valid
                || current.snapshot.generation > previous.snapshot.generation.saturating_add(1)
                || current.snapshot.generation < previous.snapshot.generation
                || (current.inode != previous.inode
                    && current.snapshot.generation <= previous.snapshot.generation);
            previous = current;
            listener(GrantObservation {
                snapshot: previous.snapshot.clone(),
                conservative,
            });
        }
    });
    Observation { stop, task }
}

#[cfg(test)]
mod lock_tests {
    use super::*;
    #[test]
    fn windows_replace_retries_are_bounded_and_check_lock_before_every_attempt() {
        use std::cell::Cell;
        for code in [5, 32, 33] {
            let attempts = Cell::new(0);
            let mut delays = Vec::new();
            replace_with_retry(
                true,
                || Ok(()),
                || {
                    attempts.set(attempts.get() + 1);
                    if attempts.get() < 3 {
                        Err(std::io::Error::from_raw_os_error(code))
                    } else {
                        Ok(())
                    }
                },
                |delay| delays.push(delay.as_millis()),
            )
            .expect("eventual replace");
            assert_eq!(attempts.get(), 3);
            assert_eq!(delays, [50, 100]);
        }
        let attempts = Cell::new(0);
        let mut delays = Vec::new();
        assert!(
            replace_with_retry(
                true,
                || Ok(()),
                || {
                    attempts.set(attempts.get() + 1);
                    Err(std::io::Error::from_raw_os_error(5))
                },
                |delay| delays.push(delay.as_millis())
            )
            .is_err()
        );
        assert_eq!(attempts.get(), 6);
        assert_eq!(delays, [50, 100, 200, 400, 800]);
        let lost = Cell::new(false);
        let attempts = Cell::new(0);
        let error = replace_with_retry(
            true,
            || {
                if lost.get() {
                    Err("ELOCKLOST".into())
                } else {
                    Ok(())
                }
            },
            || {
                attempts.set(attempts.get() + 1);
                Err(std::io::Error::from_raw_os_error(5))
            },
            |_| lost.set(true),
        )
        .expect_err("lock lost during backoff");
        assert_eq!(error, "ELOCKLOST");
        assert_eq!(attempts.get(), 1);
        for (windows, code) in [(false, 5), (false, 32), (false, 33), (true, 2)] {
            let attempts = Cell::new(0);
            assert!(
                replace_with_retry(
                    windows,
                    || Ok(()),
                    || {
                        attempts.set(attempts.get() + 1);
                        Err(std::io::Error::from_raw_os_error(code))
                    },
                    |_| panic!("non-retryable failure must not delay")
                )
                .is_err()
            );
            assert_eq!(attempts.get(), 1);
        }
    }
    #[test]
    fn failed_rename_removes_temporary_and_lock() {
        let root = tempfile::tempdir().expect("root");
        let path = resolve_grants_store_path(Some(root.path()));
        fs::create_dir(&path).expect("unreplaceable directory");
        assert!(
            add_grant(
                &path,
                &"a".repeat(64),
                GrantLabel {
                    project: "project".into(),
                    skill: "skill".into(),
                    server: "server".into(),
                },
                &["ODD_DSN".into()]
            )
            .is_err()
        );
        assert!(read(&path).grants.is_empty());
        assert_eq!(fs::read_dir(root.path()).expect("directory").count(), 1);
    }
    #[test]
    fn compromised_lock_never_replaces_previous_snapshot() {
        let root = tempfile::tempdir().expect("root");
        let path = resolve_grants_store_path(Some(root.path()));
        let digest = "a".repeat(64);
        let label = GrantLabel {
            project: "project".into(),
            skill: "skill".into(),
            server: "server".into(),
        };
        add_grant(&path, &digest, label, &["KEPT".into()]).expect("initial grant");
        let before = fs::read(&path).expect("before");
        let result = update(&path, |grants| {
            grants.clear();
            fs::rename(
                path.with_extension("json.lock"),
                root.path().join("old-lock"),
            )
            .expect("steal lock");
            fs::create_dir(path.with_extension("json.lock")).expect("replacement lock");
        });
        assert!(result.expect_err("lost lock").contains("ELOCKLOST"));
        assert_eq!(fs::read(&path).expect("after"), before);
        assert!(path.with_extension("json.lock").is_dir());
    }
}
