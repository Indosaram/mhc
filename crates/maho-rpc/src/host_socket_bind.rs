//! Port of senpi `packages/coding-agent/src/modes/rpc/host-socket-bind.ts`.
//!
//! Where a socket host listens and how it binds: the `--listen` address forms and the owned bind
//! (publishing into `socket_path` via `hard_link` from a private 0700 bind scratch directory so
//! symlink races and `bind(2)` link following on POSIX are refused, `#5319`).

use crate::socket_ownership::{
    BIND_SCRATCH_INFIX, BIND_SCRATCH_SOCKET, MAX_SOCKET_PATH_BYTES, SocketFileIdentity,
};
use std::path::{Path, PathBuf};

pub const ALIAS_PREFIX: &str = "senpi-bind-";
pub const ALIAS_LINK: &str = "d";
/// The alias owner record `{ pid, processStartTime, createdAt }` the alias reaper proves the owner dead by.
pub const ALIAS_OWNER: &str = ".owner";
const SHORT_TEMP_ROOT: &str = "/tmp";
const BIND_DIR_ATTEMPTS: usize = 8;

/// senpi `resolveSocketPath`: expands `unix://` / `unix://...` `--listen` values.
pub fn resolve_socket_path(value: &str, agent_dir: &str) -> String {
    if value == "unix://" {
        return Path::new(agent_dir)
            .join("rpc")
            .join("rpc.sock")
            .to_string_lossy()
            .into_owned();
    }
    if let Some(rest) = value.strip_prefix("unix://") {
        if rest.is_empty() {
            return Path::new(agent_dir)
                .join("rpc")
                .join("rpc.sock")
                .to_string_lossy()
                .into_owned();
        }
        if let Some(abstract_name) = rest.strip_prefix('@')
            && cfg!(target_os = "linux")
        {
            return format!("\0{abstract_name}");
        }
        return rest.to_owned();
    }
    value.to_owned()
}

pub fn format_socket_address(socket_path: &str) -> String {
    if let Some(abstract_name) = socket_path.strip_prefix('\0') {
        format!("unix://@{abstract_name}")
    } else {
        format!("unix://{socket_path}")
    }
}

/// The roots a long-path bind may place its alias in, in preference order (also what the alias reaper scans).
pub fn bind_alias_roots() -> Vec<PathBuf> {
    vec![std::env::temp_dir(), PathBuf::from(SHORT_TEMP_ROOT)]
}

/// Exported for tests: checks whether a root directory's ownership & mode prevent another user from
/// renaming our bind alias away (owned by us or root, and sticky `0o1000` or not group/other writable).
pub fn root_mode_is_guarded(mode: u32, uid: u32, self_uid: Option<u32>) -> bool {
    let owned_by_us_or_root = uid == 0 || self_uid.is_some_and(|self_id| uid == self_id);
    owned_by_us_or_root && ((mode & 0o1000) != 0 || (mode & 0o022) == 0)
}

#[cfg(unix)]
fn guarded_root(root: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    let Ok(meta) = std::fs::metadata(root) else {
        return false;
    };
    if !meta.is_dir() {
        return false;
    }
    // SAFETY: libc::geteuid() has no preconditions or memory side effects; avoid unsafe via nix/std or
    // check ownership when uid matches file owner or root. Wait: `unsafe_code = "forbid"` applies in maho-rpc!
    // We can check effective uid without unsafe by comparing against our `/proc/self` / tempfile owner,
    // or probing `std::fs::metadata("/proc/self")` on Linux (and `root_mode_is_guarded` directly).
    let self_uid = probe_self_uid();
    root_mode_is_guarded(meta.mode(), meta.uid(), self_uid)
}

#[cfg(unix)]
fn probe_self_uid() -> Option<u32> {
    use std::os::unix::fs::MetadataExt;
    if let Ok(meta) = std::fs::metadata("/proc/self") {
        return Some(meta.uid());
    }
    std::fs::metadata(std::env::temp_dir())
        .ok()
        .map(|meta| meta.uid())
}

#[cfg(unix)]
fn alias_root() -> std::io::Result<PathBuf> {
    let mut tried = Vec::new();
    for root in bind_alias_roots() {
        let longest = root
            .join(format!("{ALIAS_PREFIX}XXXXXX"))
            .join(ALIAS_LINK)
            .join(BIND_SCRATCH_SOCKET);
        let fits = longest.to_string_lossy().len() <= MAX_SOCKET_PATH_BYTES;
        if fits && guarded_root(&root) {
            return Ok(root);
        }
        tried.push(root.display().to_string());
    }
    Err(std::io::Error::other(format!(
        "bind_alias_unavailable: no short temp root is sticky or private to this user ({}); set TMPDIR to one",
        tried.join(", ")
    )))
}

#[cfg(unix)]
fn random_hex_12() -> String {
    let mut bytes = [0u8; 6];
    let _ = getrandom::fill(&mut bytes);
    hex::encode(bytes)
}

#[cfg(unix)]
fn make_bind_dir(socket_path: &str) -> std::io::Result<PathBuf> {
    use std::os::unix::fs::DirBuilderExt;
    for attempt in 1..=BIND_DIR_ATTEMPTS {
        let suffix = random_hex_12();
        let candidate = PathBuf::from(format!("{socket_path}{BIND_SCRATCH_INFIX}{suffix}"));
        let mut builder = std::fs::DirBuilder::new();
        builder.mode(0o700);
        match builder.create(&candidate) {
            Ok(()) => return Ok(candidate),
            Err(err)
                if err.kind() == std::io::ErrorKind::AlreadyExists
                    && attempt < BIND_DIR_ATTEMPTS =>
            {
                continue;
            }
            Err(err) => return Err(err),
        }
    }
    unreachable!("loop returns on final attempt")
}

