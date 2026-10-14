//! Port of the shell-resolution and detached-child registry of senpi
//! `packages/coding-agent/src/utils/shell.ts`. It lives in maho-tools because the core shell
//! tools, hooks, terminal tools and the CLI shutdown paths must share one registry.

use std::{collections::BTreeMap, path::Path, sync::{LazyLock, Mutex, MutexGuard, PoisonError}};

/// Family of a resolved shell executable, used to pick invocation arguments.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShellKind { Bash, Sh, Cmd, Powershell }

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShellConfig { pub shell: String, pub args: Vec<String>, pub command_transport: Option<&'static str>, pub kind: Option<ShellKind> }

/// Environment variable that overrides shell resolution with an explicit bash path (Windows-first).
pub const GIT_BASH_PATH_ENV: &str = "SENPI_GIT_BASH_PATH";

fn is_legacy_wsl_bash_path(path: &str) -> bool {
    let normalized = path.replace('/', "\\").to_lowercase();
    normalized.as_bytes().first().is_some_and(u8::is_ascii_alphabetic)
        && normalized.get(1..).is_some_and(|rest| matches!(rest, ":\\windows\\system32\\bash.exe" | ":\\windows\\sysnative\\bash.exe"))
}

/// Classify a shell executable by its basename so non-bash shells get correct args.
pub fn resolve_shell_kind(path: &str) -> ShellKind {
    let normalized = path.replace('\\', "/").to_lowercase();
    let base = normalized.rsplit('/').next().unwrap_or_default();
    match base.strip_suffix(".exe").unwrap_or(base) { "cmd" => ShellKind::Cmd, "powershell" | "pwsh" => ShellKind::Powershell, "sh" => ShellKind::Sh, _ => ShellKind::Bash }
}

fn get_bash_shell_config(shell: &str) -> ShellConfig {
    if is_legacy_wsl_bash_path(shell) { ShellConfig { shell: shell.to_owned(), args: vec!["-s".to_owned()], command_transport: Some("stdin"), kind: Some(ShellKind::Bash) } }
    else { ShellConfig { shell: shell.to_owned(), args: vec!["-c".to_owned()], command_transport: None, kind: Some(ShellKind::Bash) } }
}

/// `getShellConfigForPath`: cmd.exe uses `/c`, PowerShell `-NoProfile -Command`, bash/sh `-c`
/// (or WSL bash `-s` via stdin).
pub fn shell_config_for_path(path: &str) -> ShellConfig {
    let kind = resolve_shell_kind(path);
    let args: &[&str] = match kind { ShellKind::Cmd => &["/c"], ShellKind::Powershell => &["-NoProfile", "-Command"], ShellKind::Sh => &["-c"], ShellKind::Bash => return get_bash_shell_config(path) };
    ShellConfig { shell: path.to_owned(), args: args.iter().map(|arg| (*arg).to_owned()).collect(), command_transport: None, kind: Some(kind) }
}

fn find_executable_on_path(executable: &str) -> Option<String> {
    let output = std::process::Command::new(if cfg!(windows) { "where" } else { "which" }).arg(executable)
        .stdin(std::process::Stdio::null()).stderr(std::process::Stdio::null()).output().ok()?;
    if !output.status.success() { return None; }
    let text = String::from_utf8_lossy(&output.stdout);
    let first = text.trim().lines().next()?.trim_end_matches('\r');
    // Windows `where` can return non-existent paths; unix `which` output is trusted (Termux).
    if first.is_empty() || (cfg!(windows) && !Path::new(first).exists()) { None } else { Some(first.to_owned()) }
}

/// `getShellConfig(customShellPath)`. Resolution order:
/// 1. User-specified shellPath
/// 2. `SENPI_GIT_BASH_PATH`
/// 3. On Windows: Git Bash in known locations, then bash on PATH
/// 4. On Unix: /bin/bash, then bash on PATH, then fallback to sh
pub fn get_shell_config(custom_shell_path: Option<&str>) -> Result<ShellConfig, String> {
    if let Some(path) = custom_shell_path.filter(|path| !path.is_empty()) {
        return if Path::new(path).exists() { Ok(shell_config_for_path(path)) } else { Err(format!("Custom shell path not found: {path}")) };
    }
    if let Ok(path) = std::env::var(GIT_BASH_PATH_ENV) && !path.is_empty() {
        return if Path::new(&path).exists() { Ok(shell_config_for_path(&path)) } else { Err(format!("{GIT_BASH_PATH_ENV} points to a missing shell: {path}")) };
    }
    if cfg!(windows) {
        let paths: Vec<_> = ["ProgramFiles", "ProgramFiles(x86)"].into_iter().filter_map(|key| std::env::var(key).ok()).filter(|root| !root.is_empty()).map(|root| format!("{root}\\Git\\bin\\bash.exe")).collect();
        for path in &paths { if Path::new(path).exists() { return Ok(get_bash_shell_config(path)); } }
        if let Some(path) = find_executable_on_path("bash.exe") { return Ok(get_bash_shell_config(&path)); }
        return Err(format!("No bash shell found. Options:\n  1. Install Git for Windows: https://git-scm.com/download/win\n  2. Add your bash to PATH (Cygwin, MSYS2, etc.)\n  3. Set shellPath in settings.json\n\nSearched Git Bash in:\n{}", paths.iter().map(|path| format!("  {path}")).collect::<Vec<_>>().join("\n")));
    }
    if Path::new("/bin/bash").exists() { return Ok(get_bash_shell_config("/bin/bash")); }
    if let Some(path) = find_executable_on_path("bash") { return Ok(get_bash_shell_config(&path)); }
    Ok(ShellConfig { shell: "sh".to_owned(), args: vec!["-c".to_owned()], command_transport: None, kind: None })
}

/// A detached child we own until its whole process group is gone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TrackedDetachedChild { pub pid: i32, pub pgid: i32, pub leader_exited: bool }

/// Detached children are tracked as process GROUPS on unix so shutdown (SIGHUP/SIGTERM) kills
/// what a shell backgrounded (`sleep 30 &`, `nohup server &`) after the shell itself exited.
static TRACKED: LazyLock<Mutex<BTreeMap<i32, TrackedDetachedChild>>> = LazyLock::new(|| Mutex::new(BTreeMap::new()));
fn tracked() -> MutexGuard<'static, BTreeMap<i32, TrackedDetachedChild>> { TRACKED.lock().unwrap_or_else(PoisonError::into_inner) }

pub fn track_detached_child_pid(pid: i32) { tracked().insert(pid, TrackedDetachedChild { pid, pgid: pid, leader_exited: false }); }
pub fn untrack_detached_child_pid(pid: i32) { tracked().remove(&pid); }
/// Read-only snapshot of the tracked groups (diagnostics and tests).
pub fn list_tracked_detached_children() -> Vec<TrackedDetachedChild> { tracked().values().copied().collect() }

#[cfg(unix)]
fn signal_target_exists(target: i32) -> bool {
    // EPERM means the target exists but is not ours to signal; only ESRCH proves it is gone.
    nix::sys::signal::kill(nix::unistd::Pid::from_raw(target), None) != Err(nix::errno::Errno::ESRCH)
}
#[cfg(unix)]
fn process_group_is_alive(pgid: i32) -> bool { signal_target_exists(-pgid) }
#[cfg(unix)]
fn tracked_detached_child_is_gone(entry: &TrackedDetachedChild) -> bool {
    if process_group_is_alive(entry.pgid) { return false; }
    // A leader that already exited must not be probed by pid: that number can be recycled.
    entry.leader_exited || !signal_target_exists(entry.pid)
}

/// Record that a tracked child exited. Ownership is released only when its process group is
/// empty; Windows has no such group, so the entry is dropped on exit.
pub fn note_detached_child_exited(pid: i32) {
    let mut tracked = tracked();
    let Some(entry) = tracked.get_mut(&pid) else { return; };
    #[cfg(unix)]
    if process_group_is_alive(entry.pgid) { entry.leader_exited = true; return; }
    #[cfg(not(unix))]
    let _ = entry;
    tracked.remove(&pid);
}

/// Drop tracked groups that have no members left, so a long session cannot accumulate entries.
pub fn prune_tracked_detached_children() {
    #[cfg(unix)]
    tracked().retain(|_, entry| !tracked_detached_child_is_gone(entry));
}

pub fn kill_tracked_detached_children() {
    prune_tracked_detached_children();
    let mut tracked = tracked();
    for entry in tracked.values() {
        #[cfg(unix)]
        kill_tracked_detached_group(entry);
        #[cfg(not(unix))]
        if let Ok(pid) = u32::try_from(entry.pid) { kill_windows_process_tree(pid, &windows_taskkill_candidates(&std::env::vars().collect())); }
    }
    tracked.clear();
}

/// Kill a tracked group on unix. A stale leader pid is never signalled directly.
#[cfg(unix)]
fn kill_tracked_detached_group(entry: &TrackedDetachedChild) {
    use nix::{sys::signal::{kill, Signal}, unistd::Pid};
    if kill(Pid::from_raw(-entry.pgid), Signal::SIGKILL).is_ok() || entry.leader_exited { return; }
    let _already_dead = kill(Pid::from_raw(entry.pid), Signal::SIGKILL);
}

/// Ordered `taskkill` launchers to try, most reliable first.
pub fn windows_taskkill_candidates(env: &BTreeMap<String, String>) -> Vec<String> {
    let drive = env.get("SystemDrive").filter(|drive| !drive.is_empty()).map(|drive| format!("{drive}\\Windows"));
    let roots = [env.get("SystemRoot"), env.get("SYSTEMROOT"), env.get("windir"), drive.as_ref()];
    let mut paths = Vec::new();
    for root in roots.into_iter().flatten().filter(|root| !root.is_empty()) {
        for directory in ["System32", "Sysnative"] {
            let path = Path::new(root).join(directory).join("taskkill.exe").to_string_lossy().into_owned();
            if !paths.contains(&path) && Path::new(&path).exists() { paths.push(path); }
        }
    }
    paths.push("taskkill.exe".to_owned()); paths
}

fn taskkill_handled_tree(pid: u32, taskkill_path: &str) -> bool {
    let mut command = std::process::Command::new(taskkill_path);
    command.args(["/F", "/T", "/PID", &pid.to_string()]).stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
    #[cfg(windows)]
    { use std::os::windows::process::CommandExt; command.creation_flags(CREATE_NO_WINDOW); }
    // A launch failure means the launcher never started; any real taskkill exit code counts as handled.
    command.status().is_ok_and(|status| status.code().is_some())
}

#[cfg(windows)]
pub(crate) const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Kill a process and all its children on Windows via `taskkill /T`; synchronous on purpose.
pub fn kill_windows_process_tree(pid: u32, taskkill_paths: &[String]) {
    for taskkill_path in taskkill_paths { if taskkill_handled_tree(pid, taskkill_path) { return; } }
}

/// Kill a process and all its children (cross-platform).
pub fn kill_process_tree(pid: i32) {
    #[cfg(unix)]
    {
        use nix::{sys::signal::{kill, Signal}, unistd::Pid};
        if kill(Pid::from_raw(-pid), Signal::SIGKILL).is_err() { let _already_dead = kill(Pid::from_raw(pid), Signal::SIGKILL); }
    }
    #[cfg(not(unix))]
    if let Ok(pid) = u32::try_from(pid) { kill_windows_process_tree(pid, &windows_taskkill_candidates(&std::env::vars().collect())); }
}
