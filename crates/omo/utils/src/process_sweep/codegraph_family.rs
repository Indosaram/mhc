use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;

use regex::Regex;

use super::command_match::{
    find_token_start, has_executable_token, has_executable_token_under_root_with_suffix,
    normalize_for_comparison, normalize_roots, split_command_tokens, token_looks_executable,
};
use super::family_sweeper::SweepTarget;
use super::process_table::{ProcessInfo, is_orphaned};
use crate::runtime::node_platform;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodegraphProcessMatchKind {
    ServeWrapper,
    UpstreamCodegraph,
    UpstreamDaemon,
}

impl CodegraphProcessMatchKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ServeWrapper => "serve-wrapper",
            Self::UpstreamCodegraph => "upstream-codegraph",
            Self::UpstreamDaemon => "upstream-daemon",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodegraphZombieProcess {
    pub command: String,
    pub daemon_project_root: Option<String>,
    pub match_kind: CodegraphProcessMatchKind,
    pub matched_root: String,
    pub pid: u32,
    pub ppid: u32,
}

impl SweepTarget for CodegraphZombieProcess {
    fn pid(&self) -> u32 {
        self.pid
    }
}

pub struct SelectZombieCodegraphProcessesOptions<'a> {
    pub owned_roots: &'a [String],
    pub platform: Option<&'a str>,
}

const SERVE_WRAPPER_SUFFIX: &str = "/components/codegraph/dist/serve.js";
const STANDALONE_LAUNCHER_SUFFIXES: [&str; 2] = ["/bin/codegraph", "/bin/codegraph.exe"];
const BUNDLE_SCRIPT_SUFFIX: &str = "/lib/dist/bin/codegraph.js";

static UPSTREAM_PACKAGE_PATTERN: LazyLock<Option<Regex>> = LazyLock::new(|| {
    Regex::new(r"/@colbymchenry/codegraph(?:-(?:darwin|linux|win32)-(?:arm64|x64))?/").ok()
});

struct DaemonMatch {
    project_root: String,
    root: String,
}

struct WorkerMatch {
    kind: CodegraphProcessMatchKind,
    root: String,
}

/// Selects orphaned OMO-owned CodeGraph processes. Detached daemons (ppid 1 by
/// design) are returned as `UpstreamDaemon` candidates; the sweeper's lockfile
/// staleness gate decides whether they die.
pub fn select_zombie_codegraph_processes(
    processes: &[ProcessInfo],
    options: &SelectZombieCodegraphProcessesOptions<'_>,
) -> Vec<CodegraphZombieProcess> {
    let platform = options.platform.unwrap_or(node_platform());
    let live_pids: HashSet<u32> = processes.iter().map(|info| info.pid).collect();
    let process_by_pid: HashMap<u32, &ProcessInfo> =
        processes.iter().map(|info| (info.pid, info)).collect();
    let roots = normalize_roots(options.owned_roots, platform);
    let mut daemon_matches: HashMap<u32, DaemonMatch> = HashMap::new();
    let mut worker_matches: HashMap<u32, WorkerMatch> = HashMap::new();

    for info in processes {
        if let Some(daemon) = match_daemon_command(&info.command, &roots, platform) {
            daemon_matches.insert(info.pid, daemon);
            continue;
        }
        if let Some(worker) = match_owned_codegraph_command(&info.command, &roots, platform) {
            worker_matches.insert(info.pid, worker);
        }
    }

    let mut zombies = Vec::new();
    for info in processes {
        if let Some(daemon) = daemon_matches.get(&info.pid) {
            if is_orphaned(info, &live_pids) {
                zombies.push(CodegraphZombieProcess {
                    command: info.command.clone(),
                    daemon_project_root: Some(daemon.project_root.clone()),
                    match_kind: CodegraphProcessMatchKind::UpstreamDaemon,
                    matched_root: daemon.root.clone(),
                    pid: info.pid,
                    ppid: info.ppid,
                });
            }
            continue;
        }
        let Some(worker) = worker_matches.get(&info.pid) else {
            continue;
        };
        if !is_orphaned(info, &live_pids)
            && !has_orphaned_worker_ancestor(info, &live_pids, &process_by_pid, &worker_matches)
        {
            continue;
        }
        zombies.push(CodegraphZombieProcess {
            command: info.command.clone(),
            daemon_project_root: None,
            match_kind: worker.kind,
            matched_root: worker.root.clone(),
            pid: info.pid,
            ppid: info.ppid,
        });
    }
    zombies
}

