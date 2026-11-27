//! Port of `packages/coding-agent/src/modes/rpc/machine-op/terminal-args.ts`.
//!
//! The terminal ops' args, decoded as the relay host decodes them: `handle_id`, `last_seq`,
//! `yield_ms` (`250..10_000`), `expectActivityMs`, `lifecycleVersion` `1`.

use crate::machine_op::live_handle::MachineLifecycle;
use crate::machine_op::op_args::{ArgsError, bad};
use crate::machine_op::terminal_input::encode_key;
use crate::machine_op::terminal_wait::WaitUntil;
use crate::machine_op::wire::Args;
use serde_json::Value;

const YIELD_MIN_MS: u64 = 250;
const YIELD_MAX_MS: u64 = 10_000;
const EXEC_YIELD_MS: u64 = 1_000;
const WRITE_YIELD_MS: u64 = 250;
const WAIT_YIELD_MS: u64 = 1_000;
const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExecArgs {
    pub command: String,
    pub cwd: Option<String>,
    pub yield_ms: u64,
    pub cols: Option<u16>,
    pub rows: Option<u16>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WriteStdinArgs {
    pub handle: String,
    pub data: Option<String>,
    pub keys: Option<Vec<Vec<u8>>>,
    pub paste: bool,
    pub yield_ms: u64,
    pub last_seq: Option<usize>,
    pub expect_ms: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WaitArgs {
    pub handle: String,
    pub yield_ms: u64,
    pub last_seq: Option<usize>,
    pub until: Option<WaitUntil>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KillArgs {
    pub handle: String,
}

fn only(a: &Args, allowed: &[&str], required: &[&str]) -> Result<(), ArgsError> {
    for &k in required {
        if !a.contains_key(k) {
            return bad(format!("{k} is required"));
        }
    }
    for k in a.keys() {
        if !allowed.contains(&k.as_str()) {
            return bad(format!("unknown argument {k}"));
        }
    }
    Ok(())
}

fn text(a: &Args, k: &str) -> Result<Option<String>, ArgsError> {
    match a.get(k) {
        None => Ok(None),
        Some(v) => match v.as_str() {
            Some(s) if !s.is_empty() => Ok(Some(s.to_owned())),
            _ => bad(format!("{k} must be a non-empty string")),
        },
    }
}

fn safe_int(v: &Value) -> Option<u64> {
    if let Some(n) = v.as_u64() {
        return (n <= MAX_SAFE_INTEGER).then_some(n);
    }
    if let Some(f) = v.as_f64()
        && f.is_finite()
        && f.fract() == 0.0
        && f >= 0.0
        && f <= MAX_SAFE_INTEGER as f64
    {
        return Some(f as u64);
    }
    None
}

fn int(a: &Args, k: &str, min: u64, max: u64) -> Result<Option<u64>, ArgsError> {
    match a.get(k) {
        None => Ok(None),
        Some(v) => match safe_int(v) {
            Some(n) if n >= min && n <= max => Ok(Some(n)),
            _ => bad(format!("{k} must be an integer in {min}..{max}")),
        },
    }
}

fn until_of(v: Option<&Value>) -> Result<Option<WaitUntil>, ArgsError> {
    let Some(v) = v else {
        return Ok(None);
    };
    if let Some(arr) = v.as_array() {
        let mut states = Vec::with_capacity(arr.len());
        for item in arr {
            let Some(st) = item.as_str().and_then(MachineLifecycle::from_str_name) else {
                return bad("until lists an unknown state");
            };
            states.push(st);
        }
        return Ok(Some(WaitUntil {
            states: Some(states),
            output: None,
            idle: false,
        }));
    }
    let Some(m) = v.as_object() else {
        return bad("until must be a list of states or a map");
    };
    only(m, &["output", "idle"], &[])?;
    let idle = match m.get("idle") {
        None => false,
        Some(Value::Bool(true)) => true,
        Some(_) => return bad("until.idle must be true"),
    };
    let output = text(m, "output")?;
    Ok(Some(WaitUntil {
        states: None,
        output,
        idle,
    }))
}

fn keys_of(v: Option<&Value>) -> Result<Option<Vec<Vec<u8>>>, ArgsError> {
    let Some(v) = v else {
        return Ok(None);
    };
    let Some(arr) = v.as_array().filter(|a| !a.is_empty()) else {
        return bad("keys must be a non-empty list");
    };
    let mut out = Vec::with_capacity(arr.len());
    for item in arr {
        let Some(k) = item.as_str() else {
            return bad("a key must be a string");
        };
        let Some(encoded) = encode_key(k) else {
            return bad(format!("unknown key {k}"));
        };
        out.push(encoded);
    }
    Ok(Some(out))
}

/// Checked first for every terminal op, whatever its own keys: `lifecycleVersion` and a well-formed `last_seq`.
fn common(a: &Args) -> Result<Option<usize>, ArgsError> {
    if let Some(version) = a.get("lifecycleVersion")
        && safe_int(version) != Some(1)
    {
        return bad("lifecycleVersion must be 1");
    }
    let seq = int(a, "last_seq", 0, MAX_SAFE_INTEGER)?;
    match seq {
        None => Ok(None),
        Some(n) => usize::try_from(n).map(Some).map_err(|_| {
            ArgsError(format!(
                "last_seq must be an integer in 0..{MAX_SAFE_INTEGER}"
            ))
        }),
    }
}

pub fn exec_command_args(a: &Args) -> Result<ExecArgs, ArgsError> {
    common(a)?;
    only(
        a,
        &[
            "cmd",
            "command",
            "workdir",
            "cwd",
            "yield_ms",
            "lifecycleVersion",
            "cols",
            "rows",
        ],
        &[],
    )?;
    if a.contains_key("cmd") && a.contains_key("command") {
        return bad("cmd and command are mutually exclusive");
    }
    if a.contains_key("workdir") && a.contains_key("cwd") {
        return bad("workdir and cwd are mutually exclusive");
    }
    let command = match text(a, "cmd")? {
        Some(cmd) => cmd,
        None => match text(a, "command")? {
            Some(cmd) => cmd,
            None => return bad("cmd or command is required"),
        },
    };
    let cwd = match text(a, "workdir")? {
        Some(dir) => Some(dir),
        None => text(a, "cwd")?,
    };
    let cols = int(a, "cols", 2, 1_000)?.map(|v| v as u16);
    let rows = int(a, "rows", 2, 1_000)?.map(|v| v as u16);
    let yield_ms = int(a, "yield_ms", YIELD_MIN_MS, YIELD_MAX_MS)?.unwrap_or(EXEC_YIELD_MS);
    Ok(ExecArgs {
        command,
        cwd,
        yield_ms,
        cols,
        rows,
    })
}

pub fn write_stdin_args(a: &Args) -> Result<WriteStdinArgs, ArgsError> {
    let seq = common(a)?;
    only(
        a,
        &[
            "handle_id",
            "data",
            "keys",
            "paste",
            "yield_ms",
            "last_seq",
            "expectActivityMs",
            "lifecycleVersion",
        ],
        &["handle_id"],
    )?;
    let data = match a.get("data") {
        None => None,
        Some(v) => Some(
            v.as_str()
                .map(str::to_owned)
                .ok_or_else(|| ArgsError("data must be a string".to_owned()))?,
        ),
    };
    let paste = match a.get("paste") {
        None => false,
        Some(Value::Bool(b)) => *b,
        Some(_) => return bad("paste must be a boolean"),
    };
    let keys = keys_of(a.get("keys"))?;
    let expect_ms = int(a, "expectActivityMs", YIELD_MIN_MS, YIELD_MAX_MS)?;
    let handle =
        text(a, "handle_id")?.ok_or_else(|| ArgsError("handle_id is required".to_owned()))?;
    let yield_ms = int(a, "yield_ms", YIELD_MIN_MS, YIELD_MAX_MS)?.unwrap_or(WRITE_YIELD_MS);
    Ok(WriteStdinArgs {
        handle,
        data,
        keys,
        paste,
        yield_ms,
        last_seq: seq,
        expect_ms,
    })
}

pub fn wait_for_command_args(a: &Args) -> Result<WaitArgs, ArgsError> {
    let seq = common(a)?;
    only(
        a,
        &[
            "handle_id",
            "last_seq",
            "yield_ms",
            "until",
            "lifecycleVersion",
        ],
        &["handle_id"],
    )?;
    let until = until_of(a.get("until"))?;
    let handle =
        text(a, "handle_id")?.ok_or_else(|| ArgsError("handle_id is required".to_owned()))?;
    let yield_ms = int(a, "yield_ms", YIELD_MIN_MS, YIELD_MAX_MS)?.unwrap_or(WAIT_YIELD_MS);
    Ok(WaitArgs {
        handle,
        yield_ms,
        last_seq: seq,
        until,
    })
}

pub fn kill_terminal_args(a: &Args) -> Result<KillArgs, ArgsError> {
    common(a)?;
    only(a, &["handle_id", "lifecycleVersion"], &["handle_id"])?;
    let handle =
        text(a, "handle_id")?.ok_or_else(|| ArgsError("handle_id is required".to_owned()))?;
    Ok(KillArgs { handle })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn map(v: Value) -> Args {
        v.as_object().expect("obj").clone()
    }

    #[test]
    fn validates_exec_write_wait_and_kill_args() {
        assert_eq!(
            exec_command_args(&map(json!({ "cmd": "a", "command": "b" }))).expect_err("exclusive"),
            ArgsError("cmd and command are mutually exclusive".to_owned())
        );
        assert_eq!(
            exec_command_args(&map(json!({ "command": "ls", "yield_ms": 100 })))
                .expect_err("yield min"),
            ArgsError("yield_ms must be an integer in 250..10000".to_owned())
        );
        let parsed = wait_for_command_args(&map(json!({
            "handle_id": "h1",
            "until": ["exited"],
            "yield_ms": 10_000,
            "lifecycleVersion": 1,
        })))
        .expect("valid wait");
        assert_eq!(
            parsed.until,
            Some(WaitUntil {
                states: Some(vec![MachineLifecycle::Exited]),
                output: None,
                idle: false,
            })
        );
        assert_eq!(
            write_stdin_args(&map(json!({ "handle_id": "h1", "keys": ["bogus_key"] })))
                .expect_err("bad key"),
            ArgsError("unknown key bogus_key".to_owned())
        );
        assert_eq!(
            kill_terminal_args(&map(json!({ "handle_id": "h1", "lifecycleVersion": 2 })))
                .expect_err("bad version"),
            ArgsError("lifecycleVersion must be 1".to_owned())
        );
    }
}
