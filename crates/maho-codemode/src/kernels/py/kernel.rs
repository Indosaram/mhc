use std::{collections::VecDeque, sync::{Arc, Mutex}, time::Duration};
use serde_json::Value;
use tokio::sync::{mpsc, oneshot};
use crate::kernels::shared::kernel_prelude_plan::KernelPreludeTracker;
use super::{kernel_contract::{PendingRun, PythonKernelRunOptions, PythonKernelStartOptions}, transport::{PythonKernelTransport, PythonTransportOptions, failed_python_result}};

type QueueSnapshot = (Option<String>, Vec<String>);

enum Command {
    Run(PythonKernelRunOptions, oneshot::Sender<Result<Value, String>>),
    Cancel(String, String, oneshot::Sender<bool>),
    Interrupt(String, Option<String>, oneshot::Sender<bool>),
    Reset(oneshot::Sender<Result<(), String>>),
    Close(oneshot::Sender<Result<(), String>>),
}

pub struct PythonKernel {
    commands: mpsc::UnboundedSender<Command>,
    snapshot: Arc<Mutex<QueueSnapshot>>,
    actor: tokio::sync::Mutex<Option<tokio::task::JoinHandle<()>>>,
    shutdown: maho_ai::utils::abort::AbortController,
}

impl PythonKernel {
    pub async fn start(options: PythonKernelStartOptions) -> Result<Self, String> {
        Self::start_with_signal(options,&maho_ai::utils::abort::AbortController::new().signal()).await
    }
    pub async fn start_with_signal(options: PythonKernelStartOptions,signal:&maho_ai::utils::abort::AbortSignal) -> Result<Self, String> {
        let transport_options = PythonTransportOptions { interpreter_path:options.interpreter_path,session_id:options.session_id,cwd:options.cwd,connection:options.connection,env:options.env,session_env:options.session_env,startup_timeout:options.startup_timeout.unwrap_or(Duration::from_secs(5)) };
        let transport = PythonKernelTransport::start_with_signal(&transport_options,&python_prelude_path()?,||true,signal).await.map_err(|error|error.to_string())?;
        let shutdown=maho_ai::utils::abort::AbortController::new();let cancel=shutdown.clone();
        let listener=signal.add_abort_listener(move |reason|cancel.abort(Some(reason.clone())));
        if signal.aborted() {shutdown.abort(signal.reason());}
        let (commands, receiver) = mpsc::unbounded_channel();
        let snapshot = Arc::new(Mutex::new((None,Vec::new())));
        let actor_snapshot = Arc::clone(&snapshot);
        let owner=signal.clone();let actor_signal=shutdown.signal();
        let actor=tokio::spawn(async move {run_actor(transport_options,transport,receiver,actor_snapshot,options.on_message,actor_signal).await;owner.remove_abort_listener(listener);});
        Ok(Self { commands, snapshot, shutdown,actor:tokio::sync::Mutex::new(Some(actor)) })
    }

    pub async fn run(&self, input: PythonKernelRunOptions) -> Result<Value, String> {
        let (sender, receiver)=oneshot::channel();
        self.commands.send(Command::Run(input,sender)).map_err(|_| "Python kernel is closed".to_string())?;
        receiver.await.map_err(|_| "Python kernel is closed".to_string())?
    }

    pub fn queue_snapshot(&self) -> QueueSnapshot { self.snapshot.lock().expect("Python queue poisoned").clone() }

    pub async fn cancel_queued(&self, cell_id: &str, reason: &str) -> bool {
        let (sender,receiver)=oneshot::channel();
        if self.commands.send(Command::Cancel(cell_id.into(),reason.into(),sender)).is_err() { return false; }
        receiver.await.unwrap_or(false)
    }

    pub async fn interrupt(&self, reason: &str, cell_id: Option<&str>) -> Result<bool, String> {
        let (sender,receiver)=oneshot::channel();
        self.commands.send(Command::Interrupt(reason.into(),cell_id.map(str::to_owned),sender)).map_err(|_| "Python kernel is closed".to_string())?;
        receiver.await.map_err(|_| "Python interrupt outcome unavailable".to_string())
    }

    pub async fn reset(&self) -> Result<(), String> {
        let (sender,receiver)=oneshot::channel();
        self.commands.send(Command::Reset(sender)).map_err(|_| "Python kernel is closed".to_string())?;
        receiver.await.map_err(|_| "Python kernel is closed".to_string())?
    }

