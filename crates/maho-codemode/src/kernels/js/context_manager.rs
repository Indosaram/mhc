use super::{
    interrupt_bounds::{
        INTERRUPT_ACK_MS, JS_INTERRUPT_GRACE_MS, WORKER_TERMINATE_DEADLINE_MS, WorkerRetirement,
        abandoned_worker_note,
    },
    local_module_loader::{LocalModuleLoader, LocalModuleLoaderOptions, PREPARED_CELL_PREFIX},
    run_queue::{JavaScriptRunQueue, stopped_result},
    worker_slot::WorkerSlot,
    worker_startup::WorkerStartupOptions,
};
use crate::bridge::protocol::BridgeConnectionConfig;
use crate::kernels::session_env::SessionEnvironment;
use crate::kernels::shared::{
    subprocess_contract::KernelRunInput,
    subprocess_process::ProcessError,
    subprocess_queue::SubprocessRunQueue,
    subprocess_run::{KernelMessageCallback, KernelStartedCallback},
};
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::sync::{mpsc, oneshot};

type Snapshot = (Option<String>, Vec<String>);
type InterruptResponse = oneshot::Sender<Result<(bool, Option<String>), String>>;
pub type KernelToolNames =
    Arc<dyn Fn() -> Result<(Vec<String>, Vec<String>), String> + Send + Sync>;
enum Command {
    Run(
        KernelRunInput,
        Option<KernelMessageCallback>,
        Option<KernelStartedCallback>,
        super::run_queue::PrepareCell,
        Option<crate::tool::types::HostCellExecutor>,
        oneshot::Sender<Result<Value, String>>,
    ),
    Cancel(String, String, oneshot::Sender<bool>),
    Reply(Value),
    Pull(oneshot::Sender<oneshot::Receiver<Value>>),
    MemoryQuery(String, oneshot::Sender<Result<Value, String>>),
    Interrupt(
        String,
        Option<String>,
        InterruptResponse,
    ),
    Reset(oneshot::Sender<Result<(), String>>),
    Close(oneshot::Sender<Result<(), String>>),
}

pub(super) struct WorkerOptions {
    isolation: crate::config::feature_settings::JsIsolation,
    cached_memory: Arc<Mutex<Option<u64>>>,
    cwd: PathBuf,
    session_id: String,
    width: u64,
    environment: Option<SessionEnvironment>,
    connection: BridgeConnectionConfig,
    pub(super) names: KernelToolNames,
    memory: crate::bridge::protocol::KernelMemoryThresholds,
    #[cfg(test)]
    worker_entry: Option<PathBuf>,
}
impl WorkerOptions {
    pub(super) fn startup<'a>(
        &'a self,
        names: &'a (Vec<String>, Vec<String>),
    ) -> WorkerStartupOptions<'a> {
        WorkerStartupOptions {
            isolation: self.isolation,
            process_startup_deadline: super::process_worker::PROCESS_STARTUP_DEADLINE,
            cwd: &self.cwd,
            session_id: &self.session_id,
            parallel_pool_width: self.width,
            connection: &self.connection,
            generation: 0,
            host_tool_names: &names.0,
            foreign_language_names: &names.1,
            session_env: self.environment.as_ref(),
            worker_entry: {
                #[cfg(test)]
                {
                    self.worker_entry.as_deref()
                }
                #[cfg(not(test))]
                {
                    None
                }
            },
            memory: Some(&self.memory),
        }
    }
}

/// Startup inputs for [`JavaScriptKernel::start_with_memory`].
pub struct JavaScriptKernelStart {
    pub isolation: crate::config::feature_settings::JsIsolation,
    pub parallel_pool_width: u64,
    pub session_env: Option<SessionEnvironment>,
    pub connection: BridgeConnectionConfig,
    pub names: KernelToolNames,
    pub shutdown: maho_ai::utils::abort::AbortController,
    pub memory: crate::bridge::protocol::KernelMemoryThresholds,
}

pub struct JavaScriptKernel {
    memory_measure: crate::extension::kernel_registry::KernelMeasure,
    cached_memory: Arc<Mutex<Option<u64>>>,
    commands: mpsc::UnboundedSender<Command>,
    tools: Arc<super::kernel_tools_host::KernelToolHostPump>,
    loader: Arc<LocalModuleLoader>,
    snapshot: Arc<Mutex<Snapshot>>,
    pid: Arc<Mutex<Option<u32>>>,
    shutdown: maho_ai::utils::abort::AbortController,
    actor: tokio::sync::Mutex<Option<tokio::task::JoinHandle<()>>>,
    owner_listener: Option<(
        maho_ai::utils::abort::AbortSignal,
        maho_ai::utils::abort::ListenerId,
    )>,
}

