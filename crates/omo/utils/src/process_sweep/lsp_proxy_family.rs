//! lsp-daemon MCP proxy family: orphaned `<node|bun> <cliPath> mcp` stdio
//! proxies. The daemon SERVER shape (`cli.js daemon`) is never matched here; a
//! proxy whose parent is still alive is spared unconditionally (#5902).

use std::collections::HashSet;

use super::command_match::{
    has_executable_token_under_root_with_suffix, normalize_for_comparison, normalize_roots,
    split_command_tokens,
};
use super::family_sweeper::SweepTarget;
use super::process_table::{ProcessInfo, is_orphaned};
use crate::runtime::node_platform;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LspDaemonProxyMatchKind {
    LspDaemonProxy,
}

impl LspDaemonProxyMatchKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::LspDaemonProxy => "lsp-daemon-proxy",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LspDaemonProxyProcess {
    pub command: String,
    pub match_kind: LspDaemonProxyMatchKind,
    pub matched_root: String,
    pub pid: u32,
    pub ppid: u32,
}

impl SweepTarget for LspDaemonProxyProcess {
    fn pid(&self) -> u32 {
        self.pid
    }
}

pub struct SelectOrphanedLspDaemonProxiesOptions<'a> {
    pub owned_roots: &'a [String],
    pub platform: Option<&'a str>,
}

const LSP_DAEMON_CLI_SUFFIXES: [&str; 2] = ["/lsp-daemon/dist/cli.js", "/lsp-daemon/src/cli.ts"];

pub fn select_orphaned_lsp_daemon_proxies(
    processes: &[ProcessInfo],
    options: &SelectOrphanedLspDaemonProxiesOptions<'_>,
) -> Vec<LspDaemonProxyProcess> {
    let platform = options.platform.unwrap_or(node_platform());
    let live_pids: HashSet<u32> = processes.iter().map(|info| info.pid).collect();
    let roots = normalize_roots(options.owned_roots, platform);
    processes
        .iter()
        .filter_map(|info| {
            let matched_root = match_lsp_daemon_proxy_command(&info.command, &roots, platform)?;
            is_orphaned(info, &live_pids).then(|| LspDaemonProxyProcess {
                command: info.command.clone(),
                match_kind: LspDaemonProxyMatchKind::LspDaemonProxy,
                matched_root,
                pid: info.pid,
                ppid: info.ppid,
            })
        })
        .collect()
}

fn match_lsp_daemon_proxy_command(
    command: &str,
    roots: &[String],
    platform: &str,
) -> Option<String> {
    let tokens = split_command_tokens(command);
    if !tokens.iter().any(|token| token == "mcp") || tokens.iter().any(|token| token == "daemon") {
        return None;
    }
    let normalized = normalize_for_comparison(command, platform);
    roots
        .iter()
        .filter(|root| !root.is_empty())
        .find(|root| {
            LSP_DAEMON_CLI_SUFFIXES.iter().any(|suffix| {
                has_executable_token_under_root_with_suffix(&normalized, root, suffix)
            })
        })
        .cloned()
}
