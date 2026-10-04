use crate::bridge::protocol::BridgeConnectionConfig;
use crate::kernels::session_env::SessionEnvironment;
use crate::kernels::shared::{subprocess_contract::KernelRunInput, subprocess_process::ProcessError, subprocess_run::{KernelMessageCallback, KernelStartedCallback}, subprocess_queue::SubprocessRunQueue};
use serde_json::{Value, json};
use std::{path::{Path, PathBuf}, sync::{Arc, Mutex}, time::Duration};
use tokio::sync::{mpsc, oneshot};
use super::{local_module_loader::{LocalModuleLoader, LocalModuleLoaderOptions, PREPARED_CELL_PREFIX}, run_queue::{JavaScriptRunQueue, stopped_result}, worker_slot::WorkerSlot, worker_startup::WorkerStartupOptions, interrupt_bounds::{INTERRUPT_ACK_MS, JS_INTERRUPT_GRACE_MS}};

type Snapshot = (Option<String>, Vec<String>);
pub type KernelToolNames = Arc<dyn Fn()->Result<(Vec<String>,Vec<String>),String>+Send+Sync>;
enum Command {
    Run(KernelRunInput, Option<KernelMessageCallback>, Option<KernelStartedCallback>, oneshot::Sender<Result<Value, String>>),
    Cancel(String, String, oneshot::Sender<bool>),
    Reply(Value),
    Pull(oneshot::Sender<oneshot::Receiver<Value>>),
    Interrupt(String, Option<String>, oneshot::Sender<Result<bool, String>>),
    Reset(oneshot::Sender<Result<(), String>>),
    Close(oneshot::Sender<Result<(), String>>),
}

struct WorkerOptions { cwd: PathBuf, session_id: String, width: u64, environment: Option<SessionEnvironment>, connection: BridgeConnectionConfig, executable: PathBuf, names: KernelToolNames }
impl WorkerOptions {
    fn startup<'a>(&'a self,names:&'a (Vec<String>,Vec<String>)) -> WorkerStartupOptions<'a> {
        WorkerStartupOptions {cwd:&self.cwd, session_id:&self.session_id, parallel_pool_width:self.width, connection:&self.connection, generation:0, host_tool_names:&names.0, foreign_language_names:&names.1, session_env:self.environment.as_ref(), worker_entry:None, environment:crate::kernels::shared::runtime_asset::CodemodeRuntimeAssetEnvironment {bun_version:None,executable_path:&self.executable}}
    }
}

pub struct JavaScriptKernel {
    commands: mpsc::UnboundedSender<Command>,
    tools: Arc<super::kernel_tools_host::KernelToolHostPump>,
    loader: LocalModuleLoader,
    snapshot: Arc<Mutex<Snapshot>>,
    pid: Arc<Mutex<Option<u32>>>,
    shutdown: maho_ai::utils::abort::AbortController,
    actor: tokio::sync::Mutex<Option<tokio::task::JoinHandle<()>>>,
    owner_listener: Option<(maho_ai::utils::abort::AbortSignal,maho_ai::utils::abort::ListenerId)>,
}

