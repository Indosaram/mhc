use crate::{State, session_reference};
use maho_ext_api::{BusSubscription, EventKind, EventResult, Extension, ExtensionApi, ExtensionContext, ExtensionEvent, ExtensionMode};
use serde_json::{Value, json};
use std::{io::{Read, Write}, sync::{Arc, Mutex, Condvar}, thread::JoinHandle, time::{Duration, SystemTime, UNIX_EPOCH}};

struct Config { socket: String, pane: String }
impl Config {
    fn from_env() -> Option<Self> {
        if std::env::var("HERDR_ENV").ok().as_deref() != Some("1") { return None; }
        Some(Self { socket: std::env::var("HERDR_SOCKET_PATH").ok().filter(|value| !value.is_empty())?, pane: std::env::var("HERDR_PANE_ID").ok().filter(|value| !value.is_empty())? })
    }
    fn attempt(&self, payload: &str, timeout_ms: u64) -> std::io::Result<bool> {
        #[cfg(unix)]
        {
            let mut stream = std::os::unix::net::UnixStream::connect(&self.socket)?;
            let timeout = Some(Duration::from_millis(timeout_ms));
            stream.set_write_timeout(timeout)?;
            stream.set_read_timeout(timeout)?;
            stream.write_all(payload.as_bytes())?;
            let mut buffer = [0; 1024];
            Ok(stream.read(&mut buffer)? > 0)
        }
        #[cfg(not(unix))]
        { let _ = (payload, timeout_ms); Err(std::io::Error::other("Herdr named pipe transport not yet ported")) }
    }
    fn send(&self, request: &Value) {
        let payload = format!("{request}\n");
        if matches!(self.attempt(&payload, 500), Ok(true)) { return; }
        let _delivery = self.attempt(&payload, 1500);
    }
}

#[derive(Default)]
struct Queue { pending: Option<Value>, shutdown: bool }
struct Delivery { config: Arc<Config>, queue: Arc<(Mutex<Queue>, Condvar)>, thread: Mutex<Option<JoinHandle<()>>> }
impl Delivery {
    fn new(config: Config) -> Self {
        let config = Arc::new(config);
        let queue = Arc::new((Mutex::new(Queue::default()), Condvar::new()));
        let worker_queue = Arc::clone(&queue);
        let worker_config = Arc::clone(&config);
        let thread = std::thread::spawn(move || loop {
            let (lock, signal) = &*worker_queue;
            let mut queue = lock.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            while queue.pending.is_none() && !queue.shutdown { queue = signal.wait(queue).unwrap_or_else(std::sync::PoisonError::into_inner); }
            let pending = queue.pending.take();
            if pending.is_none() && queue.shutdown { break; }
            drop(queue);
            if let Some(request) = pending { worker_config.send(&request); }
        });
        Self { config, queue, thread: Mutex::new(Some(thread)) }
    }
    fn queue(&self, request: Value) {
        let (lock, signal) = &*self.queue;
        lock.lock().unwrap_or_else(std::sync::PoisonError::into_inner).pending = Some(request);
        signal.notify_one();
    }
}
impl Drop for Delivery {
    fn drop(&mut self) {
        let (lock, signal) = &*self.queue;
        lock.lock().unwrap_or_else(std::sync::PoisonError::into_inner).shutdown = true;
        signal.notify_one();
        if let Some(thread) = self.thread.get_mut().unwrap_or_else(std::sync::PoisonError::into_inner).take()
            && thread.join().is_err() { eprintln!("Herdr delivery thread panicked"); }
    }
}

struct Runtime { state: State, seq: u128, reference: Option<(&'static str, String)> }
impl Runtime {
    fn new() -> Self { Self { state: State::default(), seq: now_ms().saturating_mul(1000), reference: None } }
    fn update_reference(&mut self, ctx: &ExtensionContext) {
        let path = ctx.session_manager.session_file().map(|path| path.to_string_lossy().into_owned());
        self.reference = session_reference(path.as_deref(), Some(ctx.session_manager.session_id()));
    }
    fn request(&mut self, config: &Config, method: &str, reason: Option<&str>) -> Value {
        self.seq = self.seq.saturating_add(1);
        let mut params = json!({"pane_id":config.pane,"source":"herdr:pi","agent":"pi","seq":self.seq});
        if let Some((key, value)) = &self.reference { params[*key] = json!(value); }
        if let Some(reason) = reason { params["session_start_source"] = json!(reason); }
        json!({"id":format!("herdr:pi:{}:{}", now_ms(), self.seq),"method":method,"params":params})
    }
    fn publish(&mut self, delivery: &Delivery, force: bool) {
        let Some((state, message)) = self.state.publish(force) else { return; };
        let mut request = self.request(&delivery.config, "pane.report_agent", None);
        request["params"]["state"] = json!(state.as_str());
        if let Some(message) = message { request["params"]["message"] = json!(message); }
        delivery.queue(request);
    }
}
fn now_ms() -> u128 { SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |value| value.as_millis()) }

#[derive(Default)]
pub struct HerdrAgentState { subscriptions: Mutex<Vec<BusSubscription>> }
impl Extension for HerdrAgentState {
    fn register(&self, api: &mut ExtensionApi) {
        let Some(config) = Config::from_env() else { return; };
        let delivery = Arc::new(Delivery::new(config));
        let runtime = Arc::new(Mutex::new(Runtime::new()));
        for kind in [EventKind::SessionStart, EventKind::AgentStart, EventKind::AgentSettled] {
            let runtime = Arc::clone(&runtime);
            let delivery = Arc::clone(&delivery);
            api.on(kind, Arc::new(move |event, ctx| {
                {
                    let mut runtime = runtime.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                    if kind == EventKind::SessionStart && ctx.mode == ExtensionMode::Tui {
                        runtime.state.root_session = true;
                        runtime.update_reference(ctx);
                        if runtime.reference.is_some() {
                            let reason = match event { ExtensionEvent::SessionStart(start) => Some(format!("{:?}", start.reason).to_lowercase()), _ => None };
                            let request = runtime.request(&delivery.config, "pane.report_agent_session", reason.as_deref());
                            delivery.config.send(&request);
                        }
                        runtime.state.agent_active = !(ctx.is_idle_fn)();
                        runtime.publish(&delivery, true);
                    } else if kind == EventKind::AgentStart && runtime.state.root_session {
                        runtime.update_reference(ctx);
                        runtime.state.agent_active = true;
                        runtime.publish(&delivery, false);
                    } else if kind == EventKind::AgentSettled && runtime.state.root_session && (ctx.is_idle_fn)() {
                        runtime.state.agent_active = false;
                        runtime.publish(&delivery, false);
                    }
                }
                Box::pin(async { Ok(EventResult::None) })
            }));
        }
        let subscription = api.events.on("herdr:blocked", Arc::new(move |data| {
            let mut runtime = runtime.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            runtime.state.blocked(data.get("active").and_then(Value::as_bool).unwrap_or(false), data.get("label").and_then(Value::as_str));
            runtime.publish(&delivery, false);
        }));
        self.subscriptions.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push(subscription);
    }
}
