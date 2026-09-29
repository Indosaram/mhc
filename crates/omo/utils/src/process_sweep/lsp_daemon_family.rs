//! Stale old-version lsp-daemon family. Base-dir, auth-token, owner-file and
//! ping layouts mirror packages/lsp-daemon/src/{paths,auth,ownership,ensure-daemon}.ts
//! and must change in lockstep. This family only KILLS provably stale daemons;
//! version-dir removal stays with the daemon's own version reaping.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::LazyLock;

use regex::Regex;

use super::exec::default_is_process_alive;
use super::family_sweeper::SweepTarget;
use super::lsp_daemon_owner_attestation::{
    LspDaemonOwnerAttestationDeps, LspDaemonOwnerTarget, attest_lsp_daemon_owner,
    parse_lsp_daemon_owner,
};
use crate::runtime::node_platform;

pub const OMO_LSP_DAEMON_DIR_ENV: &str = "MAHO_LSP_DAEMON_DIR";
pub const OMO_LSP_DAEMON_VERSION_ENV: &str = "OMO_LSP_DAEMON_VERSION";

#[derive(Default, Clone)]
pub struct LspDaemonBaseDirOptions {
    pub env: Option<HashMap<String, String>>,
    pub home_dir: Option<PathBuf>,
    pub lsp_daemon_dir: Option<PathBuf>,
}

impl LspDaemonBaseDirOptions {
    pub(crate) fn env_value(&self, key: &str) -> Option<String> {
        match &self.env {
            Some(env) => env.get(key).cloned(),
            None => std::env::var(key).ok(),
        }
    }
}

pub fn resolve_lsp_daemon_base_dir(options: &LspDaemonBaseDirOptions) -> PathBuf {
    if let Some(dir) = options
        .lsp_daemon_dir
        .as_ref()
        .filter(|dir| !dir.as_os_str().is_empty())
    {
        return absolute(dir);
    }
    if let Some(dir) = options
        .env_value(OMO_LSP_DAEMON_DIR_ENV)
        .filter(|dir| !dir.trim().is_empty())
    {
        return absolute(Path::new(&dir));
    }
    resolve_home_dir(options.home_dir.as_deref(), |key| options.env_value(key))
        .join(".maho")
        .join("lsp-daemon")
}

pub(crate) fn resolve_home_dir(
    home_dir: Option<&Path>,
    env: impl Fn(&str) -> Option<String>,
) -> PathBuf {
    home_dir
        .map(Path::to_path_buf)
        .or_else(|| env("HOME").map(PathBuf::from))
        .or_else(|| env("USERPROFILE").map(PathBuf::from))
        .or_else(dirs::home_dir)
        .unwrap_or_default()
}

fn absolute(path: &Path) -> PathBuf {
    std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LspDaemonVersionDir {
    pub dir: PathBuf,
    pub version: String,
}

static VERSION_ENTRY_PATTERN: LazyLock<Option<Regex>> =
    LazyLock::new(|| Regex::new(r"^v([A-Za-z0-9][A-Za-z0-9._+-]{0,127})$").ok());

pub fn list_lsp_daemon_version_dirs(base_dir: &Path) -> Vec<LspDaemonVersionDir> {
    let (Ok(entries), Some(pattern)) =
        (std::fs::read_dir(base_dir), VERSION_ENTRY_PATTERN.as_ref())
    else {
        return Vec::new();
    };
    let mut versions: Vec<LspDaemonVersionDir> = entries
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            let version = pattern.captures(&name)?.get(1)?.as_str().to_string();
            Some(LspDaemonVersionDir {
                dir: base_dir.join(&name),
                version,
            })
        })
        .collect();
    versions.sort_by(|left, right| left.version.cmp(&right.version));
    versions
}