fn match_daemon_command(command: &str, roots: &[String], platform: &str) -> Option<DaemonMatch> {
    let project_root = extract_daemon_project_root(&split_command_tokens(command))?;
    let normalized = normalize_for_comparison(command, platform);
    roots
        .iter()
        .filter(|root| !root.is_empty())
        .find(|root| {
            upstream_package_path_is_under_root(&normalized, root)
                || STANDALONE_LAUNCHER_SUFFIXES
                    .iter()
                    .any(|suffix| has_executable_token(&normalized, &format!("{root}{suffix}")))
                || has_executable_token_under_root_with_suffix(
                    &normalized,
                    root,
                    BUNDLE_SCRIPT_SUFFIX,
                )
        })
        .map(|root| DaemonMatch {
            project_root,
            root: root.clone(),
        })
}

fn extract_daemon_project_root(tokens: &[String]) -> Option<String> {
    if !tokens.iter().any(|token| token == "serve") || !tokens.iter().any(|token| token == "--mcp")
    {
        return None;
    }
    let path_index = tokens.iter().position(|token| token == "--path")?;
    let value = tokens.get(path_index + 1)?;
    (!value.is_empty() && !value.starts_with("--")).then(|| value.clone())
}

fn match_owned_codegraph_command(
    command: &str,
    roots: &[String],
    platform: &str,
) -> Option<WorkerMatch> {
    let normalized = normalize_for_comparison(command, platform);
    for root in roots.iter().filter(|root| !root.is_empty()) {
        if has_executable_token(&normalized, &format!("{root}{SERVE_WRAPPER_SUFFIX}")) {
            return Some(WorkerMatch {
                kind: CodegraphProcessMatchKind::ServeWrapper,
                root: root.clone(),
            });
        }
        if has_executable_token_under_root_with_suffix(&normalized, root, BUNDLE_SCRIPT_SUFFIX)
            || has_executable_token(&normalized, &format!("{root}/npm-shim.js"))
            || upstream_package_path_is_under_root(&normalized, root)
        {
            return Some(WorkerMatch {
                kind: CodegraphProcessMatchKind::UpstreamCodegraph,
                root: root.clone(),
            });
        }
    }
    None
}

fn upstream_package_path_is_under_root(command: &str, root: &str) -> bool {
    let Some(pattern) = UPSTREAM_PACKAGE_PATTERN.as_ref() else {
        return false;
    };
    let root_prefix = format!("{root}/");
    pattern.find_iter(command).any(|found| {
        let token_start = find_token_start(command, found.start());
        command[token_start..].starts_with(&root_prefix)
            && token_looks_executable(command, token_start)
    })
}

fn has_orphaned_worker_ancestor(
    info: &ProcessInfo,
    live_pids: &HashSet<u32>,
    process_by_pid: &HashMap<u32, &ProcessInfo>,
    worker_matches: &HashMap<u32, WorkerMatch>,
) -> bool {
    let mut visited = HashSet::new();
    let mut parent_pid = info.ppid;
    loop {
        if !visited.insert(parent_pid) {
            return false;
        }
        let Some(parent) = process_by_pid.get(&parent_pid) else {
            return false;
        };
        if !worker_matches.contains_key(&parent.pid) {
            return false;
        }
        if is_orphaned(parent, live_pids) {
            return true;
        }
        parent_pid = parent.ppid;
    }
}
