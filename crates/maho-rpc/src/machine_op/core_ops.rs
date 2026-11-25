//! Port of `packages/coding-agent/src/modes/rpc/machine-op/core-ops.ts`.
//!
//! The run step of the relay host's file, shell, process and mesh ops, each on the shared
//! `maho-machine-ops` layer with the op's `OpContext`.

use crate::machine_op::real_path::spawn_path_in;
use crate::machine_op::wire::{OpOutcome, OpState};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use maho_machine_ops::{
    EditRequest, EntryType, ExecRequest, ExecResult, GlobRequest, GrepRequest, ListRequest,
    MachineOpError, MachineOps, MessageRequest, OpContext, OpErrorCode, ProcessInfoRequest,
    ReadRequest, StatRequest, WriteRequest,
};
use serde_json::{Map, Value, json};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

pub struct CoreCall<'a> {
    pub ctx: &'a OpContext,
    pub ops: &'a Arc<dyn MachineOps>,
    pub root: &'a str,
    pub confined: bool,
    /// The relay host's `env_overlay`, laid last over a child's environment.
    pub env_overlay: &'a BTreeMap<String, String>,
}

fn entry_type_str(entry_type: EntryType) -> &'static str {
    match entry_type {
        EntryType::File => "file",
        EntryType::Directory => "directory",
        EntryType::Symlink => "symlink",
        EntryType::Other => "other",
    }
}

fn applied(response: Map<String, Value>) -> OpOutcome {
    OpOutcome::applied(response)
}

fn shell_outcome(r: ExecResult) -> OpOutcome {
    if r.aborted {
        return OpOutcome::cancelled("aborted", "the op was cancelled");
    }
    if r.timed_out {
        return OpOutcome::timed_out("timed_out", format!("timed out after {} ms", r.duration_ms));
    }
    let mut map = Map::new();
    map.insert("exitCode".to_owned(), json!(r.exit_code));
    map.insert("signal".to_owned(), json!(r.signal));
    map.insert(
        "stdout".to_owned(),
        Value::String(String::from_utf8_lossy(&r.stdout).into_owned()),
    );
    map.insert(
        "stderr".to_owned(),
        Value::String(String::from_utf8_lossy(&r.stderr).into_owned()),
    );
    map.insert("timedOut".to_owned(), Value::Bool(false));
    map.insert("durationMs".to_owned(), json!(r.duration_ms));
    map.insert(
        "truncated".to_owned(),
        Value::Bool(r.stdout_truncated || r.stderr_truncated),
    );
    applied(map)
}

/// A read above the limit names the chunked form that reads it, first, so the message cap never cuts it.
fn chunked_read_hint(error: MachineOpError, limit: Option<usize>) -> MachineOpError {
    if error.code == OpErrorCode::LimitExceeded {
        let limit_text = limit
            .map(|n| n.to_string())
            .unwrap_or_else(|| "the read limit".to_owned());
        MachineOpError::new(
            OpErrorCode::LimitExceeded,
            format!(
                "read it in chunks: fs.read {{path, offset, limit_bytes}} with limit_bytes at most {limit_text} ({})",
                error.message
            ),
        )
    } else {
        error
    }
}

pub async fn run_read(req: ReadRequest, c: &CoreCall<'_>) -> Result<OpOutcome, MachineOpError> {
    let r = c
        .ops
        .files()
        .read(req, c.ctx)
        .await
        .map_err(|err| chunked_read_hint(err, c.ctx.limits.read_max_bytes))?;
    let mut map = Map::new();
    map.insert(
        "path".to_owned(),
        Value::String(r.path.to_string_lossy().into_owned()),
    );
    map.insert("bytes".to_owned(), Value::String(STANDARD.encode(&r.bytes)));
    map.insert("size".to_owned(), json!(r.size));
    map.insert("offset".to_owned(), json!(r.offset));
    Ok(applied(map))
}

pub async fn run_write(req: WriteRequest, c: &CoreCall<'_>) -> Result<OpOutcome, MachineOpError> {
    let r = c.ops.files().write(req, c.ctx).await?;
    let mut map = Map::new();
    map.insert(
        "path".to_owned(),
        Value::String(r.path.to_string_lossy().into_owned()),
    );
    map.insert("writtenBytes".to_owned(), json!(r.written_bytes));
    Ok(applied(map))
}

pub async fn run_edit(req: EditRequest, c: &CoreCall<'_>) -> Result<OpOutcome, MachineOpError> {
    let r = c.ops.files().edit(req, c.ctx).await?;
    let mut map = Map::new();
    map.insert(
        "path".to_owned(),
        Value::String(r.path.to_string_lossy().into_owned()),
    );
    map.insert("replacements".to_owned(), json!(r.replacements));
    map.insert(
        "writtenBytes".to_owned(),
        json!(r.write_result.written_bytes),
    );
    Ok(applied(map))
}

pub async fn run_glob(req: GlobRequest, c: &CoreCall<'_>) -> Result<OpOutcome, MachineOpError> {
    let r = c.ops.files().glob(req, c.ctx).await?;
    let entries: Vec<Value> = r
        .entries
        .into_iter()
        .map(|e| Value::String(e.path.to_string_lossy().into_owned()))
        .collect();
    let mut map = Map::new();
    map.insert("entries".to_owned(), Value::Array(entries));
    map.insert("truncated".to_owned(), Value::Bool(r.truncated));
    Ok(applied(map))
}