impl JavaScriptKernel {
    pub async fn start(
        cwd: &Path,
        session_id: &str,
        parallel_pool_width: u64,
        session_env: Option<SessionEnvironment>,
    ) -> Result<Self, ProcessError> {
        Self::start_with_connection(
            cwd,
            session_id,
            parallel_pool_width,
            session_env,
            BridgeConnectionConfig {
                port: 1,
                token: "worker-transport".into(),
                local_roots: None,
                artifacts_dir: None,
                parallel_pool_width: Some(parallel_pool_width),
            },
        )
        .await
    }
    pub async fn start_with_connection(
        cwd: &Path,
        session_id: &str,
        parallel_pool_width: u64,
        session_env: Option<SessionEnvironment>,
        connection: BridgeConnectionConfig,
    ) -> Result<Self, ProcessError> {
        Self::start_with_names(
            cwd,
            session_id,
            parallel_pool_width,
            session_env,
            connection,
            Arc::new(|| Ok((vec![], vec![]))),
        )
        .await
    }
    pub async fn start_with_names(
        cwd: &Path,
        session_id: &str,
        parallel_pool_width: u64,
        session_env: Option<SessionEnvironment>,
        connection: BridgeConnectionConfig,
        names: KernelToolNames,
    ) -> Result<Self, ProcessError> {
        Self::start_with_signal(
            cwd,
            session_id,
            parallel_pool_width,
            session_env,
            connection,
            names,
            maho_ai::utils::abort::AbortController::new(),
        )
        .await
    }
    pub async fn start_with_signal(
        cwd: &Path,
        session_id: &str,
        parallel_pool_width: u64,
        session_env: Option<SessionEnvironment>,
        connection: BridgeConnectionConfig,
        names: KernelToolNames,
        shutdown: maho_ai::utils::abort::AbortController,
    ) -> Result<Self, ProcessError> {
        Self::start_with_memory(
            cwd,
            session_id,
            JavaScriptKernelStart {
                isolation: Default::default(),
                parallel_pool_width,
                session_env,
                connection,
                names,
                shutdown,
                memory: crate::bridge::protocol::KernelMemoryThresholds {
                    gc_watermark_bytes: 1_073_741_824,
                    notice_bytes: 1_610_612_736,
                    ceiling_bytes: 2_147_483_648,
                },
            },
        )
        .await
    }
    pub async fn start_with_memory(
        cwd: &Path,
        session_id: &str,
        start: JavaScriptKernelStart,
    ) -> Result<Self, ProcessError> {
        let JavaScriptKernelStart {
            isolation,
            parallel_pool_width,
            session_env,
            connection,
            names,
            shutdown,
            memory,
        } = start;
        let owner_signal = shutdown.signal();
        let startup_owner = owner_signal.clone();
        let shutdown = maho_ai::utils::abort::AbortController::new();
        let loader = LocalModuleLoader::new(&LocalModuleLoaderOptions {
            cwd: cwd.into(),
            local_roots: connection.local_roots.clone(),
            artifacts_dir: connection.artifacts_dir.as_ref().map(PathBuf::from),
        })?;
        let options = WorkerOptions {
            isolation,
            cached_memory: Default::default(),
            cwd: cwd.into(),
            session_id: session_id.into(),
            width: parallel_pool_width,
            environment: session_env,
            connection,
            names,
            memory,
            #[cfg(test)]
            worker_entry: None,
        };
        let startup_options = options;
        let startup_shutdown = shutdown.clone();
        let (mut sender, receiver) = oneshot::channel();
        tokio::spawn(async move {
            let mut slot = WorkerSlot::default();
            let signal = startup_shutdown.signal();
            let result = {
                let ready = async {
                    let names = (startup_options.names)().map_err(ProcessError::Startup)?;
                    slot.ensure_ready(startup_options.startup(&names), &signal)
                        .await
                };
                tokio::pin!(ready);
                tokio::select! {result=&mut ready=>result,()=sender.closed()=>{startup_shutdown.abort(None);ready.await},()=startup_owner.cancelled()=>{startup_shutdown.abort(startup_owner.reason());ready.await}}
            };
            match result {
                Ok(()) => {
                    if let Err(Ok((_, mut slot))) = sender.send(Ok((startup_options, slot))) {
                        let _ = slot.retire().await;
                    }
                }
                Err(error) => {
                    let _ = slot.retire().await;
                    let _ = sender.send(Err(error));
                }
            }
        });
        let (options, slot) = receiver.await.map_err(|_| ProcessError::Closed)??;
        let cached_memory = options.cached_memory.clone();
        let memory_measure = match isolation {
            crate::config::feature_settings::JsIsolation::Worker => {
                crate::extension::kernel_registry::KernelMeasure::Heap
            }
            crate::config::feature_settings::JsIsolation::Process => {
                crate::extension::kernel_registry::KernelMeasure::Footprint
            }
        };
        let pid = Arc::new(Mutex::new(slot.pid()));
        let snapshot = Arc::new(Mutex::new((None, vec![])));
        let (commands, receiver) = mpsc::unbounded_channel();
        let posts = commands.downgrade();
        let worker_pid = pid.clone();
        let tools = Arc::new(super::kernel_tools_host::KernelToolHostPump::new(
            Arc::new(move |message| {
                if let Some(posts) = posts.upgrade() {
                    let _ = posts.send(Command::Reply(message));
                }
            }),
            Arc::new(move || worker_pid.lock().expect("JS pid lock").is_some()),
        ));
        let actor = tokio::spawn(run_actor(
            options,
            slot,
            receiver,
            snapshot.clone(),
            pid.clone(),
            tools.clone(),
            shutdown.signal(),
        ));
        let child = shutdown.clone();
        let listener =
            owner_signal.add_abort_listener(move |reason| child.abort(Some(reason.clone())));
        if owner_signal.aborted() {
            shutdown.abort(owner_signal.reason());
        }
        Ok(Self {
            memory_measure,
            cached_memory,
            commands,
            tools,
            loader: Arc::new(loader),
            snapshot,
            pid,
            shutdown,
            actor: tokio::sync::Mutex::new(Some(actor)),
            owner_listener: Some((owner_signal, listener)),
        })
    }

