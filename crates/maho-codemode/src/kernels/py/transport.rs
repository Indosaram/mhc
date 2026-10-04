use std::{collections::HashMap, path::{Path, PathBuf}, time::Duration};
use serde_json::{Value, json};
use tokio::{io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader}, process::{Child, ChildStdin}, sync::mpsc, task::JoinHandle};
use super::process::{KernelSpawnOptions, default_spawn, hard_kill, split_command, sweep_process_group, wait_for_exit};
use crate::{bridge::protocol::BridgeConnectionConfig, kernels::{session_env::apply_session_environment, shared::runtime_asset::{CodemodeRuntimeAssetEnvironment, CodemodeRuntimeAssetMissingError, require_codemode_runtime_asset}}};

pub struct PythonPreludePathOptions<'a> {
    pub local_path: Option<&'a Path>,
    pub environment: CodemodeRuntimeAssetEnvironment<'a>,
}

#[cfg(test)]
mod startup_tests {
    use super::*;
    #[cfg(unix)]
    #[tokio::test]
    async fn dropped_startup_signals_owned_producer_to_retire_process() {
        let root=tempfile::tempdir().unwrap();let socket=root.path().join("started.sock");
        let listener=tokio::net::UnixListener::bind(&socket).unwrap();
        let prelude=root.path().join("silent.py");
        std::fs::write(&prelude,format!("import socket, signal\ns=socket.socket(socket.AF_UNIX)\ns.connect({})\ns.sendall(b'STARTED')\nwhile True: signal.pause()\n",serde_json::to_string(&socket.to_string_lossy()).unwrap())).unwrap();
        let options=PythonTransportOptions {interpreter_path:"python3".into(),session_id:"drop-startup".into(),cwd:root.path().into(),connection:BridgeConnectionConfig {port:1,token:"test".into(),local_roots:None,artifacts_dir:None,parallel_pool_width:None},env:None,session_env:None,startup_timeout:Duration::from_secs(30)};
        let mut startup=Box::pin(PythonKernelTransport::start(&options,&prelude,||true));
        let accepted=tokio::time::timeout(Duration::from_secs(5),async {tokio::select! {stream=listener.accept()=>stream.unwrap().0,result=&mut startup=>panic!("silent worker unexpectedly settled: {}",result.err().unwrap())}}).await;
        drop(startup);
        let mut stream=accepted.unwrap();let mut bytes=Vec::new();
        tokio::time::timeout(Duration::from_secs(5),stream.read_to_end(&mut bytes)).await.unwrap().unwrap();
        assert_eq!(bytes,b"STARTED");
    }
    #[tokio::test]
    async fn pre_cancelled_owned_startup_rejects_admission() {
        let root=tempfile::tempdir().unwrap();let prelude=root.path().join("silent.py");
        std::fs::write(&prelude,"import signal\nwhile True: signal.pause()\n").unwrap();
        let options=PythonTransportOptions {interpreter_path:"python3".into(),session_id:"silent".into(),cwd:root.path().into(),connection:BridgeConnectionConfig {port:1,token:"test".into(),local_roots:None,artifacts_dir:None,parallel_pool_width:None},env:None,session_env:None,startup_timeout:Duration::from_secs(5)};
        let shutdown=maho_ai::utils::abort::AbortController::new();shutdown.abort(None);
        assert!(PythonKernelTransport::start_with_signal(&options,&prelude,||true,&shutdown.signal()).await.is_err());
    }
}

