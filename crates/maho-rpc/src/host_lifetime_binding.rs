//! The supervisor-to-host lifetime binding (senpi `host-lifetime-binding.ts`, senpi#3816 round 7): supervisor death,
//! for any cause, reaches its POSIX host as an event. Every platform choice lives here.
//! - Linux: a FIFO, mode 0600, in the supervisor's private 0700 hop directory. The supervisor creates it and opens
//!   the only writer O_RDWR (close-on-exec, so no descendant inherits it) before it spawns the host; the host reads
//!   it non-blocking, and EOF arrives when the kernel closes that writer.
//! - macOS: the host's reopened stdin (the key pipe), when the internal bootstrap supplies one.
//! - Windows: none here; the inherited lifetime pipe is unchanged.
use std::path::{Path, PathBuf};

/// The FIFO path the supervisor passes to its host.
pub const HOST_WATCH_LIFETIME_FIFO_ENV: &str = "SENPI_RPC_HOST_LIFETIME_FIFO";

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LifetimeBinding {
    /// Linux: the supervisor's lifetime FIFO.
    pub lifetime_fifo: Option<PathBuf>,
    /// macOS: the reopened stdin descriptor from the host's stdin bootstrap.
    pub lifetime_fd: Option<i32>,
}

/// What the supervisor keeps: the held writer (its close is the signal) and the path the host is told.
pub struct PreparedLifetimeBinding {
    pub lifetime_fifo: PathBuf,
    /// Held, never closed: the supervisor's death is what closes it.
    pub writer: std::fs::File,
}

/// Supervisor side: creates this platform's binding in the private hop directory. `None` leaves the host on the
/// recorded-identity poll.
pub fn prepare_lifetime_binding(hop_dir: &Path, platform: &str) -> Option<PreparedLifetimeBinding> {
    if platform != "linux" {
        return None;
    }
    let path = hop_dir.join("lifetime.fifo");
    let _ = std::fs::remove_file(&path);
    // One mkfifo, as the pinned source does: the standard library has no call for it and this crate forbids unsafe.
    let made = std::process::Command::new("mkfifo")
        .arg("-m")
        .arg("600")
        .arg(&path)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|status| status.success());
    if !made {
        return None;
    }
    // O_RDWR never blocks on a FIFO; std opens close-on-exec.
    let writer = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&path)
        .ok()?;
    Some(PreparedLifetimeBinding {
        lifetime_fifo: path,
        writer,
    })
}

/// How an armed binding ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LifetimeEvent {
    /// The supervisor is gone; carries the reason.
    Gone(String),
    /// The binding failed later: a read error is unknown, not supervisor death.
    Unavailable,
}

/// Host side: arms this platform's binding. Returns the receiver for its single event, or `None` when the platform
/// or launch provides no binding.
pub fn arm_lifetime_binding(
    binding: &LifetimeBinding,
    platform: &str,
) -> Option<tokio::sync::oneshot::Receiver<LifetimeEvent>> {
    if platform == "win32" {
        return None;
    }
    #[cfg(unix)]
    {
        let fifo = if platform == "linux" {
            binding
                .lifetime_fifo
                .as_deref()
                .and_then(open_fifo_read_end)
        } else {
            None
        };
        if let Some(fifo) = fifo {
            return Some(crate::host_lifetime_reader::watch_host_lifetime(
                fifo,
                "supervisor lifetime FIFO closed",
            ));
        }
        let fd = binding.lifetime_fd?;
        let file = std::fs::File::open(format!("/dev/fd/{fd}")).ok()?;
        Some(crate::host_lifetime_reader::watch_host_lifetime(
            file,
            "supervisor stdin lifetime closed",
        ))
    }
    #[cfg(not(unix))]
    {
        let _ = binding;
        None
    }
}