/// Reads `<versionDir>/daemon.owner`; unparseable bodies fail closed.
pub fn read_lsp_daemon_owner_target(version_dir: &Path) -> Option<LspDaemonOwnerTarget> {
    let owner_path = version_dir.join("daemon.owner");
    let raw = std::fs::read_to_string(&owner_path).ok()?;
    let owner = parse_lsp_daemon_owner(&serde_json::from_str(&raw).ok()?)?;
    Some(LspDaemonOwnerTarget {
        auth_path: version_dir.join("daemon.auth"),
        pid: owner.pid,
        owner,
        owner_path,
    })
}

pub fn read_lsp_daemon_owner_pid(version_dir: &Path) -> Option<u32> {
    read_lsp_daemon_owner_target(version_dir).map(|target| target.pid)
}

pub type LspDaemonReadProcFile<'a> = &'a dyn Fn(&str) -> Result<Vec<u8>, String>;
pub type LspDaemonExecuteForStdout<'a> = &'a dyn Fn(&str, &[&str]) -> Option<String>;

#[derive(Default)]
pub struct LspDaemonAttestationDeps<'a> {
    pub read_proc_file: Option<LspDaemonReadProcFile<'a>>,
    pub execute_for_stdout: Option<LspDaemonExecuteForStdout<'a>>,
}