pub fn resolve_python_prelude_path(options: PythonPreludePathOptions<'_>) -> Result<PathBuf, CodemodeRuntimeAssetMissingError> {
    require_codemode_runtime_asset(options.local_path.unwrap_or(Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/kernels/py/prelude.py"))), Path::new("kernels/py/prelude.py"), &options.environment)
}

pub fn failed_python_result(cell_id: &str, message: &str, stack: Option<&str>) -> Value {
    let mut result = json!({"type":"result","cellId":cell_id,"ok":false,"error":{"message":message},"durationMs":0});
    if let Some(stack) = stack.filter(|stack| !stack.is_empty()) { result["error"]["stack"] = json!(stack); }
    result
}

#[derive(Clone)]
pub struct PythonTransportOptions {
    pub interpreter_path: String,
    pub session_id: String,
    pub cwd: PathBuf,
    pub connection: BridgeConnectionConfig,
    pub env: Option<HashMap<String, String>>,
    pub session_env: Option<HashMap<String, String>>,
    pub startup_timeout: Duration,
}

pub fn spawn_python_transport(options: &PythonTransportOptions, prelude: &Path) -> Result<tokio::process::Child, std::io::Error> {
    let (command, mut args) = split_command(&options.interpreter_path).map_err(|message| std::io::Error::new(std::io::ErrorKind::InvalidInput, message))?;
    args.extend(["-u".into(), prelude.to_string_lossy().into_owned()]);
    let mut env = apply_session_environment(&std::env::vars().collect(), options.session_env.as_ref());
    if let Some(overrides) = &options.env { env.extend(overrides.clone()); }
    env.insert("PYTHONUNBUFFERED".into(), "1".into());
    env.insert("PYTHONIOENCODING".into(), "utf-8".into());
    default_spawn(&KernelSpawnOptions { command, args, cwd: options.cwd.clone(), env })
}

#[derive(Debug, thiserror::Error)]
pub enum PythonTransportError {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Retirement(#[from] super::process::PythonKernelRetirementError),
    #[error("{0}")]
    Startup(String),
    #[error("Python kernel is closed")]
    Closed,
}

pub struct PythonKernelTransport {
    child: Child,
    input: ChildStdin,
    messages: mpsc::Receiver<Value>,
    readers: Vec<JoinHandle<()>>,
    active: bool,
}

impl PythonKernelTransport {
    pub async fn start(options: &PythonTransportOptions, prelude: &Path, is_owned: impl Fn() -> bool) -> Result<Self, PythonTransportError> {
        Self::start_with_signal(options,prelude,is_owned,&maho_ai::utils::abort::AbortController::new().signal()).await
    }
    pub async fn start_with_signal(options: &PythonTransportOptions, prelude: &Path, is_owned: impl Fn() -> bool, signal:&maho_ai::utils::abort::AbortSignal) -> Result<Self, PythonTransportError> {
        let options=options.clone();let prelude=prelude.to_owned();
        let shutdown=maho_ai::utils::abort::AbortController::new();let cancel=shutdown.clone();
        let listener=signal.add_abort_listener(move |reason|cancel.abort(Some(reason.clone())));
        if signal.aborted() {shutdown.abort(signal.reason());}
        let owner=signal.clone();let (mut sender,receiver)=tokio::sync::oneshot::channel();
        tokio::spawn(async move {
            let signal=shutdown.signal();
            let result={
                let startup=Self::start_owned(&options,&prelude,&signal);tokio::pin!(startup);
                tokio::select! {result=&mut startup=>result,()=sender.closed()=>{shutdown.abort(None);startup.await}}
            };
            owner.remove_abort_listener(listener);
            if let Err(Ok(mut transport))=sender.send(result) {let _=transport.retire().await;}
        });
        let mut transport=receiver.await.map_err(|_|PythonTransportError::Closed)??;
        if !is_owned() {transport.retire().await?;return Err(PythonTransportError::Startup("Python kernel startup was superseded".into()));}
        Ok(transport)
    }
    async fn start_owned(options: &PythonTransportOptions, prelude: &Path, signal:&maho_ai::utils::abort::AbortSignal) -> Result<Self, PythonTransportError> {
        if signal.aborted() {return Err(PythonTransportError::Startup("Python kernel startup was cancelled".into()));}
        let mut child = spawn_python_transport(options, prelude)?;
        let input = child.stdin.take().expect("Python stdin is piped");
        let stdout = child.stdout.take().expect("Python stdout is piped");
        let mut stderr = child.stderr.take().expect("Python stderr is piped");
        let (sender, messages) = mpsc::channel(256);
        let stderr_sender = sender.clone();
        let stdout_reader = tokio::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let message = match crate::bridge::protocol::decode_bridge_frame(&line, None) {
                    Ok(message) if crate::bridge::protocol::is_kernel_to_host_message(&message) => message,
                    Ok(_) => continue,
                    Err(error) => json!({"type":"text","stream":"stderr","data":format!("{error}\n")}),
                };
                if sender.send(message).await.is_err() { return; }
            }
        });
        let stderr_reader = tokio::spawn(async move {
            let mut buffer = vec![0; 4096];
            loop {
                match stderr.read(&mut buffer).await {
                    Ok(0) | Err(_) => return,
                    Ok(bytes) => {
                        if stderr_sender.send(json!({"type":"text","stream":"stderr","data":String::from_utf8_lossy(&buffer[..bytes])})).await.is_err() { return; }
                    }
                }
            }
        });
        let mut transport = Self { child, input, messages, readers:vec![stdout_reader,stderr_reader], active:true };
        let startup = async {
            transport.write(&json!({"type":"init","sessionId":options.session_id,"connection":options.connection})).await?;
            loop {
                let message = transport.next_message().await?;
                match message["type"].as_str() {
                    Some("ready") => break,
                    Some("init-failed") => return Err(PythonTransportError::Startup(message["error"]["message"].as_str().unwrap_or("Python kernel initialization failed").into())),
                    _ => {}
                }
            }
            Ok(())
        };
        let ready = tokio::select! {
            ()=signal.cancelled()=>Err(PythonTransportError::Startup("Python kernel startup was cancelled".into())),
            ready=tokio::time::timeout(options.startup_timeout, startup)=>ready.unwrap_or_else(|_| Err(PythonTransportError::Startup("Python kernel did not become ready".into()))),
        };
        if let Err(error) = ready { transport.retire().await?; return Err(error); }
        Ok(transport)
    }

    pub fn pid(&self) -> Option<u32> { self.child.id() }

    async fn write(&mut self, message: &Value) -> Result<(), PythonTransportError> {
        self.input.write_all(crate::bridge::protocol::encode_bridge_frame(message)?.as_bytes()).await?;
        self.input.flush().await?;
        Ok(())
    }

    pub async fn run(&mut self, cell_id: &str, code: &str, timeout_ms: Option<u64>) -> Result<(), PythonTransportError> {
        if !self.active { return Err(PythonTransportError::Closed); }
        let mut frame = json!({"type":"run","cellId":cell_id,"code":code});
        if let Some(timeout_ms) = timeout_ms { frame["timeoutMs"] = json!(timeout_ms); }
        self.write(&frame).await
    }

    pub async fn next_message(&mut self) -> Result<Value, PythonTransportError> {
        self.messages.recv().await.ok_or_else(|| PythonTransportError::Startup("Python kernel exited (unknown)".into()))
    }

    pub async fn interrupt(&mut self, reason: &str) -> Result<(), PythonTransportError> {
        let write_result = self.write(&json!({"type":"interrupt","reason":reason})).await;
        #[cfg(unix)]
        if let Some(pid) = self.child.id() {
            let status = tokio::process::Command::new("kill").args(["-INT", "--", &pid.to_string()])
                .stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).status().await?;
            if !status.success() { return Err(PythonTransportError::Closed); }
        }
        #[cfg(not(unix))]
        self.child.start_kill()?;
        write_result
    }

    pub async fn retire(&mut self) -> Result<(), PythonTransportError> {
        self.active = false;
        let result = hard_kill(&mut self.child, Duration::from_millis(500)).await;
        for reader in &self.readers { reader.abort(); }
        for reader in self.readers.drain(..) {let _=reader.await;}
        result?;
        Ok(())
    }

    pub async fn close(&mut self) -> Result<(), PythonTransportError> {
        if !self.active { return self.retire().await; }
        self.active = false;
        let pid = self.child.id();
        let _ = self.write(&json!({"type":"close"})).await;
        let result = async {
            if wait_for_exit(&mut self.child, Duration::from_millis(500)).await? {
                sweep_process_group(pid).await;
            } else { hard_kill(&mut self.child, Duration::from_millis(500)).await?; }
            Ok::<(), PythonTransportError>(())
        }.await;
        for reader in &self.readers { reader.abort(); }
        for reader in self.readers.drain(..) {let _=reader.await;}
        result
    }
}

impl Drop for PythonKernelTransport {
    fn drop(&mut self) { for reader in &self.readers { reader.abort(); } }
}
