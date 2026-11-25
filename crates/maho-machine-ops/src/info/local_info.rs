//! Port of `packages/machine-ops/src/info/local-info.ts`.

use crate::errors::{MachineOpError, OpErrorCode};
use crate::policy::gate::gated;
use crate::types::{
    HostFacts, InfoOps, MessageRequest, MessageResult, OpContext, OpFuture, OpName, ProcessHandle,
    ProcessInfo, ProcessInfoRequest,
};
use serde_json::json;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::io::AsyncWriteExt;

static TOPIC_RE: LazyLock<regex::Regex> = LazyLock::new(|| {
    #[allow(clippy::expect_used)]
    regex::Regex::new(r"^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$").expect("valid topic regex")
});

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

async fn ps_field(pid: u32, field: &str) -> Result<String, MachineOpError> {
    let output = tokio::process::Command::new("ps")
        .args(["-o", &format!("{field}="), "-p", &pid.to_string()])
        .output()
        .await
        .map_err(|err| {
            MachineOpError::new(OpErrorCode::NotFound, format!("no process {pid}: {err}"))
        })?;
    if !output.status.success() {
        return Err(MachineOpError::new(
            OpErrorCode::NotFound,
            format!("no process {pid}"),
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

/// `[[dd-]hh:]mm:ss` (`ps etime`/`time`) to milliseconds.
fn ps_duration_ms(value: &str) -> u64 {
    let (days, rest) = match value.split_once('-') {
        Some((d, r)) => (d.parse::<u64>().unwrap_or(0), r),
        None => (0, value),
    };
    let mut parts: Vec<u64> = rest
        .split(':')
        .map(|part| part.trim().parse::<u64>().unwrap_or(0))
        .collect();
    while parts.len() < 3 {
        parts.insert(0, 0);
    }
    let hours = parts.first().copied().unwrap_or(0);
    let minutes = parts.get(1).copied().unwrap_or(0);
    let seconds = parts.get(2).copied().unwrap_or(0);
    ((days * 24 + hours) * 60 + minutes) * 3_600_000 / 60 + seconds * 1000
}

async fn query_process_info(
    pid: u32,
    last_op_at_ms: Option<u64>,
) -> Result<ProcessInfo, MachineOpError> {
    let (rss_kib, cpu_time, elapsed, command) = tokio::try_join!(
        ps_field(pid, "rss"),
        ps_field(pid, "time"),
        ps_field(pid, "etime"),
        ps_field(pid, "command"),
    )?;
    if command.is_empty() {
        return Err(MachineOpError::new(
            OpErrorCode::NotFound,
            format!("no process {pid}"),
        ));
    }
    let rss_bytes = rss_kib.trim().parse::<u64>().unwrap_or(0) * 1024;
    let started_at_ms = now_ms().saturating_sub(ps_duration_ms(&elapsed));
    let cpu_time_ms = ps_duration_ms(&cpu_time);
    Ok(ProcessInfo {
        pid,
        started_at_ms,
        cpu_time_ms,
        rss_bytes,
        command,
        last_op_at_ms,
    })
}

fn node_platform() -> &'static str {
    match std::env::consts::OS {
        "windows" => "win32",
        "macos" => "darwin",
        other => other,
    }
}

fn node_arch() -> &'static str {
    match std::env::consts::ARCH {
        "x86_64" => "x64",
        "aarch64" => "arm64",
        "x86" => "ia32",
        other => other,
    }
}

fn read_load1() -> f64 {
    #[cfg(target_os = "linux")]
    if let Ok(text) = std::fs::read_to_string("/proc/loadavg")
        && let Some(first) = text.split_whitespace().next()
        && let Ok(val) = first.parse::<f64>()
    {
        return val;
    }
    0.0
}

pub type HandleLookup = Arc<dyn Fn(&str) -> Option<Arc<dyn ProcessHandle>> + Send + Sync>;
pub type LastOpAtLookup = Arc<dyn Fn() -> Option<u64> + Send + Sync>;

struct TopicQueue {
    lock: tokio::sync::Mutex<Option<usize>>,
}

pub struct LocalInfoOps {
    state_dir: Option<PathBuf>,
    handle: HandleLookup,
    last_op_at_ms: LastOpAtLookup,
    topics: Mutex<HashMap<String, Arc<TopicQueue>>>,
}

impl LocalInfoOps {
    pub fn new(
        state_dir: Option<PathBuf>,
        handle: HandleLookup,
        last_op_at_ms: LastOpAtLookup,
    ) -> Self {
        Self {
            state_dir,
            handle,
            last_op_at_ms,
            topics: Mutex::new(HashMap::new()),
        }
    }

    fn topic_queue(&self, topic: &str) -> Arc<TopicQueue> {
        let mut map = self
            .topics
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        map.entry(topic.to_owned())
            .or_insert_with(|| {
                Arc::new(TopicQueue {
                    lock: tokio::sync::Mutex::new(None),
                })
            })
            .clone()
    }
}

async fn count_existing_lines(path: &Path) -> usize {
    match tokio::fs::read_to_string(path).await {
        Ok(text) => text.split('\n').filter(|line| !line.is_empty()).count(),
        Err(_) => 0,
    }
}

impl InfoOps for LocalInfoOps {
    fn process_info(&self, req: ProcessInfoRequest) -> OpFuture<'_, ProcessInfo> {
        Box::pin(async move {
            let mut pid = req.pid;
            if let Some(handle_id) = req.handle_id.as_deref() {
                let Some(handle) = (self.handle)(handle_id) else {
                    return Err(MachineOpError::new(
                        OpErrorCode::NotFound,
                        format!("no handle {handle_id}"),
                    ));
                };
                pid = Some(handle.pid());
            }
            let own_pid = std::process::id();
            let target_pid = pid.unwrap_or(own_pid);
            let last_op = if target_pid == own_pid {
                (self.last_op_at_ms)()
            } else {
                None
            };
            query_process_info(target_pid, last_op).await
        })
    }

    fn host(&self) -> HostFacts {
        HostFacts {
            os: node_platform().to_owned(),
            arch: node_arch().to_owned(),
            load1: read_load1(),
            cores: std::thread::available_parallelism()
                .map(usize::from)
                .unwrap_or(1),
            runtime: format!("rust {}", env!("CARGO_PKG_VERSION")),
        }
    }

    fn message<'a>(
        &'a self,
        req: MessageRequest,
        ctx: &'a OpContext,
    ) -> OpFuture<'a, MessageResult> {
        Box::pin(async move {
            if !TOPIC_RE.is_match(&req.topic) {
                return Err(MachineOpError::new(
                    OpErrorCode::InvalidArgument,
                    format!("invalid topic: {}", req.topic),
                ));
            }
            let Some(state_dir) = &self.state_dir else {
                return Err(MachineOpError::new(
                    OpErrorCode::InvalidArgument,
                    "info.message needs a stateDir",
                ));
            };
            let inbox_dir = state_dir.join("machine").join("inbox");
            let args = json!({ "topic": req.topic });
            gated(OpName::InfoMessage, &args, ctx, || async {
                let inbox_path = inbox_dir.join(format!("{}.jsonl", req.topic));
                let queue = self.topic_queue(&req.topic);
                let mut seq_guard = queue.lock.lock().await;
                let last_seq = match *seq_guard {
                    Some(prev) => prev,
                    None => count_existing_lines(&inbox_path).await,
                };
                let seq = last_seq + 1;
                tokio::fs::create_dir_all(&inbox_dir).await.map_err(|err| {
                    MachineOpError::from_io(&err, Some("mkdir"), Some(&inbox_dir))
                })?;
                let line = json!({
                    "seq": seq,
                    "topic": req.topic,
                    "atMs": now_ms(),
                    "principal": ctx.principal,
                    "body": req.body,
                });
                let payload = format!("{line}\n");
                let mut file = tokio::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&inbox_path)
                    .await
                    .map_err(|err| {
                        MachineOpError::from_io(&err, Some("open"), Some(&inbox_path))
                    })?;
                file.write_all(payload.as_bytes()).await.map_err(|err| {
                    MachineOpError::from_io(&err, Some("write"), Some(&inbox_path))
                })?;
                file.flush().await.map_err(|err| {
                    MachineOpError::from_io(&err, Some("flush"), Some(&inbox_path))
                })?;
                *seq_guard = Some(seq);
                Ok(MessageResult {
                    accepted: true,
                    seq,
                    inbox_path,
                })
            })
            .await
        })
    }
}