    pub async fn close(&self) -> Result<(), String> {
        self.shutdown.abort(None);
        let (sender,receiver)=oneshot::channel();
        let result=if self.commands.send(Command::Close(sender)).is_err() {Ok(())} else {receiver.await.map_err(|_| "Python kernel close failed".to_string()).and_then(|result|result)};
        if let Some(actor)=self.actor.lock().await.take() {actor.await.map_err(|error|error.to_string())?;}
        result
    }
}

impl Drop for PythonKernel {
    fn drop(&mut self) {self.shutdown.abort(None);}
}

async fn start_transport(options: &PythonTransportOptions,signal:&maho_ai::utils::abort::AbortSignal) -> Result<PythonKernelTransport,String> {
    PythonKernelTransport::start_with_signal(options,&python_prelude_path()?,||true,signal).await.map_err(|error|error.to_string())
}

fn python_prelude_path() -> Result<std::path::PathBuf,String> {
    let executable=std::env::current_exe().map_err(|error|error.to_string())?;
    super::transport::resolve_python_prelude_path(super::transport::PythonPreludePathOptions {local_path:None,environment:crate::kernels::shared::runtime_asset::CodemodeRuntimeAssetEnvironment {bun_version:None,executable_path:&executable}}).map_err(|error|error.to_string())
}

fn settle(mut run: PendingRun, mut result: Value, retained: bool) {
    let duration=run.started_at.map_or(0.0,|started|started.elapsed().as_secs_f64()*1000.0);
    if let Some(reason)=run.interrupt_reason {
        let message=if reason=="Eval interrupted" {reason} else {format!("Eval interrupted: {reason}")};
        let stack=result["error"]["stack"].as_str().map(str::to_owned);
        result=failed_python_result(&run.input.cell_id,&message,stack.as_deref());
        result["durationMs"]=serde_json::json!(duration);
    } else if result["durationMs"]==0 { result["durationMs"]=serde_json::json!(duration); }
    if let Some(sender)=run.resolve_state_retained.take() { let _=sender.send(retained); }
    let _=run.resolve.send(Ok(result));
}

fn settle_all(active: &mut Option<PendingRun>, queue: &mut VecDeque<PendingRun>, message: &str) {
    for run in active.take().into_iter().chain(queue.drain(..)) {
        let result=failed_python_result(&run.input.cell_id,message,None);
        settle(run,result,false);
    }
}