impl JavaScriptKernel {
    pub async fn start(cwd: &Path, session_id: &str, parallel_pool_width: u64, session_env: Option<SessionEnvironment>) -> Result<Self, ProcessError> {
        Self::start_with_connection(cwd,session_id,parallel_pool_width,session_env,BridgeConnectionConfig {port:1,token:"worker-transport".into(),local_roots:None,artifacts_dir:None,parallel_pool_width:Some(parallel_pool_width)}).await
    }
    pub async fn start_with_connection(cwd:&Path,session_id:&str,parallel_pool_width:u64,session_env:Option<SessionEnvironment>,connection:BridgeConnectionConfig)->Result<Self,ProcessError> {
        Self::start_with_names(cwd,session_id,parallel_pool_width,session_env,connection,Arc::new(||Ok((vec![],vec![])))).await
    }
    pub async fn start_with_names(cwd:&Path,session_id:&str,parallel_pool_width:u64,session_env:Option<SessionEnvironment>,connection:BridgeConnectionConfig,names:KernelToolNames)->Result<Self,ProcessError> {
        Self::start_with_signal(cwd,session_id,parallel_pool_width,session_env,connection,names,maho_ai::utils::abort::AbortController::new()).await
    }
    pub async fn start_with_signal(cwd:&Path,session_id:&str,parallel_pool_width:u64,session_env:Option<SessionEnvironment>,connection:BridgeConnectionConfig,names:KernelToolNames,shutdown:maho_ai::utils::abort::AbortController)->Result<Self,ProcessError> {
        let owner_signal=shutdown.signal();
        let startup_owner=owner_signal.clone();
        let shutdown=maho_ai::utils::abort::AbortController::new();
        let loader=LocalModuleLoader::new(&LocalModuleLoaderOptions {cwd:cwd.into(),local_roots:connection.local_roots.clone(),artifacts_dir:connection.artifacts_dir.as_ref().map(PathBuf::from)})?;
        let options=WorkerOptions {cwd:cwd.into(),session_id:session_id.into(),width:parallel_pool_width,environment:session_env,executable:std::env::current_exe()?,connection,names};
        let startup_options=options;let startup_shutdown=shutdown.clone();
        let (mut sender,receiver)=oneshot::channel();
        tokio::spawn(async move {
            let mut slot=WorkerSlot::default();
            let signal=startup_shutdown.signal();
            let result={
                let ready=async {let names=(startup_options.names)().map_err(ProcessError::Startup)?;slot.ensure_ready(startup_options.startup(&names),&signal).await};
                tokio::pin!(ready);
                tokio::select! {result=&mut ready=>result,()=sender.closed()=>{startup_shutdown.abort(None);ready.await},()=startup_owner.cancelled()=>{startup_shutdown.abort(startup_owner.reason());ready.await}}
            };
            match result {
                Ok(())=>{if let Err(Ok((_,mut slot)))=sender.send(Ok((startup_options,slot))) {let _=slot.retire().await;}},
                Err(error)=>{let _=slot.retire().await;let _=sender.send(Err(error));}
            }
        });
        let (options,slot)=receiver.await.map_err(|_|ProcessError::Closed)??;
        let pid=Arc::new(Mutex::new(slot.pid()));
        let snapshot=Arc::new(Mutex::new((None,vec![])));
        let (commands,receiver)=mpsc::unbounded_channel();
        let posts=commands.downgrade();let worker_pid=pid.clone();
        let tools=Arc::new(super::kernel_tools_host::KernelToolHostPump::new(Arc::new(move |message| {if let Some(posts)=posts.upgrade() {let _=posts.send(Command::Reply(message));}}),Arc::new(move ||worker_pid.lock().expect("JS pid lock").is_some())));
        let actor=tokio::spawn(run_actor(options,slot,receiver,snapshot.clone(),pid.clone(),tools.clone(),shutdown.signal()));
        let child=shutdown.clone();
        let listener=owner_signal.add_abort_listener(move |reason|child.abort(Some(reason.clone())));
        if owner_signal.aborted() {shutdown.abort(owner_signal.reason());}
        Ok(Self {commands,tools,loader,snapshot,pid,shutdown,actor:tokio::sync::Mutex::new(Some(actor)),owner_listener:Some((owner_signal,listener))})
    }

