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
fn truthy(value: Option<&Value>) -> bool {
    match value {
        None | Some(Value::Null) => false,
        Some(Value::Bool(value)) => *value,
        Some(Value::Number(value)) => value.as_f64().is_some_and(|number| number != 0.0),
        Some(Value::String(value)) => !value.is_empty(),
        Some(Value::Array(_) | Value::Object(_)) => true,
    }
}

#[derive(Default)]
pub struct HerdrAgentState;
impl Extension for HerdrAgentState {
    fn register(&self, api: &mut ExtensionApi) {
        let Some(config) = Config::from_env() else { return; };
        let delivery = Arc::new(Delivery::new(config));
        let runtime = Arc::new(Mutex::new(Runtime::new()));
        let subscriptions = Arc::new(Mutex::new(Vec::<BusSubscription>::new()));
        let retained = Arc::clone(&subscriptions);
        api.on(EventKind::SessionShutdown, Arc::new(move |_, _| {
            retained.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clear();
            Box::pin(async { Ok(EventResult::None) })
        }));
        for kind in [EventKind::SessionStart, EventKind::AgentStart, EventKind::AgentSettled] {
            let runtime = Arc::clone(&runtime);
            let delivery = Arc::clone(&delivery);
            api.on(kind, Arc::new(move |event, ctx| {
                let runtime = Arc::clone(&runtime);
                let delivery = Arc::clone(&delivery);
                Box::pin(async move {
                    if kind == EventKind::SessionStart && ctx.mode == ExtensionMode::Tui {
                        let request = {
                            let mut runtime = runtime.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                            runtime.state.root_session = true;
                            runtime.update_reference(ctx);
                            let reason = match event { ExtensionEvent::SessionStart(start) => Some(format!("{:?}", start.reason).to_lowercase()), _ => None };
                            runtime.reference.is_some().then(|| runtime.request(&delivery.config, "pane.report_agent_session", reason.as_deref()))
                        };
                        if let Some(request) = request {
                            let config = Arc::clone(&delivery.config);
                            tokio::task::spawn_blocking(move || config.send(&request)).await.map_err(|error| maho_ext_api::ExtensionFailure::new(error.to_string()))?;
                        }
                        let mut runtime = runtime.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                        runtime.state.agent_active = !(ctx.is_idle_fn)();
                        runtime.publish(&delivery, true);
                        return Ok(EventResult::None);
                    }
                    let mut runtime = runtime.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                    if kind == EventKind::AgentStart && runtime.state.root_session {
                        runtime.update_reference(ctx);
                        if runtime.reference.is_some() {
                            let request = runtime.request(&delivery.config, "pane.report_agent_session", None);
                            let config = Arc::clone(&delivery.config);
                            tokio::task::spawn_blocking(move || config.send(&request));
                        }
                        runtime.state.agent_active = true;
                        runtime.publish(&delivery, false);
                    } else if kind == EventKind::AgentSettled && runtime.state.root_session && (ctx.is_idle_fn)() {
                        runtime.state.agent_active = false;
                        runtime.publish(&delivery, false);
                    }
                    Ok(EventResult::None)
                })
            }));
        }
        let subscription = api.events.on("herdr:blocked", Arc::new(move |data| {
            let mut runtime = runtime.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            runtime.state.blocked(truthy(data.get("active")), data.get("label").and_then(Value::as_str));
            runtime.publish(&delivery, false);
        }));
        subscriptions.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push(subscription);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn inactive_when_falsy() { for value in [Value::Null, json!(false), json!(0), json!("")] { assert!(!truthy(Some(&value))); } assert!(!truthy(None)); }
    #[test] fn active_when_truthy() { for value in [json!(true), json!(1), json!("active"), json!([]), json!({})] { assert!(truthy(Some(&value))); } }
    #[cfg(unix)]
    #[test]
    fn delivered_when_unix_peer_acknowledges() {
        use std::io::BufRead;
        let directory = tempfile::tempdir().expect("create socket fixture");
        let socket = directory.path().join("herdr.sock");
        let listener = std::os::unix::net::UnixListener::bind(&socket).expect("bind fixture socket");
        let peer = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept request");
            stream.set_read_timeout(Some(Duration::from_secs(2))).expect("bound request read");
            let mut text = String::new();
            std::io::BufReader::new(stream.try_clone().expect("clone stream")).read_line(&mut text).expect("read framed request");
            stream.write_all(b"ok\n").expect("acknowledge request");
            text
        });
        let config = Config { socket: socket.to_string_lossy().into_owned(), pane: "fixture".into() };
        assert!(config.attempt("{\"method\":\"pane.report_agent\"}\n", 500).expect("deliver request"));
        assert_eq!(peer.join().expect("join peer"), "{\"method\":\"pane.report_agent\"}\n");
    }
    #[cfg(unix)]
    #[test]
    fn retried_when_first_peer_closes_without_ack() {
        use std::io::BufRead;
        let directory = tempfile::tempdir().expect("create socket fixture");
        let socket = directory.path().join("herdr.sock");
        let listener = std::os::unix::net::UnixListener::bind(&socket).expect("bind fixture socket");
        let peer = std::thread::spawn(move || {
            let mut requests = Vec::new();
            for attempt in 0..2 {
                let (mut stream, _) = listener.accept().expect("accept retry");
                stream.set_read_timeout(Some(Duration::from_secs(2))).expect("bound retry read");
                let mut text = String::new();
                std::io::BufReader::new(stream.try_clone().expect("clone stream")).read_line(&mut text).expect("read retry request");
                requests.push(text);
                if attempt == 1 { stream.write_all(b"ok\n").expect("acknowledge retry"); }
            }
            requests
        });
        let config = Config { socket: socket.to_string_lossy().into_owned(), pane: "fixture".into() };
        let request = json!({"method":"pane.report_agent"});
        config.send(&request);
        assert_eq!(peer.join().expect("join retry peer"), vec![format!("{request}\n"); 2]);
    }
}
