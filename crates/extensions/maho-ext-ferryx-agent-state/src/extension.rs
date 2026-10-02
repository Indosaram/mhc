use crate::State;
use maho_ext_api::{BusSubscription, EventKind, EventResult, Extension, ExtensionApi, ExtensionContext, ExtensionMode};
use serde_json::{Value, json};
use std::{io::Write, net::{SocketAddr, TcpStream}, sync::{Arc, Mutex, mpsc}, thread::JoinHandle, time::Duration};

struct Config { socket: Option<String>, port: Option<u16>, token: Option<String>, session_id: String }
fn state_port(value: &str) -> Option<u16> {
    let number = maho_ai::utils::js::string_to_number(value);
    if !number.is_finite() || number.fract() != 0.0 || number <= 0.0 || number > 65535.0 { return None; }
    number.to_string().parse().ok()
}
impl Config {
    fn from_env() -> Option<Self> {
        let socket = std::env::var("FERRYX_AGENT_STATE_SOCKET").ok().filter(|value| !value.is_empty());
        let token = std::env::var("FERRYX_AGENT_STATE_TOKEN").ok().filter(|value| !value.is_empty());
        let port = std::env::var("FERRYX_AGENT_STATE_PORT").ok().and_then(|value| state_port(&value)).filter(|_| token.is_some());
        let session_id = std::env::var("FERRYX_SESSION_ID").ok().filter(|value| !value.is_empty())?;
        if socket.is_none() && port.is_none() { return None; }
        Some(Self { socket, port, token, session_id })
    }
    fn deliver(&self, payload: &str) -> std::io::Result<()> {
        let timeout = Duration::from_millis(1000);
        if let Some(port) = self.port {
            let mut stream = TcpStream::connect_timeout(&SocketAddr::from(([127, 0, 0, 1], port)), timeout)?;
            stream.set_write_timeout(Some(timeout))?;
            return stream.write_all(payload.as_bytes());
        }
        #[cfg(unix)]
        if let Some(path) = &self.socket {
            let mut stream = socket2::Socket::new(socket2::Domain::UNIX, socket2::Type::STREAM, None)?;
            stream.connect_timeout(&socket2::SockAddr::unix(path)?, timeout)?;
            stream.set_write_timeout(Some(timeout))?;
            return stream.write_all(payload.as_bytes());
        }
        Err(std::io::Error::other("Ferryx socket unavailable"))
    }
}

struct Delivery { config: Arc<Config>, sender: Mutex<Option<mpsc::Sender<String>>>, thread: Mutex<Option<JoinHandle<()>>> }
impl Delivery {
    fn new(config: Config) -> Self {
        let config = Arc::new(config);
        let worker_config = Arc::clone(&config);
        let (sender, receiver) = mpsc::channel::<String>();
        let thread = std::thread::spawn(move || {
            for payload in receiver {
                if let Err(error) = worker_config.deliver(&payload) { let _delivery_error = error; }
            }
        });
        Self { config, sender: Mutex::new(Some(sender)), thread: Mutex::new(Some(thread)) }
    }
    fn send(&self, state: &mut State, force: bool, provider: Option<Value>) {
        let id = provider.as_ref().and_then(|value| value.get("id")).and_then(Value::as_str);
        let Some(next) = state.publish(force, id) else { return; };
        let mut payload = json!({"type":"agentState","sessionId":self.config.session_id,"state":next.as_str(),"agent":"omo"});
        if let Some(provider) = provider { payload["providerSession"] = provider; }
        if let Some(detail) = state.blocked_detail() { payload["detail"] = json!(detail); }
        if self.config.port.is_some() { payload["token"] = json!(self.config.token); }
        if let Some(sender) = self.sender.lock().unwrap_or_else(std::sync::PoisonError::into_inner).as_ref()
            && let Err(error) = sender.send(format!("{payload}\n")) { eprintln!("Ferryx delivery queue: {error}"); }
    }
}
impl Drop for Delivery {
    fn drop(&mut self) {
        self.sender.get_mut().unwrap_or_else(std::sync::PoisonError::into_inner).take();
        if let Some(thread) = self.thread.get_mut().unwrap_or_else(std::sync::PoisonError::into_inner).take()
            && thread.join().is_err() { eprintln!("Ferryx delivery thread panicked"); }
    }
}

fn provider_session(ctx: &ExtensionContext) -> Option<Value> {
    let id = ctx.session_manager.session_id();
    let (id, path) = if id.is_empty() {
        (std::env::var("PI_SESSION_ID").ok().filter(|value| !value.is_empty())?, std::env::var("PI_SESSION_FILE").ok())
    } else { (id.to_owned(), ctx.session_manager.session_file().map(|path| path.to_string_lossy().into_owned())) };
    let mut value = json!({"key":"session_id","id":id});
    if let Some(path) = path.filter(|value| !value.is_empty()) { value["transcriptPath"] = json!(path); }
    Some(value)
}