    pub async fn run(
        &self,
        input: KernelRunInput,
        mut on_message: impl FnMut(&Value),
    ) -> Result<Value, ProcessError> {
        let (sender, mut frames) = mpsc::unbounded_channel();
        let operation = self.run_with_callbacks(
            input,
            Some(Arc::new(move |message| {
                let _ = sender.send(message.clone());
            })),
            None,
        );
        tokio::pin!(operation);
        let result = loop {
            tokio::select! {result=&mut operation=>break result,Some(message)=frames.recv()=>on_message(&message)}
        };
        while let Ok(message) = frames.try_recv() {
            on_message(&message);
        }
        result
    }
    pub async fn run_with_callbacks(
        &self,
        input: KernelRunInput,
        on_message: Option<KernelMessageCallback>,
        on_started: Option<KernelStartedCallback>,
    ) -> Result<Value, ProcessError> {
        self.run_with_preludes(input, on_message, on_started, &[])
            .await
    }
    pub async fn run_with_preludes(
        &self,
        input: KernelRunInput,
        on_message: Option<KernelMessageCallback>,
        on_started: Option<KernelStartedCallback>,
        preludes: &[maho_tools::KernelPrelude],
    ) -> Result<Value, ProcessError> {
        self.run_eval(crate::tool::types::EvalKernelRunInput {
            cell_id: input.cell_id,
            code: input.code,
            timeout_ms: input.timeout_ms,
            on_message,
            on_started,
            preludes: preludes.to_vec(),
            ..Default::default()
        })
        .await
    }
    pub async fn run_eval(
        &self,
        mut input: crate::tool::types::EvalKernelRunInput,
    ) -> Result<Value, ProcessError> {
        let (sender, receiver) = oneshot::channel();
        let queued = KernelRunInput {
            cell_id: input.cell_id.clone(),
            code: input.code.clone(),
            timeout_ms: input.timeout_ms,
        };
        let message = input.on_message.take();
        let started = input.on_started.take();
        let host = input.host_executor.take();
        let loader = self.loader.clone();
        let prepare = Box::new(move || {
            let (code, source) =
                crate::kernels::shared::cell_source_at_start::input_at_start(&input)?;
            if code.starts_with(PREPARED_CELL_PREFIX) {
                return Ok(code);
            }
            let root = input
                .package_root
                .as_ref()
                .map(|root| root())
                .transpose()?
                .flatten();
            loader
                .prepare_cell_with_packages(
                    &code,
                    &input.preludes,
                    source.as_deref(),
                    root.as_deref(),
                )
                .map_err(|error| error.to_string())
        });
        self.commands
            .send(Command::Run(
                queued, message, started, prepare, host, sender,
            ))
            .map_err(|_| ProcessError::Closed)?;
        receiver
            .await
            .map_err(|_| ProcessError::Closed)?
            .map_err(ProcessError::Startup)
    }
    pub fn queue_snapshot(&self) -> Snapshot {
        self.snapshot.lock().expect("JS queue lock").clone()
    }
    pub fn memory_measure(&self) -> crate::extension::kernel_registry::KernelMeasure {
        self.memory_measure
    }
    pub fn last_live_bytes(&self) -> Option<u64> {
        *self.cached_memory.lock().expect("JS memory cache lock")
    }
    pub async fn cancel_queued(&self, id: &str, reason: &str) -> bool {
        let (sender, receiver) = oneshot::channel();
        if self
            .commands
            .send(Command::Cancel(id.into(), reason.into(), sender))
            .is_err()
        {
            return false;
        }
        receiver.await.unwrap_or(false)
    }
    pub fn deliver_tool_reply(&self, message: Value) -> Result<(), String> {
        self.commands
            .send(Command::Reply(message))
            .map_err(|_| "JavaScript kernel is closed".into())
    }
    pub async fn next_tool_call(&self) -> Result<Value, ProcessError> {
        let (sender, receiver) = oneshot::channel();
        self.commands
            .send(Command::Pull(sender))
            .map_err(|_| ProcessError::Closed)?;
        receiver
            .await
            .map_err(|_| ProcessError::Closed)?
            .await
            .map_err(|_| ProcessError::Closed)
    }
    pub async fn memory_query(&self) -> Result<Value, ProcessError> {
        let (sender, receiver) = oneshot::channel();
        let request_id = uuid::Uuid::new_v4().to_string();
        self.commands
            .send(Command::MemoryQuery(request_id, sender))
            .map_err(|_| ProcessError::Closed)?;
        receiver
            .await
            .map_err(|_| ProcessError::Closed)?
            .map_err(ProcessError::Startup)
    }
    /// Interrupts the active (or a queued) cell; reports whether worker state survived and, when a blocked
    /// worker had to be abandoned, the note that explains it.
    pub async fn interrupt(
        &self,
        reason: &str,
        id: Option<&str>,
    ) -> Result<(bool, Option<String>), String> {
        let (sender, receiver) = oneshot::channel();
        self.commands
            .send(Command::Interrupt(
                reason.into(),
                id.map(str::to_owned),
                sender,
            ))
            .map_err(|_| "JavaScript kernel is closed".to_string())?;
        receiver
            .await
            .map_err(|_| "JavaScript interrupt outcome unavailable".to_string())?
    }
    pub async fn reset(&self) -> Result<(), ProcessError> {
        let (sender, receiver) = oneshot::channel();
        self.commands
            .send(Command::Reset(sender))
            .map_err(|_| ProcessError::Closed)?;
        receiver
            .await
            .map_err(|_| ProcessError::Closed)?
            .map_err(ProcessError::Startup)
    }
    pub async fn close(&self) -> Result<(), ProcessError> {
        self.shutdown.abort(None);
        let (sender, receiver) = oneshot::channel();
        let result = if self.commands.send(Command::Close(sender)).is_err() {
            Ok(())
        } else {
            match receiver.await {
                Ok(result) => result.map_err(ProcessError::Startup),
                Err(_) => Err(ProcessError::Closed),
            }
        };
        if let Some(actor) = self.actor.lock().await.take() {
            actor
                .await
                .map_err(|error| ProcessError::Startup(error.to_string()))?;
        }
        result
    }
    pub fn pid(&self) -> Option<u32> {
        *self.pid.lock().expect("JS pid lock")
    }
    pub fn kernel_tool_events(&self) -> tokio::sync::broadcast::Receiver<&'static str> {
        self.tools.events()
    }
    pub async fn describe_kernel_tools(
        &self,
        names: &[String],
    ) -> Result<Value, super::kernel_tools_errors::KernelToolError> {
        self.tools.describe(names).await
    }
    pub async fn invoke_kernel_tool(
        &self,
        request: super::kernel_tools_types::KernelToolsInvokeRequest,
        options: super::kernel_tools_types::KernelToolsInvokeOptions,
    ) -> Result<Value, super::kernel_tools_errors::KernelToolError> {
        self.tools.invoke(request, options).await
    }
}

