use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::OnceLock;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use super::proc_view::{
    EntryVerdict, ProcScan, ProcView, classify_entry_error, live_proc_view, scan_every_process,
};

pub const LSOF_ANSWER_BOUND_MS: u64 = 60_000;

#[derive(Debug, Clone, Copy, Default)]
pub struct HolderLookupOptions {
    pub lsof_bound_ms: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LsofRun {
    pub exit_code: Option<i32>,
    pub stdout: String,
}

struct LockIdentity {
    real: Option<PathBuf>,
    dev: u64,
    ino: u64,
}

fn fd_verdict(view: &ProcView, pid: u32, fd_path: &Path, lock: &LockIdentity) -> EntryVerdict {
    let target = match std::fs::read_link(fd_path) {
        Ok(target) => target,
        Err(error) => {
            return if error.kind() == std::io::ErrorKind::NotFound {
                EntryVerdict::NoMatch
            } else {
                classify_entry_error(view, pid, &error)
            };
        }
    };
    if lock.real.as_deref() == Some(target.as_path()) {
        return EntryVerdict::Match;
    }
    match crate::fs::metadata(fd_path) {
        Ok(opened) => {
            #[cfg(unix)]
            {
                use std::os::unix::fs::MetadataExt;
                if opened.dev() == lock.dev && opened.ino() == lock.ino {
                    EntryVerdict::Match
                } else {
                    EntryVerdict::NoMatch
                }
            }
            #[cfg(not(unix))]
            {
                let _ = (opened, lock.dev, lock.ino);
                EntryVerdict::NoMatch
            }
        }
        Err(error) => {
            if error.kind() == std::io::ErrorKind::NotFound {
                EntryVerdict::NoMatch
            } else {
                classify_entry_error(view, pid, &error)
            }
        }
    }
}

fn process_holds_lock(view: &ProcView, pid: u32, lock: &LockIdentity) -> EntryVerdict {
    let fd_dir = view.root.join(pid.to_string()).join("fd");
    let fds = match crate::fs::read_dir_names(&fd_dir) {
        Ok(fds) => fds,
        Err(error) => return classify_entry_error(view, pid, &error),
    };
    let mut saw_indeterminate = false;
    for fd in fds {
        match fd_verdict(view, pid, &fd_dir.join(fd), lock) {
            EntryVerdict::Match => return EntryVerdict::Match,
            EntryVerdict::Indeterminate => saw_indeterminate = true,
            EntryVerdict::NoMatch | EntryVerdict::Gone => {}
        }
    }
    if saw_indeterminate {
        EntryVerdict::Indeterminate
    } else {
        EntryVerdict::NoMatch
    }
}

pub fn scan_proc_fd_holders(lock_path: &Path, view: &ProcView) -> ProcScan {
    let Ok(identity) = crate::fs::metadata(lock_path) else {
        return ProcScan::Holders(Vec::new());
    };
    #[cfg(unix)]
    let (dev, ino) = {
        use std::os::unix::fs::MetadataExt;
        (identity.dev(), identity.ino())
    };
    #[cfg(not(unix))]
    let (dev, ino) = (0_u64, 0_u64);

    let lock = LockIdentity {
        real: std::fs::canonicalize(lock_path).ok(),
        dev,
        ino,
    };
    scan_every_process(view, |pid| process_holds_lock(view, pid, &lock))
}

pub fn run_lsof_bounded(argv: &[&str], bound_ms: u64) -> Option<LsofRun> {
    let mut cmd = Command::new("lsof");
    cmd.args(argv);
    cmd.stdin(Stdio::null());
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::null());

    let mut child = cmd.spawn().ok()?;
    let mut stdout_handle = child.stdout.take();
    let (out_tx, out_rx) = mpsc::channel();
    let out_thread = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        if let Some(mut stream) = stdout_handle.take() {
            let _ = stream.read_to_end(&mut bytes);
        }
        let _ = out_tx.send(bytes);
    });

    let start = Instant::now();
    let bound = Duration::from_millis(bound_ms);
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {
                if start.elapsed() >= bound {
                    let _ = child.kill();
                    let _ = child.wait();
                    return None;
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    };

    let _ = out_thread.join();
    let stdout_bytes = out_rx.recv().ok()?;
    Some(LsofRun {
        exit_code: status.code(),
        stdout: String::from_utf8_lossy(&stdout_bytes).into_owned(),
    })
}

fn scan_lsof_holders(path: &Path, bound_ms: u64) -> Option<Vec<u32>> {
    let path_str = path.to_string_lossy();
    let run = run_lsof_bounded(&["-t", "--", &path_str], bound_ms)?;
    let listed = run.stdout.trim();
    if !listed.is_empty() {
        let mut pids = Vec::new();
        for line in listed.lines() {
            let line = line.trim_end_matches('\r');
            if line.is_empty() || !line.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            pids.push(line.parse::<u32>().ok()?);
        }
        return Some(pids);
    }
    match run.exit_code {
        Some(0) | Some(1) => Some(Vec::new()),
        _ => None,
    }
}

pub fn current_uid() -> Option<u32> {
    static UID: OnceLock<Option<u32>> = OnceLock::new();
    *UID.get_or_init(|| {
        #[cfg(unix)]
        {
            if let Ok(meta) = std::fs::symlink_metadata("/proc/self") {
                use std::os::unix::fs::MetadataExt;
                return Some(meta.uid());
            }
            let output = Command::new("id")
                .arg("-u")
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .output()
                .ok()?;
            if !output.status.success() {
                return None;
            }
            String::from_utf8_lossy(&output.stdout)
                .trim()
                .parse::<u32>()
                .ok()
        }
        #[cfg(not(unix))]
        {
            None
        }
    })
}

pub fn answer_from_proc_or_lsof(
    proc_scan: impl FnOnce(&ProcView) -> ProcScan,
    lsof_scan: impl FnOnce() -> Option<Vec<u32>>,
) -> Option<Vec<u32>> {
    if cfg!(windows) {
        return None;
    }
    if cfg!(target_os = "linux") {
        let uid = current_uid()?;
        match proc_scan(&live_proc_view(uid)) {
            ProcScan::Holders(pids) => return Some(pids),
            ProcScan::Indeterminate => return None,
            ProcScan::Unavailable => {}
        }
    }
    lsof_scan()
}

pub fn find_open_file_holder_pids(path: &Path, options: HolderLookupOptions) -> Option<Vec<u32>> {
    let bound_ms = options.lsof_bound_ms.unwrap_or(LSOF_ANSWER_BOUND_MS);
    answer_from_proc_or_lsof(
        |view| scan_proc_fd_holders(path, view),
        || scan_lsof_holders(path, bound_ms),
    )
}

pub fn has_open_file_holder(path: &Path, options: HolderLookupOptions) -> bool {
    match find_open_file_holder_pids(path, options) {
        Some(holders) => !holders.is_empty(),
        None => true,
    }
}
