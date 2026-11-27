//! Host-ensure & status runner invoking current `mhc` executable (`thread/wake/host-ensure.ts`).

use std::collections::BTreeMap;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub type BoxFuture<T> = Pin<Box<dyn Future<Output = T> + Send>>;

const HOST_ENSURE_TIMEOUT_MS: u64 = 60_000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostEnsureReport {
    pub ok: bool,
    #[serde(default)]
    pub action: Option<String>,
    #[serde(default)]
    pub socket: Option<String>,
    #[serde(default)]
    pub error: Option<String>,
}

#[derive(Clone)]
pub struct EngineHostEnsureOptions {
    pub socket: String,
    pub agent_dir: PathBuf,
    pub idle_exit_ms: Option<u64>,
    pub launch_spec_path: Option<PathBuf>,
    pub executable: Option<PathBuf>,
    pub env: BTreeMap<String, String>,
    pub warn: Arc<dyn Fn(&str) + Send + Sync>,
}

impl std::fmt::Debug for EngineHostEnsureOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EngineHostEnsureOptions")
            .field("socket", &self.socket)
            .field("agent_dir", &self.agent_dir)
            .field("idle_exit_ms", &self.idle_exit_ms)
            .finish_non_exhaustive()
    }
}

impl EngineHostEnsureOptions {
    pub fn new(
        socket: impl Into<String>,
        agent_dir: PathBuf,
        warn: Arc<dyn Fn(&str) + Send + Sync>,
    ) -> Self {
        Self {
            socket: socket.into(),
            agent_dir,
            idle_exit_ms: None,
            launch_spec_path: None,
            executable: None,
            env: BTreeMap::new(),
            warn,
        }
    }
}

pub fn parse_host_ensure_json(stdout: &str) -> Option<HostEnsureReport> {
    let line = stdout
        .lines()
        .rev()
        .map(str::trim)
        .find(|l| !l.is_empty())?;
    let val: Value = serde_json::from_str(line).ok()?;
    let obj = val.as_object()?;
    let ok = obj
        .get("ok")
        .and_then(Value::as_bool)
        .or_else(|| obj.get("reachable").and_then(Value::as_bool))
        .unwrap_or(true);
    Some(HostEnsureReport {
        ok,
        action: obj.get("action").and_then(Value::as_str).map(str::to_owned),
        socket: obj.get("socket").and_then(Value::as_str).map(str::to_owned),
        error: obj.get("error").and_then(Value::as_str).map(str::to_owned),
    })
}

/// Ensures the local operator daemon host via `std::env::current_exe()` (`host ensure --policy never --json`).
pub async fn ensure_host_via_cli(options: &EngineHostEnsureOptions) {
    let exe = match &options.executable {
        Some(p) => p.clone(),
        None => match std::env::current_exe() {
            Ok(p) => p,
            Err(err) => {
                (options.warn)(&format!(
                    "thread wake: no engine CLI to ensure the operator host: {err}"
                ));
                return;
            }
        },
    };

    let mut args = vec![
        "host".to_owned(),
        "ensure".to_owned(),
        "--socket".to_owned(),
        options.socket.clone(),
        "--policy".to_owned(),
        "never".to_owned(),
        "--json".to_owned(),
    ];
    if let Some(ref spec) = options.launch_spec_path {
        args.push("--launch-spec".to_owned());
        args.push(spec.to_string_lossy().into_owned());
    }

    let mut cmd = tokio::process::Command::new(&exe);
    cmd.args(&args)
        .env("OMO_AGENT_DIR", &options.agent_dir)
        .env("OMO_CODING_AGENT_DIR", &options.agent_dir)
        .kill_on_drop(true);
    for (k, v) in &options.env {
        cmd.env(k, v);
    }
    if let Some(ms) = options.idle_exit_ms
        && ms > 0
    {
        cmd.env("SENPI_RPC_HOST_IDLE_EXIT_MS", ms.to_string());
    }

    let child = match cmd.output().await {
        Ok(out) => out,
        Err(err) => {
            (options.warn)(&format!(
                "thread wake: the engine did not ensure the operator host: {err}"
            ));
            return;
        }
    };
    let stdout = String::from_utf8_lossy(&child.stdout);
    let parsed = parse_host_ensure_json(&stdout);
    if !child.status.success() || parsed.as_ref().is_some_and(|r| !r.ok) {
        let code = child
            .status
            .code()
            .map(|c| c.to_string())
            .unwrap_or_else(|| "signal".to_owned());
        (options.warn)(&format!(
            "thread wake: the engine did not ensure the operator host: {code} {}",
            stdout.trim()
        ));
    }
}

/// Runs `mhc host status --all --include-workers --json` using `std::env::current_exe()`.
pub async fn run_host_status_all_cli(
    agent_dir: Option<&std::path::Path>,
    executable: Option<&std::path::Path>,
) -> Option<String> {
    let exe = match executable {
        Some(p) => p.to_path_buf(),
        None => std::env::current_exe().ok()?,
    };
    let mut cmd = tokio::process::Command::new(exe);
    cmd.args(["host", "status", "--all", "--include-workers", "--json"])
        .kill_on_drop(true);
    if let Some(dir) = agent_dir {
        cmd.env("OMO_AGENT_DIR", dir)
            .env("OMO_CODING_AGENT_DIR", dir);
    }
    let out = tokio::time::timeout(Duration::from_millis(HOST_ENSURE_TIMEOUT_MS), cmd.output())
        .await
        .ok()?
        .ok()?;
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    (!stdout.trim().is_empty()).then_some(stdout)
}

pub fn engine_host_ensure(
    options: EngineHostEnsureOptions,
) -> Arc<dyn Fn() -> BoxFuture<()> + Send + Sync> {
    Arc::new(move || {
        let opts = options.clone();
        Box::pin(async move {
            ensure_host_via_cli(&opts).await;
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_host_ensure_and_status_json_lines() {
        let parsed = parse_host_ensure_json(
            "log line\n{\"ok\":true,\"action\":\"reused\",\"socket\":\"/tmp/rpc.sock\"}\n",
        )
        .unwrap();
        assert!(parsed.ok);
        assert_eq!(parsed.action.as_deref(), Some("reused"));
        assert_eq!(parsed.socket.as_deref(), Some("/tmp/rpc.sock"));
    }
}
