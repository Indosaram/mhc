//! senpi `tool-child/socket-root.ts`: where a tool child's channel socket and its secret live (senpi#3566, #3576,
//! #3694).
//!
//! A socket root is used only once it is proven to be a real private directory of this user: nothing is removed from it
//! before that proof, and the only write before it is an exclusive, no-follow create of a new secret file, which can never
//! follow, truncate or reuse anything already at that name. The proof holds afterwards: the root is ours and 0700, so no
//! other user can rename, remove or replace it (and `/tmp` is sticky), and our own secret keeps it non-empty, so a
//! sibling's close-time sweep cannot remove it either.
use std::{
    fs, io,
    path::{Path, PathBuf},
};

use sha2::{Digest, Sha256};

#[cfg(unix)]
use crate::exclusive_secret::O_NOFOLLOW;

use crate::{
    exclusive_secret::{FileIdentity, file_identity, write_exclusive_secret},
    rpc_socket::{MAX_SOCKET_PATH_BYTES, SOCKET_SECRET_BYTES, socket_secret_path},
};

/// `<dir>/c-<generation>.sock` when it fits `sun_path`; otherwise the same name under a short private directory,
/// `/tmp/senpi-tc-<sha256(dir)[:12]>/` - never `TMPDIR`, whose macOS default is itself too long. The fallback is
/// predictable, so it is used only when it is a real directory of this user with no group or other access; when
/// anything else holds that name (another user's directory, a symlink), the socket goes under a fresh `mkdtemp`
/// directory instead, so a squatter on the predictable name can neither take the channel over nor block the start.
pub async fn channel_socket_path(dir: &Path, generation: u64) -> io::Result<PathBuf> {
    let name = format!("c-{generation}.sock");
    let primary = dir.join(&name);
    if cfg!(windows) || primary.as_os_str().len() <= MAX_SOCKET_PATH_BYTES {
        make_private_directory(dir)?;
        return Ok(primary);
    }
    let root = fallback_socket_root(dir);
    let claimed = match claim_private_directory(&root, true) {
        Ok(()) => true,
        Err(error) if is_socket_dir_unsafe(&error) => false,
        Err(error) => return Err(error),
    };
    // `mkdtemp` creates a new 0700 directory with an unpredictable name atomically, so nothing can be planted there.
    let chosen = if claimed {
        root
    } else {
        mkdtemp(&format!("{}-", root.display()))?
    };
    Ok(chosen.join(name))
}

/// The short private directory a socket falls back to when `<dir>/c-<generation>.sock` does not fit `sun_path`.
pub fn fallback_socket_root(dir: &Path) -> PathBuf {
    let digest = Sha256::digest(dir.to_string_lossy().as_bytes());
    let hex: String = digest.iter().map(|byte| format!("{byte:02x}")).collect();
    Path::new("/tmp").join(format!("senpi-tc-{}", hex.get(..12).unwrap_or(&hex)))
}

