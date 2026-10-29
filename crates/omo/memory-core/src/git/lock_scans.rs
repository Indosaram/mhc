//! Conservative host scans; an unreadable same-user process is never reported idle.
use std::path::Path;

pub trait GitLockScans {
    fn find_holders(&self, path: &Path) -> Option<Vec<u32>>;
    fn find_repo_users(&self, path: &Path) -> Option<Vec<u32>>;
}
pub struct ProductionLockScans;

#[cfg(target_os = "linux")]
fn processes(mut inspect: impl FnMut(u32, &Path) -> std::io::Result<bool>) -> Option<Vec<u32>> {
    use std::os::unix::fs::MetadataExt;
    let uid = std::fs::metadata("/proc/self").ok()?.uid();
    let mut found = Vec::new();
    for entry in std::fs::read_dir("/proc").ok()? {
        let entry = entry.ok()?;
        let Ok(pid) = entry.file_name().to_string_lossy().parse::<u32>() else {
            continue;
        };
        if pid == std::process::id() {
            continue;
        }
        let metadata = match entry.metadata() {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(_) => return None,
        };
        if metadata.uid() != uid {
            continue;
        }
        match inspect(pid, &entry.path()) {
            Ok(true) => found.push(pid),
            Ok(false) => (),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
            Err(_) => return None,
        }
    }
    Some(found)
}

#[cfg(target_os = "macos")]
fn lsof(args: &[String]) -> Option<String> {
    use std::{
        io::Read,
        process::{Command, Stdio},
        time::{Duration, Instant},
    };
    let mut child = Command::new("lsof")
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let mut output = child.stdout.take()?;
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut bytes = String::new();
        let result = output.read_to_string(&mut bytes).map(|_| bytes);
        let _ = tx.send(result);
    });
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => {
                return rx
                    .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                    .ok()?
                    .ok();
            }
            Ok(Some(_)) | Err(_) => return None,
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(5)),
        }
    }
}

impl GitLockScans for ProductionLockScans {
    fn find_holders(&self, path: &Path) -> Option<Vec<u32>> {
        #[cfg(target_os = "linux")]
        {
            use std::os::unix::fs::MetadataExt;
            let target = std::fs::metadata(path).ok()?;
            processes(|_, process| {
                for fd in std::fs::read_dir(process.join("fd"))? {
                    let fd = fd?;
                    match std::fs::metadata(fd.path()) {
                        Ok(meta) if meta.dev() == target.dev() && meta.ino() == target.ino() => {
                            return Ok(true);
                        }
                        Ok(_) => (),
                        Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
                        Err(e) => return Err(e),
                    }
                }
                Ok(false)
            })
        }
        #[cfg(target_os = "macos")]
        {
            let output = lsof(&[
                "-w".into(),
                "-Fpn".into(),
                "--".into(),
                path.to_string_lossy().into_owned(),
            ])?;
            let mut pids = Vec::new();
            for line in output.lines().filter(|line| line.starts_with('p')) {
                pids.push(line[1..].parse::<u32>().ok()?);
            }
            Some(pids)
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        {
            let _ = path;
            None
        }
    }
    fn find_repo_users(&self, path: &Path) -> Option<Vec<u32>> {
        #[cfg(target_os = "linux")]
        {
            use std::os::unix::fs::MetadataExt;
            let target = std::fs::metadata(path).ok()?;
            let root = std::fs::canonicalize(path).ok()?;
            processes(|_, process| {
                let cwd = process.join("cwd");
                if std::fs::read_link(&cwd)?.starts_with(&root) {
                    return Ok(true);
                }
                let meta = std::fs::metadata(cwd)?;
                Ok(meta.dev() == target.dev() && meta.ino() == target.ino())
            })
        }
        #[cfg(target_os = "macos")]
        {
            use std::os::unix::fs::MetadataExt;
            let uid = std::fs::metadata(path).ok()?.uid();
            let output = lsof(&[
                "-w".into(),
                "-a".into(),
                "-d".into(),
                "cwd".into(),
                "-u".into(),
                uid.to_string(),
                "-Fpn".into(),
            ])?;
            let root = std::fs::canonicalize(path).ok()?;
            let mut pid = None;
            let mut saw_self = false;
            let mut users = Vec::new();
            for line in output.lines() {
                if let Some(value) = line.strip_prefix('p') {
                    let value = value.parse::<u32>().ok()?;
                    saw_self |= value == std::process::id();
                    pid = Some(value);
                } else if let Some(name) = line.strip_prefix('n')
                    && let Some(pid) = pid.filter(|pid| *pid != std::process::id())
                    && Path::new(name).starts_with(&root)
                {
                    users.push(pid);
                }
            }
            saw_self.then_some(users)
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        {
            let _ = path;
            None
        }
    }
}
