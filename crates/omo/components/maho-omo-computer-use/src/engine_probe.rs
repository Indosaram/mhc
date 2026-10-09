use std::collections::BTreeMap;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;
use tokio::time::timeout;

use senpi_desktop_core::protocol::{ABI as ENGINE_ABI, PROTOCOL_VERSION};
use senpi_desktop_protocol::DesktopCapabilities;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

pub const COMPUTER_USE_DOCTOR_TIMEOUT_MS: u64 = 5_000;
pub const STDERR_TAIL_CHARS: usize = 4_096;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HelloData {
    pub protocol_version: String,
    pub engine_version: String,
    pub build_sha: String,
    pub abi: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct EngineProbe {
    pub hello: HelloData,
    pub capabilities: DesktopCapabilities,
}

#[derive(Clone, Debug, PartialEq)]
pub enum EngineProbeResult {
    Ok(Box<EngineProbe>),
    Err {
        code: String,
        message: String,
    },
}

impl EngineProbeResult {
    pub fn is_ok(&self) -> bool {
        matches!(self, Self::Ok(_))
    }
}

pub struct ProcessGroupGuard {
    child: tokio::process::Child,
    pid: Option<u32>,
}

impl ProcessGroupGuard {
    pub fn new(child: tokio::process::Child) -> Self {
        let pid = child.id();
        Self { child, pid }
    }

    pub fn kill_group(&mut self) {
        if let Some(pid) = self.pid.take() {
            #[cfg(unix)]
            {
                let _ = std::process::Command::new("kill")
                    .args(["-9", &format!("-{pid}")])
                    .status();
            }
            #[cfg(windows)]
            {
                let _ = std::process::Command::new("taskkill")
                    .args(["/pid", &pid.to_string(), "/T", "/F"])
                    .status();
            }
            let _ = self.child.start_kill();
        }
    }

    pub async fn wait_exited(&mut self) {
        let _ = self.child.wait().await;
        self.pid = None;
    }
}

impl Drop for ProcessGroupGuard {
    fn drop(&mut self) {
        self.kill_group();
    }
}

pub struct AbortOnDropHandle<T> {
    handle: tokio::task::JoinHandle<T>,
}

impl<T> AbortOnDropHandle<T> {
    pub fn new(handle: tokio::task::JoinHandle<T>) -> Self {
        Self { handle }
    }
}

impl<T> Drop for AbortOnDropHandle<T> {
    fn drop(&mut self) {
        self.handle.abort();
    }
}

pub fn append_stderr_tail_chars(tail: &mut String, chunk: &str, limit_chars: usize) {
    tail.push_str(chunk);
    let char_count = tail.chars().count();
    if char_count > limit_chars {
        let drop_count = char_count - limit_chars;
        let byte_offset = tail.char_indices().nth(drop_count).map(|(idx, _)| idx).unwrap_or(0);
        *tail = tail[byte_offset..].to_string();
    }
}

pub async fn probe_computer_use_engine(
    engine_path: &str,
    env: &BTreeMap<String, String>,
    timeout_ms: u64,
) -> EngineProbeResult {
    let mut cmd = Command::new(engine_path);
    cmd.arg("--stdio");
    for (k, v) in env {
        cmd.env(k, v);
    }
    cmd.stdin(Stdio::piped());
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());

    #[cfg(unix)]
    {
        cmd.process_group(0);
    }

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            return EngineProbeResult::Err {
                code: "handshake-failed".to_string(),
                message: format!("desktop engine spawn failed at {engine_path}: {e}"),
            };
        }
    };

    let stdin = child.stdin.take();
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();

    let mut guard = ProcessGroupGuard::new(child);

    let (mut stdin, stdout, stderr) = match (stdin, stdout, stderr) {
        (Some(i), Some(o), Some(e)) => (i, o, e),
        _ => {
            guard.kill_group();
            guard.wait_exited().await;
            return EngineProbeResult::Err {
                code: "handshake-failed".to_string(),
                message: format!("failed to acquire stdio pipes at {engine_path}"),
            };
        }
    };

    let stderr_tail = Arc::new(std::sync::Mutex::new(String::new()));
    let tail_clone = Arc::clone(&stderr_tail);
    let stderr_task = tokio::spawn(async move {
        let mut reader = BufReader::new(stderr).lines();
        while let Ok(Some(line)) = reader.next_line().await {
            let mut tail = tail_clone.lock().unwrap_or_else(|e| e.into_inner());
            append_stderr_tail_chars(&mut tail, &format!("{line}\n"), STDERR_TAIL_CHARS);
        }
    });
    let _stderr_guard = AbortOnDropHandle::new(stderr_task);

    let probe_interaction = async {
        let req1 = json!({ "jsonrpc": "2.0", "id": 1, "method": "engine.hello", "params": {} });
        let req2 = json!({ "jsonrpc": "2.0", "id": 2, "method": "capabilities", "params": {} });

        if let Err(e) = stdin.write_all(format!("{req1}\n{req2}\n").as_bytes()).await {
            return EngineProbeResult::Err {
                code: "handshake-failed".to_string(),
                message: format!("desktop engine write failed at {engine_path}: {e}"),
            };
        }
        let _ = stdin.flush().await;

        let mut reader = BufReader::new(stdout).lines();
        let mut hello_opt: Option<HelloData> = None;
        let mut caps_opt: Option<DesktopCapabilities> = None;

        while let Ok(Some(line)) = reader.next_line().await {
            let val: Value = match serde_json::from_str(&line) {
                Ok(v) => v,
                Err(_) => {
                    return EngineProbeResult::Err {
                        code: "handshake-failed".to_string(),
                        message: format!("desktop engine emitted non-JSON output at {engine_path}"),
                    };
                }
            };

            let id = val.get("id").and_then(|v| v.as_i64());
            if let Some(err) = val.get("error") {
                return EngineProbeResult::Err {
                    code: "handshake-failed".to_string(),
                    message: format!("desktop engine request {id:?} failed at {engine_path}: {err}"),
                };
            }

            if let Some(res) = val.get("result") {
                if id == Some(1) {
                    if let Ok(h) = serde_json::from_value::<HelloData>(res.clone()) {
                        if h.abi != ENGINE_ABI || h.protocol_version != PROTOCOL_VERSION {
                            return EngineProbeResult::Err {
                                code: "abi-mismatch".to_string(),
                                message: format!(
                                    "desktop engine ABI mismatch at {engine_path}: expected abi {ENGINE_ABI} protocol {PROTOCOL_VERSION}, engine reported abi {} protocol {}",
                                    h.abi, h.protocol_version
                                ),
                            };
                        }
                        hello_opt = Some(h);
                    } else {
                        return EngineProbeResult::Err {
                            code: "handshake-failed".to_string(),
                            message: format!("malformed engine.hello result at {engine_path}"),
                        };
                    }
                } else if id == Some(2) {
                    if let Ok(c) = serde_json::from_value::<DesktopCapabilities>(res.clone()) {
                        caps_opt = Some(c);
                    } else {
                        return EngineProbeResult::Err {
                            code: "handshake-failed".to_string(),
                            message: format!("malformed capabilities result at {engine_path}"),
                        };
                    }
                }
            }

            if hello_opt.is_some() && caps_opt.is_some() {
                break;
            }
        }

        match (hello_opt, caps_opt) {
            (Some(hello), Some(capabilities)) => EngineProbeResult::Ok(Box::new(EngineProbe { hello, capabilities })),
            _ => {
                let tail = stderr_tail.lock().unwrap_or_else(|e| e.into_inner()).trim().to_string();
                let err_str = if tail.is_empty() { "<empty>" } else { &tail };
                EngineProbeResult::Err {
                    code: "handshake-failed".to_string(),
                    message: format!("desktop engine exited before the probe completed at {engine_path}; stderr: {err_str}"),
                }
            }
        }
    };

    let result = match timeout(Duration::from_millis(timeout_ms), probe_interaction).await {
        Ok(res) => res,
        Err(_) => EngineProbeResult::Err {
            code: "timeout".to_string(),
            message: format!("desktop engine probe timed out after {timeout_ms} ms at {engine_path}"),
        },
    };

    guard.kill_group();
    guard.wait_exited().await;

    result
}
