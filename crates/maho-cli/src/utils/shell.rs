use std::{collections::BTreeMap, sync::{LazyLock, Mutex}};
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShellKind { Bash, Sh, Cmd, Powershell }
pub struct ShellConfig { pub shell: String, pub args: Vec<String>, pub command_transport: Option<&'static str>, pub kind: Option<ShellKind> }
pub const GIT_BASH_PATH_ENV: &str = "SENPI_GIT_BASH_PATH";
pub fn resolve_shell_kind(path: &str) -> ShellKind {
    let normalized = path.replace('\\', "/").to_lowercase();
    let base = normalized.rsplit('/').next().unwrap_or_default();
    match base.strip_suffix(".exe").unwrap_or(base) { "cmd" => ShellKind::Cmd, "powershell" | "pwsh" => ShellKind::Powershell, "sh" => ShellKind::Sh, _ => ShellKind::Bash }
}
pub fn shell_config_for_path(path: &str) -> ShellConfig {
    let kind = resolve_shell_kind(path);
    let normalized = path.replace('/', "\\").to_lowercase();
    let legacy = normalized.as_bytes().first().is_some_and(u8::is_ascii_alphabetic) && normalized.get(1..).is_some_and(|rest| matches!(rest, ":\\windows\\system32\\bash.exe" | ":\\windows\\sysnative\\bash.exe"));
    let args = match kind { ShellKind::Cmd => vec!["/c"], ShellKind::Powershell => vec!["-NoProfile", "-Command"], ShellKind::Bash if legacy => vec!["-s"], _ => vec!["-c"] };
    ShellConfig { shell: path.to_owned(), args: args.into_iter().map(str::to_owned).collect(), command_transport: legacy.then_some("stdin"), kind: Some(kind) }
}
pub fn sanitize_binary_output(value: &str) -> String {
    value.chars().filter(|character| matches!(character, '\t' | '\n' | '\r') || (*character > '\u{1f}' && !('\u{fff9}'..='\u{fffb}').contains(character))).collect()
}
pub fn get_shell_env() -> BTreeMap<String, String> {
    let mut env: BTreeMap<String, String> = std::env::vars().collect();
    let key = env.keys().find(|key| key.eq_ignore_ascii_case("path")).cloned().unwrap_or_else(|| "PATH".to_owned());
    let current = env.get(&key).cloned().unwrap_or_default();
    let bin = crate::config::get_bin_dir();
    let delimiter = if cfg!(windows) { ';' } else { ':' };
    if !current.split(delimiter).any(|entry| entry == bin) { env.insert(key, if current.is_empty() { bin } else { format!("{bin}{delimiter}{current}") }); }
    env
}
async fn find_executable_on_path(executable: &str) -> Option<String> {
    let bytes = super::clipboard_command::run_clipboard_command(if cfg!(windows) { "where" } else { "which" }, &[executable], super::clipboard_command::ClipboardCommandOptions { timeout_ms: Some(5000), ..Default::default() }).await?;
    let text = String::from_utf8_lossy(&bytes);
    let path = text.trim().lines().next()?.trim_end_matches('\r');
    if path.is_empty() || (cfg!(windows) && !std::path::Path::new(path).exists()) { None } else { Some(path.to_owned()) }
}
pub async fn get_shell_config(custom_shell_path: Option<&str>) -> Result<ShellConfig, String> {
    if let Some(path) = custom_shell_path.filter(|path| !path.is_empty()) {
        return if std::path::Path::new(path).exists() { Ok(shell_config_for_path(path)) } else { Err(format!("Custom shell path not found: {path}")) };
    }
    if let Ok(path) = std::env::var(GIT_BASH_PATH_ENV) && !path.is_empty() {
        return if std::path::Path::new(&path).exists() { Ok(shell_config_for_path(&path)) } else { Err(format!("{GIT_BASH_PATH_ENV} points to a missing shell: {path}")) };
    }
    if cfg!(windows) {
        let paths: Vec<_> = ["ProgramFiles", "ProgramFiles(x86)"].into_iter().filter_map(|key| std::env::var(key).ok()).filter(|root| !root.is_empty()).map(|root| format!("{root}\\Git\\bin\\bash.exe")).collect();
        for path in &paths { if std::path::Path::new(path).exists() { return Ok(shell_config_for_path(path)); } }
        if let Some(path) = find_executable_on_path("bash.exe").await { return Ok(shell_config_for_path(&path)); }
        return Err(format!("No bash shell found. Options:\n  1. Install Git for Windows: https://git-scm.com/download/win\n  2. Add your bash to PATH (Cygwin, MSYS2, etc.)\n  3. Set shellPath in settings.json\n\nSearched Git Bash in:\n{}", paths.iter().map(|path| format!("  {path}")).collect::<Vec<_>>().join("\n")));
    }
    if std::path::Path::new("/bin/bash").exists() { return Ok(shell_config_for_path("/bin/bash")); }
    if let Some(path) = find_executable_on_path("bash").await { return Ok(shell_config_for_path(&path)); }
    Ok(ShellConfig { shell: "sh".to_owned(), args: vec!["-c".to_owned()], command_transport: None, kind: None })
}
pub const POWERSHELL_ARGS: [&str; 5] = ["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-Command"];
pub async fn get_powershell_config() -> Result<ShellConfig, String> {
    if !cfg!(windows) { return Err("The powershell tool is only available on Windows.".to_owned()); }
    let path = match find_executable_on_path("pwsh.exe").await { Some(path) => Some(path), None => find_executable_on_path("powershell.exe").await };
    let shell = path.ok_or("No PowerShell executable found. Install PowerShell or add powershell.exe/pwsh.exe to PATH.")?;
    Ok(ShellConfig { shell, args: POWERSHELL_ARGS.into_iter().map(str::to_owned).collect(), command_transport: None, kind: None })
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TrackedDetachedChild { pub pid: i32, pub pgid: i32, pub leader_exited: bool }
static TRACKED: LazyLock<Mutex<BTreeMap<i32, TrackedDetachedChild>>> = LazyLock::new(|| Mutex::new(BTreeMap::new()));
pub fn track_detached_child_pid(pid: i32) { TRACKED.lock().expect("tracked process lock").insert(pid, TrackedDetachedChild { pid, pgid: pid, leader_exited: false }); }
pub fn untrack_detached_child_pid(pid: i32) { TRACKED.lock().expect("tracked process lock").remove(&pid); }
pub fn list_tracked_detached_children() -> Vec<TrackedDetachedChild> { TRACKED.lock().expect("tracked process lock").values().copied().collect() }
#[cfg(unix)]
fn target_exists(pid: i32) -> bool { nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), None) != Err(nix::errno::Errno::ESRCH) }
#[cfg(unix)]
pub fn note_detached_child_exited(pid: i32) {
    let mut tracked = TRACKED.lock().expect("tracked process lock");
    if let Some(entry) = tracked.get_mut(&pid) { if target_exists(-entry.pgid) { entry.leader_exited = true; } else { tracked.remove(&pid); } }
}
#[cfg(unix)]
pub fn prune_tracked_detached_children() { TRACKED.lock().expect("tracked process lock").retain(|_, entry| target_exists(-entry.pgid) || (!entry.leader_exited && target_exists(entry.pid))); }
#[cfg(unix)]
pub fn kill_tracked_detached_children() {
    prune_tracked_detached_children();
    let mut tracked = TRACKED.lock().expect("tracked process lock");
    for entry in tracked.values() {
        if nix::sys::signal::kill(nix::unistd::Pid::from_raw(-entry.pgid), nix::sys::signal::Signal::SIGKILL).is_err() && !entry.leader_exited { let _ = nix::sys::signal::kill(nix::unistd::Pid::from_raw(entry.pid), nix::sys::signal::Signal::SIGKILL); }
    }
    tracked.clear();
}
#[cfg(unix)]
pub fn kill_process_tree(pid: i32) {
    if nix::sys::signal::kill(nix::unistd::Pid::from_raw(-pid), nix::sys::signal::Signal::SIGKILL).is_err() { let _ = nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), nix::sys::signal::Signal::SIGKILL); }
}
