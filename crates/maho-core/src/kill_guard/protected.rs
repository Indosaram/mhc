//! Port of `packages/coding-agent/src/core/kill-guard/protected.ts` (senpi#5349).

use super::types::{KillGuardContext, KillGuardVantage, ProcessRow};
use std::collections::{HashMap, HashSet};

/// Protected pid -> why it is protected.
pub type Protected = HashMap<i64, String>;

pub fn row_of(rows: &[ProcessRow], pid: i64) -> Option<&ProcessRow> {
    rows.iter().find(|row| row.pid == pid)
}

/// Ancestors of `pid` (nearest first), stopping at pid 1 or a loop.
fn ancestors(rows: &[ProcessRow], pid: i64) -> Vec<i64> {
    let mut chain = Vec::new();
    let mut seen = HashSet::from([pid]);
    let mut current = row_of(rows, pid);
    while let Some(row) = current
        && row.ppid > 0
        && !seen.contains(&row.ppid)
    {
        chain.push(row.ppid);
        seen.insert(row.ppid);
        current = row_of(rows, row.ppid);
    }
    chain
}

/// P without a table: pid 1, the agent and its parent.
pub fn minimal_protected(context: &KillGuardContext) -> Protected {
    let mut set = Protected::new();
    set.insert(1, "pid 1".to_owned());
    set.insert(context.agent_pid, "the agent itself".to_owned());
    if let Some(ppid) = context.agent_ppid
        && ppid > 1
    {
        set.insert(ppid, "the agent's parent".to_owned());
    }
    set
}

fn is_systemd_user_manager(args: &str) -> bool {
    args.split_whitespace().any(|token| token == "--user")
}

/// P (DESIGN-12096 v2 §3'): pid 1, the agent, its ancestors, its session leader, and on Linux the user's systemd.
pub fn protected_set(context: &KillGuardContext, rows: &[ProcessRow]) -> Protected {
    let mut set = minimal_protected(context);
    for pid in ancestors(rows, context.agent_pid) {
        set.entry(pid)
            .or_insert_with(|| "an ancestor of the agent".to_owned());
    }
    let agent = row_of(rows, context.agent_pid);
    if let Some(agent) = agent
        && agent.sid > 0
    {
        set.entry(agent.sid)
            .or_insert_with(|| "the agent's session leader".to_owned());
    }
    if context.platform == "linux"
        && let Some(agent) = agent
    {
        for row in rows {
            if row.uid == agent.uid && row.comm == "systemd" && is_systemd_user_manager(&row.args) {
                set.entry(row.pid)
                    .or_insert_with(|| "the user's service manager".to_owned());
            }
        }
    }
    set
}

pub fn describe(pid: i64, rows: Option<&[ProcessRow]>, reason: &str) -> String {
    let comm = rows
        .and_then(|rows| row_of(rows, pid))
        .map(|r| r.comm.as_str());
    match comm {
        Some(comm) => format!("{pid} {comm} ({reason})"),
        None => format!("{pid} ({reason})"),
    }
}

/// What BSD `pkill` leaves out by default: its own ancestors, i.e. the caller's chain up to pid 1.
pub fn vantage_chain(context: &KillGuardContext, rows: &[ProcessRow]) -> HashSet<i64> {
    let mut chain = HashSet::new();
    chain.insert(context.agent_pid);
    chain.extend(ancestors(rows, context.agent_pid));
    if let KillGuardVantage::Process { pid } = context.vantage {
        chain.insert(pid);
        chain.extend(ancestors(rows, pid));
    }
    chain
}

/// The caller's own process group; `None` for a fresh shell, whose group holds only the command's processes.
pub fn own_group(context: &KillGuardContext, rows: &[ProcessRow]) -> Option<i64> {
    let pid = match context.vantage {
        KillGuardVantage::FreshSession => return None,
        KillGuardVantage::Agent => context.agent_pid,
        KillGuardVantage::Process { pid } => pid,
    };
    row_of(rows, pid).map(|row| row.pgid)
}