/// `mkdtemp(prefix)`: a new 0700 directory named `<prefix><6 random chars>`.
fn mkdtemp(prefix: &str) -> io::Result<PathBuf> {
    const ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
    loop {
        let mut random = [0u8; 6];
        getrandom::fill(&mut random).map_err(|error| io::Error::other(error.to_string()))?;
        let suffix: String = random
            .iter()
            .map(|byte| char::from(ALPHABET[usize::from(*byte) % ALPHABET.len()]))
            .collect();
        let path = PathBuf::from(format!("{prefix}{suffix}"));
        match private_dir_builder(false).create(&path) {
            Ok(()) => return Ok(path),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
}

fn private_dir_builder(recursive: bool) -> fs::DirBuilder {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(recursive);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder
}

#[derive(Debug)]
struct SocketDirUnsafeError {
    message: String,
}

impl SocketDirUnsafeError {
    fn new(root: &Path) -> Self {
        Self {
            message: format!(
                "socket_dir_unsafe: {} is not a private directory of this user",
                root.display()
            ),
        }
    }
}

impl std::fmt::Display for SocketDirUnsafeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for SocketDirUnsafeError {}

fn unsafe_error(root: &Path) -> io::Error {
    io::Error::other(SocketDirUnsafeError::new(root))
}

fn is_socket_dir_unsafe(error: &io::Error) -> bool {
    error
        .get_ref()
        .is_some_and(|inner| inner.is::<SocketDirUnsafeError>())
}

#[cfg(unix)]
fn current_uid() -> u32 {
    rustix::process::getuid().as_raw()
}

#[cfg(unix)]
fn owned_by_this_user(metadata: &fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    metadata.uid() == current_uid()
}

#[cfg(not(unix))]
fn owned_by_this_user(_metadata: &fs::Metadata) -> bool {
    true
}

/// A predictable directory under the shared `/tmp` is used only if it is a real directory of this user. It is opened
/// without following a symlink and checked through that handle before its mode is tightened, so nothing planted at the
/// path - a symlink or another user's directory - is ever changed or used.
fn claim_private_directory(root: &Path, create: bool) -> io::Result<()> {
    if create {
        match private_dir_builder(false).create(root) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
        let handle = fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | O_NOFOLLOW)
            .open(root)
            .map_err(|_| unsafe_error(root))?;
        let stat = handle.metadata()?;
        if !stat.is_dir() || stat.uid() != current_uid() {
            return Err(unsafe_error(root));
        }
        if stat.mode() & 0o077 != 0 {
            handle.set_permissions(fs::Permissions::from_mode(0o700))?;
        }
        Ok(())
    }
    #[cfg(not(unix))]
    {
        let stat = fs::symlink_metadata(root).map_err(|_| unsafe_error(root))?;
        if !stat.is_dir() {
            return Err(unsafe_error(root));
        }
        Ok(())
    }
}

fn make_private_directory(directory: &Path) -> io::Result<()> {
    private_dir_builder(true).create(directory)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(directory, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

#[derive(Debug, Clone)]
pub struct ListenerFiles {
    pub logical_path: PathBuf,
    pub secret_path: PathBuf,
    pub secret: [u8; SOCKET_SECRET_BYTES],
}

/// Claims the socket root, writes this generation's secret into it as a new file, and proves both before anything else
/// touches the root. A sibling's close-time sweep can remove the still-empty root between the claim and the secret write,
/// and another user can take the name in that window. The exclusive, no-follow write then either fails (ENOENT, EACCES,
/// EEXIST on a planted entry) or creates a new file in the taken directory; it never follows or truncates what is there.
/// So after the write the root is claimed again and the secret checked to be our own regular file in it, and only then is
/// a stale socket of this generation removed. Any failure withdraws a secret this attempt created without ever acting on
/// another file: its bytes are destroyed through its own handle once that handle is proven to be the file the create made,
/// and its name is unlinked only through a root proven ours again. A root swapped before the withdrawal therefore never
/// leads it to another directory's file; the exact limit is that an unprovable root may keep our now-empty file. A
/// first-attempt failure a taken fallback root explains retries, and the retry's claim refuses that root and moves to a
/// fresh `mkdtemp` directory.
pub async fn claim_listener_files(dir: &Path, generation: u64) -> io::Result<ListenerFiles> {
    let mut attempt = 1;
    loop {
        let logical_path = channel_socket_path(dir, generation).await?;
        let root = logical_path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_default();
        let secret_path = PathBuf::from(
            socket_secret_path(&logical_path.to_string_lossy()).map_err(io::Error::other)?,
        );
        let mut created: Option<FileIdentity> = None;
        match try_claim(&logical_path, &secret_path, &root, dir, &mut created).await {
            Ok(files) => return Ok(files),
            Err(error) => {
                if let Some(created) = created {
                    withdraw(&WithdrawTarget {
                        secret_path: &secret_path,
                        root: &root,
                        dir,
                        created,
                    })
                    .await;
                }
                if attempt >= 2 || !(root != dir && foreign_root_error(&error)) {
                    return Err(error);
                }
            }
        }
        attempt += 1;
    }
}

async fn try_claim(
    logical_path: &Path,
    secret_path: &Path,
    root: &Path,
    dir: &Path,
    created: &mut Option<FileIdentity>,
) -> io::Result<ListenerFiles> {
    let secret = write_secret(secret_path, root, dir, &mut |identity| {
        *created = Some(identity)
    })
    .await?;
    prove_root(root, dir, true)?;
    prove_own_file(secret_path, root)?;
    rm_socket(logical_path)?;
    Ok(ListenerFiles {
        logical_path: logical_path.to_path_buf(),
        secret_path: secret_path.to_path_buf(),
        secret,
    })
}

/// The exclusive write fails with EEXIST when a secret of this generation is left from an earlier run. Such a leftover is
/// removed only once the root is proven ours and the entry proven our own regular file: a root holding it is non-empty,
/// so no sibling sweep can remove it and no other user can replace it before the write is repeated.
async fn write_secret(
    secret_path: &Path,
    root: &Path,
    dir: &Path,
    on_created: &mut (dyn FnMut(FileIdentity) + Send),
) -> io::Result<[u8; SOCKET_SECRET_BYTES]> {
    match write_exclusive_secret(secret_path, on_created).await {
        Ok(secret) => Ok(secret),
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            prove_root(root, dir, true)?;
            prove_own_file(secret_path, root)?;
            remove_file_force(secret_path)?;
            write_exclusive_secret(secret_path, on_created).await
        }
        Err(error) => Err(error),
    }
}

fn prove_root(root: &Path, dir: &Path, create: bool) -> io::Result<()> {
    if root == dir {
        make_private_directory(root)
    } else {
        claim_private_directory(root, create)
    }
}

/// `rm(path, { force: true })` for a file or symlink: a missing entry is not an error.
pub(crate) fn remove_file_force(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error),
        _ => Ok(()),
    }
}

