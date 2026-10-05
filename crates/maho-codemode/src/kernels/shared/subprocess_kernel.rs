use super::{subprocess_contract::{KernelRunInput, SubprocessKernelOptions}, subprocess_process::{ProcessError, SubprocessProcess}, subprocess_queue::SubprocessRunQueue, subprocess_run::{KernelMessageCallback, KernelStartedCallback, failure_result, timeout_result, settle_pending_run}};
use crate::kernels::session_env::apply_session_environment;
use serde_json::{Value, json};
use std::{sync::{Arc, Mutex}, time::Duration};
use tokio::sync::{mpsc, oneshot};

type QueueSnapshot = (Option<String>, Vec<String>);
enum Command {
    Run(KernelRunInput, Option<KernelMessageCallback>, Option<KernelStartedCallback>, oneshot::Sender<Value>),
    Cancel(String, String, oneshot::Sender<bool>),
    Interrupt(String, Option<String>, oneshot::Sender<Result<bool, String>>),
    Reply(Value),
    NextToolCall(oneshot::Sender<oneshot::Receiver<Value>>),
    Reset(oneshot::Sender<Result<(), String>>),
    Close(oneshot::Sender<Result<(), String>>),
}

pub struct SubprocessKernel {
    commands: mpsc::UnboundedSender<Command>,
    snapshot: Arc<Mutex<QueueSnapshot>>,
    pid: Arc<Mutex<Option<u32>>>,
    actor: tokio::sync::Mutex<Option<tokio::task::JoinHandle<()>>>,
    shutdown: maho_ai::utils::abort::AbortController,
}

impl SubprocessKernel {
    pub async fn start(options: SubprocessKernelOptions) -> Result<Self, ProcessError> {
        Self::start_with_signal(options,&maho_ai::utils::abort::AbortController::new().signal()).await
    }
    pub async fn start_with_signal(options: SubprocessKernelOptions,signal:&maho_ai::utils::abort::AbortSignal) -> Result<Self, ProcessError> {
        let process = spawn_process_with_signal(&options,signal).await?;
        let shutdown=maho_ai::utils::abort::AbortController::new();let cancel=shutdown.clone();
        let listener=signal.add_abort_listener(move |reason|cancel.abort(Some(reason.clone())));
        if signal.aborted() {shutdown.abort(signal.reason());}
        let pid = Arc::new(Mutex::new(process.pid()));
        let snapshot = Arc::new(Mutex::new((None, vec![])));
        let (commands, receiver) = mpsc::unbounded_channel();
        let owner=signal.clone();let actor_signal=shutdown.signal();let actor_snapshot=snapshot.clone();let actor_pid=pid.clone();
        let actor=tokio::spawn(async move {run_actor(options, process, receiver, actor_snapshot, actor_pid,actor_signal).await;owner.remove_abort_listener(listener);});
        Ok(Self { commands, snapshot, pid,shutdown,actor:tokio::sync::Mutex::new(Some(actor)) })
    }

    pub fn pid(&self) -> Option<u32> { *self.pid.lock().expect("kernel pid lock") }
    pub fn queue_snapshot(&self) -> QueueSnapshot { self.snapshot.lock().expect("kernel queue lock").clone() }

    pub async fn run(&self, input: KernelRunInput, mut on_message: impl FnMut(&Value)) -> Result<Value, ProcessError> {
        let (frames, mut messages) = mpsc::unbounded_channel();
        let result = self.run_with_callbacks(input, Some(Arc::new(move |message| { let _ = frames.send(message.clone()); })), None);
        tokio::pin!(result);
        let result = loop {
            tokio::select! {
                result = &mut result => break result,
                Some(message) = messages.recv() => on_message(&message),
            }
        };
        while let Ok(message) = messages.try_recv() { on_message(&message); }
        result
    }

    pub async fn run_with_callbacks(&self, input: KernelRunInput, on_message: Option<KernelMessageCallback>, on_started: Option<KernelStartedCallback>) -> Result<Value, ProcessError> {
        let (response, receiver) = oneshot::channel();
        self.commands.send(Command::Run(input, on_message, on_started, response)).map_err(|_| ProcessError::Closed)?;
        receiver.await.map_err(|_| ProcessError::Closed)
    }

