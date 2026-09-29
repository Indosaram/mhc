//! Port of `version-reap.ts`: retire older per-version daemon directories safely.
//!
//! Runs synchronously; the daemon server calls it on a blocking worker thread.

use std::path::Path;
use std::time::{Duration, Instant};

use crate::lock::{is_process_alive, read_lock_pid};
use crate::ownership::{DaemonOwner, read_daemon_owner};
use crate::paths::DaemonPaths;
use crate::platform;

const TERM_GRACE_MS: u64 = 5_000;
const KILL_GRACE_MS: u64 = 1_000;

/// `NodeJS.Platform` values the reaper branches on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReapPlatform {
    Linux,
    Darwin,
    Win32,
    OtherUnix,
}

impl ReapPlatform {
    pub const fn current() -> Self {
        if cfg!(target_os = "linux") {
            Self::Linux
        } else if cfg!(target_os = "macos") {
            Self::Darwin
        } else if cfg!(windows) {
            Self::Win32
        } else {
            Self::OtherUnix
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReapSignal {
    Term,
    Kill,
}

/// TS `DaemonCliAttestationDeps`.
pub struct DaemonCliAttestationDeps<'a> {
    pub read_proc_file: &'a dyn Fn(&str) -> Option<Vec<u8>>,
    pub execute_for_stdout: &'a dyn Fn(&str, &[String]) -> Option<String>,
}

impl Default for DaemonCliAttestationDeps<'_> {
    fn default() -> Self {
        Self {
            read_proc_file: &|path| std::fs::read(path).ok(),
            execute_for_stdout: &default_execute_for_stdout,
        }
    }
}

/// TS `ReapStaleDaemonVersionsDeps`; `Default` wires the real operating system.
pub struct ReapStaleDaemonVersionsDeps<'a> {
    pub platform: ReapPlatform,
    pub is_alive: &'a dyn Fn(u32) -> bool,
    pub attest: &'a dyn Fn(u32, ReapPlatform) -> bool,
    pub send_signal: &'a dyn Fn(u32, ReapSignal) -> bool,
    pub wait_for_exit: &'a dyn Fn(u32, u64) -> bool,
    pub remove_dir: &'a dyn Fn(&Path),
    pub read_owner: &'a dyn Fn(&DaemonPaths) -> Option<DaemonOwner>,
    pub log: &'a dyn Fn(&str),
    pub term_grace_ms: u64,
    pub kill_grace_ms: u64,
}