/// Removes a `.sock` path (the stale socket of a claim, or the live one at close). Tests inject the race window here.
pub(crate) fn rm_socket(path: &Path) -> io::Result<()> {
    #[cfg(test)]
    test_window::before_socket_removal(path)?;
    remove_file_force(path)
}

struct WithdrawTarget<'a> {
    secret_path: &'a Path,
    root: &'a Path,
    dir: &'a Path,
    created: FileIdentity,
}

/// Withdraws the secret this attempt created. Its bytes go first, through a handle checked to be that very file, so no
/// swap of the root can turn the truncation onto another file. Its name goes only through a root proven ours again (then
/// this user's 0700 directory, where nothing else can swap the entry) and only while the entry is still that file. Every
/// step is bounded: the open never blocks (a FIFO planted at the name fails at once with ENXIO), and re-proving a fallback
/// root never recreates it. A withdrawal that stops short emits a structured warning naming the path and its state; the
/// claim's own error is never replaced by it.
async fn withdraw(target: &WithdrawTarget<'_>) {
    let result = (|| -> io::Result<Option<&'static str>> {
        if !empty_created_file(target)? {
            return Ok(Some(
                "the entry is not the file this claim created; it was left untouched",
            ));
        }
        prove_root(target.root, target.dir, false)?;
        if !same_file(
            &file_identity(&fs::symlink_metadata(target.secret_path)?),
            &target.created,
        ) {
            return Ok(Some(
                "our file was emptied, then replaced by another entry, left untouched",
            ));
        }
        remove_file_force(target.secret_path)?;
        Ok(None)
    })();
    match result {
        Ok(None) => {}
        Ok(Some(reason)) => warn_withdrawal(target, "foreign", reason),
        Err(cleanup) => warn_withdrawal(target, "unknown", &error_code_or_message(&cleanup)),
    }
}

/// `state`: what is left at the path - `foreign` (another entry, untouched) or `unknown` (the step that failed is named).
fn warn_withdrawal(target: &WithdrawTarget<'_>, state: &str, reason: &str) {
    let detail = serde_json::json!({ "path": target.secret_path.to_string_lossy(), "state": state, "reason": reason });
    emit_warning(
        "tool-child socket secret withdrawal stopped short",
        "SENPI_TOOL_CHILD_SECRET_WITHDRAWAL",
        &detail.to_string(),
    );
}

/// `process.emitWarning(message, { code, detail })`.
fn emit_warning(message: &str, code: &str, detail: &str) {
    #[cfg(test)]
    test_window::record_warning(code, detail);
    eprintln!(
        "(senpi:{}) [{code}] Warning: {message}\n{detail}",
        std::process::id()
    );
}

