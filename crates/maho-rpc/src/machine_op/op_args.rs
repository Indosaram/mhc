//! Port of `packages/coding-agent/src/modes/rpc/machine-op/op-args.ts`.
//!
//! The relay host's own decoders of each op's args, so every refusal reads the same:
//! any missing, mistyped or unknown key is malformed, refused before any effect.

use crate::machine_op::wire::Args;
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use maho_machine_ops::{
    EditRequest, EnvMode, ExecRequest, GlobRequest, GrepRequest, ListRequest, MessageRequest,
    ProcessInfoRequest, ReadRequest, StatRequest, WriteRequest,
};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::PathBuf;

const READ_RANGE_MAX_BYTES: u64 = 1024 * 1024;
const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct ArgsError(pub String);

pub fn bad<T>(detail: impl Into<String>) -> Result<T, ArgsError> {
    Err(ArgsError(detail.into()))
}

fn keys(a: &Args, required: &[&str], optional: &[&str]) -> Result<(), ArgsError> {
    for &k in required {
        if !a.contains_key(k) {
            return bad(format!("{k} is required"));
        }
    }
    for k in a.keys() {
        let key = k.as_str();
        if !required.contains(&key) && !optional.contains(&key) {
            return bad(format!("unknown argument {key}"));
        }
    }
    Ok(())
}

fn text(v: &Value, name: &str) -> Result<String, ArgsError> {
    v.as_str()
        .map(str::to_owned)
        .ok_or_else(|| ArgsError(format!("{name} must be a string")))
}

fn non_empty(v: &Value, name: &str) -> Result<String, ArgsError> {
    let s = text(v, name)?;
    if s.is_empty() {
        return bad(format!("{name} must not be empty"));
    }
    Ok(s)
}

fn uint(v: &Value, name: &str) -> Result<u64, ArgsError> {
    if let Some(n) = v.as_u64()
        && n <= MAX_SAFE_INTEGER
    {
        return Ok(n);
    }
    if let Some(f) = v.as_f64()
        && f.is_finite()
        && f.fract() == 0.0
        && f >= 0.0
        && f <= MAX_SAFE_INTEGER as f64
    {
        return Ok(f as u64);
    }
    bad(format!("{name} must be a non-negative integer"))
}

fn uint_usize(v: &Value, name: &str) -> Result<usize, ArgsError> {
    let n = uint(v, name)?;
    usize::try_from(n).map_err(|_| ArgsError(format!("{name} must be a non-negative integer")))
}

fn opt<T>(
    v: Option<&Value>,
    f: fn(&Value, &str) -> Result<T, ArgsError>,
    name: &str,
) -> Result<Option<T>, ArgsError> {
    match v {
        None => Ok(None),
        Some(val) => f(val, name).map(Some),
    }
}

fn flag(v: &Value, name: &str) -> Result<bool, ArgsError> {
    v.as_bool()
        .ok_or_else(|| ArgsError(format!("{name} must be a boolean")))
}

fn bytes(v: &Value, name: &str) -> Result<Vec<u8>, ArgsError> {
    if let Some(s) = v.as_str()
        && s.len() % 4 == 0
        && is_base64_chars(s)
        && let Ok(decoded) = STANDARD.decode(s)
    {
        return Ok(decoded);
    }
    bad(format!("{name} must be bytes or base64"))
}

fn is_base64_chars(s: &str) -> bool {
    let bytes = s.as_bytes();
    let pad_start = bytes.iter().position(|&b| b == b'=').unwrap_or(bytes.len());
    if bytes.len() - pad_start > 2 {
        return false;
    }
    bytes[..pad_start]
        .iter()
        .all(|b| b.is_ascii_alphanumeric() || *b == b'+' || *b == b'/')
        && bytes[pad_start..].iter().all(|&b| b == b'=')
}

fn env(v: &Value, name: &str) -> Result<BTreeMap<String, String>, ArgsError> {
    let m = v
        .as_object()
        .ok_or_else(|| ArgsError(format!("{name} must be a map")))?;
    let mut out = BTreeMap::new();
    for (k, val) in m {
        out.insert(k.clone(), text(val, &format!("{name}.{k}"))?);
    }
    Ok(out)
}

pub fn read_args(a: &Args) -> Result<ReadRequest, ArgsError> {
    keys(a, &["path"], &["offset", "limit_bytes"])?;
    Ok(ReadRequest {
        path: non_empty(&a["path"], "path")?,
        offset: opt(a.get("offset"), uint, "offset")?,
        limit_bytes: opt(a.get("limit_bytes"), uint_usize, "limit_bytes")?,
    })
}

pub fn read_range_args(a: &Args) -> Result<ReadRequest, ArgsError> {
    keys(a, &["path", "offset", "length"], &[])?;
    let length = uint(&a["length"], "length")?;
    if length > READ_RANGE_MAX_BYTES {
        return bad(format!("length is above {READ_RANGE_MAX_BYTES}"));
    }
    Ok(ReadRequest {
        path: non_empty(&a["path"], "path")?,
        offset: Some(uint(&a["offset"], "offset")?),
        limit_bytes: Some(length as usize),
    })
}

pub fn write_args(a: &Args) -> Result<WriteRequest, ArgsError> {
    keys(a, &["path", "bytes"], &["create_parent_dirs"])?;
    Ok(WriteRequest {
        path: non_empty(&a["path"], "path")?,
        bytes: bytes(&a["bytes"], "bytes")?,
        create_parent_dirs: opt(a.get("create_parent_dirs"), flag, "create_parent_dirs")?
            .unwrap_or(false),
        offset: None,
        total: None,
    })
}