/// Mirror of version-reap.ts attestDaemonCliProcess: node running the
/// lsp-daemon cli with the `daemon` arg. Windows fails closed.
pub fn attest_lsp_daemon_cli_process(
    pid: u32,
    platform: &str,
    deps: &LspDaemonAttestationDeps<'_>,
) -> bool {
    match platform {
        "win32" => false,
        "linux" => {
            let path = format!("/proc/{pid}/cmdline");
            let cmdline = match deps.read_proc_file {
                Some(read) => read(&path),
                None => std::fs::read(&path).map_err(|error| error.to_string()),
            };
            cmdline.is_ok_and(|bytes| is_node_cli_daemon_argv(&split_cmdline(&bytes)))
        }
        _ => {
            let pid_arg = pid.to_string();
            let args = ["-p", pid_arg.as_str(), "-o", "command="];
            let command = match deps.execute_for_stdout {
                Some(execute) => execute("/bin/ps", &args),
                None => default_execute_for_stdout("/bin/ps", &args),
            };
            command.is_some_and(|command| is_node_cli_daemon_command(command.trim()))
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StaleLspDaemonVersionTarget {
    pub target: LspDaemonOwnerTarget,
    pub version: String,
    pub version_dir: PathBuf,
}

impl SweepTarget for StaleLspDaemonVersionTarget {
    fn pid(&self) -> u32 {
        self.target.pid
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SparedLspDaemonReason {
    AttestationFailed,
    WindowsAttestationUnsupported,
}

impl SparedLspDaemonReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::AttestationFailed => "attestation-failed",
            Self::WindowsAttestationUnsupported => "windows-attestation-unsupported",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SparedLspDaemonVersion {
    pub reason: SparedLspDaemonReason,
    pub target: StaleLspDaemonVersionTarget,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StaleLspDaemonVersionSweepPlan {
    pub spared: Vec<SparedLspDaemonVersion>,
    pub targets: Vec<StaleLspDaemonVersionTarget>,
}

pub type LspDaemonAttestTarget<'a> = &'a dyn Fn(&StaleLspDaemonVersionTarget, &str) -> bool;

pub struct PlanStaleLspDaemonVersionSweepOptions<'a> {
    pub attest_target: Option<LspDaemonAttestTarget<'a>>,
    pub base_dir: &'a Path,
    pub current_version: &'a str,
    pub is_alive: Option<&'a dyn Fn(u32) -> bool>,
    pub log: Option<&'a dyn Fn(&str)>,
    pub platform: Option<&'a str>,
}

pub(crate) fn default_attest_target(target: &StaleLspDaemonVersionTarget, _platform: &str) -> bool {
    attest_lsp_daemon_owner(&target.target, &LspDaemonOwnerAttestationDeps::default())
}

/// A non-current version's owner pid is a kill target only when alive AND
/// attested; anything unprovable is spared.
pub fn plan_stale_lsp_daemon_version_sweep(
    options: &PlanStaleLspDaemonVersionSweepOptions<'_>,
) -> StaleLspDaemonVersionSweepPlan {
    let platform = options.platform.unwrap_or(node_platform());
    let attest = options.attest_target.unwrap_or(&default_attest_target);
    let log = |message: String| {
        if let Some(log) = options.log {
            log(&message);
        }
    };
    let mut targets = Vec::new();
    let mut spared = Vec::new();
    for entry in list_lsp_daemon_version_dirs(options.base_dir) {
        if entry.version == options.current_version {
            continue;
        }
        let Some(owner_target) = read_lsp_daemon_owner_target(&entry.dir) else {
            continue;
        };
        let pid = owner_target.pid;
        let alive = options
            .is_alive
            .map_or_else(|| default_is_process_alive(pid), |is_alive| is_alive(pid));
        if !alive {
            continue;
        }
        let version = entry.version;
        let target = StaleLspDaemonVersionTarget {
            target: owner_target,
            version: version.clone(),
            version_dir: entry.dir,
        };
        if platform == "win32" {
            log(format!(
                "lsp-daemon stale-version sweep sparing v{version}: Windows cannot prove pid ownership safely (named-pipe policy)"
            ));
            spared.push(SparedLspDaemonVersion {
                reason: SparedLspDaemonReason::WindowsAttestationUnsupported,
                target,
            });
            continue;
        }
        if !attest(&target, platform) {
            log(format!(
                "lsp-daemon stale-version sweep sparing v{version}: pid {pid} is alive but owner identity attestation failed (possible recycled pid)"
            ));
            spared.push(SparedLspDaemonVersion {
                reason: SparedLspDaemonReason::AttestationFailed,
                target,
            });
            continue;
        }
        targets.push(target);
    }
    StaleLspDaemonVersionSweepPlan { spared, targets }
}

fn split_cmdline(bytes: &[u8]) -> Vec<String> {
    String::from_utf8_lossy(bytes)
        .split('\0')
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .collect()
}

static NODE_BASENAME: LazyLock<Option<Regex>> =
    LazyLock::new(|| Regex::new(r"(?i)^node(?:\.exe)?$").ok());
static NODE_WORD: LazyLock<Option<Regex>> =
    LazyLock::new(|| Regex::new(r"(?i)\bnode(?:\.exe)?\b").ok());
static CLI_JS_WORD: LazyLock<Option<Regex>> = LazyLock::new(|| Regex::new(r"\bcli\.js\b").ok());
static DAEMON_TOKEN: LazyLock<Option<Regex>> =
    LazyLock::new(|| Regex::new(r"(?:^|\s)daemon(?:\s|$)").ok());

fn is_node_cli_daemon_argv(argv: &[String]) -> bool {
    if argv.len() < 2 || !argv.iter().any(|value| value == "daemon") {
        return false;
    }
    let executable = argv[0].rsplit(['/', '\\']).next().unwrap_or_default();
    if !NODE_BASENAME
        .as_ref()
        .is_some_and(|re| re.is_match(executable))
    {
        return false;
    }
    argv.iter()
        .any(|value| value == "cli.js" || value.ends_with("/cli.js") || value.ends_with("\\cli.js"))
}

fn is_node_cli_daemon_command(command: &str) -> bool {
    // Token-strict `daemon` (version-reap.ts uses \bdaemon\b, which also hits
    // "lsp-daemon"); stricter attests fewer processes, the safe direction.
    [&NODE_WORD, &CLI_JS_WORD, &DAEMON_TOKEN]
        .iter()
        .all(|re| re.as_ref().is_some_and(|re| re.is_match(command)))
}

fn default_execute_for_stdout(file: &str, args: &[&str]) -> Option<String> {
    let output = Command::new(file).args(args).output().ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).into_owned())
}
