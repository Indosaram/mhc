//! Port of `packages/coding-agent/src/core/kill-guard/check-command.ts` (senpi#5349, L2).
//!
//! `__kill-guard check --vantage <pid> -- <argv...>`: what the runtime shims ask before a real
//! `kill`, `pkill` or `killall` runs. Hidden: it is reached only through the shims and never listed in help.

use super::request::{KillGuardRequestOptions, decide_kill_guard_request};
use super::types::{KillGuardVerdict, ViewFn};
use serde_json::json;
use std::collections::BTreeMap;
use std::io::Write;

pub const KILL_GUARD_COMMAND: &str = "__kill-guard";
const USAGE: &str = "__kill-guard check --vantage <pid> -- <command> [arguments...]";

pub struct KillGuardCheckOptions<'a> {
    pub env: &'a BTreeMap<String, String>,
    pub write_error: &'a mut dyn FnMut(&str),
    pub platform: Option<String>,
    pub view: Option<ViewFn>,
}

fn positive_pid(text: Option<&str>) -> Option<i64> {
    let text = text?;
    if text.is_empty() || !text.bytes().all(|b| b.is_ascii_digit()) || text.starts_with('0') {
        return None;
    }
    text.parse::<i64>().ok().filter(|&pid| pid > 0)
}

/// The exit status the shim acts on: 0 runs the command, 126 refuses it (the refusal is written to
/// stderr), 2 is a malformed call, which the shim treats as a failed judge and so refuses too.
pub fn run_kill_guard_check(args: &[String], options: &mut KillGuardCheckOptions<'_>) -> i32 {
    let command = args.first().map(String::as_str);
    let verb = args.get(1).map(String::as_str);
    let flag = args.get(2).map(String::as_str);
    let vantage_text = args.get(3).map(String::as_str);
    let separator = args.get(4).map(String::as_str);
    let argv = args.get(5..).unwrap_or_default();
    let vantage_pid = positive_pid(vantage_text);
    if command != Some(KILL_GUARD_COMMAND)
        || verb != Some("check")
        || flag != Some("--vantage")
        || vantage_pid.is_none()
        || separator != Some("--")
        || argv.is_empty()
    {
        (options.write_error)(&format!("usage: {USAGE}\n"));
        return 2;
    }
    let Some(agent_pid) = positive_pid(
        options
            .env
            .get("SENPI_KILL_GUARD_AGENT_PID")
            .map(String::as_str),
    ) else {
        (options.write_error)(&format!(
            "{KILL_GUARD_COMMAND}: SENPI_KILL_GUARD_AGENT_PID does not name the agent's pid\n"
        ));
        return 2;
    };
    let verdict = decide_kill_guard_request(
        &json!({
            "kind": "argv",
            "argv": argv,
            "vantagePid": vantage_pid,
        }),
        &KillGuardRequestOptions {
            agent_pid,
            agent_ppid: None,
            platform: options.platform.clone(),
            view: options.view.clone(),
        },
    );
    match verdict {
        KillGuardVerdict::Refuse { message, .. } => {
            (options.write_error)(&format!("{message}\n"));
            126
        }
        KillGuardVerdict::Allow | KillGuardVerdict::Defer { .. } => 0,
    }
}

/// The production call: this process's argv and environment; stderr is written synchronously before exit.
pub fn run_kill_guard_check_command() -> i32 {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let env: BTreeMap<String, String> = std::env::vars().collect();
    let mut write_err = |text: &str| {
        let mut stderr = std::io::stderr().lock();
        let _ = stderr.write_all(text.as_bytes());
        let _ = stderr.flush();
    };
    run_kill_guard_check(
        &args,
        &mut KillGuardCheckOptions {
            env: &env,
            write_error: &mut write_err,
            platform: None,
            view: None,
        },
    )
}