impl Drop for JavaScriptKernel {
    fn drop(&mut self) {
        if let Some((signal, listener)) = self.owner_listener.take() {
            signal.remove_abort_listener(listener);
        }
        self.shutdown.abort(None);
    }
}

impl maho_ext_api::ExtensionKernelTools for JavaScriptKernel {
    fn invoke_scope(&self) -> bool {
        super::kernel_tools_types::KERNEL_TOOLS_INVOKE_SCOPE
    }
    fn describe<'a>(&'a self, names: &'a [String]) -> maho_ext_api::ExtensionFuture<'a, Value> {
        Box::pin(async move {
            self.describe_kernel_tools(names)
                .await
                .map_err(|error| maho_ext_api::ExtensionFailure::new(error.to_string()))
        })
    }
    fn invoke(
        &self,
        request: maho_ext_api::KernelToolInvokeRequest,
        options: maho_ext_api::KernelToolInvokeOptions,
    ) -> maho_ext_api::ExtensionFuture<'_, Value> {
        Box::pin(async move {
            self.invoke_kernel_tool(request, options)
                .await
                .map_err(|error| maho_ext_api::ExtensionFailure::new(error.to_string()))
        })
    }
}

fn route(message: Value, runs: &mut JavaScriptRunQueue, calls: &mut SubprocessRunQueue) -> bool {
    if runs
        .memory
        .as_mut()
        .is_some_and(|memory| memory.consume(&message))
    {
        return false;
    }
    if message["type"] == "status"
        && matches!(
            message["event"]["op"].as_str(),
            Some(
                crate::bridge::reserved::INTERRUPT_ACK_OP
                    | crate::bridge::reserved::CHILD_LIFECYCLE_OP
            )
        )
    {
        return false;
    }
    if message["type"] == "kernel-memory-collected" {
        return false;
    }
    if let Some(run) = runs.active()
        && let Some(callback) = &run.on_message
    {
        callback(&message);
    }
    if message["type"] == "tool-call" {
        calls.push_tool_call(message);
        return false;
    }
    if message["type"] != "result"
        || runs
            .active()
            .is_none_or(|run| message["cellId"] != run.input.cell_id)
    {
        return false;
    }
    if let Some(mut run) = runs.release_active() {
        run.settled_by_worker = true;
        let message = runs
            .memory
            .as_mut()
            .map_or_else(|| message.clone(), |memory| memory.settled(message.clone()));
        let result = run.interrupt_result.take().unwrap_or(message);
        JavaScriptRunQueue::settle(&mut run, result);
    }
    true
}

async fn stop_active(
    slot: &mut WorkerSlot,
    runs: &mut JavaScriptRunQueue,
    calls: &mut SubprocessRunQueue,
    tools: &super::kernel_tools_host::KernelToolHostPump,
    reason: &str,
    message: &str,
    duration: u64,
) -> Result<(bool, Option<String>), ProcessError> {
    let Some(run) = runs.active_mut() else {
        return Ok((true, None));
    };
    let mut result = stopped_result(&run.input.cell_id, message);
    result["durationMs"] = json!(duration);
    run.interrupt_result = Some(result);
    slot.post_message(&json!({"type":"interrupt","reason":reason}))
        .await?;
    let mut deadline = tokio::time::Instant::now() + Duration::from_millis(INTERRUPT_ACK_MS);
    let mut acknowledged = false;
    loop {
        let frame = tokio::time::timeout_at(deadline, slot.next_message()).await;
        let Ok(Ok(frame)) = frame else {
            break;
        };
        if tools.consume(frame.clone()) {
            continue;
        }
        if frame["type"] == "status"
            && frame["event"]["op"] == crate::bridge::reserved::INTERRUPT_ACK_OP
        {
            if frame["event"]["shellWaitActive"] == true {
                deadline = tokio::time::Instant::now()
                    + Duration::from_millis(WORKER_TERMINATE_DEADLINE_MS);
            } else if !acknowledged {
                acknowledged = true;
                deadline =
                    tokio::time::Instant::now() + Duration::from_millis(JS_INTERRUPT_GRACE_MS);
            }
        }
        if route(frame, runs, calls) {
            calls.clear_tool_calls();
            return Ok((true, None));
        }
    }
    let retirement = slot.retire().await?;
    if let Some(mut run) = runs.release_active() {
        let result = run.interrupt_result.take().expect("interrupt result");
        JavaScriptRunQueue::settle(&mut run, result);
    }
    calls.clear_tool_calls();
    Ok((
        false,
        (retirement == WorkerRetirement::Abandoned)
            .then(|| abandoned_worker_note(WORKER_TERMINATE_DEADLINE_MS)),
    ))
}

