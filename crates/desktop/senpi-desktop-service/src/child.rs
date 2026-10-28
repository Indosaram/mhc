//! Child process spawn and NDJSON stream handling matching `src/service/child.ts`.

use std::collections::HashMap;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::{mpsc, oneshot, Mutex};

use crate::rpc_client::DesktopServiceError;

const STDERR_TAIL_CHARS: usize = 4096;

/// Resolves the sibling `senpi-desktop-engine` binary relative to the current executable.
pub fn default_engine_path() -> Result<PathBuf, DesktopServiceError> {
    let current_exe = std::env::current_exe().map_err(|e| {
        DesktopServiceError::closed(format!("cannot determine current executable: {e}"))
    })?;
    let parent_dir = current_exe.parent().ok_or_else(|| {
        DesktopServiceError::closed("current executable has no parent directory".to_string())
    })?;
    let sibling_name = if cfg!(windows) {
        "senpi-desktop-engine.exe"
    } else {
        "senpi-desktop-engine"
    };
    Ok(parent_dir.join(sibling_name))
}

pub type ChildFactory = Arc<
    dyn Fn() -> Pin<Box<dyn Future<Output = Result<EngineChild, DesktopServiceError>> + Send>>
        + Send
        + Sync,
>;

#[derive(Debug, Clone)]
pub struct EngineSpawnConfig {
    pub binary_path: PathBuf,
    pub args: Vec<String>,
    pub env: HashMap<String, String>,
}

