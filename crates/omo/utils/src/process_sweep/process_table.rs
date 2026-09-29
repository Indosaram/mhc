//! Generic process-table parsing shared by every sweep family.

use std::collections::HashSet;
use std::sync::LazyLock;

use regex::Regex;
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessInfo {
    pub command: String,
    pub pid: u32,
    pub ppid: u32,
}

/// Backward-compatible alias: the codegraph family was the first consumer.
pub type CodegraphProcessInfo = ProcessInfo;

static POSIX_ROW: LazyLock<Option<Regex>> =
    LazyLock::new(|| Regex::new(r"^\s*(\d+)\s+(\d+)\s+(.+?)\s*$").ok());

/// Parses `ps -eo pid=,ppid=,command=` output.
pub fn parse_posix_process_table(output: &str) -> Vec<ProcessInfo> {
    let Some(row) = POSIX_ROW.as_ref() else {
        return Vec::new();
    };
    output
        .lines()
        .filter_map(|line| {
            let captures = row.captures(line.trim_end_matches('\r'))?;
            let pid: u32 = captures.get(1)?.as_str().parse().ok()?;
            let ppid: u32 = captures.get(2)?.as_str().parse().ok()?;
            let command = captures.get(3)?.as_str().to_string();
            (pid > 0).then_some(ProcessInfo { command, pid, ppid })
        })
        .collect()
}

/// Parses `Get-CimInstance Win32_Process | ConvertTo-Json` output (array or single object).
pub fn parse_windows_process_table(output: &str) -> Vec<ProcessInfo> {
    let entries = match serde_json::from_str::<Value>(output) {
        Ok(Value::Array(entries)) => entries,
        Ok(entry) => vec![entry],
        Err(_) => Vec::new(),
    };
    entries
        .iter()
        .filter_map(|entry| {
            let record = entry.as_object()?;
            let pid = process_id_field(record.get("ProcessId"))?;
            let ppid = process_id_field(record.get("ParentProcessId"))?;
            let command = record.get("CommandLine")?.as_str()?;
            (!command.trim().is_empty()).then(|| ProcessInfo {
                command: command.to_string(),
                pid,
                ppid,
            })
        })
        .collect()
}

fn process_id_field(value: Option<&Value>) -> Option<u32> {
    let id = value?.as_u64()?;
    u32::try_from(id).ok().filter(|id| *id > 0)
}

/// A process is orphaned when reparented to init or its parent is gone.
pub fn is_orphaned(process_info: &ProcessInfo, live_pids: &HashSet<u32>) -> bool {
    process_info.ppid == 1 || !live_pids.contains(&process_info.ppid)
}
