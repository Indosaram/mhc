//! Port of `packages/coding-agent/src/core/kill-guard/runtime.ts` (senpi#5349, DESIGN-12096 §4' L2).
//!
//! A shim directory `<agent dir>/kill-guard/<version>/` written once per version, and the
//! environment that puts it first on PATH, points BASH_ENV at its wrapper and exports what the
//! shims need. POSIX only: on Windows nothing is installed.

use super::request::default_platform;
use super::shim_script::{
    KILL_GUARD_BASH_ENV, KILL_GUARD_PROMPT_HOOK_PREFIX, KILL_GUARD_SHIM_MARKER, KillGuardShimFiles,
    kill_guard_prompt_command, render_kill_guard_shims,
};
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex};

pub use super::shim_script::{
    KILL_GUARD_SHIM_MARKER as SHIM_MARKER, render_kill_guard_shims as render_shims,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KillGuardRuntime {
    pub dir: PathBuf,
    pub bash_env: PathBuf,
    /// The executable the shims run the judge with: the compiled binary.
    pub exec: String,
    /// Optional judge entry script; `None` for the compiled binary's hidden `__kill-guard` subcommand.
    pub entry: Option<String>,
    pub agent_pid: i64,
    pub protected_pids: Vec<i64>,
}

/// Unique holder handle for an active kill-guard runtime lease.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct KillGuardHolder(u64);

static NEXT_HOLDER_ID: AtomicU64 = AtomicU64::new(1);

impl KillGuardHolder {
    pub fn new() -> Self {
        Self(NEXT_HOLDER_ID.fetch_add(1, Ordering::Relaxed))
    }
}

impl Default for KillGuardHolder {
    fn default() -> Self {
        Self::new()
    }
}

fn sanitize_version(version: &str) -> String {
    version
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-') {
                ch
            } else {
                '_'
            }
        })
        .collect()
}

fn matches_shims(dir: &Path, files: &KillGuardShimFiles) -> bool {
    if !dir.join(KILL_GUARD_SHIM_MARKER).exists() {
        return false;
    }
    files.entries().iter().all(|(name, content)| {
        std::fs::read_to_string(dir.join(name)).ok().as_deref() == Some(*content)
    })
}

static NEXT_TEMP_SEQ: AtomicU64 = AtomicU64::new(1);

fn create_unique_dir_in(parent: &Path, prefix: &str) -> std::io::Result<PathBuf> {
    let pid = std::process::id();
    loop {
        let seq = NEXT_TEMP_SEQ.fetch_add(1, Ordering::Relaxed);
        let candidate = parent.join(format!("{prefix}{pid}-{seq}"));
        match std::fs::create_dir(&candidate) {
            Ok(()) => return Ok(candidate),
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(err) => return Err(err),
        }
    }
}

fn unique_temp_path_in(parent: &Path, prefix: &str) -> PathBuf {
    let pid = std::process::id();
    let seq = NEXT_TEMP_SEQ.fetch_add(1, Ordering::Relaxed);
    parent.join(format!("{prefix}{pid}-{seq}"))
}

fn place_shims(staged: &Path, dir: &Path, files: &KillGuardShimFiles) -> std::io::Result<()> {
    match std::fs::rename(staged, dir) {
        Ok(()) => return Ok(()),
        Err(error) => {
            if matches_shims(dir, files) {
                return Ok(());
            }
            if !dir.exists() {
                return Err(error);
            }
        }
    }
    let parent = dir.parent().unwrap_or_else(|| Path::new("."));
    let stale_path = unique_temp_path_in(parent, ".stale-");
    let _ = std::fs::remove_dir_all(&stale_path);
    std::fs::rename(dir, &stale_path)?;
    let rename_res = std::fs::rename(staged, dir);
    let _ = std::fs::remove_dir_all(&stale_path);
    rename_res
}

