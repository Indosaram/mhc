//! Shared helpers for lsp-daemon integration tests.
#![allow(dead_code)]

use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use lsp_core::request_context::{LspRequestContext, parse_lsp_request_context};
use lsp_daemon::daemon_client::EnsureFn;
use lsp_daemon::ipc_protocol::{ensure_private_directory, write_private_file};
use lsp_daemon::paths::DaemonPaths;
use lsp_daemon::socket_jsonrpc::{LineBuffer, encode_json_line};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixListener;
use tokio::sync::Notify;

pub const TIMEOUT: Duration = Duration::from_secs(10);

/// Unix socket paths are length-limited (104 bytes on macOS), so tests root in /tmp.
pub fn short_tempdir() -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix("lspd-")
        .tempdir_in("/tmp")
        .unwrap()
}

pub fn test_paths(root: &tempfile::TempDir) -> DaemonPaths {
    DaemonPaths::under_dir(root.path().join("v1.0.0"), "1.0.0")
}

pub fn context_for(cwd: &Path) -> LspRequestContext {
    let cwd = std::fs::canonicalize(cwd).unwrap();
    let join = |name: &str| cwd.join(name).to_string_lossy().into_owned();
    parse_lsp_request_context(&json!({
        "cwd": cwd.to_string_lossy(),
        "projectConfigPaths": [join("lsp-client.json")],
        "userConfigPath": join("user-lsp.json"),
        "installDecisionsPath": join("install-decisions.json"),
        "capabilities": {"installDecisionTool": true},
    }))
    .unwrap()
}

pub fn noop_ensure() -> EnsureFn {
    Arc::new(|_paths, _signal| Box::pin(async { Ok(()) }))
}

pub fn write_token(paths: &DaemonPaths, token: &str) {
    ensure_private_directory(&paths.dir).unwrap();
    write_private_file(&paths.auth, token).unwrap();
}

pub async fn within<T>(future: impl Future<Output = T>) -> T {
    tokio::time::timeout(TIMEOUT, future)
        .await
        .expect("operation timed out")
}

/// What a scripted fake daemon does with one received line.
pub enum Reply {
    Respond(Value),
    /// Keep the connection open without answering.
    Hold,
    /// Close the connection immediately.
    Close,
}

/// Scripted stand-in for the daemon socket that records every received line.
#[derive(Clone, Default)]
pub struct FakeDaemon {
    pub received: Arc<Mutex<Vec<Value>>>,
    pub connections_closed: Arc<Mutex<usize>>,
    pub changed: Arc<Notify>,
}

impl FakeDaemon {
    pub fn received(&self) -> Vec<Value> {
        self.received.lock().unwrap().clone()
    }

    pub fn closed(&self) -> usize {
        *self.connections_closed.lock().unwrap()
    }

    /// Waits until `predicate` holds over the recorded state.
    pub async fn wait_until(&self, predicate: impl Fn(&FakeDaemon) -> bool) {
        within(async {
            loop {
                let notified = self.changed.notified();
                if predicate(self) {
                    return;
                }
                notified.await;
            }
        })
        .await;
    }

    pub fn tool_calls(&self) -> usize {
        self.received()
            .iter()
            .filter(|message| message["method"] == json!("tools/call"))
            .count()
    }
}

pub type Script = Arc<dyn Fn(&Value, usize) -> Reply + Send + Sync>;

/// Binds a fake daemon at `socket`; `script(message, tool_call_index)` decides each reply.
pub fn spawn_fake_daemon(socket: &Path, script: Script) -> FakeDaemon {
    std::fs::create_dir_all(socket.parent().unwrap()).unwrap();
    let listener = UnixListener::bind(socket).unwrap();
    let fake = FakeDaemon::default();
    let state = fake.clone();
    tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                return;
            };
            let state = state.clone();
            let script = Arc::clone(&script);
            tokio::spawn(async move {
                let mut decoder = LineBuffer::new();
                let mut chunk = [0_u8; 8192];
                'connection: loop {
                    let read = match stream.read(&mut chunk).await {
                        Ok(0) | Err(_) => break,
                        Ok(read) => read,
                    };
                    for message in decoder
                        .push(&chunk[..read])
                        .into_iter()
                        .filter_map(Result::ok)
                    {
                        let index = {
                            let mut received = state.received.lock().unwrap();
                            received.push(message.clone());
                            received
                                .iter()
                                .filter(|m| m["method"] == json!("tools/call"))
                                .count()
                                .saturating_sub(1)
                        };
                        state.changed.notify_waiters();
                        match script(&message, index) {
                            Reply::Respond(response) => {
                                let _ignored = stream
                                    .write_all(encode_json_line(&response).as_bytes())
                                    .await;
                            }
                            Reply::Hold => {}
                            Reply::Close => break 'connection,
                        }
                    }
                }
                *state.connections_closed.lock().unwrap() += 1;
                state.changed.notify_waiters();
            });
        }
    });
    fake
}

pub fn tool_result(message: &Value, text: &str) -> Value {
    json!({"jsonrpc": "2.0", "id": message["id"], "result": {"content": [{"type": "text", "text": text}], "isError": false}})
}

pub fn socket_of(paths: &DaemonPaths) -> PathBuf {
    paths.socket.clone()
}