async fn run_actor(
    options: WorkerOptions,
    mut slot: WorkerSlot,
    mut commands: mpsc::UnboundedReceiver<Command>,
    snapshot: Arc<Mutex<Snapshot>>,
    pid: Arc<Mutex<Option<u32>>>,
    tools: Arc<super::kernel_tools_host::KernelToolHostPump>,
    shutdown: maho_ai::utils::abort::AbortSignal,
) {
    let options = Arc::new(options);
    let mut recovery: Option<super::worker_recovery::WorkerRecovery> = None;
    let mut reset_responses: Vec<oneshot::Sender<Result<(), String>>> = Vec::new();
    let mut interrupt_responses: Vec<InterruptResponse> = Vec::new();
    let memory = match options.isolation {
        crate::config::feature_settings::JsIsolation::Worker => {
            super::kernel_memory_bridge::KernelMemoryBridge::new(options.memory.clone())
        }
        crate::config::feature_settings::JsIsolation::Process => {
            super::kernel_memory_bridge::KernelMemoryBridge::process(options.memory.clone())
        }
    };
    let mut runs =
        JavaScriptRunQueue::with_memory(memory.with_cache(options.cached_memory.clone()));
    let mut memory_generation = slot.generation();
    let mut calls = SubprocessRunQueue::default();
    let mut pending_memory_queries = std::collections::HashMap::<
        String,
        (oneshot::Sender<Result<Value, String>>, tokio::time::Instant),
    >::new();
    let origin = tokio::time::Instant::now();
    let mut deadline = None;
    let mut host_entry: Option<super::host_entries::HostEntry> = None;
    loop {
        if recovery.is_none() && memory_generation != slot.generation() {
            if let Some(memory) = &mut runs.memory {
                memory.worker_lost();
            }
            for (_, (pending, _)) in pending_memory_queries.drain() {
                let _ = pending.send(Err("JavaScript worker retired".into()));
            }
            memory_generation = slot.generation();
        }
        let idle = recovery.is_none() && runs.active().is_none() && !runs.has_waiting();
        if runs
            .memory
            .as_mut()
            .is_some_and(|memory| memory.claim_recycle(idle))
        {
            tools.reject_all(super::kernel_tools_errors::kernel_tool_error(
                super::kernel_tools_errors::KernelToolErrorCode::KernelToolStale,
                "JavaScript worker recycled",
                None,
            ));
            if let Err(error) = slot.retire().await {
                maho_ext_api::diagnostic(&format!("JS memory recycle failed: {error}"));
            }
            if let Some(memory) = &mut runs.memory {
                memory.worker_lost();
            }
            for (_, (pending, _)) in pending_memory_queries.drain() {
                let _ = pending.send(Err("JavaScript worker recycled".into()));
            }
            memory_generation = slot.generation();
        }
        if runs.active().is_none() && runs.has_waiting() {
            if !slot.present() && recovery.is_none() {
                recovery = Some(super::worker_recovery::WorkerRecovery::start(
                    std::mem::take(&mut slot),
                    options.clone(),
                    shutdown.clone(),
                ));
            }
            if recovery.is_none()
                && slot.present()
                && runs
                    .start_next(origin.elapsed().as_secs_f64() * 1000.0)
                    .is_some()
            {
                if let Some(host) = runs.active_mut().and_then(|run| run.host_executor.take()) {
                    if let Some(run) = runs.active() {
                        host_entry = Some(super::host_entries::HostEntry::start(
                            run.input.cell_id.clone(),
                            host,
                            run.on_message.clone(),
                        ));
                    }
                    continue;
                }
                let prepared = runs
                    .active_mut()
                    .and_then(|run| run.prepare.take())
                    .map(|prepare| prepare());
                match prepared {
                    Some(Ok(code)) => {
                        if let Some(run) = runs.active_mut() {
                            run.input.code = code;
                        }
                    }
                    Some(Err(error)) => {
                        if let Some(mut run) = runs.release_active() {
                            let result =
                                super::run_queue::unstarted_result(&run.input.cell_id, &error);
                            JavaScriptRunQueue::settle(&mut run, result);
                        }
                        continue;
                    }
                    None => {}
                }
                // Keep the Send-only callbacks in the queue out of the posting future.
                let run_message = {
                    let Some(run) = runs.active() else {
                        continue;
                    };
                    deadline = run
                        .input
                        .timeout_ms
                        .filter(|ms| *ms > 0)
                        .map(|ms| tokio::time::Instant::now() + Duration::from_millis(ms));
                    json!({"type":"run","cellId":run.input.cell_id,"code":run.input.code,"timeoutMs":run.input.timeout_ms})
                };
                let posted=async {
                    let names=(options.names)().map_err(ProcessError::Startup)?;
                    slot.post_message(&json!({"type":"kernel-tools-names","hostToolNames":names.0,"foreignLanguageNames":names.1})).await?;
                    slot.post_message(&run_message).await
                }.await;
                if let Err(error) = posted {
                    runs.settle_all(&error.to_string());
                    if let Err(error) = slot.retire().await {
                        maho_ext_api::diagnostic(&format!("JS retirement failed: {error}"));
                    }
                    deadline = None;
                }
            }
        }
        *snapshot.lock().expect("JS queue lock") = runs.snapshot();
        *pid.lock().expect("JS pid lock") = slot.pid();
        if !slot.present() {
            tools.reject_all(super::kernel_tools_errors::kernel_tool_error(
                super::kernel_tools_errors::KernelToolErrorCode::KernelToolStale,
                "JavaScript worker reset",
                None,
            ));
        }
        tokio::select! {
            ready=async {match recovery.as_mut() {Some(recovery)=>recovery.finish().await,None=>std::future::pending().await}},if recovery.is_some()=>{
                let (ready_slot,result)=ready;
                recovery.take();
                slot=ready_slot;
                let result=result.map_err(|error|error.to_string());
                if let Err(error)=&result {
                    runs.reject_waiting(error);
                    if let Err(error)=slot.retire().await {maho_ext_api::diagnostic(&format!("JS startup retirement failed: {error}"));}
                }
                for response in reset_responses.drain(..) {let _=response.send(result.clone());}
                for response in interrupt_responses.drain(..) {let _=response.send(result.clone().map(|()|(false,None)));}
                continue;
            },
            command=commands.recv()=>match command {
                Some(Command::Run(input,message,started,prepare,host,response))=>{
                    let mut result=runs.enqueue_prepared(input,started,message,Some(prepare));
                    runs.set_last_host(host);
                    tokio::spawn(async move {let outcome=result.wait_for(Option::is_some).await.map(|value|value.as_ref().expect("run outcome").clone()).unwrap_or_else(|_|Err("JavaScript kernel is closed".into()));let _=response.send(outcome);});
                }
                Some(Command::Cancel(id,reason,response))=>{let _=response.send(runs.remove(&id,&reason));}
                Some(Command::Reply(message))=>{if let Err(error)=slot.post_message(&message).await {runs.settle_all(&error.to_string());if let Err(error)=slot.retire().await {maho_ext_api::diagnostic(&format!("JS retirement failed: {error}"));}deadline=None;}}
                Some(Command::Pull(response))=>{let _=response.send(calls.next_tool_call());}
                Some(Command::MemoryQuery(request_id,response))=>{
                    if !slot.present() {let _=response.send(Ok(Value::Null));continue;}
                    if options.isolation == crate::config::feature_settings::JsIsolation::Process {
                        if let Some(bytes)=super::process_memory_host::read_process_footprint(slot.pid()).await {
                            if let Some(memory)=&runs.memory {memory.observe_query(bytes);}
                            let _=response.send(Ok(json!({"type":"memory-query-result","requestId":request_id,"liveBytes":bytes,"measure":"footprint"})));
                            continue;
                        }
                        let _=response.send(Ok(Value::Null));
                        continue;
                    }
                    if let Err(error)=slot.post_message(&json!({"type":"memory-query","requestId":request_id})).await {let _=response.send(Err(error.to_string()));continue;}
                    pending_memory_queries.insert(request_id, (response,tokio::time::Instant::now()+Duration::from_secs(5)));
                }
                Some(Command::Interrupt(reason,id,response))=>{
                    if id.is_none() && runs.active().is_none() {
                        runs.settle_all(&format!("JS cell interrupted: {reason}"));
                        if let Some(starting)=recovery.take() {
                            slot=starting.cancel().await;
                            match slot.retire().await {
                                Ok(_)=>{
                                    interrupt_responses.push(response);
                                    recovery=Some(super::worker_recovery::WorkerRecovery::start(std::mem::take(&mut slot),options.clone(),shutdown.clone()));
                                }
                                Err(error)=>{let _=response.send(Err(error.to_string()));}
                            }
                        } else {let _=response.send(Ok((true,None)));}
                        continue;
                    }
                    if host_entry.is_some() && id.as_ref().is_none_or(|id|runs.active().is_some_and(|run|&run.input.cell_id==id)) {
                        if let Some(entry)=host_entry.take() {entry.abort().await;}
                        if let Some(mut run)=runs.release_active() {let result=stopped_result(&run.input.cell_id,&reason);JavaScriptRunQueue::settle(&mut run,result);}
                        let _=response.send(Ok((true,None)));
                        continue;
                    }
                    let result=if id.as_ref().is_some_and(|id|runs.active().is_none_or(|run|&run.input.cell_id!=id)) {let cancelled=id.as_ref().is_some_and(|id|runs.remove(id,&reason));Ok((true,(!cancelled).then(||"cell not found".to_owned())))} else {
                        deadline=None;
                        stop_active(&mut slot,&mut runs,&mut calls,&tools,&reason,&format!("JS cell interrupted: {reason}"),0).await.map_err(|error|error.to_string())
                    };
                    let _=response.send(result);
                }
                Some(Command::Reset(response))=>{
                    if let Some(starting)=recovery.take() {slot=starting.cancel().await;}
                    if let Some(entry)=host_entry.take() {entry.abort().await;}
                    for (_, (pending, _)) in pending_memory_queries.drain() { let _ = pending.send(Err("JavaScript worker reset".into())); }
                    tools.reject_all(super::kernel_tools_errors::kernel_tool_error(super::kernel_tools_errors::KernelToolErrorCode::KernelToolStale,"JavaScript worker reset",None));
                    runs.settle_all("JS kernel reset");calls.clear_tool_calls();deadline=None;
                    match slot.retire().await {
                        Ok(_)=>{
                            reset_responses.push(response);
                            recovery=Some(super::worker_recovery::WorkerRecovery::start(std::mem::take(&mut slot),options.clone(),shutdown.clone()));
                        }
                        Err(error)=>{let _=response.send(Err(error.to_string()));}
                    }
                }
                Some(Command::Close(response))=>{
                    if let Some(starting)=recovery.take() {slot=starting.cancel().await;}
                    if let Some(entry)=host_entry.take() {entry.abort().await;}
                    for (_, (pending, _)) in pending_memory_queries.drain() { let _ = pending.send(Err("JavaScript worker closed".into())); }
                    tools.reject_all(super::kernel_tools_errors::kernel_tool_error(super::kernel_tools_errors::KernelToolErrorCode::KernelToolStale,"JS kernel closed",None));
                    runs.settle_all("JS kernel closed");calls.clear_tool_calls();
                    let _=slot.post_message(&json!({"type":"close"})).await;
                    let result=slot.retire().await.map(|_|()).map_err(|error|error.to_string());
                    let _=response.send(result);break;
                }
                None=>{
                    if let Some(starting)=recovery.take() {slot=starting.cancel().await;}
                    if let Some(entry)=host_entry.take() {entry.abort().await;}
                    runs.settle_all("JS kernel closed");
                    if let Err(error)=slot.retire().await {maho_ext_api::diagnostic(&format!("JS retirement failed: {error}"));}
                    break;
                }
            },
            result=async {match host_entry.as_mut() {Some(entry)=>entry.result().await,None=>std::future::pending().await}},if host_entry.is_some()=>{
                host_entry.take();
                if let Some(mut run)=runs.release_active() {JavaScriptRunQueue::settle(&mut run,result);}
            },
            message=slot.next_message(),if slot.present()=>match message {
                Ok(mut message)=>{
                    if options.isolation == crate::config::feature_settings::JsIsolation::Process
                        && message["type"] == "result"
                        && let Some(bytes)=super::process_memory_host::read_process_footprint(slot.pid()).await
                    {
                        message["memory"]=json!({"liveBytes":bytes,"measure":"footprint"});
                    }
                    if tools.consume(message.clone()) {continue;}
                    if message["type"]=="memory-query-result" {
                        if let Some(request_id)=message["requestId"].as_str() && let Some((response,_))=pending_memory_queries.remove(request_id) {if let Some(bytes)=message["liveBytes"].as_u64() && let Some(memory)=&runs.memory {memory.observe_query(bytes);}let _=response.send(Ok(message));}
                        continue;
                    }
                    if route(message,&mut runs,&mut calls) {deadline=None;}
                }
                Err(error)=>{
                    if let Some(memory)=&mut runs.memory {memory.mark_restarted(super::worker_host::bridge_error(&error)["message"].as_str().unwrap_or("JavaScript worker failed"));}
                    if let Some(entry)=host_entry.take() {entry.abort().await;}
                    for (_, (pending, _)) in pending_memory_queries.drain() { let _ = pending.send(Err(error.to_string())); }
                    if let Some(mut run)=runs.release_active() {JavaScriptRunQueue::settle_crashed(&mut run,&error,origin.elapsed().as_secs_f64()*1000.0);}
                    calls.clear_tool_calls();deadline=None;
                    if let Err(error)=slot.retire().await {maho_ext_api::diagnostic(&format!("JS retirement failed: {error}"));}
                }
            },
            ()=async {match deadline {Some(deadline)=>tokio::time::sleep_until(deadline).await,None=>std::future::pending().await}},if deadline.is_some()=>{
                let duration=runs.active().and_then(|run|run.input.timeout_ms).unwrap_or(0);
                if let Err(error)=stop_active(&mut slot,&mut runs,&mut calls,&tools,&format!("timed out after {duration}ms"),&format!("JS cell timed out after {duration}ms"),duration).await {runs.settle_all(&error.to_string());if let Err(error)=slot.retire().await {maho_ext_api::diagnostic(&format!("JS retirement failed: {error}"));}}
                deadline=None;
            }
            ()=async {let until=pending_memory_queries.values().map(|(_,until)|*until).min();match until {Some(until)=>tokio::time::sleep_until(until).await,None=>std::future::pending().await}},if !pending_memory_queries.is_empty()=>{
                let now=tokio::time::Instant::now();
                let expired=pending_memory_queries.iter().filter_map(|(id,(_,until))|(*until<=now).then_some(id.clone())).collect::<Vec<_>>();
                for id in expired {if let Some((response,_))=pending_memory_queries.remove(&id) {let _=response.send(Err("memory query timed out".into()));}}
            }
        }
    }
    *snapshot.lock().expect("JS queue lock") = (None, vec![]);
    for response in reset_responses {
        let _ = response.send(Err("JavaScript kernel is closed".into()));
    }
    for response in interrupt_responses {
        let _ = response.send(Err("JavaScript kernel is closed".into()));
    }
    *options.cached_memory.lock().expect("JS memory cache lock") = None;
    *pid.lock().expect("JS pid lock") = None;
    tools.reject_all(super::kernel_tools_errors::kernel_tool_error(
        super::kernel_tools_errors::KernelToolErrorCode::KernelToolStale,
        "JS kernel closed",
        None,
    ));
}

