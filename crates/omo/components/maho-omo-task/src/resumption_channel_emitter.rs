use std::sync::Arc;
use maho_ext_api::EventBus;
use serde_json::{json, Value};
use senpi_task::state::TaskRecord;
use crate::status_row_format::task_status_description;

pub const RESUMPTION_CHANNEL_STATE_EVENT: &str = "wake_source_state";

pub trait ResumptionChannelManager: Send + Sync {
    fn list(&self, session_id: &str) -> Vec<TaskRecord>;
    fn was_background(&self, task_id: &str) -> bool;
    fn is_owned_team_member(&self, record: &TaskRecord, session_id: &str) -> bool;
    fn resolve_owned_team_member<'a>(&'a self, record: &'a TaskRecord, session_id: &'a str) -> std::pin::Pin<Box<dyn std::future::Future<Output = bool> + Send + 'a>> {
        Box::pin(async move { self.is_owned_team_member(record, session_id) })
    }
}
pub struct TaskResumptionChannelManager {
    pub manager: Arc<senpi_task::manager::TaskManager>,
    pub ownership: senpi_task::team::liveness_ownership::TeamMemberOwnershipDeps,
}

#[derive(Clone, Copy)]
enum Emission { Start, Changed, Shutdown }
struct QueuedState { last_count: usize, tail: Option<tokio::sync::oneshot::Receiver<()>> }
pub struct QueuedResumptionChannelEmitter {
    events: EventBus,
    manager: Arc<dyn ResumptionChannelManager>,
    session_id: Arc<dyn Fn() -> Option<String> + Send + Sync>,
    active: std::sync::atomic::AtomicBool,
    state: std::sync::Mutex<QueuedState>,
}
struct Completion(Option<tokio::sync::oneshot::Sender<()>>);
impl Drop for Completion { fn drop(&mut self) { if let Some(sender) = self.0.take() { let _ = sender.send(()); } } }
type EmissionFuture = std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>;
struct EmissionJob { future: EmissionFuture, completed: tokio::sync::oneshot::Sender<()> }
struct EmissionWorker { sender: std::sync::mpsc::Sender<EmissionJob>, stop: Arc<tokio::sync::Notify>, thread: std::thread::JoinHandle<()> }
pub struct OwnedResumptionChannels {
    emitter: Arc<QueuedResumptionChannelEmitter>,
    worker: std::sync::Mutex<Option<EmissionWorker>>,
}
impl OwnedResumptionChannels {
    pub fn new(events: EventBus, manager: Arc<dyn ResumptionChannelManager>, session_id: Arc<dyn Fn() -> Option<String> + Send + Sync>) -> Result<Self, std::io::Error> {
        let runtime = tokio::runtime::Builder::new_current_thread().build()?;
        let (sender, receiver) = std::sync::mpsc::channel::<EmissionJob>();
        let emitter = QueuedResumptionChannelEmitter::new(events, manager, session_id);
        let shutdown = emitter.clone(); let stop = Arc::new(tokio::sync::Notify::new()); let stopping = stop.clone();
        let thread = std::thread::Builder::new().name("task-resumption".into()).spawn(move || {
            for job in &receiver {
                let completed = runtime.block_on(async { tokio::select! { biased; () = stopping.notified() => false, () = job.future => true } });
                if !completed { break; }
                let _ = job.completed.send(());
            }
            for job in receiver.try_iter() { drop(job); }
            runtime.block_on(shutdown.emit_shutdown());
        })?;
        Ok(Self { emitter, worker: std::sync::Mutex::new(Some(EmissionWorker { sender, stop, thread })) })
    }
    fn submit(&self, emission: Emission) -> tokio::sync::oneshot::Receiver<()> {
        let (completed, receiver) = tokio::sync::oneshot::channel();
        let worker = self.worker.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(worker) = worker.as_ref() {
            let future = match emission { Emission::Start => self.emitter.emit_session_start(), Emission::Changed => self.emitter.emit_if_changed(), Emission::Shutdown => self.emitter.emit_shutdown() };
            if worker.sender.send(EmissionJob { future, completed }).is_err() { eprintln!("task resumption worker stopped before emission"); }
        }
        receiver
    }
    pub async fn emit_session_start(&self) -> Result<(), String> { self.submit(Emission::Start).await.map_err(|error| error.to_string()) }
    pub async fn emit_shutdown(&self) -> Result<(), String> { self.submit(Emission::Shutdown).await.map_err(|error| error.to_string()) }
    pub fn emit_if_changed(&self) { drop(self.submit(Emission::Changed)); }
    pub fn dispose(&self) {
        let worker = self.worker.lock().unwrap_or_else(std::sync::PoisonError::into_inner).take();
        if let Some(worker) = worker {
            worker.stop.notify_one(); drop(worker.sender);
            if worker.thread.join().is_err() { eprintln!("task resumption worker panicked"); }
        }
    }
}
impl Drop for OwnedResumptionChannels { fn drop(&mut self) { self.dispose(); } }
impl QueuedResumptionChannelEmitter {
    pub fn new(events: EventBus, manager: Arc<dyn ResumptionChannelManager>, session_id: Arc<dyn Fn() -> Option<String> + Send + Sync>) -> Arc<Self> {
        Arc::new(Self { events, manager, session_id, active: std::sync::atomic::AtomicBool::new(false), state: std::sync::Mutex::new(QueuedState { last_count: 0, tail: None }) })
    }
    pub fn emit_session_start(self: &Arc<Self>) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>> {
        self.active.store(true, std::sync::atomic::Ordering::SeqCst); self.enqueue(Emission::Start)
    }
    pub fn emit_if_changed(self: &Arc<Self>) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>> { self.enqueue(Emission::Changed) }
    pub fn emit_shutdown(self: &Arc<Self>) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>> {
        self.active.store(false, std::sync::atomic::Ordering::SeqCst); self.enqueue(Emission::Shutdown)
    }
    fn enqueue(self: &Arc<Self>, emission: Emission) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>> {
        let (sender, receiver) = tokio::sync::oneshot::channel();
        let previous = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner).tail.replace(receiver);
        let completion = Completion(Some(sender)); let emitter = self.clone();
        Box::pin(async move {
            let _completion = completion;
            if let Some(previous) = previous { let _ = previous.await; }
            if matches!(emission, Emission::Changed) && !emitter.active.load(std::sync::atomic::Ordering::SeqCst) { return; }
            let mut channels = Vec::new();
            if !matches!(emission, Emission::Shutdown) && let Some(session) = (emitter.session_id)() {
                for record in emitter.manager.list(&session) {
                    if record.status.is_terminal() { continue; }
                    if !emitter.manager.was_background(&record.task_id) && !emitter.manager.resolve_owned_team_member(&record, &session).await { continue; }
                    let started = chrono::DateTime::parse_from_rfc3339(&record.created_at).map_or(Value::Null, |date| json!(date.timestamp_millis()));
                    channels.push(json!({"id":record.task_id,"description":task_status_description(&record),"startedAtMs":started}));
                }
            }
            {
                let mut state = emitter.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                if matches!(emission, Emission::Changed) && channels.len() == state.last_count { return; }
                state.last_count = channels.len();
            }
            emitter.events.emit(RESUMPTION_CHANNEL_STATE_EVENT, &json!({"source":"senpi-task","activeCount":channels.len(),"channels":channels}));
        })
    }
}
impl ResumptionChannelManager for TaskResumptionChannelManager {
    fn list(&self,session:&str)->Vec<TaskRecord> {
        self.manager.list(&senpi_task::manager::types::ListScope::ParentSession(session.into())).into_iter().map(|entry| entry.record).collect()
    }
    fn was_background(&self,id:&str)->bool { self.manager.was_background(id) }
    fn is_owned_team_member(&self,record:&TaskRecord,session:&str)->bool {
        senpi_task::team::liveness_ownership::is_owned_team_member_task(record.name.as_deref(),Some(session),&self.ownership)
    }
}