    pub async fn run(&self, input: KernelRunInput, mut on_message: impl FnMut(&Value)) -> Result<Value, ProcessError> {
        let (sender,mut frames)=mpsc::unbounded_channel();
        let operation=self.run_with_callbacks(input,Some(Arc::new(move |message| {let _=sender.send(message.clone());})),None);
        tokio::pin!(operation);
        let result=loop {tokio::select! {result=&mut operation=>break result,Some(message)=frames.recv()=>on_message(&message)}};
        while let Ok(message)=frames.try_recv() {on_message(&message);}
        result
    }
    pub async fn run_with_callbacks(&self, mut input: KernelRunInput, on_message: Option<KernelMessageCallback>, on_started: Option<KernelStartedCallback>) -> Result<Value,ProcessError> {
        if !input.code.starts_with(PREPARED_CELL_PREFIX) {input.code=self.loader.prepare_cell(&input.code);}
        let (sender,receiver)=oneshot::channel();
        self.commands.send(Command::Run(input,on_message,on_started,sender)).map_err(|_|ProcessError::Closed)?;
        receiver.await.map_err(|_|ProcessError::Closed)?.map_err(ProcessError::Startup)
    }
    pub fn queue_snapshot(&self)->Snapshot {self.snapshot.lock().expect("JS queue lock").clone()}
    pub async fn cancel_queued(&self,id:&str,reason:&str)->bool {
        let (sender,receiver)=oneshot::channel();
        if self.commands.send(Command::Cancel(id.into(),reason.into(),sender)).is_err() {return false;}
        receiver.await.unwrap_or(false)
    }
    pub fn deliver_tool_reply(&self,message:Value)->Result<(),String> {self.commands.send(Command::Reply(message)).map_err(|_|"JavaScript kernel is closed".into())}
    pub async fn next_tool_call(&self)->Result<Value,ProcessError> {
        let (sender,receiver)=oneshot::channel();
        self.commands.send(Command::Pull(sender)).map_err(|_|ProcessError::Closed)?;
        receiver.await.map_err(|_|ProcessError::Closed)?.await.map_err(|_|ProcessError::Closed)
    }
    pub async fn interrupt(&self,reason:&str,id:Option<&str>)->Result<bool,String> {
        let (sender,receiver)=oneshot::channel();
        self.commands.send(Command::Interrupt(reason.into(),id.map(str::to_owned),sender)).map_err(|_|"JavaScript kernel is closed".to_string())?;
        receiver.await.map_err(|_|"JavaScript interrupt outcome unavailable".to_string())?
    }
    pub async fn reset(&self)->Result<(),ProcessError> {
        let (sender,receiver)=oneshot::channel();
        self.commands.send(Command::Reset(sender)).map_err(|_|ProcessError::Closed)?;
        receiver.await.map_err(|_|ProcessError::Closed)?.map_err(ProcessError::Startup)
    }
    pub async fn close(&self)->Result<(),ProcessError> {
        self.shutdown.abort(None);
        let (sender,receiver)=oneshot::channel();
        let result=if self.commands.send(Command::Close(sender)).is_err() {Ok(())} else {match receiver.await {Ok(result)=>result.map_err(ProcessError::Startup),Err(_)=>Err(ProcessError::Closed)}};
        if let Some(actor)=self.actor.lock().await.take() {actor.await.map_err(|error|ProcessError::Startup(error.to_string()))?;}
        result
    }
    pub fn pid(&self)->Option<u32> {*self.pid.lock().expect("JS pid lock")}
    pub fn kernel_tool_events(&self)->tokio::sync::broadcast::Receiver<&'static str> {self.tools.events()}
    pub async fn describe_kernel_tools(&self,names:&[String])->Result<Value,super::kernel_tools_errors::KernelToolError> {self.tools.describe(names).await}
    pub async fn invoke_kernel_tool(&self,request:super::kernel_tools_types::KernelToolsInvokeRequest,options:super::kernel_tools_types::KernelToolsInvokeOptions)->Result<Value,super::kernel_tools_errors::KernelToolError> {self.tools.invoke(request,options).await}
}

impl Drop for JavaScriptKernel {
    fn drop(&mut self) {if let Some((signal,listener))=self.owner_listener.take() {signal.remove_abort_listener(listener);}self.shutdown.abort(None);}
}