fn empty_created_file(target: &WithdrawTarget<'_>) -> io::Result<bool> {
    let mut options = fs::OpenOptions::new();
    options.write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NONBLOCK | O_NOFOLLOW);
    }
    let handle = options.open(target.secret_path)?;
    if !same_file(&file_identity(&handle.metadata()?), &target.created) {
        return Ok(false);
    }
    handle.set_len(0)?;
    Ok(true)
}

fn same_file(entry: &FileIdentity, created: &FileIdentity) -> bool {
    entry.dev == created.dev && entry.ino == created.ino
}

/// The errno name Node reports as `error.code` (`String(code ?? cleanup)`), else the error's own text.
fn error_code_or_message(error: &io::Error) -> String {
    #[cfg(unix)]
    if let Some(name) = error.raw_os_error().and_then(errno_name) {
        return name.to_owned();
    }
    error.to_string()
}

#[cfg(unix)]
fn errno_name(errno: i32) -> Option<&'static str> {
    Some(match errno {
        libc::EACCES => "EACCES",
        libc::EPERM => "EPERM",
        libc::ENOENT => "ENOENT",
        libc::ENOTDIR => "ENOTDIR",
        libc::ELOOP => "ELOOP",
        libc::ENXIO => "ENXIO",
        libc::EEXIST => "EEXIST",
        libc::EISDIR => "EISDIR",
        libc::ENOTEMPTY => "ENOTEMPTY",
        libc::EBUSY => "EBUSY",
        _ => return None,
    })
}

/// A failure a swept-and-replaced fallback root explains: a refused claim, or a secret write the taken name refused.
/// (`FOREIGN_ROOT_CODES`: EACCES, EPERM, ENOENT, ENOTDIR, ELOOP.)
fn foreign_root_error(error: &io::Error) -> bool {
    if is_socket_dir_unsafe(error) {
        return true;
    }
    #[cfg(unix)]
    if let Some(errno) = error.raw_os_error() {
        return matches!(
            errno,
            libc::EACCES | libc::EPERM | libc::ENOENT | libc::ENOTDIR | libc::ELOOP
        );
    }
    matches!(
        error.kind(),
        io::ErrorKind::PermissionDenied | io::ErrorKind::NotFound
    )
}

fn prove_own_file(path: &Path, root: &Path) -> io::Result<()> {
    let stat = fs::symlink_metadata(path)?;
    if !stat.is_file() || !owned_by_this_user(&stat) {
        return Err(unsafe_error(root));
    }
    Ok(())
}

/// senpi `test/helpers/socket-root-window.ts`: the race window between claiming a tool-child socket root and the first
/// write into it, injected at the fs layer so the same tests drive every version of the claim.
#[cfg(test)]
pub(crate) mod test_window {
    use std::{
        collections::HashSet,
        io,
        path::{Path, PathBuf},
        sync::{Arc, Mutex, MutexGuard, PoisonError},
    };

    pub(crate) type Window = Box<dyn FnOnce(&Path) -> io::Result<()> + Send>;
    pub(crate) type SocketRemoval = Arc<dyn Fn(&Path) -> io::Result<()> + Send + Sync>;

    #[derive(Default)]
    pub(crate) struct SocketRootWindow {
        pub windows: Vec<Window>,
        pub on_socket_removal: Option<SocketRemoval>,
        pub order: Vec<String>,
        pub fired: HashSet<PathBuf>,
        pub warnings: Vec<(String, String)>,
    }

    static STATE: Mutex<Option<SocketRootWindow>> = Mutex::new(None);
    #[cfg(unix)]
    static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    pub(crate) fn with<T>(f: impl FnOnce(&mut SocketRootWindow) -> T) -> T {
        let mut guard: MutexGuard<'_, Option<SocketRootWindow>> =
            STATE.lock().unwrap_or_else(PoisonError::into_inner);
        f(guard.get_or_insert_with(SocketRootWindow::default))
    }

    /// Holds the global window state for one test (they share one process) and resets it before and after.
    #[cfg(unix)]
    pub(crate) struct Serial {
        _guard: tokio::sync::MutexGuard<'static, ()>,
    }

    #[cfg(unix)]
    pub(crate) async fn serial() -> Serial {
        let guard = SERIAL.lock().await;
        with(|state| *state = SocketRootWindow::default());
        Serial { _guard: guard }
    }

    #[cfg(unix)]
    impl Drop for Serial {
        fn drop(&mut self) {
            with(|state| *state = SocketRootWindow::default());
        }
    }