pub fn write_range_args(a: &Args) -> Result<WriteRequest, ArgsError> {
    keys(a, &["path", "offset", "bytes", "total"], &[])?;
    Ok(WriteRequest {
        path: non_empty(&a["path"], "path")?,
        offset: Some(uint(&a["offset"], "offset")?),
        bytes: bytes(&a["bytes"], "bytes")?,
        create_parent_dirs: false,
        total: Some(uint(&a["total"], "total")?),
    })
}

pub fn edit_args(a: &Args) -> Result<EditRequest, ArgsError> {
    keys(a, &["path", "exact", "replacement"], &[])?;
    Ok(EditRequest {
        path: non_empty(&a["path"], "path")?,
        exact: text(&a["exact"], "exact")?,
        replacement: text(&a["replacement"], "replacement")?,
    })
}

pub fn glob_args(a: &Args) -> Result<GlobRequest, ArgsError> {
    keys(a, &["pattern"], &["cwd", "limit"])?;
    Ok(GlobRequest {
        pattern: non_empty(&a["pattern"], "pattern")?,
        cwd: opt(a.get("cwd"), non_empty, "cwd")?,
        limit: opt(a.get("limit"), uint_usize, "limit")?,
    })
}

pub fn grep_args(a: &Args) -> Result<GrepRequest, ArgsError> {
    keys(a, &["path", "pattern"], &["limit"])?;
    Ok(GrepRequest {
        path: non_empty(&a["path"], "path")?,
        pattern: non_empty(&a["pattern"], "pattern")?,
        limit: opt(a.get("limit"), uint_usize, "limit")?,
    })
}

pub fn stat_args(a: &Args) -> Result<StatRequest, ArgsError> {
    keys(a, &["path"], &[])?;
    Ok(StatRequest {
        path: non_empty(&a["path"], "path")?,
        access: None,
    })
}

pub fn list_args(a: &Args) -> Result<ListRequest, ArgsError> {
    keys(a, &["path"], &[])?;
    Ok(ListRequest {
        path: non_empty(&a["path"], "path")?,
    })
}

pub fn shell_args(a: &Args) -> Result<ExecRequest, ArgsError> {
    keys(a, &["command"], &["kind", "cwd", "env", "timeoutMs"])?;
    if let Some(kind) = a.get("kind")
        && kind.as_str() != Some("shell")
    {
        return bad("kind must be shell");
    }
    let timeout_ms = opt(a.get("timeoutMs"), uint, "timeoutMs")?;
    if timeout_ms == Some(0) {
        return bad("timeoutMs must be positive");
    }
    Ok(ExecRequest {
        command: non_empty(&a["command"], "command")?,
        cwd: opt(a.get("cwd"), non_empty, "cwd")?.map(PathBuf::from),
        env: opt(a.get("env"), env, "env")?,
        env_mode: EnvMode::Overlay,
        shell: None,
        timeout_ms: timeout_ms.map(Some),
        on_output: None,
        on_spawn: None,
        on_exit: None,
    })
}

pub fn process_info_args(a: &Args) -> Result<ProcessInfoRequest, ArgsError> {
    keys(a, &[], &["pid", "handleId"])?;
    if a.contains_key("pid") && a.contains_key("handleId") {
        return bad("pid and handleId are exclusive");
    }
    let pid = match opt(a.get("pid"), uint, "pid")? {
        None => None,
        Some(n) => Some(
            u32::try_from(n)
                .map_err(|_| ArgsError("pid must be a non-negative integer".to_owned()))?,
        ),
    };
    Ok(ProcessInfoRequest {
        pid,
        handle_id: opt(a.get("handleId"), non_empty, "handleId")?,
    })
}

pub fn mesh_message_args(a: &Args) -> Result<MessageRequest, ArgsError> {
    keys(a, &["topic", "body"], &[])?;
    Ok(MessageRequest {
        topic: non_empty(&a["topic"], "topic")?,
        body: a["body"].clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn map(v: Value) -> Args {
        v.as_object().expect("obj").clone()
    }

    #[test]
    fn checks_required_keys_before_unknown_keys() {
        assert_eq!(
            write_args(&map(json!({ "path": "a", "bytes_path": "/secret" })))
                .expect_err("missing bytes"),
            ArgsError("bytes is required".to_owned())
        );
        assert_eq!(
            write_args(&map(
                json!({ "path": "a", "bytes": "eA==", "bytes_path": "/secret" })
            ))
            .expect_err("unknown arg"),
            ArgsError("unknown argument bytes_path".to_owned())
        );
    }

    #[test]
    fn validates_read_range_and_shell_and_process_info() {
        assert_eq!(
            read_range_args(&map(
                json!({ "path": "a", "offset": 0, "length": 1_048_577 })
            ))
            .expect_err("too large"),
            ArgsError("length is above 1048576".to_owned())
        );
        let err = match shell_args(&map(json!({ "command": "ls", "timeoutMs": 0 }))) {
            Err(e) => e,
            Ok(_) => panic!("expected zero timeout error"),
        };
        assert_eq!(err, ArgsError("timeoutMs must be positive".to_owned()));
        assert_eq!(
            process_info_args(&map(json!({ "pid": 1, "handleId": "h" }))).expect_err("exclusive"),
            ArgsError("pid and handleId are exclusive".to_owned())
        );
    }
}