impl maho_ext_api::ExtensionKernelTools for JavaScriptKernel {
    fn invoke_scope(&self) -> bool { super::kernel_tools_types::KERNEL_TOOLS_INVOKE_SCOPE }
    fn describe<'a>(&'a self, names: &'a [String]) -> maho_ext_api::ExtensionFuture<'a, Value> {
        Box::pin(async move { self.describe_kernel_tools(names).await.map_err(|error| maho_ext_api::ExtensionFailure::new(error.to_string())) })
    }
    fn invoke(&self, request: maho_ext_api::KernelToolInvokeRequest, options: maho_ext_api::KernelToolInvokeOptions) -> maho_ext_api::ExtensionFuture<'_, Value> {
        Box::pin(async move { self.invoke_kernel_tool(request, options).await.map_err(|error| maho_ext_api::ExtensionFailure::new(error.to_string())) })
    }
}

fn route(message:Value,runs:&mut JavaScriptRunQueue,calls:&mut SubprocessRunQueue)->bool {
    if message["type"]=="status" && matches!(message["event"]["op"].as_str(),Some(crate::bridge::reserved::INTERRUPT_ACK_OP|crate::bridge::reserved::CHILD_LIFECYCLE_OP)) {return false;}
    if let Some(run)=runs.active() && let Some(callback)=&run.on_message {callback(&message);}
    if message["type"]=="tool-call" {calls.push_tool_call(message);return false;}
    if message["type"]!="result" || runs.active().is_none_or(|run|message["cellId"]!=run.input.cell_id) {return false;}
    if let Some(mut run)=runs.release_active() {
        run.settled_by_worker=true;
        let result=run.interrupt_result.take().unwrap_or(message);
        JavaScriptRunQueue::settle(&mut run,result);
    }
    true
}

async fn stop_active(slot:&mut WorkerSlot,runs:&mut JavaScriptRunQueue,calls:&mut SubprocessRunQueue,tools:&super::kernel_tools_host::KernelToolHostPump,reason:&str,message:&str,duration:u64)->Result<bool,ProcessError> {
    let Some(run)=runs.active_mut() else {return Ok(true);};
    let mut result=stopped_result(&run.input.cell_id,message);
    result["durationMs"]=json!(duration);
    run.interrupt_result=Some(result);
    slot.post_message(&json!({"type":"interrupt","reason":reason})).await?;
    let mut deadline=tokio::time::Instant::now()+Duration::from_millis(INTERRUPT_ACK_MS);
    let mut acknowledged=false;
    loop {
        let frame=tokio::time::timeout_at(deadline,slot.next_message()).await;
        let Ok(Ok(frame))=frame else {break;};
        if tools.consume(frame.clone()) {continue;}
        if frame["type"]=="status" && frame["event"]["op"]==crate::bridge::reserved::INTERRUPT_ACK_OP && !acknowledged {
            acknowledged=true;
            deadline=tokio::time::Instant::now()+Duration::from_millis(JS_INTERRUPT_GRACE_MS);
        }
        if route(frame,runs,calls) {calls.clear_tool_calls();return Ok(true);}
    }
    slot.retire().await?;
    if let Some(mut run)=runs.release_active() {
        let result=run.interrupt_result.take().expect("interrupt result");
        JavaScriptRunQueue::settle(&mut run,result);
    }
    calls.clear_tool_calls();
    Ok(false)
}