/// O_NONBLOCK returns at once with or without a writer; O_NOFOLLOW refuses a symlink at the path. Only this user's
/// own 0600 FIFO is the binding; anything else (a regular file reads EOF at once) is refused.
#[cfg(unix)]
fn open_fifo_read_end(path: &Path) -> Option<std::fs::File> {
    use std::os::unix::fs::{FileTypeExt, MetadataExt, OpenOptionsExt};
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK | libc::O_NOFOLLOW)
        .open(path)
        .ok()?;
    let metadata = file.metadata().ok()?;
    let owned = metadata.uid() == rustix::process::getuid().as_raw();
    (metadata.file_type().is_fifo() && owned && metadata.mode() & 0o777 == 0o600).then_some(file)
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    #[tokio::test]
    async fn the_supervisor_writer_closing_is_the_gone_event() {
        let temp = tempfile::tempdir().unwrap();
        let prepared = prepare_lifetime_binding(temp.path(), "linux").expect("mkfifo is available");
        let binding = LifetimeBinding {
            lifetime_fifo: Some(prepared.lifetime_fifo.clone()),
            lifetime_fd: None,
        };
        let event = arm_lifetime_binding(&binding, "linux").expect("the FIFO arms");
        drop(prepared.writer);
        let event = tokio::time::timeout(std::time::Duration::from_secs(5), event)
            .await
            .expect("EOF arrives")
            .unwrap();
        assert_eq!(
            event,
            LifetimeEvent::Gone("supervisor lifetime FIFO closed".into())
        );
    }
    #[test]
    fn a_regular_file_or_foreign_mode_is_not_a_binding() {
        let temp = tempfile::tempdir().unwrap();
        let file = temp.path().join("lifetime.fifo");
        std::fs::write(&file, "").unwrap();
        assert!(open_fifo_read_end(&file).is_none());
        assert!(prepare_lifetime_binding(temp.path(), "darwin").is_none());
        let binding = LifetimeBinding {
            lifetime_fifo: Some(file),
            lifetime_fd: None,
        };
        assert!(arm_lifetime_binding(&binding, "win32").is_none());
    }

    fn mkfifo(path: &Path, mode: &str) {
        let made = std::process::Command::new("mkfifo")
            .args(["-m", mode])
            .arg(path)
            .status()
            .expect("mkfifo runs");
        assert!(made.success());
    }

    /// senpi rpc-internal-lifetime-binding: the binding is this user's 0600 FIFO in the private hop directory.
    #[test]
    fn prepares_a_private_fifo_in_the_hop_directory() {
        use std::os::unix::fs::{FileTypeExt, PermissionsExt};
        let temp = tempfile::tempdir().unwrap();
        let prepared = prepare_lifetime_binding(temp.path(), "linux").expect("mkfifo is available");
        assert_eq!(prepared.lifetime_fifo, temp.path().join("lifetime.fifo"));
        let metadata = std::fs::metadata(&prepared.lifetime_fifo).unwrap();
        assert!(metadata.file_type().is_fifo());
        assert_eq!(metadata.permissions().mode() & 0o777, 0o600);
    }

    /// senpi rpc-internal-lifetime-fifo-open (#3816 review L5): a symlink to a FIFO and a FIFO with a foreign mode are
    /// refused; this user's 0600 FIFO is accepted.
    #[test]
    fn only_this_users_0600_fifo_opens_and_never_through_a_symlink() {
        let temp = tempfile::tempdir().unwrap();
        let real = temp.path().join("real.fifo");
        mkfifo(&real, "600");
        let link = temp.path().join("link.fifo");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        assert!(open_fifo_read_end(&link).is_none());
        let open = temp.path().join("open.fifo");
        mkfifo(&open, "644");
        assert!(open_fifo_read_end(&open).is_none());
        assert!(open_fifo_read_end(&real).is_some());
    }

    /// The macOS binding: the reopened stdin descriptor the internal bootstrap hands over ends in `Gone` at EOF.
    #[tokio::test]
    async fn the_reopened_stdin_descriptor_is_the_binding_off_linux() {
        use std::os::fd::AsRawFd;
        let (read_end, write_end) = rustix::pipe::pipe().unwrap();
        let binding = LifetimeBinding {
            lifetime_fifo: None,
            lifetime_fd: Some(read_end.as_raw_fd()),
        };
        let event = arm_lifetime_binding(&binding, "darwin").expect("the descriptor arms");
        drop(write_end);
        let event = tokio::time::timeout(std::time::Duration::from_secs(5), event)
            .await
            .expect("EOF arrives")
            .unwrap();
        assert_eq!(
            event,
            LifetimeEvent::Gone("supervisor stdin lifetime closed".into())
        );
        drop(read_end);
    }
}