/// Writes the shim directory for `version` under `root` when it is missing or stale: staged in a
/// temp directory and renamed into place, so a reader sees either no directory or a complete one.
pub fn install_kill_guard_shims(
    root: &Path,
    version: &str,
    deadline_seconds: Option<u64>,
) -> std::io::Result<PathBuf> {
    let dir = root.join(sanitize_version(version));
    let dir_str = dir.to_string_lossy().into_owned();
    let files = render_kill_guard_shims(&dir_str, deadline_seconds);
    if matches_shims(&dir, &files) {
        return Ok(dir);
    }
    std::fs::create_dir_all(root)?;
    let staged_path = create_unique_dir_in(root, ".staging-")?;
    let write_res: std::io::Result<()> = (|| {
        for (name, content) in files.entries() {
            let target = staged_path.join(name);
            std::fs::write(&target, content)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mode = if name.ends_with(".sh") { 0o644 } else { 0o755 };
                std::fs::set_permissions(&target, std::fs::Permissions::from_mode(mode))?;
            }
        }
        std::fs::write(staged_path.join(KILL_GUARD_SHIM_MARKER), "")
    })();
    if let Err(error) = write_res {
        let _ = std::fs::remove_dir_all(&staged_path);
        return Err(error);
    }
    let result = place_shims(&staged_path, &dir, &files);
    let _ = std::fs::remove_dir_all(&staged_path);
    result.map(|()| dir)
}

fn is_kill_guard_bash_env(base: &BTreeMap<String, String>) -> bool {
    base.get("BASH_ENV")
        .and_then(|value| Path::new(value).file_name())
        .and_then(|name| name.to_str())
        == Some(KILL_GUARD_BASH_ENV)
        && base.contains_key("SENPI_KILL_GUARD_USER_BASH_ENV")
}

fn nested_shell_level(level: Option<&str>) -> bool {
    let Some(level) = level else {
        return false;
    };
    if level.is_empty() || level.len() > 3 || !level.bytes().all(|b| b.is_ascii_digit()) {
        return false;
    }
    if level.starts_with('0') {
        return false;
    }
    level.parse::<u16>().is_ok_and(|n| (1..999).contains(&n))
}

/// The variables that carry `runtime` into a process started with `base`.
pub fn kill_guard_environment(
    base: &BTreeMap<String, String>,
    runtime: &KillGuardRuntime,
) -> BTreeMap<String, String> {
    let path_key = base
        .keys()
        .find(|key| key.eq_ignore_ascii_case("path"))
        .cloned()
        .unwrap_or_else(|| "PATH".to_owned());
    let delimiter = if cfg!(windows) { ';' } else { ':' };
    let dir_str = runtime.dir.to_string_lossy().into_owned();
    let mut path_parts: Vec<&str> = Vec::new();
    path_parts.push(&dir_str);
    if let Some(current) = base.get(&path_key) {
        for entry in current.split(delimiter) {
            if !entry.is_empty() && entry != dir_str {
                path_parts.push(entry);
            }
        }
    }
    let user_bash_env = if is_kill_guard_bash_env(base) {
        base.get("SENPI_KILL_GUARD_USER_BASH_ENV")
            .cloned()
            .unwrap_or_default()
    } else {
        base.get("BASH_ENV").cloned().unwrap_or_default()
    };
    let user_prompt_command = match base.get("PROMPT_COMMAND") {
        Some(value) if value.starts_with(KILL_GUARD_PROMPT_HOOK_PREFIX) => base
            .get("SENPI_KILL_GUARD_USER_PROMPT_COMMAND")
            .cloned()
            .unwrap_or_default(),
        Some(value) => value.clone(),
        None => String::new(),
    };
    let protected = runtime
        .protected_pids
        .iter()
        .map(i64::to_string)
        .collect::<Vec<_>>()
        .join(",");

    let mut out = BTreeMap::new();
    out.insert(path_key, path_parts.join(&delimiter.to_string()));
    out.insert(
        "BASH_ENV".to_owned(),
        runtime.bash_env.to_string_lossy().into_owned(),
    );
    out.insert("SENPI_KILL_GUARD_USER_BASH_ENV".to_owned(), user_bash_env);
    out.insert(
        "PROMPT_COMMAND".to_owned(),
        kill_guard_prompt_command(&dir_str),
    );
    out.insert(
        "SENPI_KILL_GUARD_USER_PROMPT_COMMAND".to_owned(),
        user_prompt_command,
    );
    out.insert("SENPI_KILL_GUARD_PROTECTED".to_owned(), protected);
    out.insert("SENPI_KILL_GUARD_EXEC".to_owned(), runtime.exec.clone());
    out.insert(
        "SENPI_KILL_GUARD_EXEC_ENTRY".to_owned(),
        runtime.entry.clone().unwrap_or_default(),
    );
    out.insert(
        "SENPI_KILL_GUARD_AGENT_PID".to_owned(),
        runtime.agent_pid.to_string(),
    );
    if !nested_shell_level(base.get("SHLVL").map(String::as_str)) {
        out.insert("SHLVL".to_owned(), "1".to_owned());
    }
    out
}