pub async fn run_grep(req: GrepRequest, c: &CoreCall<'_>) -> Result<OpOutcome, MachineOpError> {
    let r = c.ops.files().grep(req, c.ctx).await?;
    let matches: Vec<Value> = r
        .matches
        .into_iter()
        .map(|m| {
            json!({
                "path": m.path.to_string_lossy(),
                "line": m.line,
                "text": m.text,
            })
        })
        .collect();
    let mut map = Map::new();
    map.insert("matches".to_owned(), Value::Array(matches));
    map.insert("truncated".to_owned(), Value::Bool(r.truncated));
    Ok(applied(map))
}

pub async fn run_stat(req: StatRequest, c: &CoreCall<'_>) -> Result<OpOutcome, MachineOpError> {
    let r = c.ops.files().stat(req, c.ctx).await?;
    let mut map = Map::new();
    map.insert(
        "path".to_owned(),
        Value::String(r.path.to_string_lossy().into_owned()),
    );
    map.insert(
        "type".to_owned(),
        Value::String(entry_type_str(r.entry_type).to_owned()),
    );
    map.insert("size".to_owned(), json!(r.size));
    map.insert("mtimeMs".to_owned(), json!(r.mtime_ms));
    map.insert("mode".to_owned(), json!(r.mode));
    Ok(applied(map))
}

pub async fn run_list(req: ListRequest, c: &CoreCall<'_>) -> Result<OpOutcome, MachineOpError> {
    let r = c.ops.files().list(req, c.ctx).await?;
    let entries: Vec<Value> = r
        .entries
        .into_iter()
        .map(|e| {
            json!({
                "name": e.name,
                "type": entry_type_str(e.entry_type),
            })
        })
        .collect();
    let mut map = Map::new();
    map.insert(
        "path".to_owned(),
        Value::String(r.path.to_string_lossy().into_owned()),
    );
    map.insert("entries".to_owned(), Value::Array(entries));
    Ok(applied(map))
}

/// A shell runs in the scope root, or where its cwd really resolves under it; never in the engine's own working directory.
pub async fn run_shell(
    mut req: ExecRequest,
    c: &CoreCall<'_>,
) -> Result<OpOutcome, MachineOpError> {
    let cwd = match &req.cwd {
        None => PathBuf::from(c.root),
        Some(raw) => spawn_path_in(c.root, &raw.to_string_lossy(), c.confined).await?,
    };
    req.cwd = Some(cwd);
    let mut env = req.env.take().unwrap_or_default();
    for (k, v) in c.env_overlay {
        env.insert(k.clone(), v.clone());
    }
    req.env = Some(env);
    let result = c.ops.process().exec(req, c.ctx).await?;
    Ok(shell_outcome(result))
}

fn process_not_found(message: String) -> OpOutcome {
    OpOutcome::failed("process_not_found", message)
}

/// A handleId names a terminal the engine runs (`pid_of_handle`); a missing process is `process_not_found`.
pub async fn run_process_info(
    req: ProcessInfoRequest,
    c: &CoreCall<'_>,
    pid_of_handle: &(dyn Fn(&str) -> Option<u32> + Send + Sync),
) -> Result<OpOutcome, MachineOpError> {
    let pid = match &req.handle_id {
        None => req.pid,
        Some(handle_id) => match pid_of_handle(handle_id) {
            Some(pid) => Some(pid),
            None => return Ok(process_not_found(format!("no handle {handle_id}"))),
        },
    };
    let info_req = ProcessInfoRequest {
        pid,
        handle_id: None,
    };
    match c.ops.info().process_info(info_req).await {
        Ok(info) => {
            let mut map = Map::new();
            map.insert("pid".to_owned(), json!(info.pid));
            map.insert("startedAtMs".to_owned(), json!(info.started_at_ms));
            map.insert("cpuTimeMs".to_owned(), json!(info.cpu_time_ms));
            map.insert("rssBytes".to_owned(), json!(info.rss_bytes));
            map.insert("command".to_owned(), json!(info.command));
            if let Some(last_op_at_ms) = info.last_op_at_ms {
                map.insert("lastOpAtMs".to_owned(), json!(last_op_at_ms));
            }
            Ok(applied(map))
        }
        Err(err) if err.code == OpErrorCode::NotFound => Ok(process_not_found(err.message)),
        Err(err) => Err(err),
    }
}

/// The client parses exactly `{accepted, seq, inboxPath}` and rejects a partial reply.
pub async fn run_mesh_message(
    req: MessageRequest,
    c: &CoreCall<'_>,
) -> Result<OpOutcome, MachineOpError> {
    let r = c.ops.info().message(req, c.ctx).await?;
    let mut map = Map::new();
    map.insert("accepted".to_owned(), Value::Bool(r.accepted));
    map.insert("seq".to_owned(), json!(r.seq));
    map.insert(
        "inboxPath".to_owned(),
        Value::String(r.inbox_path.to_string_lossy().into_owned()),
    );
    let _ = OpState::Applied;
    Ok(applied(map))
}