    pub async fn cancel_queued(&self, cell_id: &str, reason: &str) -> bool {
        let (response, receiver) = oneshot::channel();
        if self.commands.send(Command::Cancel(cell_id.into(), reason.into(), response)).is_err() { return false; }
        receiver.await.unwrap_or(false)
    }

    pub async fn interrupt(&self, reason: &str, cell_id: Option<&str>) -> Result<bool, String> {
        let (response, receiver) = oneshot::channel();
        self.commands.send(Command::Interrupt(reason.into(), cell_id.map(str::to_owned), response)).map_err(|_| "Kernel is closed".to_string())?;
        receiver.await.map_err(|_| "Kernel interrupt outcome unavailable".to_string())?
    }

    pub fn deliver_tool_reply(&self, message: Value) -> Result<(), String> {
        self.commands.send(Command::Reply(message)).map_err(|_| "Kernel is closed".into())
    }

    pub async fn next_tool_call(&self) -> Result<Value, ProcessError> {
        let (response,receiver)=oneshot::channel();
        self.commands.send(Command::NextToolCall(response)).map_err(|_|ProcessError::Closed)?;
        receiver.await.map_err(|_|ProcessError::Closed)?.await.map_err(|_|ProcessError::Closed)
    }

    pub async fn reset(&self) -> Result<(), ProcessError> {
        let (response, receiver) = oneshot::channel();
        self.commands.send(Command::Reset(response)).map_err(|_| ProcessError::Closed)?;
        receiver.await.map_err(|_| ProcessError::Closed)?.map_err(ProcessError::Startup)
    }

    pub async fn close(&self) -> Result<(), ProcessError> {
        self.shutdown.abort(None);
        let (response, receiver) = oneshot::channel();
        let result=if self.commands.send(Command::Close(response)).is_err() {Ok(())} else {match receiver.await {Ok(result)=>result.map_err(ProcessError::Startup),Err(_)=>Err(ProcessError::Closed)}};
        if let Some(actor)=self.actor.lock().await.take() {actor.await.map_err(|error|ProcessError::Startup(error.to_string()))?;}
        result
    }
}

impl Drop for SubprocessKernel {
    fn drop(&mut self) {self.shutdown.abort(None);}
}

async fn spawn_process_with_signal(options: &SubprocessKernelOptions, signal:&maho_ai::utils::abort::AbortSignal) -> Result<SubprocessProcess, ProcessError> {
    let options=options.clone();let owner=signal.clone();
    let shutdown=maho_ai::utils::abort::AbortController::new();let cancel=shutdown.clone();
    let listener=signal.add_abort_listener(move |reason|cancel.abort(Some(reason.clone())));
    if signal.aborted() {shutdown.abort(signal.reason());}
    let (mut sender,receiver)=oneshot::channel();
    tokio::spawn(async move {
        let signal=shutdown.signal();
        let result={let startup=spawn_owned_process(&options,&signal);tokio::pin!(startup);tokio::select! {result=&mut startup=>result,()=sender.closed()=>{shutdown.abort(None);startup.await}}};
        owner.remove_abort_listener(listener);
        if let Err(Ok(mut process))=sender.send(result) {let _=process.terminate("TERM",Duration::from_millis(1500)).await;}
    });
    receiver.await.map_err(|_|ProcessError::Closed)?
}
async fn spawn_owned_process(options: &SubprocessKernelOptions, signal:&maho_ai::utils::abort::AbortSignal) -> Result<SubprocessProcess, ProcessError> {
    if signal.aborted() {return Err(ProcessError::Startup("Kernel startup was cancelled".into()));}
    let inherited = std::env::vars().collect();
    let env = options.env.clone().unwrap_or_else(|| apply_session_environment(&inherited, options.session_env.as_ref()));
    let mut process = SubprocessProcess::spawn(&options.command, &options.args, &options.cwd, &env)?;
    if let Err(error)=process.send(&json!({"type":"init","sessionId":options.session_id,"connection":options.connection})).await {process.terminate("TERM",Duration::from_millis(1500)).await?;return Err(error);}
    let startup = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let message = process.next_message().await?;
            if let Some(callback) = &options.on_message { callback(&message); }
            match message["type"].as_str() {
                Some("ready") => return Ok(()),
                Some("init-failed") => return Err(ProcessError::Startup(message["error"]["message"].as_str().unwrap_or("initialization failed").into())),
                _ => {}
            }
        }
    });
    let ready=tokio::select! {
        ()=signal.cancelled()=>Ok(Err(ProcessError::Startup("Kernel startup was cancelled".into()))),
        ready=startup=>ready,
    };
    match ready {
        Ok(Ok(())) => Ok(process),
        result => {
            process.terminate("TERM", Duration::from_millis(1500)).await?;
            match result { Ok(Err(error)) => Err(error), _ => Err(ProcessError::Startup("Kernel did not become ready".into())) }
        }
    }
}