    fn is_secret(path: &Path) -> bool {
        path.to_string_lossy().ends_with(".sock.secret")
    }

    /// A `windows` hook runs once per root, at the first secret create that targets it.
    pub(crate) fn before_secret_create(path: &Path) -> io::Result<()> {
        if !is_secret(path) {
            return Ok(());
        }
        let root = path.parent().map(Path::to_path_buf).unwrap_or_default();
        let hook = with(|state| {
            if state.fired.contains(&root) || state.windows.is_empty() {
                return None;
            }
            state.fired.insert(root.clone());
            Some(state.windows.remove(0))
        });
        match hook {
            Some(hook) => hook(&root),
            None => Ok(()),
        }
    }

    /// Records a secret create (`order` holds secret writes and socket removals).
    pub(crate) fn record_secret(path: &Path) {
        if is_secret(path) {
            with(|state| state.order.push("secret".to_owned()));
        }
    }

    /// Runs before every removal of a `.sock` path.
    pub(crate) fn before_socket_removal(path: &Path) -> io::Result<()> {
        if !path.to_string_lossy().ends_with(".sock") {
            return Ok(());
        }
        let hook = with(|state| {
            state.order.push("rm-socket".to_owned());
            state.on_socket_removal.clone()
        });
        match hook {
            Some(hook) => hook(path),
            None => Ok(()),
        }
    }

    pub(crate) fn record_warning(code: &str, detail: &str) {
        with(|state| state.warnings.push((code.to_owned(), detail.to_owned())));
    }
}