impl Default for ReapStaleDaemonVersionsDeps<'_> {
    fn default() -> Self {
        Self {
            platform: ReapPlatform::current(),
            is_alive: &|pid| is_process_alive(i64::from(pid)),
            attest: &|pid, platform| {
                attest_daemon_cli_process(pid, platform, &DaemonCliAttestationDeps::default())
            },
            send_signal: &default_send_signal,
            wait_for_exit: &default_wait_for_exit,
            remove_dir: &|path| {
                let _ignored = std::fs::remove_dir_all(path);
            },
            read_owner: &read_daemon_owner,
            log: &|message| eprintln!("[lsp-daemon] {message}"),
            term_grace_ms: TERM_GRACE_MS,
            kill_grace_ms: KILL_GRACE_MS,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VersionReapStatus {
    Terminated,
    Removed,
    Deferred,
    Spared,
}

impl VersionReapStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Terminated => "terminated",
            Self::Removed => "removed",
            Self::Deferred => "deferred",
            Self::Spared => "spared",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VersionReapResult {
    pub version: String,
    pub status: VersionReapStatus,
    pub reason: String,
}

/// TS `attestDaemonCliProcess`: prove a live pid is a daemon before signalling it.
///
/// Accepts the legacy Node daemon (`node … cli.js … daemon`) and this Rust binary
/// (`…/lsp-daemon daemon`), since both may own sibling version directories.
pub fn attest_daemon_cli_process(
    pid: u32,
    platform: ReapPlatform,
    deps: &DaemonCliAttestationDeps<'_>,
) -> bool {
    match platform {
        ReapPlatform::Win32 => false,
        ReapPlatform::Linux => (deps.read_proc_file)(&format!("/proc/{pid}/cmdline"))
            .is_some_and(|cmdline| is_daemon_argv(&split_cmdline(&cmdline))),
        ReapPlatform::Darwin | ReapPlatform::OtherUnix => {
            let args = [
                "-p".to_string(),
                pid.to_string(),
                "-o".to_string(),
                "command=".to_string(),
            ];
            (deps.execute_for_stdout)("/bin/ps", &args)
                .is_some_and(|command| is_daemon_command(command.trim()))
        }
    }
}

/// TS `reapStaleDaemonVersions`.
pub fn reap_stale_daemon_versions(
    own_paths: &DaemonPaths,
    deps: &ReapStaleDaemonVersionsDeps<'_>,
) -> Vec<VersionReapResult> {
    let Some(base_dir) = own_paths.dir.parent() else {
        return Vec::new();
    };
    let Ok(entries) = std::fs::read_dir(base_dir) else {
        return Vec::new();
    };
    let mut entries: Vec<(String, bool)> = entries
        .filter_map(Result::ok)
        .map(|entry| {
            let is_dir = entry.file_type().is_ok_and(|kind| kind.is_dir());
            (entry.file_name().to_string_lossy().into_owned(), is_dir)
        })
        .collect();
    entries.sort();
    let own_entry = format!("v{}", own_paths.version);
    let mut results = Vec::new();
    for (name, is_dir) in entries {
        if name == own_entry {
            continue;
        }
        let Some(version) = parse_version_entry(&name) else {
            continue;
        };
        if !is_dir {
            continue;
        }
        let version_dir = base_dir.join(&name);
        let sibling_paths = DaemonPaths {
            cli_path: own_paths.cli_path.clone(),
            ..DaemonPaths::under_dir(version_dir.clone(), version)
        };
        results.push(reap_one_version(
            version,
            &version_dir,
            &sibling_paths,
            deps,
        ));
    }
    results
}

fn result(version: &str, status: VersionReapStatus, reason: String) -> VersionReapResult {
    VersionReapResult {
        version: version.to_string(),
        status,
        reason,
    }
}

fn reap_one_version(
    version: &str,
    version_dir: &Path,
    sibling_paths: &DaemonPaths,
    deps: &ReapStaleDaemonVersionsDeps<'_>,
) -> VersionReapResult {
    let Some(owner) = (deps.read_owner)(sibling_paths) else {
        let lock_pid =
            read_lock_pid(&version_dir.join("daemon.lock")).and_then(|pid| u32::try_from(pid).ok());
        if let Some(lock_pid) = lock_pid.filter(|pid| (deps.is_alive)(*pid)) {
            (deps.log)(&format!(
                "reap: sparing v{version}: owner metadata missing but daemon.lock is held by live pid {lock_pid}"
            ));
            return result(
                version,
                VersionReapStatus::Spared,
                format!("owner metadata missing but lock held by live pid {lock_pid}"),
            );
        }
        (deps.remove_dir)(version_dir);
        return result(
            version,
            VersionReapStatus::Removed,
            "removed stale version dir without readable owner metadata".to_string(),
        );
    };
    let pid = owner.pid;
    if !(deps.is_alive)(pid) {
        (deps.remove_dir)(version_dir);
        return result(
            version,
            VersionReapStatus::Removed,
            format!("removed stale version dir for dead owner pid {pid}"),
        );
    }
    if deps.platform == ReapPlatform::Win32 {
        (deps.log)(&format!(
            "reap: deferring v{version}: Windows cannot prove pid ownership safely (named-pipe policy)"
        ));
        return result(
            version,
            VersionReapStatus::Deferred,
            "Windows cannot prove pid ownership safely; named-pipe reap deferred".to_string(),
        );
    }
    if !(deps.attest)(pid, deps.platform) {
        (deps.log)(&format!(
            "reap: sparing v{version}: pid {pid} is alive but cmdline attestation failed (possible recycled pid)"
        ));
        return result(
            version,
            VersionReapStatus::Spared,
            format!("pid {pid} attestation failed; possible recycled pid"),
        );
    }
    terminate_attested_owner(version, version_dir, pid, deps)
}

fn terminate_attested_owner(
    version: &str,
    version_dir: &Path,
    pid: u32,
    deps: &ReapStaleDaemonVersionsDeps<'_>,
) -> VersionReapResult {
    if !(deps.send_signal)(pid, ReapSignal::Term) {
        (deps.remove_dir)(version_dir);
        return result(
            version,
            VersionReapStatus::Removed,
            format!("owner pid {pid} exited before SIGTERM; removed stale version dir"),
        );
    }
    if (deps.wait_for_exit)(pid, deps.term_grace_ms) {
        (deps.remove_dir)(version_dir);
        return result(
            version,
            VersionReapStatus::Terminated,
            format!("terminated attested older daemon pid {pid} with SIGTERM"),
        );
    }
    (deps.log)(&format!(
        "reap: v{version} pid {pid} survived SIGTERM; escalating to SIGKILL"
    ));
    if !(deps.send_signal)(pid, ReapSignal::Kill) || !(deps.wait_for_exit)(pid, deps.kill_grace_ms)
    {
        (deps.log)(&format!(
            "reap: deferring v{version}: pid {pid} survived SIGKILL"
        ));
        return result(
            version,
            VersionReapStatus::Deferred,
            format!("attested daemon pid {pid} survived SIGKILL; dir kept"),
        );
    }
    (deps.remove_dir)(version_dir);
    result(
        version,
        VersionReapStatus::Terminated,
        format!("terminated attested older daemon pid {pid} after SIGKILL escalation"),
    )
}

/// `^v([A-Za-z0-9][A-Za-z0-9._+-]{0,127})$`.
fn parse_version_entry(name: &str) -> Option<&str> {
    let version = name.strip_prefix('v')?;
    crate::runtime_contract::validate_daemon_version(version).ok()?;
    Some(version)
}

fn split_cmdline(buffer: &[u8]) -> Vec<String> {
    String::from_utf8_lossy(buffer)
        .split('\0')
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .collect()
}

fn basename(value: &str) -> &str {
    value.rsplit(['/', '\\']).next().unwrap_or(value)
}

fn is_daemon_argv(argv: &[String]) -> bool {
    if argv.len() < 2 || !argv.iter().any(|value| value == "daemon") {
        return false;
    }
    let executable = basename(&argv[0]).to_ascii_lowercase();
    if executable == "lsp-daemon" || executable == "lsp-daemon.exe" {
        return true;
    }
    if executable != "node" && executable != "node.exe" {
        return false;
    }
    argv.iter()
        .any(|value| value == "cli.js" || value.ends_with("/cli.js") || value.ends_with("\\cli.js"))
}

fn has_word(haystack: &str, word: &str, ignore_case: bool) -> bool {
    let is_word = |character: char| character.is_ascii_alphanumeric() || character == '_';
    let (haystack, word) = if ignore_case {
        (haystack.to_ascii_lowercase(), word.to_ascii_lowercase())
    } else {
        (haystack.to_string(), word.to_string())
    };
    haystack.match_indices(&word).any(|(start, matched)| {
        let before = haystack[..start].chars().next_back();
        let after = haystack[start + matched.len()..].chars().next();
        !before.is_some_and(is_word) && !after.is_some_and(is_word)
    })
}

fn is_daemon_command(command: &str) -> bool {
    let has_daemon = has_word(command, "daemon", false);
    let node_cli = (has_word(command, "node", true) || has_word(command, "node.exe", true))
        && has_word(command, "cli.js", false);
    let rust_binary = command
        .split_whitespace()
        .next()
        .is_some_and(|executable| basename(executable) == "lsp-daemon");
    has_daemon && (node_cli || rust_binary)
}

fn default_execute_for_stdout(file: &str, args: &[String]) -> Option<String> {
    let output = std::process::Command::new(file).args(args).output().ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).into_owned())
}

fn default_send_signal(pid: u32, signal: ReapSignal) -> bool {
    #[cfg(unix)]
    {
        platform::send_signal(
            pid,
            match signal {
                ReapSignal::Term => libc::SIGTERM,
                ReapSignal::Kill => libc::SIGKILL,
            },
        )
    }
    #[cfg(not(unix))]
    {
        let _unused = (pid, signal);
        false
    }
}

/// Polls liveness every 100ms until `timeout_ms` (TS `defaultWaitForExit`).
fn default_wait_for_exit(pid: u32, timeout_ms: u64) -> bool {
    let deadline = Instant::now() + Duration::from_millis(timeout_ms);
    loop {
        if !is_process_alive(i64::from(pid)) {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[cfg(test)]
#[path = "version_reap_tests.rs"]
mod tests;