pub struct ResumptionChannelEmitter {
    pub events: EventBus,
    pub manager: Arc<dyn ResumptionChannelManager>,
    pub session_id: Arc<dyn Fn() -> Option<String> + Send + Sync>,
    active: bool,
    last_count: usize,
}

impl ResumptionChannelEmitter {
    pub fn new(events: EventBus, manager: Arc<dyn ResumptionChannelManager>,
        session_id: Arc<dyn Fn() -> Option<String> + Send + Sync>) -> Self {
        Self { events, manager, session_id, active: false, last_count: 0 }
    }

    fn snapshot(&self) -> Vec<Value> {
        let Some(session) = (self.session_id)() else { return vec![] };
        self.manager.list(&session).into_iter().filter(|record| !record.status.is_terminal())
            .filter(|record| self.manager.was_background(&record.task_id)
                || self.manager.is_owned_team_member(record, &session))
            .map(|record| {
                let started = chrono::DateTime::parse_from_rfc3339(&record.created_at)
                    .map_or(Value::Null, |date| json!(date.timestamp_millis()));
                json!({"id": record.task_id, "description": task_status_description(&record), "startedAtMs": started})
            }).collect()
    }

    fn publish(&mut self, channels: Vec<Value>) {
        self.last_count = channels.len();
        self.events.emit(RESUMPTION_CHANNEL_STATE_EVENT,
            &json!({"source":"senpi-task", "activeCount":channels.len(), "channels":channels}));
    }

    pub fn emit_if_changed(&mut self) {
        if !self.active { return; }
        let channels = self.snapshot();
        if channels.len() != self.last_count { self.publish(channels); }
    }

    pub fn emit_session_start(&mut self) {
        self.active = true;
        self.publish(self.snapshot());
    }

    pub fn emit_shutdown(&mut self) {
        self.active = false;
        self.publish(vec![]);
    }
}