#[derive(Default)]
pub struct FerryxAgentState;
impl Extension for FerryxAgentState {
    fn register(&self, api: &mut ExtensionApi) {
        let Some(config) = Config::from_env() else { return; };
        let delivery = Arc::new(Delivery::new(config));
        let state = Arc::new(Mutex::new(State::default()));
        let subscriptions = Arc::new(Mutex::new(Vec::<BusSubscription>::new()));
        let retained = Arc::clone(&subscriptions);
        api.on(EventKind::SessionShutdown, Arc::new(move |_, _| {
            retained.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clear();
            Box::pin(async { Ok(EventResult::None) })
        }));
        for kind in [EventKind::SessionStart, EventKind::AgentStart, EventKind::AgentSettled] {
            let state = Arc::clone(&state);
            let delivery = Arc::clone(&delivery);
            api.on(kind, Arc::new(move |_, ctx| {
                {
                    let mut state = state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                    match kind {
                        EventKind::SessionStart if ctx.mode == ExtensionMode::Tui => { state.root_session = true; state.agent_active = !(ctx.is_idle_fn)(); delivery.send(&mut state, true, provider_session(ctx)); }
                        EventKind::AgentStart if state.root_session => { state.agent_active = true; delivery.send(&mut state, false, provider_session(ctx)); }
                        EventKind::AgentSettled if state.root_session && (ctx.is_idle_fn)() => { state.agent_active = false; delivery.send(&mut state, false, provider_session(ctx)); }
                        _ => {}
                    }
                }
                Box::pin(async { Ok(EventResult::None) })
            }));
        }
        for channel in ["ask-user:asked", "herdr:blocked"] {
            let state = Arc::clone(&state);
            let delivery = Arc::clone(&delivery);
            let subscription = api.events.on(channel, Arc::new(move |data| {
                let mut state = state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                let changed = if channel == "ask-user:asked" { state.asked(data) } else { state.blocked(data) };
                if changed { delivery.send(&mut state, false, None); }
            }));
            subscriptions.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push(subscription);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::BufRead;
    #[test]
    fn port_accepts_javascript_integer_number_forms() {
        for value in [" 1234 ", "1234.0", "1.234e3", "+1234", "0x4d2", "0o2322", "0b10011010010", "\u{feff}1234\u{feff}", ".1234e4", "1234."] { assert_eq!(state_port(value), Some(1234), "{value}"); }
        for value in ["", "0", "-1", "65536", "1.2", "NaN", "Infinity", "port", "1234junk", "\u{0085}1234", "+0x4d2", "0x+4d2", "1_234", "inf"] { assert_eq!(state_port(value), None, "{value}"); }
    }
    #[test]
    fn tcp_receives_framed_state_payload() {
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).expect("bind fixture listener");
        let port = listener.local_addr().expect("listener address").port();
        let peer = std::thread::spawn(move || {
            let (stream, _) = listener.accept().expect("accept payload");
            stream.set_read_timeout(Some(Duration::from_secs(2))).expect("bound payload read");
            let mut text = String::new();
            std::io::BufReader::new(stream).read_line(&mut text).expect("read framed payload");
            serde_json::from_str::<Value>(&text).expect("parse state payload")
        });
        let delivery = Delivery::new(Config { socket: None, port: Some(port), token: Some("fixture-token".into()), session_id: "fixture-session".into() });
        let mut state = State { root_session: true, ..Default::default() };
        delivery.send(&mut state, true, Some(json!({"key":"session_id","id":"provider-session"})));
        let payload = peer.join().expect("join fixture peer");
        assert_eq!(payload["type"], "agentState");
        assert_eq!(payload["sessionId"], "fixture-session");
        assert_eq!(payload["state"], "idle");
        assert_eq!(payload["token"], "fixture-token");
        assert_eq!(payload["providerSession"]["id"], "provider-session");
        drop(delivery);
    }
    #[cfg(unix)]
    #[test]
    fn unix_receives_payload_without_tcp_token() {
        let fixture = tempfile::tempdir().expect("Unix delivery fixture");
        let path = fixture.path().join("state.sock");
        let listener = std::os::unix::net::UnixListener::bind(&path).expect("bind Unix fixture");
        let peer = std::thread::spawn(move || {
            let (stream, _) = listener.accept().expect("accept Unix payload");
            stream.set_read_timeout(Some(Duration::from_secs(2))).expect("bound payload read");
            let mut text = String::new();
            std::io::BufReader::new(stream).read_line(&mut text).expect("read Unix frame");
            serde_json::from_str::<Value>(&text).expect("parse Unix payload")
        });
        let delivery = Delivery::new(Config { socket: Some(path.to_string_lossy().into_owned()), port: None, token: Some("ignored".into()), session_id: "fixture-session".into() });
        delivery.send(&mut State::default(), true, None);
        let payload = peer.join().expect("join Unix peer");
        assert_eq!(payload["state"], "idle");
        assert!(payload.get("token").is_none());
        assert!(payload.get("providerSession").is_none());
        drop(delivery);
    }
}