#[derive(Default)]
struct KillGuardRegistry {
    installed: HashMap<PathBuf, KillGuardRuntime>,
    holders: Vec<(KillGuardHolder, KillGuardRuntime)>,
}

static REGISTRY: LazyLock<Mutex<KillGuardRegistry>> =
    LazyLock::new(|| Mutex::new(KillGuardRegistry::default()));

static TOOLS_HOOK_ONCE: std::sync::Once = std::sync::Once::new();

/// Ensures `maho-tools` merges `active_kill_guard_environment` into default local shell envs
/// without creating a Cargo dependency cycle (`maho-core` depends on `maho-tools`).
pub fn ensure_tools_env_hook_registered() {
    TOOLS_HOOK_ONCE.call_once(|| {
        maho_tools::bash_executor::set_kill_guard_env_overlay(Some(Arc::new(|base| {
            active_kill_guard_environment(base)
        })));
    });
}

fn current_exec_path() -> String {
    std::env::current_exe()
        .map(|path| path.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn build_runtime(agent_dir: &Path, protected_pids: Vec<i64>) -> std::io::Result<KillGuardRuntime> {
    let version = crate::config::display_version(
        option_env!("MHC_BUILD_VERSION").unwrap_or(env!("CARGO_PKG_VERSION")),
    );
    let dir = install_kill_guard_shims(&agent_dir.join("kill-guard"), &version, None)?;
    Ok(KillGuardRuntime {
        bash_env: dir.join(KILL_GUARD_BASH_ENV),
        dir,
        exec: current_exec_path(),
        entry: None,
        agent_pid: i64::from(std::process::id()),
        protected_pids,
    })
}

pub struct ActivateKillGuardRuntimeOptions<'a, F>
where
    F: FnOnce() -> Vec<i64>,
{
    pub holder: KillGuardHolder,
    pub agent_dir: &'a Path,
    pub protected_pids: F,
    pub platform: Option<&'a str>,
}

/// Installs (once per agent dir) and activates the runtime layer for this process; `None` on Windows.
pub fn activate_kill_guard_runtime<F>(
    options: ActivateKillGuardRuntimeOptions<'_, F>,
) -> std::io::Result<Option<KillGuardRuntime>>
where
    F: FnOnce() -> Vec<i64>,
{
    if options.platform.unwrap_or(default_platform()) == "win32" {
        return Ok(None);
    }
    ensure_tools_env_hook_registered();
    let mut registry = REGISTRY
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let runtime = if let Some(existing) = registry.installed.get(options.agent_dir) {
        existing.clone()
    } else {
        let first_pids = registry
            .installed
            .values()
            .next()
            .map(|r| r.protected_pids.clone())
            .unwrap_or_else(options.protected_pids);
        let built = build_runtime(options.agent_dir, first_pids)?;
        registry
            .installed
            .insert(options.agent_dir.to_path_buf(), built.clone());
        built
    };
    registry.holders.retain(|(h, _)| *h != options.holder);
    registry.holders.push((options.holder, runtime.clone()));
    Ok(Some(runtime))
}

/// Drops one holder (its session ended, or its user switched the guard off); the other holders keep
/// theirs, and the runtime layer stops reaching new processes once none is left.
pub fn release_kill_guard_runtime(holder: KillGuardHolder) {
    let mut registry = REGISTRY
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    registry.holders.retain(|(h, _)| *h != holder);
}

pub fn active_kill_guard_runtime() -> Option<KillGuardRuntime> {
    let registry = REGISTRY
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    registry.holders.last().map(|(_, runtime)| runtime.clone())
}

pub fn with_kill_guard_runtime(mut env: BTreeMap<String, String>) -> BTreeMap<String, String> {
    let Some(active) = active_kill_guard_runtime() else {
        return env;
    };
    let overlay = kill_guard_environment(&env, &active);
    env.extend(overlay);
    env
}

/// The runtime layer's variables for a kernel session environment built on `base`; empty when none is active.
pub fn active_kill_guard_environment(base: &BTreeMap<String, String>) -> BTreeMap<String, String> {
    let Some(active) = active_kill_guard_runtime() else {
        return BTreeMap::new();
    };
    kill_guard_environment(base, &active)
}

#[cfg(test)]
pub fn clear_installed_runtime_cache_for_test(agent_dir: &Path) {
    let mut registry = REGISTRY
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    registry.installed.remove(agent_dir);
}