async fn replace_process(options: &SubprocessKernelOptions, process: &mut SubprocessProcess, signal: &str, grace: Duration,shutdown:&maho_ai::utils::abort::AbortSignal) -> Result<(), ProcessError> {
    process.terminate(signal, grace).await?;
    *process = spawn_process_with_signal(options,shutdown).await?;
    Ok(())
}

async fn run_actor(options: SubprocessKernelOptions, mut process: SubprocessProcess, mut commands: mpsc::UnboundedReceiver<Command>, snapshot: Arc<Mutex<QueueSnapshot>>, pid: Arc<Mutex<Option<u32>>>,shutdown:maho_ai::utils::abort::AbortSignal) {
    let mut runs = SubprocessRunQueue::default();
    let origin = tokio::time::Instant::now();
    let now = || origin.elapsed().as_secs_f64() * 1000.0;
    let mut deadline = None;
    let mut failure: Option<String> = None;
    loop {
        if runs.active().is_none() && failure.is_none() && let Some(run) = runs.start_next(now()) {
            deadline = run.input.timeout_ms.map(|ms| tokio::time::Instant::now() + Duration::from_millis(ms));
            let mut frame = json!({"type":"run","cellId":run.input.cell_id,"code":run.input.code});
            if let Some(ms) = run.input.timeout_ms { frame["timeoutMs"] = json!(ms); }
            if let Err(error) = process.send(&frame).await { failure = Some(error.to_string()); }
        }
        *snapshot.lock().expect("kernel queue lock") = runs.snapshot();
        *pid.lock().expect("kernel pid lock") = if failure.is_none() { process.pid() } else { None };
        if let Some(error) = &failure {
            runs.clear_tool_calls();
            deadline = None;
            if !process.is_retiring() && let Err(error) = process.terminate("TERM", Duration::from_millis(1500)).await { eprintln!("kernel retirement failed: {error}"); }
            runs.settle_all(error, now());
        }
        tokio::select! {
            command = commands.recv() => match command {
                Some(Command::Run(_, _, _, response)) if failure.is_some() => { drop(response); }
                Some(Command::Run(input, on_message, on_started, response)) => {
                    let mut result = runs.enqueue(input, on_message, on_started);
                    tokio::spawn(async move { if let Ok(result) = (&mut result).await { let _ = response.send(result); } });
                }
                Some(Command::Interrupt(_, _, response)) if failure.is_some() => { let _ = response.send(Ok(true)); }
                Some(Command::Cancel(id, reason, response)) => { let _ = response.send(runs.remove(&id, &reason, now())); }
                Some(Command::Interrupt(reason, id, response)) => {
                    if id.as_ref().is_some_and(|id| runs.active().is_none_or(|run| &run.input.cell_id != id)) {
                        if let Some(id) = id { runs.remove(&id, &reason, now()); }
                        let _ = response.send(Ok(true));
                    } else if let Some(mut run) = runs.release_active() {
                        let result = failure_result(&run, &format!("Cell interrupted: {reason}"), now());
                        settle_pending_run(&mut run, result);
                        runs.clear_tool_calls(); deadline = None;
                        let signal = if cfg!(windows) { "TERM" } else { "INT" };
                        let result = replace_process(&options, &mut process, signal, Duration::from_secs(5),&shutdown).await.map_err(|error| error.to_string());
                        failure = result.as_ref().err().cloned();
                        let _ = response.send(result.map(|()| false));
                    } else { runs.settle_all(&format!("Cell interrupted: {reason}"), now()); let _ = response.send(Ok(true)); }
                }
                Some(Command::Reply(_)) if failure.is_some() => {}
                Some(Command::Reply(message)) => { if let Err(error) = process.send(&message).await { failure = Some(error.to_string()); } }
                Some(Command::NextToolCall(response)) => {let _=response.send(runs.next_tool_call());}
                Some(Command::Reset(response)) if failure.is_some() => { let _ = response.send(Err(ProcessError::Closed.to_string())); }
                Some(Command::Reset(response)) => {
                    runs.settle_all("Kernel reset", now()); runs.clear_tool_calls(); deadline = None;
                    let result = replace_process(&options, &mut process, "TERM", Duration::from_millis(1500),&shutdown).await.map_err(|error| error.to_string());
                    failure = result.as_ref().err().cloned(); let _ = response.send(result);
                }
                Some(Command::Close(response)) => {
                    runs.settle_all("Kernel is closing", now()); runs.clear_tool_calls();
                    let result = process.shutdown(Some(&json!({"type":"close"}))).await.map_err(|error| error.to_string());
                    *pid.lock().expect("kernel pid lock") = None;
                    let _ = response.send(result); break;
                }
                None => { runs.settle_all("Kernel is closing", now()); if let Err(error) = process.shutdown(Some(&json!({"type":"close"}))).await { eprintln!("kernel close failed: {error}"); } break; }
            },
            message = process.next_message(), if failure.is_none() => match message {
                Ok(message) => {
                    let startup_failure = (message["type"] == "init-failed").then(|| message["error"]["message"].as_str().unwrap_or("initialization failed").to_owned());
                    if runs.handle_message(message, options.on_message.as_ref()) { deadline = None; }
                    if let Some(error) = startup_failure {
                        failure = Some(ProcessError::Startup(error).to_string());
                        runs.clear_tool_calls();
                        runs.settle_all(failure.as_deref().expect("startup failure"), now());
                        deadline = None;
                        if let Err(error) = process.terminate("TERM", Duration::from_millis(1500)).await { eprintln!("kernel retirement failed: {error}"); }
                    }
                }
                Err(error) => { failure = Some(error.to_string()); runs.clear_tool_calls(); if let Err(error) = process.terminate("TERM", Duration::from_millis(1500)).await { eprintln!("kernel retirement failed: {error}"); } }
            },
            () = async { match deadline { Some(deadline) => tokio::time::sleep_until(deadline).await, None => std::future::pending::<()>().await } }, if deadline.is_some() => {
                if let Some(mut run) = runs.release_active() {
                    let result = timeout_result(&run, run.input.timeout_ms.unwrap_or(0));
                    let replacement = replace_process(&options, &mut process, "TERM", Duration::from_millis(1500),&shutdown).await;
                    settle_pending_run(&mut run, result);
                    failure = replacement.err().map(|error| error.to_string());
                }
                runs.clear_tool_calls(); deadline = None;
            }
        }
    }
    *snapshot.lock().expect("kernel queue lock") = (None, vec![]);
    *pid.lock().expect("kernel pid lock") = None;
}

