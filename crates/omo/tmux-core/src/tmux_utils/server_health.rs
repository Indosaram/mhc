//! OpenCode server health check with a per-URL success cache.

use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

const HEALTH_TIMEOUT: Duration = Duration::from_millis(3000);
const MAX_ATTEMPTS: u32 = 2;
const RETRY_DELAY: Duration = Duration::from_millis(250);

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ServerHealthState {
    pub server_available: Option<bool>,
    pub server_check_url: Option<String>,
    pub server_running_in_process: bool,
}

/// Fetch a health URL; `Ok(status)` on any HTTP response, `Err` when unreachable.
pub type FetchFn = Arc<dyn Fn(&str, Duration) -> Result<u16, String> + Send + Sync>;

#[derive(Default)]
pub struct IsServerRunningOptions<'a> {
    pub fetch_implementation: Option<FetchFn>,
    /// Explicit state; `None` uses the process-global cache.
    pub state: Option<&'a mut ServerHealthState>,
}

static GLOBAL_STATE: Mutex<ServerHealthState> = Mutex::new(ServerHealthState {
    server_available: None,
    server_check_url: None,
    server_running_in_process: false,
});

fn global() -> std::sync::MutexGuard<'static, ServerHealthState> {
    GLOBAL_STATE.lock().unwrap_or_else(PoisonError::into_inner)
}

pub fn mark_server_running_in_process() {
    global().server_running_in_process = true;
}

#[must_use]
pub fn is_server_marked_running_in_process() -> bool {
    global().server_running_in_process
}

#[must_use]
pub fn create_server_health_state() -> ServerHealthState {
    ServerHealthState::default()
}

#[must_use]
pub fn create_server_health_state_for_testing() -> ServerHealthState {
    create_server_health_state()
}

pub fn reset_server_check() {
    *global() = ServerHealthState::default();
}

fn is_ok_status(status: u16) -> bool {
    (200..300).contains(&status)
}

/// Default fetcher: a plain HTTP/1.1 `GET` (only `http://` URLs are supported).
fn http_get_status(target: &str, timeout: Duration) -> Result<u16, String> {
    let parsed = url::Url::parse(target).map_err(|error| error.to_string())?;
    if parsed.scheme() != "http" {
        return Err(format!("unsupported scheme: {}", parsed.scheme()));
    }
    let host = parsed.host_str().ok_or("missing host")?;
    let port = parsed.port_or_known_default().ok_or("missing port")?;
    let address = (host.trim_start_matches('[').trim_end_matches(']'), port)
        .to_socket_addrs()
        .map_err(|error| error.to_string())?
        .next()
        .ok_or("unresolved host")?;
    let mut stream =
        TcpStream::connect_timeout(&address, timeout).map_err(|error| error.to_string())?;
    stream
        .set_read_timeout(Some(timeout))
        .map_err(|error| error.to_string())?;
    stream
        .set_write_timeout(Some(timeout))
        .map_err(|error| error.to_string())?;
    let request = format!(
        "GET {} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\n\r\n",
        parsed.path(),
        &parsed[url::Position::BeforeHost..url::Position::AfterPort],
    );
    stream
        .write_all(request.as_bytes())
        .map_err(|error| error.to_string())?;
    let mut head = [0_u8; 64];
    let read = stream.read(&mut head).map_err(|error| error.to_string())?;
    let status_line = String::from_utf8_lossy(&head[..read]);
    status_line
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse().ok())
        .ok_or_else(|| "malformed HTTP response".to_owned())
}

fn health_url(server_url: &str) -> Option<String> {
    url::Url::parse(server_url)
        .and_then(|base| base.join("/global/health"))
        .ok()
        .map(String::from)
}

/// Probe `<serverUrl>/global/health` (2 attempts, 3 s timeout each, 250 ms apart).
/// A successful probe is cached per URL; failures are never cached.
pub fn is_server_running(server_url: &str, options: &mut IsServerRunningOptions<'_>) -> bool {
    let marked_running = options
        .state
        .as_ref()
        .map_or_else(is_server_marked_running_in_process, |state| {
            state.server_running_in_process
        });
    if marked_running {
        return true;
    }

    let (cached_url, cached_available) = {
        let global = global();
        let state = options.state.as_deref();
        (
            state
                .and_then(|state| state.server_check_url.clone())
                .or_else(|| global.server_check_url.clone()),
            state
                .and_then(|state| state.server_available)
                .or(global.server_available),
        )
    };
    if cached_url.as_deref() == Some(server_url) && cached_available == Some(true) {
        return true;
    }

    let Some(health) = health_url(server_url) else {
        return false;
    };
    let fetch: FetchFn = options
        .fetch_implementation
        .clone()
        .unwrap_or_else(|| Arc::new(http_get_status));

    for attempt in 1..=MAX_ATTEMPTS {
        if fetch(&health, HEALTH_TIMEOUT).is_ok_and(is_ok_status) {
            match options.state.as_deref_mut() {
                Some(state) => {
                    state.server_check_url = Some(server_url.to_owned());
                    state.server_available = Some(true);
                }
                None => {
                    let mut global = global();
                    global.server_check_url = Some(server_url.to_owned());
                    global.server_available = Some(true);
                }
            }
            return true;
        }
        if attempt < MAX_ATTEMPTS {
            std::thread::sleep(RETRY_DELAY);
        }
    }
    false
}
