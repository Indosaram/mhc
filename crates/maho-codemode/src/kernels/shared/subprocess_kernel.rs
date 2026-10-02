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
}

impl SubprocessKernel {
    pub async fn start(options: SubprocessKernelOptions) -> Result<Self, ProcessError> {
        let process = spawn_process(&options).await?;
        let pid = Arc::new(Mutex::new(process.pid()));
        let snapshot = Arc::new(Mutex::new((None, vec![])));
        let (commands, receiver) = mpsc::unbounded_channel();
        tokio::spawn(run_actor(options, process, receiver, snapshot.clone(), pid.clone()));
        Ok(Self { commands, snapshot, pid })
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
        let (response, receiver) = oneshot::channel();
        if self.commands.send(Command::Close(response)).is_err() { return Ok(()); }
        receiver.await.map_err(|_| ProcessError::Closed)?.map_err(ProcessError::Startup)
    }
}

async fn spawn_process(options: &SubprocessKernelOptions) -> Result<SubprocessProcess, ProcessError> {
    let inherited = std::env::vars().collect();
    let env = options.env.clone().unwrap_or_else(|| apply_session_environment(&inherited, options.session_env.as_ref()));
    let mut process = SubprocessProcess::spawn(&options.command, &options.args, &options.cwd, &env)?;
    process.send(&json!({"type":"init","sessionId":options.session_id,"connection":options.connection})).await?;
    let ready = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let message = process.next_message().await?;
            match message["type"].as_str() {
                Some("ready") => return Ok(()),
                Some("init-failed") => return Err(ProcessError::Startup(message["error"]["message"].as_str().unwrap_or("initialization failed").into())),
                _ => {}
            }
        }
    }).await;
    match ready {
        Ok(Ok(())) => Ok(process),
        result => {
            process.terminate("TERM", Duration::from_millis(1500)).await?;
            match result { Ok(Err(error)) => Err(error), _ => Err(ProcessError::Startup("Kernel did not become ready".into())) }
        }
    }
}

async fn replace_process(options: &SubprocessKernelOptions, process: &mut SubprocessProcess, signal: &str, grace: Duration) -> Result<(), ProcessError> {
    process.terminate(signal, grace).await?;
    *process = spawn_process(options).await?;
    Ok(())
}

async fn run_actor(options: SubprocessKernelOptions, mut process: SubprocessProcess, mut commands: mpsc::UnboundedReceiver<Command>, snapshot: Arc<Mutex<QueueSnapshot>>, pid: Arc<Mutex<Option<u32>>>) {
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
        if let Some(error) = &failure { runs.settle_all(error, now()); }
        tokio::select! {
            command = commands.recv() => match command {
                Some(Command::Run(input, on_message, on_started, response)) => {
                    let mut result = runs.enqueue(input, on_message, on_started);
                    if let Some(error) = &failure { runs.settle_all(error, now()); }
                    tokio::spawn(async move { if let Ok(result) = (&mut result).await { let _ = response.send(result); } });
                }
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
                        let result = replace_process(&options, &mut process, signal, Duration::from_secs(5)).await.map_err(|error| error.to_string());
                        failure = result.as_ref().err().cloned();
                        let _ = response.send(result.map(|()| false));
                    } else { runs.settle_all(&format!("Cell interrupted: {reason}"), now()); let _ = response.send(Ok(true)); }
                }
                Some(Command::Reply(message)) => { if let Err(error) = process.send(&message).await { failure = Some(error.to_string()); } }
                Some(Command::NextToolCall(response)) => {let _=response.send(runs.next_tool_call());}
                Some(Command::Reset(response)) => {
                    runs.settle_all("Kernel reset", now()); runs.clear_tool_calls(); deadline = None;
                    let result = replace_process(&options, &mut process, "TERM", Duration::from_millis(1500)).await.map_err(|error| error.to_string());
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
                Ok(message) => { if runs.handle_message(message, None) { deadline = None; } }
                Err(error) => { failure = Some(error.to_string()); runs.clear_tool_calls(); if let Err(error) = process.terminate("TERM", Duration::from_millis(1500)).await { eprintln!("kernel retirement failed: {error}"); } }
            },
            () = async { match deadline { Some(deadline) => tokio::time::sleep_until(deadline).await, None => std::future::pending::<()>().await } }, if deadline.is_some() => {
                if let Some(mut run) = runs.release_active() {
                    let result = timeout_result(&run, run.input.timeout_ms.unwrap_or(0));
                    let replacement = replace_process(&options, &mut process, "TERM", Duration::from_millis(1500)).await;
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