async fn run_actor(options: PythonTransportOptions, mut transport: PythonKernelTransport, mut commands: mpsc::UnboundedReceiver<Command>, snapshot: Arc<Mutex<QueueSnapshot>>, fallback: Option<crate::kernels::shared::subprocess_run::KernelMessageCallback>,shutdown:maho_ai::utils::abort::AbortSignal) {
    let mut queue=VecDeque::<PendingRun>::new();
    let mut active:Option<PendingRun>=None;
    let mut deadline:Option<tokio::time::Instant>=None;
    let mut failure:Option<String>=None;
    let mut prelude_tracker = KernelPreludeTracker::default();
    loop {
        if active.is_none() && failure.is_none() && let Some(mut run)=queue.pop_front() {
            run.started_at=Some(tokio::time::Instant::now());
            if let Some(callback)=&run.input.on_started { callback(); }
            deadline=run.input.timeout_ms.map(|ms|tokio::time::Instant::now()+Duration::from_millis(ms));
            let plan = prelude_tracker.plan(&run.input.preludes);
            if let Err(error)=transport.run_with_preludes(&run.input.cell_id,&run.input.code,run.input.timeout_ms,Some(&plan)).await {
                let _=run.resolve.send(Err(error.to_string()));
            } else { active=Some(run); }
        }
        *snapshot.lock().expect("Python queue poisoned")=(active.as_ref().map(|run|run.input.cell_id.clone()),queue.iter().map(|run|run.input.cell_id.clone()).collect());
        tokio::select! {
            command=commands.recv()=>match command {
                Some(Command::Run(input,resolve))=>{
                    if let Some(error)=&failure {let _=resolve.send(Err(error.clone()));}
                    else {queue.push_back(PendingRun{input,resolve,started_at:None,interrupt_reason:None,resolve_state_retained:None});}
                }
                Some(Command::Cancel(id,reason,response))=>{
                    let found=queue.iter().position(|run|run.input.cell_id==id);
                    if let Some(index)=found && let Some(run)=queue.remove(index) {let result=failed_python_result(&id,&reason,None);settle(run,result,true);}
                    let _=response.send(found.is_some());
                }
                Some(Command::Interrupt(reason,id,response))=>{
                    if id.as_ref().is_some_and(|id|active.as_ref().is_none_or(|run| &run.input.cell_id!=id)) {
                        if let Some(index)=queue.iter().position(|run|Some(&run.input.cell_id)==id.as_ref()) && let Some(run)=queue.remove(index) {let result=failed_python_result(&run.input.cell_id,&reason,None);settle(run,result,true);}
                        let _=response.send(true);
                    } else {
                        if id.is_none() {for mut run in queue.drain(..) {run.interrupt_reason=Some(reason.clone());let result=failed_python_result(&run.input.cell_id,"Eval interrupted",None);settle(run,result,true);}}
                        if let Some(run)=&mut active {
                            if run.interrupt_reason.is_some() {let _=response.send(true);}
                            else {
                                run.interrupt_reason=Some(reason.clone());run.resolve_state_retained=Some(response);
                                deadline=Some(tokio::time::Instant::now()+Duration::from_secs(5));
                                if transport.interrupt(&reason).await.is_err() {deadline=Some(tokio::time::Instant::now());}
                            }
                        } else {let _=response.send(true);}
                    }
                }
                Some(Command::Reset(response))=>{
                    settle_all(&mut active,&mut queue,"Python kernel reset");deadline=None;
                    prelude_tracker = KernelPreludeTracker::default();
                    let result=match transport.retire().await {Ok(())=>match start_transport(&options,&shutdown).await {Ok(next)=>{transport=next;Ok(())},Err(error)=>Err(error)},Err(error)=>Err(error.to_string())};
                    failure=result.as_ref().err().cloned();let _=response.send(result);
                }
                Some(Command::Close(response))=>{
                    settle_all(&mut active,&mut queue,"Python kernel closed");
                    let result=transport.close().await.map_err(|error|error.to_string());let _=response.send(result);break;
                }
                None=>{settle_all(&mut active,&mut queue,"Python kernel closed");let _=transport.close().await;break;}
            },
            message=transport.next_message(), if failure.is_none()=>match message {
                Ok(message)=>{
                    if message["type"]=="result" {
                        if active.as_ref().is_some_and(|run|message["cellId"]==run.input.cell_id) && let Some(run)=active.take() {
                            deadline=None;
                            let mut message=message;
                            if message["durationMs"]==0 && let Some(started)=run.started_at {message["durationMs"]=serde_json::json!(started.elapsed().as_secs_f64()*1000.0);}
                            settle(run,message,true);
                        }
                    } else if message["type"]=="interrupted" {
                        if let Some(run)=&mut active && let Some(sender)=run.resolve_state_retained.take() {let _=sender.send(true);}
                    } else {
                        let active_callback=active.as_ref().and_then(|run|run.input.on_message.as_ref());
                        if let Some(callback)=active_callback.or(fallback.as_ref()) {callback(&message);}
                    }
                }
                Err(error)=>{
                    let failure_message=error.to_string();
                    settle_all(&mut active,&mut queue,&format!("Python kernel died: {failure_message}"));
                    let _=transport.retire().await;
                    match start_transport(&options,&shutdown).await {
                        Ok(next)=>{transport=next;failure=None;},
                        Err(restart_error)=>{failure=Some(restart_error);}
                    }
                }
            },
            _ = async { match deadline { Some(time)=>tokio::time::sleep_until(time).await, None=>std::future::pending().await } } => {
                if let Some(run)=active.take() {
                    let timed_out=run.interrupt_reason.is_none();
                    let message=if timed_out {format!("Python kernel timed out after {}ms",run.input.timeout_ms.unwrap_or_default())} else {"Eval interrupted".into()};
                    let result=failed_python_result(&run.input.cell_id,&message,None);
                    let _=transport.retire().await;
                    match start_transport(&options,&shutdown).await {
                        Ok(next)=>{transport=next;failure=None;},
                        Err(restart_error)=>{failure=Some(restart_error);}
                    }
                    deadline=None;settle(run,result,false);
                }
            }
        }
    }
    let _=transport.retire().await;
}