/// Drops scratch bind directories (both `<socket_path>.bind-<hex>` and any `/tmp/senpi-bind-*` alias)
/// once the bound listener is no longer active.
#[derive(Debug, Default)]
pub struct BindScratchGuard {
    paths: Vec<PathBuf>,
}

impl BindScratchGuard {
    pub fn sweep(&mut self) {
        for path in self.paths.drain(..) {
            let _ = std::fs::remove_dir_all(path);
        }
    }
}

impl Drop for BindScratchGuard {
    fn drop(&mut self) {
        self.sweep();
    }
}

/// Binds and publishes a POSIX Unix listener at `socket_path` via an owned private bind directory and
/// `hard_link` (`#5319`), returning the listener, published `SocketFileIdentity`, and `BindScratchGuard`.
#[cfg(unix)]
pub fn listen_owned(
    socket_path: &str,
) -> std::io::Result<(
    tokio::net::UnixListener,
    Option<SocketFileIdentity>,
    BindScratchGuard,
)> {
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
    if socket_path.starts_with('\0') {
        let std_listener = std::os::unix::net::UnixListener::bind(socket_path)?;
        std_listener.set_nonblocking(true)?;
        let listener = tokio::net::UnixListener::from_std(std_listener)?;
        return Ok((listener, None, BindScratchGuard::default()));
    }

    let bind_dir = make_bind_dir(socket_path)?;
    let mut guard = BindScratchGuard {
        paths: vec![bind_dir.clone()],
    };
    let bound = bind_dir.join(BIND_SCRATCH_SOCKET);
    let direct = bound.to_string_lossy().into_owned();
    let bind_target = if direct.len() <= MAX_SOCKET_PATH_BYTES {
        PathBuf::from(&direct)
    } else {
        #[cfg(unix)]
        use std::os::unix::fs::DirBuilderExt;
        let root = alias_root()?;
        let alias_path = root.join(format!("{ALIAS_PREFIX}{}", random_hex_12()));
        std::fs::DirBuilder::new().mode(0o700).create(&alias_path)?;
        guard.paths.push(alias_path.clone());
        let pid = std::process::id();
        let start_time = crate::host_reservations::read_process_start_time(pid).unwrap_or_default();
        let created_at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_millis() as u64);
        let owner_json = serde_json::json!({
            "pid": pid,
            "processStartTime": start_time,
            "createdAt": created_at,
        });
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create_new(true).mode(0o600);
        use std::io::Write;
        let mut file = opts.open(alias_path.join(ALIAS_OWNER))?;
        file.write_all(owner_json.to_string().as_bytes())?;
        std::os::unix::fs::symlink(&bind_dir, alias_path.join(ALIAS_LINK))?;
        alias_path.join(ALIAS_LINK).join(BIND_SCRATCH_SOCKET)
    };

    let std_listener = std::os::unix::net::UnixListener::bind(&bind_target)?;
    std_listener.set_nonblocking(true)?;
    std::fs::set_permissions(&bound, std::fs::Permissions::from_mode(0o600))?;
    let entry = std::fs::symlink_metadata(&bound)?;
    if let Err(err) = std::fs::hard_link(&bound, socket_path) {
        drop(std_listener);
        if err.kind() == std::io::ErrorKind::AlreadyExists {
            return Err(std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                format!(
                    "{socket_path}: address already in use (an entry took the endpoint before the bind was published)."
                ),
            ));
        }
        return Err(err);
    }
    let listener = tokio::net::UnixListener::from_std(std_listener)?;
    let identity = SocketFileIdentity {
        dev: entry.dev(),
        ino: entry.ino(),
    };
    Ok((listener, Some(identity), guard))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_and_formats_unix_socket_addresses() {
        assert_eq!(
            resolve_socket_path("unix://", "/tmp/agent"),
            "/tmp/agent/rpc/rpc.sock"
        );
        assert_eq!(
            resolve_socket_path("unix:///var/run/a.sock", "/tmp/agent"),
            "/var/run/a.sock"
        );
        assert_eq!(
            format_socket_address("/var/run/a.sock"),
            "unix:///var/run/a.sock"
        );
        assert_eq!(format_socket_address("\0senpi-abs"), "unix://@senpi-abs");
    }

    #[test]
    fn root_mode_is_guarded_checks_owner_and_sticky_or_private_bits() {
        assert!(root_mode_is_guarded(0o1777, 0, Some(1000)));
        assert!(root_mode_is_guarded(0o700, 1000, Some(1000)));
        assert!(!root_mode_is_guarded(0o777, 1000, Some(1000)));
        assert!(!root_mode_is_guarded(0o700, 2000, Some(1000)));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn listen_owned_publishes_via_hard_link_and_refuses_preexisting_entry() {
        let dir = tempfile::tempdir().expect("tempdir");
        let sock = dir.path().join("rpc.sock");
        let (listener, identity, mut guard) =
            listen_owned(sock.to_str().expect("utf8")).expect("listen_owned");
        assert!(identity.is_some());
        assert!(sock.exists());

        // Attempting to publish over an occupied path fails with AlreadyExists.
        let err = listen_owned(sock.to_str().expect("utf8")).expect_err("must fail EEXIST");
        assert_eq!(err.kind(), std::io::ErrorKind::AlreadyExists);
        drop(listener);
        guard.sweep();
    }
}
