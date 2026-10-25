//! Kernel activity locks inherited by git and its descendants.
use std::fs::{File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

pub const GIT_ACTIVITY_LOCK_NAME: &str = "git-activity.flock";
pub const GIT_ACTIVITY_SHARED_WAIT_MS: u64 = 10_000;

pub fn identity_locks_dir_for_repo(repo: &Path) -> Option<PathBuf> {
    let repo = std::path::absolute(repo).ok()?;
    if repo.file_name()? != "repo" || repo.parent()?.parent()?.file_name()? != "agents" {
        return None;
    }
    Some(repo.parent()?.join("runtime/locks"))
}

pub struct GitActivityHold(pub File);
pub enum GitActivityProbe {
    Idle {
        hold: GitActivityHold,
        vouches: bool,
    },
    Busy,
    Unavailable,
}

fn open_activity_lock(repo: &Path) -> io::Result<Option<File>> {
    let Some(locks) = identity_locks_dir_for_repo(repo) else {
        return Ok(None);
    };
    let open = || {
        for dir in [locks.parent().expect("runtime parent"), locks.as_path()] {
            match std::fs::create_dir(dir) {
                Ok(()) => (),
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => (),
                Err(e) => return Err(e),
            }
        }
        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true).truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        options.open(locks.join(GIT_ACTIVITY_LOCK_NAME))
    };
    match open() {
        Ok(file) => Ok(Some(file)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

#[cfg(unix)]
fn try_lock(file: &File, exclusive: bool) -> io::Result<bool> {
    use std::os::fd::AsRawFd;
    // SAFETY: flock receives a live descriptor and valid operation flags.
    #[allow(unsafe_code)]
    let result = unsafe {
        libc::flock(
            file.as_raw_fd(),
            (if exclusive {
                libc::LOCK_EX
            } else {
                libc::LOCK_SH
            }) | libc::LOCK_NB,
        )
    };
    if result == 0 {
        return Ok(true);
    }
    let error = io::Error::last_os_error();
    if matches!(error.raw_os_error(), Some(libc::EWOULDBLOCK | libc::EINTR)) {
        Ok(false)
    } else {
        Err(error)
    }
}
#[cfg(not(unix))]
fn try_lock(_: &File, _: bool) -> io::Result<bool> {
    Err(io::Error::from(io::ErrorKind::Unsupported))
}

fn unavailable(e: &io::Error) -> bool {
    e.kind() == io::ErrorKind::Unsupported || {
        #[cfg(unix)]
        {
            matches!(
                e.raw_os_error(),
                Some(libc::ENOSYS | libc::EOPNOTSUPP | libc::ENOLCK)
            )
        }
        #[cfg(not(unix))]
        {
            true
        }
    }
}

pub fn hold_git_activity_shared(repo: &Path) -> io::Result<Option<GitActivityHold>> {
    let file = match open_activity_lock(repo) {
        Ok(Some(file)) => file,
        Ok(None) => return Ok(None),
        Err(e) if e.kind() == io::ErrorKind::PermissionDenied || e.raw_os_error() == Some(30) => {
            return Ok(None);
        }
        Err(e) => return Err(e),
    };
    let deadline = Instant::now() + Duration::from_millis(GIT_ACTIVITY_SHARED_WAIT_MS);
    loop {
        match try_lock(&file, false) {
            Ok(true) => return Ok(Some(GitActivityHold(file))),
            Err(e) if unavailable(&e) => return Ok(None),
            Err(e) => return Err(e),
            Ok(false) if Instant::now() >= deadline => {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "git-activity lock stayed exclusively held",
                ));
            }
            Ok(false) => std::thread::sleep(Duration::from_millis(5)),
        }
    }
}

pub fn probe_git_activity_idle(repo: &Path) -> GitActivityProbe {
    let Ok(Some(file)) = open_activity_lock(repo) else {
        return GitActivityProbe::Unavailable;
    };
    match try_lock(&file, true) {
        Ok(true) => GitActivityProbe::Idle {
            vouches: identity_locks_dir_for_repo(repo)
                .is_some_and(|dir| super::local_filesystem::is_on_local_filesystem(&dir)),
            hold: GitActivityHold(file),
        },
        Ok(false) => GitActivityProbe::Busy,
        Err(_) => GitActivityProbe::Unavailable,
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    #[test]
    fn shared_holds_exclude_probe_until_last_close() {
        let root = tempfile::tempdir().unwrap();
        let repo = root.path().join("agents/a/repo");
        std::fs::create_dir_all(&repo).unwrap();
        let first = hold_git_activity_shared(&repo).unwrap().unwrap();
        let second = hold_git_activity_shared(&repo).unwrap().unwrap();
        assert!(matches!(
            probe_git_activity_idle(&repo),
            GitActivityProbe::Busy
        ));
        drop(first);
        assert!(matches!(
            probe_git_activity_idle(&repo),
            GitActivityProbe::Busy
        ));
        drop(second);
        assert!(matches!(
            probe_git_activity_idle(&repo),
            GitActivityProbe::Idle { .. }
        ));
    }
    #[test]
    fn inherited_descriptor_survives_original_owner_close() {
        use std::io::{BufRead, BufReader};
        use std::os::{fd::AsRawFd, unix::process::CommandExt};
        use std::process::{Command, Stdio};
        let root = tempfile::tempdir().unwrap();
        let repo = root.path().join("agents/a/repo");
        std::fs::create_dir_all(&repo).unwrap();
        let hold = hold_git_activity_shared(&repo).unwrap().unwrap();
        let fd = hold.0.as_raw_fd();
        let mut command = Command::new("sh");
        command
            .args(["-c", "echo ready; read release"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped());
        // SAFETY: valid live descriptor; fcntl is async-signal-safe in the child.
        #[allow(unsafe_code)]
        unsafe {
            command.pre_exec(move || {
                if libc::fcntl(fd, libc::F_SETFD, 0) < 0 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let mut child = command.spawn().unwrap();
        let output = child.stdout.take().unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut ready = String::new();
            let result = BufReader::new(output).read_line(&mut ready).map(|_| ready);
            tx.send(result).unwrap();
        });
        let ready = rx.recv_timeout(Duration::from_secs(5));
        if ready.is_err() {
            child.kill().unwrap();
            child.wait().unwrap();
        }
        assert_eq!(ready.unwrap().unwrap(), "ready\n");
        drop(hold);
        assert!(matches!(
            probe_git_activity_idle(&repo),
            GitActivityProbe::Busy
        ));
        drop(child.stdin.take());
        child.wait().unwrap();
        assert!(matches!(
            probe_git_activity_idle(&repo),
            GitActivityProbe::Idle { .. }
        ));
    }
}
