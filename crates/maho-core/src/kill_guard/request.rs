//! Port of `packages/coding-agent/src/core/kill-guard/request.ts` (senpi#5349).

use super::command::{judge_argv, judge_command, judge_signal};
use super::process_view::{create_process_view, system_spawn_sync};
use super::types::{
    KillGuardContext, KillGuardRule, KillGuardShell, KillGuardVantage, KillGuardVerdict,
    ProcessView, SignalCall, SignalId, ViewFn,
};
use super::verdict::{ALLOW, refuse};
use serde_json::Value;
use std::sync::{Arc, Mutex};

/// A check a kernel sends over the codemode bridge: a shell string, a spawn argv, or a signal call.
#[derive(Clone, Debug, PartialEq)]
pub enum KillGuardRequest {
    Command {
        command: String,
        vantage_pid: Option<i64>,
        shell: Option<KillGuardShell>,
    },
    Argv {
        argv: Vec<String>,
        vantage_pid: Option<i64>,
    },
    Signal {
        target: f64,
        group: bool,
        signal: Option<SignalId>,
        vantage_pid: Option<i64>,
    },
}

#[derive(Clone, Default)]
pub struct KillGuardRequestOptions {
    pub agent_pid: i64,
    pub agent_ppid: Option<i64>,
    pub platform: Option<String>,
    /// Defaults to the production view (`/proc` or `ps`, and `pgrep` for dry runs).
    pub view: Option<ViewFn>,
}

pub fn default_platform() -> &'static str {
    if cfg!(target_os = "linux") {
        "linux"
    } else if cfg!(target_os = "macos") {
        "darwin"
    } else if cfg!(target_os = "windows") {
        "win32"
    } else {
        std::env::consts::OS
    }
}

fn json_integer(value: &Value) -> Option<i64> {
    if let Some(i) = value.as_i64() {
        return Some(i);
    }
    let f = value.as_f64()?;
    if f.is_finite() && f.fract() == 0.0 && f >= (i64::MIN as f64) && f <= (i64::MAX as f64) {
        Some(f as i64)
    } else {
        None
    }
}

/// Parses a bridge request; `None` when it is not one of the three shapes.
pub fn parse_kill_guard_request(value: &Value) -> Option<KillGuardRequest> {
    let obj = value.as_object()?;
    let vantage_pid = obj.get("vantagePid").and_then(json_integer);
    match obj.get("kind").and_then(Value::as_str)? {
        "command" => {
            let command = obj.get("command").and_then(Value::as_str)?.to_owned();
            let shell = obj
                .get("shell")
                .and_then(|v| serde_json::from_value::<KillGuardShell>(v.clone()).ok());
            Some(KillGuardRequest::Command {
                command,
                vantage_pid,
                shell,
            })
        }
        "argv" => {
            let arr = obj.get("argv").and_then(Value::as_array)?;
            let mut argv = Vec::with_capacity(arr.len());
            for item in arr {
                argv.push(item.as_str()?.to_owned());
            }
            Some(KillGuardRequest::Argv { argv, vantage_pid })
        }
        "signal" => {
            let target = obj.get("target").and_then(Value::as_f64)?;
            let group = obj.get("group").and_then(Value::as_bool)?;
            let sig_val = obj.get("signal")?;
            let signal = if sig_val.is_null() {
                None
            } else if let Some(n) = json_integer(sig_val) {
                Some(SignalId::Number(n))
            } else if let Some(s) = sig_val.as_str() {
                Some(SignalId::Name(s.to_owned()))
            } else {
                return None;
            };
            Some(KillGuardRequest::Signal {
                target,
                group,
                signal,
                vantage_pid,
            })
        }
        _ => None,
    }
}

fn vantage_of(vantage_pid: Option<i64>, agent_pid: i64) -> KillGuardVantage {
    match vantage_pid {
        None => KillGuardVantage::FreshSession,
        Some(pid) if pid == agent_pid => KillGuardVantage::Agent,
        Some(pid) => KillGuardVantage::Process { pid },
    }
}

/// The host side of the kernel bridge's kill-guard check (E7, E8): decides one request from its kernel's vantage.
pub fn decide_kill_guard_request(
    value: &Value,
    options: &KillGuardRequestOptions,
) -> KillGuardVerdict {
    let Some(request) = parse_kill_guard_request(value) else {
        return refuse(
            KillGuardRule::R5,
            "the kill-guard check was malformed",
            None,
        );
    };
    let platform = options
        .platform
        .clone()
        .unwrap_or_else(|| default_platform().to_owned());
    let read_view: ViewFn = match &options.view {
        Some(view) => Arc::clone(view),
        None => {
            let platform_clone = platform.clone();
            let cached: Arc<Mutex<Option<Option<ProcessView>>>> = Arc::new(Mutex::new(None));
            let spawn_sync = system_spawn_sync();
            Arc::new(move || -> Option<ProcessView> {
                let mut guard = cached.lock().ok()?;
                if let Some(existing) = guard.as_ref() {
                    return existing.clone();
                }
                let loaded = create_process_view(&platform_clone, Arc::clone(&spawn_sync));
                *guard = Some(loaded.clone());
                loaded
            })
        }
    };
    let (vantage_pid, shell) = match &request {
        KillGuardRequest::Command {
            vantage_pid, shell, ..
        } => (*vantage_pid, shell.unwrap_or(KillGuardShell::Sh)),
        KillGuardRequest::Argv { vantage_pid, .. }
        | KillGuardRequest::Signal { vantage_pid, .. } => (*vantage_pid, KillGuardShell::Argv),
    };
    let context = KillGuardContext {
        platform,
        agent_pid: options.agent_pid,
        agent_ppid: options.agent_ppid,
        vantage: vantage_of(vantage_pid, options.agent_pid),
        shell,
        view: read_view,
    };
    match request {
        KillGuardRequest::Command { command, .. } => judge_command(&command, &context),
        KillGuardRequest::Argv { argv, .. } => judge_argv(&argv, &context),
        KillGuardRequest::Signal {
            target,
            group,
            signal,
            ..
        } => {
            if !target.is_finite()
                || target.fract() != 0.0
                || target < (i64::MIN as f64)
                || target > (i64::MAX as f64)
            {
                return ALLOW;
            }
            judge_signal(
                &SignalCall {
                    target: target as i64,
                    group,
                    signal,
                },
                &context,
            )
        }
    }
}