#[cfg(all(test, unix))]
mod tests {
    use std::{
        io,
        os::unix::fs::{MetadataExt, symlink},
        path::{Path, PathBuf},
        sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        },
    };

    use super::{test_window::*, *};
    use crate::channel::{TOOL_CHILD_CHANNEL_ENV, TOOL_CHILD_SECRET_ENV, open_channel_listener};

    /// Removes the paths it was given, last first, however the test ends (`cleanups` upstream).
    #[derive(Default)]
    struct Cleanups(Vec<PathBuf>);

    impl Cleanups {
        fn push(&mut self, path: impl Into<PathBuf>) {
            self.0.push(path.into());
        }

        fn scratch(&mut self, prefix: &str) -> PathBuf {
            let dir = tempfile::Builder::new()
                .prefix(prefix)
                .tempdir()
                .expect("tempdir")
                .keep();
            self.push(dir.clone());
            dir
        }
    }

    impl Drop for Cleanups {
        fn drop(&mut self) {
            for path in self.0.drain(..).rev() {
                match fs::symlink_metadata(&path) {
                    Ok(meta) if meta.is_dir() => {
                        let _ = fs::set_permissions(
                            &path,
                            std::os::unix::fs::PermissionsExt::from_mode(0o700),
                        );
                        let _ = fs::remove_dir_all(&path);
                    }
                    Ok(_) => {
                        let _ = fs::remove_file(&path);
                    }
                    Err(_) => {}
                }
            }
        }
    }

    fn deep_dir(cleanups: &mut Cleanups, prefix: &str, a: &str, b: &str) -> PathBuf {
        let base = cleanups.scratch(prefix);
        base.join(a.repeat(60))
            .join(b.repeat(60))
            .join("tool-children")
            .join("0123456789abcdef0123456789abcdef0123")
    }

    fn channel_dir(env: &std::collections::HashMap<String, String>, key: &str) -> PathBuf {
        Path::new(&env[key])
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_default()
    }

    fn window(f: impl FnOnce(&Path) -> io::Result<()> + Send + 'static) -> Window {
        Box::new(f)
    }

    fn take_root_as(target: &Path, cleanups_extra: &Arc<std::sync::Mutex<Vec<PathBuf>>>) -> Window {
        let target = target.to_path_buf();
        let extra = Arc::clone(cleanups_extra);
        window(move |root| {
            fs::remove_dir(root)?;
            symlink(&target, root)?;
            extra
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(root.to_path_buf());
            Ok(())
        })
    }

    // ---- channel-root-sweep.test.ts (senpi#3685) ----

    #[tokio::test]
    async fn keeps_the_root_while_a_sibling_listener_holds_it_and_removes_it_after_the_last_close()
    {
        let _serial = serial().await;
        let mut cleanups = Cleanups::default();
        let dir = deep_dir(&mut cleanups, "tcsweep-", "a", "b");
        let root = fallback_socket_root(&dir);
        cleanups.push(&root);
        let first = open_channel_listener(&dir, 1).await.expect("first");
        let second = open_channel_listener(&dir, 2).await.expect("second");
        assert_eq!(channel_dir(&first.env, TOOL_CHILD_CHANNEL_ENV), root);
        assert_eq!(channel_dir(&second.env, TOOL_CHILD_CHANNEL_ENV), root);
        first.close().await.expect("close first");
        assert!(root.exists());
        second.close().await.expect("close second");
        assert!(!root.exists());
    }

    #[tokio::test]
    async fn moves_to_a_private_directory_of_its_own_when_the_root_is_swept_and_replaced_before_the_secret_is_written()
     {
        let _serial = serial().await;
        let mut cleanups = Cleanups::default();
        let dir = deep_dir(&mut cleanups, "tcsweep-", "a", "b");
        let root = fallback_socket_root(&dir);
        let foreign = cleanups.scratch("tcsweep-foreign-");
        cleanups.push(&root);
        // A sibling's sweep removes the claimed, still-empty root, and something else takes the name at once.
        let extra = Arc::default();
        with(|state| state.windows = vec![take_root_as(&foreign, &extra)]);
        let listener = open_channel_listener(&dir, 1).await.expect("listener");
        let socket_root = channel_dir(&listener.env, TOOL_CHILD_CHANNEL_ENV);
        cleanups.push(&socket_root);
        assert!(
            socket_root
                .to_string_lossy()
                .starts_with(&format!("{}-", root.display()))
        );
        assert_eq!(
            channel_dir(&listener.env, TOOL_CHILD_SECRET_ENV),
            socket_root
        );
        let stat = fs::symlink_metadata(&socket_root).expect("stat");
        assert!(stat.is_dir() && !stat.file_type().is_symlink());
        assert_eq!(stat.mode() & 0o077, 0);
        // The taken root cannot be proven ours, so nothing is unlinked through it: the secret is only emptied in place.
        for entry in fs::read_dir(&foreign).expect("read_dir") {
            assert_eq!(
                fs::read(entry.expect("entry").path()).expect("read").len(),
                0
            );
        }
        assert!(
            fs::symlink_metadata(&root)
                .expect("stat")
                .file_type()
                .is_symlink()
        );
        listener.close().await.expect("close");
        assert!(!socket_root.exists());
    }

    #[tokio::test]
    async fn keeps_a_swept_root_of_its_own_private_once_the_secret_write_recreated_it() {
        let _serial = serial().await;
        let mut cleanups = Cleanups::default();
        let dir = deep_dir(&mut cleanups, "tcsweep-", "a", "b");
        let root = fallback_socket_root(&dir);
        cleanups.push(&root);
        with(|state| state.windows = vec![window(|root| fs::remove_dir(root))]);
        let listener = open_channel_listener(&dir, 1).await.expect("listener");
        assert_eq!(channel_dir(&listener.env, TOOL_CHILD_CHANNEL_ENV), root);
        assert_eq!(fs::symlink_metadata(&root).expect("stat").mode() & 0o077, 0);
        listener.close().await.expect("close");
    }

    // ---- socket-root-proof.test.ts (senpi#3694) ----

    #[tokio::test]
    async fn removes_a_stale_socket_only_after_the_secret_is_written_and_the_root_proven() {
        let _serial = serial().await;
        let mut cleanups = Cleanups::default();
        let dir = cleanups.scratch("tcproof-primary-");
        let listener = open_channel_listener(&dir, 1).await.expect("listener");
        listener.close().await.expect("close");
        let order = with(|state| state.order.iter().take(2).cloned().collect::<Vec<_>>());
        assert_eq!(order, ["secret", "rm-socket"]);
    }

    #[tokio::test]
    async fn never_writes_through_an_entry_planted_at_the_secrets_name_in_a_taken_root() {
        let _serial = serial().await;
        let mut cleanups = Cleanups::default();
        let dir = deep_dir(&mut cleanups, "tcproof-", "c", "d");
        let root = fallback_socket_root(&dir);
        cleanups.push(&root);
        let victim = cleanups.scratch("tcproof-victim-").join("authorized_keys");
        fs::write(&victim, "keep me").expect("write");
        fs::set_permissions(&victim, std::os::unix::fs::PermissionsExt::from_mode(0o644))
            .expect("chmod");
        let taken = cleanups.scratch("tcproof-taken-");
        symlink(&victim, taken.join("c-1.sock.secret")).expect("symlink");
        let extra = Arc::default();
        with(|state| state.windows = vec![take_root_as(&taken, &extra)]);
        let listener = open_channel_listener(&dir, 1).await.expect("listener");
        cleanups.push(channel_dir(&listener.env, TOOL_CHILD_CHANNEL_ENV));
        for path in extra
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
        {
            cleanups.push(path);
        }
        assert_eq!(fs::read_to_string(&victim).expect("read"), "keep me");
        assert_eq!(
            fs::symlink_metadata(&victim).expect("stat").mode() & 0o777,
            0o644
        );
        assert!(
            channel_dir(&listener.env, TOOL_CHILD_CHANNEL_ENV)
                .to_string_lossy()
                .starts_with(&format!("{}-", root.display()))
        );
        listener.close().await.expect("close");
    }

    #[tokio::test]
    async fn leaves_no_secret_material_behind_when_the_retrys_root_is_taken_too_and_refuses_the_start()
     {
        let _serial = serial().await;
        let mut cleanups = Cleanups::default();
        let dir = deep_dir(&mut cleanups, "tcproof-", "c", "d");
        let root = fallback_socket_root(&dir);
        let foreign = cleanups.scratch("tcproof-foreign-");
        cleanups.push(&root);
        let extra = Arc::default();
        with(|state| {
            state.windows = vec![
                take_root_as(&foreign, &extra),
                take_root_as(&foreign, &extra),
            ]
        });
        let error = open_channel_listener(&dir, 1)
            .await
            .err()
            .expect("start refused");
        assert!(error.to_string().contains("socket_dir_unsafe"), "{error}");
        // The taken root cannot be proven ours, so nothing is unlinked through it: the secret is only emptied in place.
        for entry in fs::read_dir(&foreign).expect("read_dir") {
            assert_eq!(
                fs::read(entry.expect("entry").path()).expect("read").len(),
                0
            );
        }
    }

    #[tokio::test]
    async fn starts_under_a_fresh_private_directory_when_the_secret_write_into_a_taken_root_is_denied()
     {
        let _serial = serial().await;
        let mut cleanups = Cleanups::default();
        let dir = deep_dir(&mut cleanups, "tcproof-", "c", "d");
        let root = fallback_socket_root(&dir);
        let denied = cleanups.scratch("tcproof-denied-");
        fs::set_permissions(&denied, std::os::unix::fs::PermissionsExt::from_mode(0o500))
            .expect("chmod");
        cleanups.push(&root);
        let extra = Arc::default();
        with(|state| state.windows = vec![take_root_as(&denied, &extra)]);
        let listener = open_channel_listener(&dir, 1).await.expect("listener");
        let socket_root = channel_dir(&listener.env, TOOL_CHILD_CHANNEL_ENV);
        cleanups.push(&socket_root);
        assert!(
            socket_root
                .to_string_lossy()
                .starts_with(&format!("{}-", root.display()))
        );
        assert!(Path::new(&listener.env[TOOL_CHILD_SECRET_ENV]).exists());
        listener.close().await.expect("close");
    }

    #[tokio::test]
    async fn never_removes_a_file_through_a_root_that_is_swapped_while_it_is_still_empty() {
        let _serial = serial().await;
        let mut cleanups = Cleanups::default();
        let dir = deep_dir(&mut cleanups, "tcproof-", "c", "d");
        let root = fallback_socket_root(&dir);
        let victim = cleanups.scratch("tcproof-victim-");
        fs::write(victim.join("c-1.sock"), "not ours").expect("write");
        cleanups.push(&root);
        let swapped = Arc::new(AtomicBool::new(false));
        let extra = Arc::default();
        let (hook_root, hook_victim, hook_extra) =
            (root.clone(), victim.clone(), Arc::clone(&extra));
        with(|state| {
            state.on_socket_removal = Some(Arc::new(move |socket_path: &Path| {
                let socket_root = socket_path
                    .parent()
                    .map(Path::to_path_buf)
                    .unwrap_or_default();
                if swapped.load(Ordering::SeqCst)
                    || socket_root != hook_root
                    || fs::read_dir(&socket_root)?.next().is_some()
                {
                    return Ok(());
                }
                swapped.store(true, Ordering::SeqCst);
                take_root_as(&hook_victim, &hook_extra)(&socket_root)
            }));
        });
        if let Ok(listener) = open_channel_listener(&dir, 1).await {
            cleanups.push(channel_dir(&listener.env, TOOL_CHILD_CHANNEL_ENV));
            let _ = listener.close().await;
        }
        for path in extra
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
        {
            cleanups.push(path);
        }
        assert!(with(|state| state.order.contains(&"rm-socket".to_owned())));
        let names: Vec<_> = fs::read_dir(&victim)
            .expect("read_dir")
            .map(|entry| entry.expect("entry").file_name())
            .collect();
        assert_eq!(names, ["c-1.sock"]);
    }

    #[tokio::test]
    async fn never_blocks_on_a_fifo_planted_at_the_secrets_name_and_warns_what_it_left_there() {
        let _serial = serial().await;
        let mut cleanups = Cleanups::default();
        let dir = deep_dir(&mut cleanups, "tcproof-", "c", "d");
        let root = fallback_socket_root(&dir);
        cleanups.push(&root);
        let hook_root = root.clone();
        with(|state| {
            state.on_socket_removal = Some(Arc::new(move |socket_path: &Path| {
                if socket_path.parent() != Some(hook_root.as_path()) {
                    return Ok(());
                }
                // Whoever holds the root replaces our secret with a FIFO nobody ever reads, then the claim fails.
                let secret = format!("{}.secret", socket_path.display());
                fs::remove_file(&secret)?;
                let status = std::process::Command::new("mkfifo").arg(&secret).status()?;
                assert!(status.success());
                Err(io::Error::other("claim fails after the secret exists"))
            }));
        });
        let error = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            open_channel_listener(&dir, 1),
        )
        .await
        .expect("the withdrawal never blocks")
        .err()
        .expect("claim refused");
        assert_eq!(error.to_string(), "claim fails after the secret exists");
        let details: Vec<serde_json::Value> = with(|state| {
            state
                .warnings
                .iter()
                .filter(|(code, _)| code == "SENPI_TOOL_CHILD_SECRET_WITHDRAWAL")
                .filter_map(|(_, detail)| serde_json::from_str(detail).ok())
                .collect()
        });
        assert_eq!(
            details,
            [
                serde_json::json!({ "path": root.join("c-1.sock.secret").to_string_lossy(), "state": "unknown", "reason": "ENXIO" })
            ]
        );
    }

    #[tokio::test]
    async fn never_withdraws_a_secret_through_a_root_swapped_after_the_secret_was_written() {
        let _serial = serial().await;
        let mut cleanups = Cleanups::default();
        let dir = deep_dir(&mut cleanups, "tcproof-", "c", "d");
        let root = fallback_socket_root(&dir);
        let moved = PathBuf::from(format!("{}-moved", root.display()));
        let victim = cleanups.scratch("tcproof-victim-");
        fs::write(victim.join("c-1.sock.secret"), "another session's secret").expect("write");
        cleanups.push(&root);
        cleanups.push(&moved);
        let (hook_root, hook_moved, hook_victim) = (root.clone(), moved.clone(), victim.clone());
        with(|state| {
            state.on_socket_removal = Some(Arc::new(move |socket_path: &Path| {
                if socket_path.parent() != Some(hook_root.as_path()) {
                    return Ok(());
                }
                fs::rename(&hook_root, &hook_moved)?;
                symlink(&hook_victim, &hook_root)?;
                Err(io::Error::other("claim fails after the secret exists"))
            }));
        });
        let error = open_channel_listener(&dir, 1)
            .await
            .err()
            .expect("claim refused");
        assert_eq!(error.to_string(), "claim fails after the secret exists");
        assert_eq!(
            fs::read_to_string(victim.join("c-1.sock.secret")).expect("read"),
            "another session's secret"
        );
    }
}
