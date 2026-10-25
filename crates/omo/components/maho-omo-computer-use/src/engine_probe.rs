use std::collections::BTreeMap;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;
use tokio::time::timeout;

use senpi_desktop_core::protocol::{ABI as ENGINE_ABI, PROTOCOL_VERSION};
use senpi_desktop_protocol::DesktopCapabilities;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

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
    LaunchContextUnavailable {
        reason: String,
        message: String,
        diagnostic: String,
    },
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
                if let Ok(raw) = i32::try_from(pid) {
                    let _ = nix::sys::signal::killpg(
                        nix::unistd::Pid::from_raw(raw),
                        nix::sys::signal::Signal::SIGKILL,
                    );
                }
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
        let byte_offset = tail
            .char_indices()
            .nth(drop_count)
            .map(|(idx, _)| idx)
            .unwrap_or(0);
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
    let refusal = Arc::new(std::sync::Mutex::new(LaunchRefusalAccumulator::default()));
    let refusal_clone = refusal.clone();
    let tail_clone = Arc::clone(&stderr_tail);
    let stderr_task = tokio::spawn(async move {
        let mut reader = BufReader::new(stderr);
        let mut chunk = [0u8; 1024];
        loop {
            let size = match reader.read(&mut chunk).await {
                Ok(0) | Err(_) => break,
                Ok(size) => size,
            };
            let text = String::from_utf8_lossy(&chunk[..size]);
            refusal_clone
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(&text);
            let mut tail = tail_clone.lock().unwrap_or_else(|e| e.into_inner());
            append_stderr_tail_chars(&mut tail, &text, STDERR_TAIL_CHARS);
        }
    });
    let mut stderr_guard = AbortOnDropHandle::new(stderr_task);
    let mut refusal_enabled = true;

    let probe_interaction = async {
        let req1 = json!({ "jsonrpc": "2.0", "id": 1, "method": "engine.hello", "params": {} });
        let req2 = json!({ "jsonrpc": "2.0", "id": 2, "method": "capabilities", "params": {} });

        if let Err(e) = stdin
            .write_all(format!("{req1}\n{req2}\n").as_bytes())
            .await
        {
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
                    refusal_enabled = false;
                    return EngineProbeResult::Err {
                        code: "handshake-failed".to_string(),
                        message: format!("desktop engine emitted non-JSON output at {engine_path}"),
                    };
                }
            };

            let id = val.get("id").and_then(|v| v.as_i64());
            if id == Some(1) || val.get("error").is_some() {
                refusal_enabled = false;
            }
            if let Some(err) = val.get("error") {
                return EngineProbeResult::Err {
                    code: "handshake-failed".to_string(),
                    message: format!(
                        "desktop engine request {id:?} failed at {engine_path}: {err}"
                    ),
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
            (Some(hello), Some(capabilities)) => EngineProbeResult::Ok(Box::new(EngineProbe {
                hello,
                capabilities,
            })),
            _ => {
                let tail = stderr_tail
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .trim()
                    .to_string();
                let err_str = if tail.is_empty() { "<empty>" } else { &tail };
                EngineProbeResult::Err {
                    code: "handshake-failed".to_string(),
                    message: format!(
                        "desktop engine exited before the probe completed at {engine_path}; stderr: {err_str}"
                    ),
                }
            }
        }
    };

    let result = match timeout(Duration::from_millis(timeout_ms), probe_interaction).await {
        Ok(res) => res,
        Err(_) => EngineProbeResult::Err {
            code: "timeout".to_string(),
            message: format!(
                "desktop engine probe timed out after {timeout_ms} ms at {engine_path}"
            ),
        },
    };

    // Only a naturally exited child, before hello/protocol failure, may supply a refusal.
    if refusal_enabled
        && matches!(&result, EngineProbeResult::Err { code, .. } if code == "handshake-failed")
        && let Ok(Ok(status)) = timeout(Duration::from_millis(timeout_ms), guard.child.wait()).await
        {
            guard.pid = None;
            let _ = (&mut stderr_guard.handle).await;
            let tail = stderr_tail
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone();
            if status.code() == Some(1)
                && let Some(reason) = refusal.lock().unwrap_or_else(|e| e.into_inner()).finish() {
                    return EngineProbeResult::LaunchContextUnavailable {
                        message: launch_context_message(&reason),
                        reason,
                        diagnostic: tail,
                    };
                }
        }
    guard.kill_group();
    guard.wait_exited().await;
    result
}