#[cfg(test)]
#[path = "context_manager/startup_tests.rs"]
mod startup_tests;

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn turn_time_refusal_leaves_worker_available_for_next_cell() {
        let kernel = JavaScriptKernel::start(
            Path::new(env!("CARGO_MANIFEST_DIR")),
            "source-refusal",
            4,
            None,
        )
        .await
        .expect("kernel");
        let refused = tokio::time::timeout(
            Duration::from_secs(5),
            kernel.run_eval(crate::tool::types::EvalKernelRunInput {
                cell_id: "refused".into(),
                code: "globalThis.refusedMarker = true".into(),
                resolve_at_start: Some(Arc::new(|| Err("unreadable source".into()))),
                ..Default::default()
            }),
        )
        .await
        .expect("bounded refusal")
        .expect("refusal result");
        let after = tokio::time::timeout(
            Duration::from_secs(5),
            kernel.run(
                KernelRunInput {
                    cell_id: "after-refusal".into(),
                    code: "typeof refusedMarker === 'undefined'".into(),
                    timeout_ms: Some(5000),
                },
                |_| {},
            ),
        )
        .await
        .expect("bounded cell")
        .expect("cell result");
        kernel.close().await.expect("close");
        assert_eq!(refused["kernelState"], "not-run");
        assert_eq!(after["valueRepr"], "true");
    }

    #[test]
    fn routed_result_applies_policy_before_settlement() {
        let mut runs = JavaScriptRunQueue::with_memory(
            super::super::kernel_memory_bridge::KernelMemoryBridge::new(
                crate::bridge::protocol::KernelMemoryThresholds {
                    gc_watermark_bytes: 50,
                    notice_bytes: 100,
                    ceiling_bytes: 200,
                },
            ),
        );
        let result = runs.enqueue(
            KernelRunInput {
                cell_id: "memory".into(),
                code: String::new(),
                timeout_ms: None,
            },
            None,
            None,
        );
        runs.start_next(0.0);
        let mut calls = SubprocessRunQueue::default();
        assert!(route(
            json!({"type":"result","cellId":"memory","ok":true,"durationMs":0,"memory":{"liveBytes":200,"measure":"heap","gcRan":true}}),
            &mut runs,
            &mut calls
        ));
        assert_eq!(
            result
                .borrow()
                .as_ref()
                .expect("settled")
                .as_ref()
                .expect("result")["memory"]["overCeiling"],
            true
        );
        let idle = !runs.has_waiting();
        assert!(runs.memory.as_mut().expect("policy").claim_recycle(idle));
    }

    #[test]
    fn internal_child_status_does_not_reach_cell_output() {
        let output = Arc::new(Mutex::new(Vec::new()));
        let observed = output.clone();
        let mut runs = JavaScriptRunQueue::default();
        let _result = runs.enqueue(
            KernelRunInput {
                cell_id: "child".into(),
                code: String::new(),
                timeout_ms: None,
            },
            None,
            Some(Arc::new(move |message| {
                observed.lock().expect("output lock").push(message.clone())
            })),
        );
        runs.start_next(0.0);
        let mut calls = SubprocessRunQueue::default();
        route(
            json!({"type":"status","event":{"op":"child","pid":123,"state":"spawned"}}),
            &mut runs,
            &mut calls,
        );
        assert!(output.lock().expect("output lock").is_empty());
    }

    #[tokio::test]
    async fn host_pump_does_not_own_actor_admission_lifetime() {
        let kernel =
            JavaScriptKernel::start(Path::new(env!("CARGO_MANIFEST_DIR")), "pump-owner", 4, None)
                .await
                .unwrap();
        let owners = kernel.commands.strong_count();
        kernel.close().await.unwrap();
        assert_eq!(
            owners, 1,
            "only the public kernel should retain command admission"
        );
    }

    #[tokio::test]
    async fn cancelled_startup_does_not_admit_a_kernel() {
        let shutdown = maho_ai::utils::abort::AbortController::new();
        shutdown.abort(None);
        let result = JavaScriptKernel::start_with_signal(
            Path::new(env!("CARGO_MANIFEST_DIR")),
            "cancelled-start",
            4,
            None,
            BridgeConnectionConfig {
                port: 1,
                token: "test".into(),
                local_roots: None,
                artifacts_dir: None,
                parallel_pool_width: None,
            },
            Arc::new(|| Ok((vec![], vec![]))),
            shutdown,
        )
        .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn close_consumes_actor_and_reaps_worker() {
        let kernel = JavaScriptKernel::start(
            Path::new(env!("CARGO_MANIFEST_DIR")),
            "joined-close",
            4,
            None,
        )
        .await
        .unwrap();
        kernel.close().await.unwrap();
        assert!(kernel.actor.lock().await.is_none());
        assert!(kernel.pid().is_none());
    }

    #[tokio::test]
    async fn closing_one_kernel_does_not_cancel_session_owner() {
        let owner = maho_ai::utils::abort::AbortController::new();
        let kernel = JavaScriptKernel::start_with_signal(
            Path::new(env!("CARGO_MANIFEST_DIR")),
            "local-close",
            4,
            None,
            BridgeConnectionConfig {
                port: 1,
                token: "test".into(),
                local_roots: None,
                artifacts_dir: None,
                parallel_pool_width: None,
            },
            Arc::new(|| Ok((vec![], vec![]))),
            owner.clone(),
        )
        .await
        .unwrap();
        kernel.close().await.unwrap();
        assert!(!owner.signal().aborted());
        assert!(kernel.pid().is_none());
    }
}