#[cfg(test)]
mod startup_ownership_tests {
    use super::*;
    #[cfg(unix)]
    #[tokio::test]
    async fn dropped_shared_startup_retires_after_started_event() {
        use tokio::io::AsyncReadExt;
        let root=tempfile::tempdir().unwrap();let socket=root.path().join("startup.sock");
        let listener=tokio::net::UnixListener::bind(&socket).unwrap();
        let code=format!("import socket,signal\ns=socket.socket(socket.AF_UNIX)\ns.connect({})\ns.sendall(b'STARTED')\nwhile True: signal.pause()\n",serde_json::to_string(&socket.to_string_lossy()).unwrap());
        let options=SubprocessKernelOptions {command:"python3".into(),args:vec!["-c".into(),code],cwd:root.path().into(),env:None,session_env:None,session_id:"shared-drop".into(),connection:crate::bridge::protocol::BridgeConnectionConfig {port:1,token:"test".into(),local_roots:None,artifacts_dir:None,parallel_pool_width:None},on_message:None};
        let signal=maho_ai::utils::abort::AbortController::new().signal();
        let mut startup=Box::pin(spawn_process_with_signal(&options,&signal));
        let accepted=tokio::time::timeout(Duration::from_secs(5),async {tokio::select! {stream=listener.accept()=>stream.unwrap().0,result=&mut startup=>panic!("silent startup settled: {}",result.err().unwrap())}}).await;
        drop(startup);
        let mut stream=accepted.unwrap();let mut bytes=Vec::new();
        tokio::time::timeout(Duration::from_secs(5),stream.read_to_end(&mut bytes)).await.unwrap().unwrap();
        assert_eq!(bytes,b"STARTED");
    }
}