pub fn launch_context_message(reason: &str) -> String {
    format!(
        "COMPUTER_LAUNCH_CONTEXT_REQUIRED: Computer use cannot start safely from this host's current macOS launch context.{} Restart this host from a local terminal and keep that terminal app running, or use the OmO desktop app (Settings > Restart engine). Opening a new terminal or reconnecting to this same host does not repair its launch context.",
        if reason == "unresolved" {
            " macOS responsibility could not be resolved."
        } else {
            ""
        }
    )
}
pub fn parse_launch_refusal(stderr: &str) -> Option<String> {
    const MARKER: &str = "SENPI_DESKTOP_LAUNCH_REFUSED";
    let mut structured = None;
    let mut legacy = None;
    for line in stderr.split_inclusive('\n') {
        let complete = line.ends_with('\n');
        let line = line.strip_suffix('\n').unwrap_or(line);
        if line.starts_with(MARKER) {
            if !complete || structured.is_some() || line.len() + 1 > 512 {
                return None;
            }
            let raw = line.strip_prefix("SENPI_DESKTOP_LAUNCH_REFUSED ")?;
            // Reject duplicate keys before serde_json's map would collapse them.
            let mut deserializer = serde_json::Deserializer::from_str(raw);
            struct Record;
            impl<'de> serde::de::Visitor<'de> for Record {
                type Value = String;
                fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                    f.write_str("a version and reason record")
                }
                fn visit_map<A: serde::de::MapAccess<'de>>(
                    self,
                    mut map: A,
                ) -> Result<String, A::Error> {
                    let mut version = None;
                    let mut reason: Option<String> = None;
                    while let Some(key) = map.next_key::<String>()? {
                        match key.as_str() {
                            "version" if version.is_none() => {
                                version = Some(map.next_value::<u32>()?)
                            }
                            "reason" if reason.is_none() => reason = Some(map.next_value()?),
                            _ => return Err(serde::de::Error::custom("duplicate or unknown key")),
                        }
                    }
                    if version != Some(1) {
                        return Err(serde::de::Error::custom("version"));
                    }
                    reason
                        .filter(|r| r == "self-responsible" || r == "unresolved")
                        .ok_or_else(|| serde::de::Error::custom("reason"))
                }
            }
            use serde::Deserializer;
            let reason = deserializer.deserialize_map(Record).ok()?;
            deserializer.end().ok()?;
            structured = Some(reason);
        } else if complete && line.len() < 512 {
            let reason = if let Some(rest) = line.strip_prefix(
                "senpi-desktop-engine: refusing to run as its own responsible process (pid ",
            ) {
                rest.split_once("): spawn the engine as a child of the app, terminal or host that holds the computer-use grants, without the responsibility-disclaim attribute and not as a launchd job or through a shell exec").filter(|(pid, suffix)| suffix.is_empty() && valid_pid(pid)).map(|_| "self-responsible")
            } else if let Some(rest) = line.strip_prefix(
                "senpi-desktop-engine: refusing to run: the responsible process of engine pid ",
            ) {
                rest.strip_suffix(" cannot be resolved")
                    .filter(|pid| valid_pid(pid))
                    .map(|_| "unresolved")
            } else {
                None
            };
            if let Some(reason) = reason {
                if legacy.is_some_and(|old| old != reason) {
                    return None;
                }
                legacy = Some(reason);
            }
        }
    }
    structured.or_else(|| legacy.map(str::to_owned))
}
fn valid_pid(pid: &str) -> bool {
    !pid.is_empty() && !pid.starts_with('0') && pid.bytes().all(|c| c.is_ascii_digit())
}

#[cfg(test)]
mod launch_refusal_tests {
    use super::*;
    #[test]
    fn only_complete_strict_records_are_refusals() {
        assert_eq!(
            parse_launch_refusal(
                "SENPI_DESKTOP_LAUNCH_REFUSED {\"version\":1,\"reason\":\"unresolved\"}\n"
            ),
            Some("unresolved".into())
        );
        for text in [
            "SENPI_DESKTOP_LAUNCH_REFUSED {\"version\":1,\"reason\":\"unresolved\"}",
            "SENPI_DESKTOP_LAUNCH_REFUSED {\"version\":1,\"reason\":\"other\"}\n",
            "SENPI_DESKTOP_LAUNCH_REFUSED {\"version\":1,\"version\":1,\"reason\":\"unresolved\"}\n",
        ] {
            assert!(parse_launch_refusal(text).is_none());
        }
    }
}

#[derive(Default)]
struct LaunchRefusalAccumulator {
    line: String,
    oversized: bool,
    records: String,
    invalid: bool,
}
impl LaunchRefusalAccumulator {
    fn push(&mut self, chunk: &str) {
        for c in chunk.chars() {
            if c == '\n' {
                let relevant = self.line.starts_with("SENPI_DESKTOP_LAUNCH_REFUSED")
                    || self
                        .line
                        .starts_with("senpi-desktop-engine: refusing to run");
                if relevant {
                    if self.oversized || self.records.len() > 1024 {
                        self.invalid = true;
                    } else {
                        self.records.push_str(&self.line);
                        self.records.push('\n');
                    }
                }
                self.line.clear();
                self.oversized = false;
            } else if self.line.len() + c.len_utf8() < 512 {
                self.line.push(c);
            } else {
                self.oversized = true;
            }
        }
    }
    fn finish(&self) -> Option<String> {
        if self.invalid || self.line.starts_with("SENPI_DESKTOP_LAUNCH_REFUSED") {
            None
        } else {
            parse_launch_refusal(&self.records)
        }
    }
}