async fn run_actor(options:WorkerOptions,mut slot:WorkerSlot,mut commands:mpsc::UnboundedReceiver<Command>,snapshot:Arc<Mutex<Snapshot>>,pid:Arc<Mutex<Option<u32>>>,tools:Arc<super::kernel_tools_host::KernelToolHostPump>,shutdown:maho_ai::utils::abort::AbortSignal) {
    let mut runs=JavaScriptRunQueue::default();
    let mut calls=SubprocessRunQueue::default();
    let origin=tokio::time::Instant::now();
    let mut deadline=None;
    loop {
        if runs.active().is_none() && runs.has_waiting() {
            let ready=async {let names=(options.names)().map_err(ProcessError::Startup)?;slot.ensure_ready(options.startup(&names),&shutdown).await}.await;
            if let Err(error)=ready {runs.reject_waiting(&error.to_string());}
            else if let Some(run)=runs.start_next(origin.elapsed().as_secs_f64()*1000.0) {
                deadline=run.input.timeout_ms.filter(|ms|*ms>0).map(|ms|tokio::time::Instant::now()+Duration::from_millis(ms));
                let posted=async {
                    let names=(options.names)().map_err(ProcessError::Startup)?;
                    slot.post_message(&json!({"type":"kernel-tools-names","hostToolNames":names.0,"foreignLanguageNames":names.1})).await?;
                    slot.post_message(&json!({"type":"run","cellId":run.input.cell_id,"code":run.input.code,"timeoutMs":run.input.timeout_ms})).await
                }.await;
                if let Err(error)=posted {
                    runs.settle_all(&error.to_string());
                    if let Err(error)=slot.retire().await {eprintln!("JS retirement failed: {error}");}
                    deadline=None;
                }
            }
        }
        *snapshot.lock().expect("JS queue lock")=runs.snapshot();
        *pid.lock().expect("JS pid lock")=slot.pid();
        if !slot.present() {tools.reject_all(super::kernel_tools_errors::kernel_tool_error(super::kernel_tools_errors::KernelToolErrorCode::KernelToolStale,"JavaScript worker reset",None));}
        tokio::select! {
            command=commands.recv()=>match command {
                Some(Command::Run(input,message,started,response))=>{
                    let mut result=runs.enqueue(input,started,message);
                    tokio::spawn(async move {let outcome=result.wait_for(Option::is_some).await.map(|value|value.as_ref().expect("run outcome").clone()).unwrap_or_else(|_|Err("JavaScript kernel is closed".into()));let _=response.send(outcome);});
                }
                Some(Command::Cancel(id,reason,response))=>{let _=response.send(runs.remove(&id,&reason));}
                Some(Command::Reply(message))=>{if let Err(error)=slot.post_message(&message).await {runs.settle_all(&error.to_string());if let Err(error)=slot.retire().await {eprintln!("JS retirement failed: {error}");}deadline=None;}}
                Some(Command::Pull(response))=>{let _=response.send(calls.next_tool_call());}
                Some(Command::Interrupt(reason,id,response))=>{
                    let result=if id.as_ref().is_some_and(|id|runs.active().is_none_or(|run|&run.input.cell_id!=id)) {if let Some(id)=id {runs.remove(&id,&reason);}Ok(true)} else {
                        deadline=None;
                        stop_active(&mut slot,&mut runs,&mut calls,&tools,&reason,&format!("JS cell interrupted: {reason}"),0).await.map_err(|error|error.to_string())
                    };
                    let _=response.send(result);
                }
                Some(Command::Reset(response))=>{
                    tools.reject_all(super::kernel_tools_errors::kernel_tool_error(super::kernel_tools_errors::KernelToolErrorCode::KernelToolStale,"JavaScript worker reset",None));
                    runs.settle_all("JS kernel reset");calls.clear_tool_calls();deadline=None;
                    let result=async {slot.retire().await?;let names=(options.names)().map_err(ProcessError::Startup)?;slot.ensure_ready(options.startup(&names),&shutdown).await}.await.map_err(|error|error.to_string());
                    let _=response.send(result);
                }
                Some(Command::Close(response))=>{
                    tools.reject_all(super::kernel_tools_errors::kernel_tool_error(super::kernel_tools_errors::KernelToolErrorCode::KernelToolStale,"JS kernel closed",None));
                    runs.settle_all("JS kernel closed");calls.clear_tool_calls();
                    let _=slot.post_message(&json!({"type":"close"})).await;
                    let result=slot.retire().await.map(|_|()).map_err(|error|error.to_string());
                    let _=response.send(result);break;
                }
                None=>{runs.settle_all("JS kernel closed");if let Err(error)=slot.retire().await {eprintln!("JS retirement failed: {error}");}break;}
            },
            message=slot.next_message(),if slot.present()=>match message {
                Ok(message)=>{
                    if tools.consume(message.clone()) {continue;}
                    if route(message,&mut runs,&mut calls) {deadline=None;}
                }
                Err(error)=>{if let Some(mut run)=runs.release_active() {let result=stopped_result(&run.input.cell_id,&error.to_string());JavaScriptRunQueue::settle(&mut run,result);}calls.clear_tool_calls();deadline=None;if let Err(error)=slot.retire().await {eprintln!("JS retirement failed: {error}");}}
            },
            ()=async {match deadline {Some(deadline)=>tokio::time::sleep_until(deadline).await,None=>std::future::pending().await}},if deadline.is_some()=>{
                let duration=runs.active().and_then(|run|run.input.timeout_ms).unwrap_or(0);
                if let Err(error)=stop_active(&mut slot,&mut runs,&mut calls,&tools,&format!("timed out after {duration}ms"),&format!("JS cell timed out after {duration}ms"),duration).await {runs.settle_all(&error.to_string());if let Err(error)=slot.retire().await {eprintln!("JS retirement failed: {error}");}}
                deadline=None;
            }
        }
    }
    *snapshot.lock().expect("JS queue lock")=(None,vec![]);
    *pid.lock().expect("JS pid lock")=None;
    tools.reject_all(super::kernel_tools_errors::kernel_tool_error(super::kernel_tools_errors::KernelToolErrorCode::KernelToolStale,"JS kernel closed",None));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn internal_child_status_does_not_reach_cell_output() {
        let output=Arc::new(Mutex::new(Vec::new()));
        let observed=output.clone();
        let mut runs=JavaScriptRunQueue::default();
        let _result=runs.enqueue(KernelRunInput {cell_id:"child".into(),code:String::new(),timeout_ms:None},None,Some(Arc::new(move |message|observed.lock().expect("output lock").push(message.clone()))));
        runs.start_next(0.0);
        let mut calls=SubprocessRunQueue::default();
        route(json!({"type":"status","event":{"op":"child","pid":123,"state":"spawned"}}),&mut runs,&mut calls);
        assert!(output.lock().expect("output lock").is_empty());
    }

    #[tokio::test]
    async fn host_pump_does_not_own_actor_admission_lifetime() {
        let kernel=JavaScriptKernel::start(Path::new(env!("CARGO_MANIFEST_DIR")),"pump-owner",4,None).await.unwrap();
        let owners=kernel.commands.strong_count();
        kernel.close().await.unwrap();
        assert_eq!(owners,1,"only the public kernel should retain command admission");
    }

    #[tokio::test]
    async fn cancelled_startup_does_not_admit_a_kernel() {
        let shutdown=maho_ai::utils::abort::AbortController::new();
        shutdown.abort(None);
        let result=JavaScriptKernel::start_with_signal(Path::new(env!("CARGO_MANIFEST_DIR")),"cancelled-start",4,None,BridgeConnectionConfig {port:1,token:"test".into(),local_roots:None,artifacts_dir:None,parallel_pool_width:None},Arc::new(||Ok((vec![],vec![]))),shutdown).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn close_consumes_actor_and_reaps_worker() {
        let kernel=JavaScriptKernel::start(Path::new(env!("CARGO_MANIFEST_DIR")),"joined-close",4,None).await.unwrap();
        kernel.close().await.unwrap();
        assert!(kernel.actor.lock().await.is_none());
        assert!(kernel.pid().is_none());
    }

    #[tokio::test]
    async fn closing_one_kernel_does_not_cancel_session_owner() {
        let owner=maho_ai::utils::abort::AbortController::new();
        let kernel=JavaScriptKernel::start_with_signal(Path::new(env!("CARGO_MANIFEST_DIR")),"local-close",4,None,BridgeConnectionConfig {port:1,token:"test".into(),local_roots:None,artifacts_dir:None,parallel_pool_width:None},Arc::new(||Ok((vec![],vec![]))),owner.clone()).await.unwrap();
        kernel.close().await.unwrap();
        assert!(!owner.signal().aborted());
        assert!(kernel.pid().is_none());
    }
}