impl EngineSpawnConfig {
    pub fn new(binary_path: PathBuf) -> Self {
        Self {
            binary_path,
            args: vec!["--stdio".to_string()],
            env: HashMap::new(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AcquiredDesktopEngine {
    pub path: Option<PathBuf>,
    pub diagnostic_message: Option<String>,
}

pub type EngineAcquirer = Arc<dyn Fn(String, Option<String>) -> Pin<Box<dyn Future<Output = Result<AcquiredDesktopEngine, DesktopServiceError>> + Send>> + Send + Sync>;
pub type EngineSpawner = Arc<dyn Fn(PathBuf) -> Result<EngineChild, DesktopServiceError> + Send + Sync>;

pub struct AcquiringEngineChildOptions {
    pub version: String,
    pub host: Option<String>,
    pub acquire: EngineAcquirer,
    pub spawn_engine: Option<EngineSpawner>,
}

pub fn engine_child_factory(config: Option<EngineSpawnConfig>) -> ChildFactory {
    Arc::new(move || {
        let config = config.clone();
        Box::pin(async move {
            let config = match config {
                Some(config) => config,
                None => EngineSpawnConfig::new(default_engine_path()?),
            };
            spawn_engine(&config)
        })
    })
}

pub fn acquiring_engine_child_factory(options: AcquiringEngineChildOptions) -> ChildFactory {
    Arc::new(move || {
        let version = options.version.clone();
        let host = options.host.clone();
        let acquire = options.acquire.clone();
        let spawn_engine_fn = options.spawn_engine.clone();

        Box::pin(async move {
            let acquired = acquire(version, host).await?;
            let engine_path = acquired.path.ok_or_else(|| {
                DesktopServiceError::closed(acquired.diagnostic_message.unwrap_or_default())
            })?;

            if let Some(ref sp) = spawn_engine_fn {
                sp(engine_path)
            } else {
                let cmd_cfg = EngineSpawnConfig::new(engine_path);
                spawn_engine(&cmd_cfg)
            }
        })
    })
}

pub fn spawn_engine(config: &EngineSpawnConfig) -> Result<EngineChild, DesktopServiceError> {
    let mut cmd = Command::new(&config.binary_path);
    cmd.args(&config.args);
    for (k, v) in &config.env {
        cmd.env(k, v);
    }
    cmd.stdin(Stdio::piped());
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());

    let child = cmd.spawn().map_err(|err| {
        DesktopServiceError::closed(format!(
            "failed to spawn desktop engine {}: {err}",
            config.binary_path.display()
        ))
    })?;

    Ok(EngineChild::new(
        config.binary_path.to_string_lossy().to_string(),
        child,
    ))
}

pub struct EngineChild {
    pub path: String,
    on_kill: Option<Arc<dyn Fn() + Send + Sync>>,
    alive: Arc<AtomicBool>,
    stdin_tx: mpsc::UnboundedSender<String>,
    stdin_close_tx: Mutex<Option<oneshot::Sender<()>>>,
    kill_tx: Mutex<Option<oneshot::Sender<()>>>,
    line_rx: Mutex<Option<mpsc::UnboundedReceiver<String>>>,
    exit_rx: Mutex<Option<oneshot::Receiver<String>>>,
    exited_rx: Arc<Mutex<Option<oneshot::Receiver<()>>>>,
}

impl EngineChild {
    pub fn new(path: String, child: Child) -> Self {
        Self::new_with_hook(path, child, None)
    }

    pub fn new_with_hook(path: String, mut child: Child, on_kill: Option<Arc<dyn Fn() + Send + Sync>>) -> Self {
        let alive = Arc::new(AtomicBool::new(true));
        let (stdin_tx, mut stdin_rx) = mpsc::unbounded_channel::<String>();
        let (stdin_close_tx, stdin_close_rx) = oneshot::channel::<()>();
        let (kill_tx, kill_rx) = oneshot::channel::<()>();
        let (line_tx, line_rx) = mpsc::unbounded_channel::<String>();
        let (exit_tx, exit_rx) = oneshot::channel::<String>();
        let (exited_tx, exited_rx) = oneshot::channel::<()>();

        let mut stdin = child.stdin.take();
        let stdout = child.stdout.take().expect("piped stdout");
        let stderr = child.stderr.take().expect("piped stderr");

        let alive_clone = Arc::clone(&alive);
        let stderr_tail = Arc::new(Mutex::new(String::new()));
        let tail_clone = Arc::clone(&stderr_tail);

        // Stdout lines task
        tokio::spawn(async move {
            let mut reader = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = reader.next_line().await {
                if line_tx.send(line).is_err() {
                    break;
                }
            }
        });

        // Stderr drainer task
        let stderr_task = tokio::spawn(async move {
            let mut reader = BufReader::new(stderr);
            let mut buf = [0u8; 1024];
            while let Ok(n) = reader.read(&mut buf).await {
                if n == 0 {
                    break;
                }
                let chunk = String::from_utf8_lossy(&buf[..n]);
                let mut tail = tail_clone.lock().await;
                tail.push_str(&chunk);
                if tail.len() > STDERR_TAIL_CHARS {
                    let mut start = tail.len() - STDERR_TAIL_CHARS;
                    while !tail.is_char_boundary(start) {
                        start += 1;
                    }
                    *tail = tail[start..].to_string();
                }
            }
        });

        // Process supervisor task
        tokio::spawn(async move {
            let mut stdin_close_rx = stdin_close_rx;
            let mut kill_rx = kill_rx;
            let mut failure = String::new();

            loop {
                tokio::select! {
                    msg = stdin_rx.recv(), if stdin.is_some() => {
                        match msg {
                            Some(data) => {
                                if let Err(e) = stdin.as_mut().expect("open stdin").write_all(data.as_bytes()).await {
                                    failure = format!("stdin: {e}");
                                    stdin.take();
                                    continue;
                                }
                                let _ = stdin.as_mut().expect("open stdin").flush().await;
                            }
                            None => { stdin.take(); },
                        }
                    }
                    _ = &mut stdin_close_rx, if stdin.is_some() => {
                        stdin.take();
                    }
                    _ = &mut kill_rx => {
                        let _ = child.kill().await;
                        break;
                    }
                    _ = child.wait() => {
                        break;
                    }
                }
            }

            let exit_status = child.wait().await;
            let _ = stderr_task.await;
            alive_clone.store(false, Ordering::SeqCst);

            let tail = stderr_tail.lock().await.trim().to_string();
            let stderr_str = if tail.is_empty() { "<empty>" } else { &tail };
            let failure_part = if failure.is_empty() {
                String::new()
            } else {
                format!("; {failure}")
            };

            let reason = match exit_status {
                Ok(status) => format!(
                    "desktop engine exited ({status}){failure_part}; stderr: {stderr_str}"
                ),
                Err(err) => format!(
                    "desktop engine exited with error: {err}{failure_part}; stderr: {stderr_str}"
                ),
            };

            let _ = exit_tx.send(reason);
            let _ = exited_tx.send(());
        });

        Self {
            path,
            on_kill,
            alive,
            stdin_tx,
            stdin_close_tx: Mutex::new(Some(stdin_close_tx)),
            kill_tx: Mutex::new(Some(kill_tx)),
            line_rx: Mutex::new(Some(line_rx)),
            exit_rx: Mutex::new(Some(exit_rx)),
            exited_rx: Arc::new(Mutex::new(Some(exited_rx))),
        }
    }

    pub async fn take_streams(
        &self,
    ) -> (
        Option<mpsc::UnboundedReceiver<String>>,
        Option<oneshot::Receiver<String>>,
    ) {
        let line_rx = self.line_rx.lock().await.take();
        let exit_rx = self.exit_rx.lock().await.take();
        (line_rx, exit_rx)
    }

    pub fn is_alive(&self) -> bool {
        self.alive.load(Ordering::SeqCst)
    }

    pub fn write_line(&self, line: &str) -> Result<(), DesktopServiceError> {
        if !self.is_alive() {
            return Err(DesktopServiceError::closed("desktop engine child not alive".to_string()));
        }
        self.stdin_tx
            .send(line.to_string())
            .map_err(|_| DesktopServiceError::closed("stdin closed".to_string()))
    }

    pub fn end(&self) {
        if let Ok(mut g) = self.stdin_close_tx.try_lock()
            && let Some(tx) = g.take() {
                let _ = tx.send(());
        }
    }

    pub fn kill(&self) {
        if !self.alive.swap(false, Ordering::SeqCst) {
            return;
        }
        if let Some(on_kill) = &self.on_kill {
            on_kill();
        }
        if let Ok(mut g) = self.kill_tx.try_lock()
            && let Some(tx) = g.take() {
                let _ = tx.send(());
        }
    }

    pub async fn wait_exited(&self) {
        let rx = {
            let mut g = self.exited_rx.lock().await;
            g.take()
        };
        if let Some(rx) = rx {
            let _ = rx.await;
        }
    }
}
