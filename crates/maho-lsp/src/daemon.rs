//! The `mhc-lsp` mux daemon (unix).
//!
//! One daemon per user listens on a unix socket. A connection ("link") first sends
//! `mhcLsp/connect` with the server identity (command, args, cwd, env, initialization options);
//! the daemon binds the link to an idle server process with the same identity or spawns one, and
//! from then on the link carries plain LSP traffic for that server. A reused server answers
//! `initialize` from its cached result; `shutdown`/`exit` from a link only release the server,
//! which then lingers idle for reuse until the idle timeout. Documents a link opened are closed
//! when it detaches. `mhcLsp/ping` answers `pong` without binding.
//!
//! Design after the MIT oh-my-pi LSP mux (`packages/coding-agent/src/lsp/mux`); see PROVENANCE.md.
use crate::client::LspClient;
use crate::rpc::{Connection, read_frame, write_frame};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::io::{AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::mpsc;

pub const CONNECT_METHOD: &str = "mhcLsp/connect";
pub const PING_METHOD: &str = "mhcLsp/ping";
pub const PING_RESULT: &str = "pong";
/// Notification on a bound link: kill this link's server instead of keeping it for reuse.
pub const RESTART_METHOD: &str = "mhcLsp/restartServer";
/// Overrides the socket path.
pub const SOCKET_ENV: &str = "MHC_LSP_SOCKET";
/// `0`/`off` disables daemon links; a path names the daemon executable.
pub const DAEMON_ENV: &str = "MHC_LSP_DAEMON";
/// Name of the daemon executable beside `mhc`.
pub const BINARY_NAME: &str = "mhc-lsp";

const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
const READY_TIMEOUT: Duration = Duration::from_secs(5);

/// Banner printed once the socket accepts connections.
pub fn ready_banner(socket: &Path) -> String {
    format!("mhc-lsp listening on {}", socket.display())
}

/// Default socket: `$MHC_LSP_SOCKET`, else `$XDG_RUNTIME_DIR/mhc-lsp/daemon.sock`, else
/// `<tmp>/mhc-lsp-<uid>/daemon.sock`.
pub fn default_socket_path() -> PathBuf {
    if let Some(path) = std::env::var_os(SOCKET_ENV).filter(|value| !value.is_empty()) {
        return PathBuf::from(path);
    }
    if let Some(runtime) = std::env::var_os("XDG_RUNTIME_DIR").filter(|value| !value.is_empty()) {
        return PathBuf::from(runtime).join("mhc-lsp").join("daemon.sock");
    }
    std::env::temp_dir()
        .join(format!("mhc-lsp-{}", nix::unistd::getuid()))
        .join("daemon.sock")
}

/// The daemon executable for links: `$MHC_LSP_DAEMON` when it names a path, else `mhc-lsp`
/// beside the running executable. `None` when disabled or not found.
pub fn daemon_binary() -> Option<PathBuf> {
    let setting = std::env::var(DAEMON_ENV).unwrap_or_default();
    match setting.as_str() {
        "0" | "off" | "false" => return None,
        "" | "1" | "on" | "true" => {}
        path => return Some(PathBuf::from(path)).filter(|path| path.is_file()),
    }
    let sibling = std::env::current_exe().ok()?.parent()?.join(BINARY_NAME);
    sibling.is_file().then_some(sibling)
}

/// Server identity inside the daemon.
#[derive(Clone, Debug, PartialEq)]
pub struct ConnectParams {
    pub command: String,
    pub args: Vec<String>,
    pub cwd: String,
    pub env: BTreeMap<String, String>,
    pub initialization_options: Option<Value>,
}

impl ConnectParams {
    pub fn to_json(&self) -> Value {
        let mut value =
            json!({"command": self.command, "args": self.args, "cwd": self.cwd, "env": self.env});
        if let Some(options) = &self.initialization_options {
            value["initializationOptions"] = options.clone();
        }
        value
    }

    pub fn from_json(value: &Value) -> Option<Self> {
        Some(Self {
            command: value.get("command")?.as_str()?.to_owned(),
            args: value
                .get("args")
                .and_then(Value::as_array)
                .map(|args| {
                    args.iter()
                        .filter_map(|arg| arg.as_str().map(str::to_owned))
                        .collect()
                })
                .unwrap_or_default(),
            cwd: value.get("cwd")?.as_str()?.to_owned(),
            env: value
                .get("env")
                .and_then(Value::as_object)
                .map(|env| {
                    env.iter()
                        .filter_map(|(key, value)| Some((key.clone(), value.as_str()?.to_owned())))
                        .collect()
                })
                .unwrap_or_default(),
            initialization_options: value
                .get("initializationOptions")
                .cloned()
                .filter(|value| !value.is_null()),
        })
    }

    /// `sha256:` of the canonical JSON of every identity input (object keys sorted at every depth),
    /// so no raw env value or option leaks through the key.
    pub fn key(&self) -> String {
        let identity = Value::Array(vec![
            json!(self.command),
            json!(self.args),
            json!(self.cwd),
            json!(self.env),
            self.initialization_options
                .clone()
                .unwrap_or_else(|| json!({})),
        ]);
        let mut canonical = String::new();
        write_canonical(&identity, &mut canonical);
        let digest = Sha256::digest(canonical.as_bytes());
        format!(
            "sha256:{}",
            digest
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>()
        )
    }
}

fn write_canonical(value: &Value, out: &mut String) {
    match value {
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            out.push('{');
            for (index, key) in keys.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                out.push_str(&Value::String((*key).clone()).to_string());
                out.push(':');
                write_canonical(&map[*key], out);
            }
            out.push('}');
        }
        Value::Array(items) => {
            out.push('[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                write_canonical(item, out);
            }
            out.push(']');
        }
        other => out.push_str(&other.to_string()),
    }
}

// ---------------------------------------------------------------------------------------------
// Client side
// ---------------------------------------------------------------------------------------------

type Halves = (
    BufReader<tokio::net::unix::OwnedReadHalf>,
    tokio::net::unix::OwnedWriteHalf,
);

/// Sends one request on a fresh link and returns its result with the still open link halves.
async fn request_once(stream: UnixStream, method: &str, params: Value) -> Option<(Value, Halves)> {
    let (read, mut write) = stream.into_split();
    let body = json!({"jsonrpc": "2.0", "id": 0, "method": method, "params": params}).to_string();
    write_frame(&mut write, body.as_bytes()).await.ok()?;
    let mut reader = BufReader::new(read);
    let frame = tokio::time::timeout(HANDSHAKE_TIMEOUT, read_frame(&mut reader))
        .await
        .ok()?
        .ok()??;
    let reply: Value = serde_json::from_slice(&frame).ok()?;
    let result = reply.get("result").cloned()?;
    Some((result, (reader, write)))
}

async fn dial(socket: &Path) -> Option<UnixStream> {
    tokio::time::timeout(CONNECT_TIMEOUT, UnixStream::connect(socket))
        .await
        .ok()?
        .ok()
}

/// Whether a daemon answers `ping` on `socket`.
pub async fn ping(socket: &Path) -> bool {
    let Some(stream) = dial(socket).await else {
        return false;
    };
    matches!(request_once(stream, PING_METHOD, json!({})).await, Some((Value::String(answer), _)) if answer == PING_RESULT)
}

/// Starts `binary serve --socket <socket>` detached and waits until it answers `ping`.
pub async fn ensure_daemon(binary: &Path, socket: &Path) -> bool {
    if ping(socket).await {
        return true;
    }
    let mut command = std::process::Command::new(binary);
    command
        .arg("serve")
        .arg("--socket")
        .arg(socket)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let Ok(mut child) = command.spawn() else {
        return false;
    };
    let deadline = Instant::now() + READY_TIMEOUT;
    while Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(50)).await;
        if ping(socket).await {
            std::thread::spawn(move || child.wait());
            return true;
        }
        if let Ok(Some(_)) = child.try_wait() {
            // Another process may have won the bind race.
            return ping(socket).await;
        }
    }
    false
}

/// Links `client`'s server through the daemon. `None` on any failure (callers spawn locally).
pub async fn connect_shared(
    binary: &Path,
    socket: &Path,
    client: &LspClient,
) -> Option<Arc<Connection>> {
    if !ensure_daemon(binary, socket).await {
        return None;
    }
    let (command, args) = client.server().command.split_first()?;
    let params = ConnectParams {
        command: command.clone(),
        args: args.to_vec(),
        cwd: client.root().display().to_string(),
        env: client.spawn_env().into_iter().collect(),
        initialization_options: client.server().initialization.clone(),
    };
    let stream = dial(socket).await?;
    let (result, (reader, writer)) = request_once(stream, CONNECT_METHOD, params.to_json()).await?;
    result.get("key")?;
    Some(Connection::start_buffered(reader, writer))
}

// ---------------------------------------------------------------------------------------------
// Daemon side
// ---------------------------------------------------------------------------------------------

/// Daemon settings.
#[derive(Clone, Debug)]
pub struct DaemonOptions {
    /// How long an idle server is kept for reuse.
    pub server_idle: Duration,
    /// The daemon exits after this long without links or servers.
    pub daemon_idle: Duration,
    pub reap_interval: Duration,
}

impl Default for DaemonOptions {
    fn default() -> Self {
        Self {
            server_idle: Duration::from_secs(5 * 60),
            daemon_idle: Duration::from_secs(10 * 60),
            reap_interval: Duration::from_secs(15),
        }
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

struct ServerProcess {
    key: String,
    pid: Option<u32>,
    to_server: mpsc::UnboundedSender<Vec<u8>>,
    sink: Mutex<Option<mpsc::UnboundedSender<Vec<u8>>>>,
    init_result: Mutex<Option<Value>>,
    pending_init: Mutex<Option<Value>>,
    initialized_sent: AtomicBool,
    alive: Arc<AtomicBool>,
    idle_since: Mutex<Option<Instant>>,
    kill: mpsc::UnboundedSender<()>,
}

impl ServerProcess {
    fn is_idle(&self) -> bool {
        lock(&self.sink).is_none()
    }
}

#[derive(Default)]
struct Registry {
    servers: Mutex<Vec<Arc<ServerProcess>>>,
    links: Mutex<usize>,
    last_activity: Mutex<Option<Instant>>,
}

impl Registry {
    fn touch(&self) {
        *lock(&self.last_activity) = Some(Instant::now());
    }
}

fn spawn_server(params: &ConnectParams, key: String) -> std::io::Result<Arc<ServerProcess>> {
    let mut command = tokio::process::Command::new(&params.command);
    command
        .args(&params.args)
        .current_dir(&params.cwd)
        .envs(&params.env)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    let mut child = command.spawn()?;
    let pid = child.id();
    let (Some(mut stdin), Some(stdout)) = (child.stdin.take(), child.stdout.take()) else {
        return Err(std::io::Error::other("server stdio missing"));
    };
    let (to_server, mut queue) = mpsc::unbounded_channel::<Vec<u8>>();
    let (kill, mut kill_requests) = mpsc::unbounded_channel::<()>();
    let alive = Arc::new(AtomicBool::new(true));
    let process = Arc::new(ServerProcess {
        key,
        pid,
        to_server,
        sink: Mutex::new(None),
        init_result: Mutex::new(None),
        pending_init: Mutex::new(None),
        initialized_sent: AtomicBool::new(false),
        alive: Arc::clone(&alive),
        idle_since: Mutex::new(Some(Instant::now())),
        kill,
    });
    tokio::spawn(async move {
        while let Some(body) = queue.recv().await {
            if write_frame(&mut stdin, &body).await.is_err() {
                break;
            }
        }
    });
    let flag = Arc::clone(&alive);
    tokio::spawn(async move {
        tokio::select! {
            _ = child.wait() => {}
            _ = kill_requests.recv() => {
                let _signal = child.start_kill();
                let _waited = child.wait().await;
            }
        }
        flag.store(false, Ordering::SeqCst);
    });
    let reader_process = Arc::clone(&process);
    tokio::spawn(async move {
        let mut reader = BufReader::new(stdout);
        while let Ok(Some(body)) = read_frame(&mut reader).await {
            route_from_server(&reader_process, body);
        }
        reader_process.alive.store(false, Ordering::SeqCst);
        if let Some(sink) = lock(&reader_process.sink).take() {
            drop(sink);
        }
    });
    Ok(process)
}

fn route_from_server(process: &ServerProcess, body: Vec<u8>) {
    let message: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let is_response = message.get("method").is_none();
    if is_response {
        let mut pending = lock(&process.pending_init);
        if pending
            .as_ref()
            .is_some_and(|id| Some(id) == message.get("id"))
            && let Some(result) = message.get("result")
        {
            *lock(&process.init_result) = Some(result.clone());
            *pending = None;
        }
    }
    let sink = lock(&process.sink).clone();
    match sink {
        Some(sink) => {
            let _forwarded = sink.send(body);
        }
        None => {
            // Idle: answer server requests so the server does not stall; drop the rest.
            if let (Some(method), Some(id)) = (
                message.get("method").and_then(Value::as_str),
                message.get("id"),
            ) {
                let result = crate::rpc::server_request_result(method, message.get("params"))
                    .unwrap_or(Value::Null);
                let _answered = process.to_server.send(
                    json!({"jsonrpc": "2.0", "id": id, "result": result})
                        .to_string()
                        .into_bytes(),
                );
            }
        }
    }
}

fn reply(id: &Value, result: Value) -> Vec<u8> {
    json!({"jsonrpc": "2.0", "id": id, "result": result})
        .to_string()
        .into_bytes()
}

async fn handle_link(stream: UnixStream, registry: Arc<Registry>) {
    let (read, mut write) = stream.into_split();
    let mut reader = BufReader::new(read);
    // Handshake: pings until a connect.
    let (process, spawned, id) = loop {
        let Ok(Some(frame)) = read_frame(&mut reader).await else {
            return;
        };
        let Ok(message) = serde_json::from_slice::<Value>(&frame) else {
            return;
        };
        let id = message.get("id").cloned().unwrap_or(Value::Null);
        match message.get("method").and_then(Value::as_str) {
            Some(PING_METHOD) => {
                if write_frame(&mut write, &reply(&id, json!(PING_RESULT)))
                    .await
                    .is_err()
                {
                    return;
                }
            }
            Some(CONNECT_METHOD) => {
                let Some(params) = message.get("params").and_then(ConnectParams::from_json) else {
                    let error = json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32602, "message": "invalid connect params"}});
                    let _written = write_frame(&mut write, error.to_string().as_bytes()).await;
                    return;
                };
                let key = params.key();
                let reusable = lock(&registry.servers)
                    .iter()
                    .find(|server| {
                        server.key == key && server.is_idle() && server.alive.load(Ordering::SeqCst)
                    })
                    .cloned();
                let (process, spawned) = match reusable {
                    Some(process) => (process, false),
                    None => match spawn_server(&params, key) {
                        Ok(process) => {
                            lock(&registry.servers).push(Arc::clone(&process));
                            (process, true)
                        }
                        Err(error) => {
                            let failure = json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32000, "message": format!("spawn failed: {error}")}});
                            let _written =
                                write_frame(&mut write, failure.to_string().as_bytes()).await;
                            return;
                        }
                    },
                };
                break (process, spawned, id);
            }
            _ => return,
        }
    };
    let (sink, mut from_server) = mpsc::unbounded_channel::<Vec<u8>>();
    *lock(&process.sink) = Some(sink);
    *lock(&process.idle_since) = None;
    *lock(&registry.links) += 1;
    registry.touch();
    let handshake = reply(
        &id,
        json!({"key": process.key, "spawned": spawned, "pid": process.pid}),
    );
    if write_frame(&mut write, &handshake).await.is_err() {
        release(&process, &registry, &HashSet::new(), false);
        return;
    }
    let writer = tokio::spawn(async move {
        while let Some(body) = from_server.recv().await {
            if write_frame(&mut write, &body).await.is_err() {
                break;
            }
        }
        let _closed = write.shutdown().await;
    });
    let mut opened: HashSet<String> = HashSet::new();
    let mut restart = false;
    while let Ok(Some(frame)) = read_frame(&mut reader).await {
        let Ok(message) = serde_json::from_slice::<Value>(&frame) else {
            continue;
        };
        let id = message.get("id").cloned().filter(|id| !id.is_null());
        let method = message
            .get("method")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let to_link = lock(&process.sink).clone();
        match (method, id) {
            ("initialize", Some(id)) => {
                let cached = lock(&process.init_result).clone();
                match cached {
                    Some(result) => {
                        if let Some(link) = to_link {
                            let _answered = link.send(reply(&id, result));
                        }
                    }
                    None => {
                        *lock(&process.pending_init) = Some(id);
                        let _forwarded = process.to_server.send(frame);
                    }
                }
            }
            ("initialized", None) => {
                if !process.initialized_sent.swap(true, Ordering::SeqCst) {
                    let _forwarded = process.to_server.send(frame);
                }
            }
            ("shutdown", Some(id)) => {
                if let Some(link) = to_link {
                    let _answered = link.send(reply(&id, Value::Null));
                }
            }
            ("exit", None) => break,
            (RESTART_METHOD, None) => {
                restart = true;
                break;
            }
            (method, _) => {
                let uri = message
                    .get("params")
                    .and_then(|params| params.get("textDocument"))
                    .and_then(|doc| doc.get("uri"))
                    .and_then(Value::as_str);
                match (method, uri) {
                    ("textDocument/didOpen", Some(uri)) => {
                        opened.insert(uri.to_owned());
                    }
                    ("textDocument/didClose", Some(uri)) => {
                        opened.remove(uri);
                    }
                    _ => {}
                }
                let _forwarded = process.to_server.send(frame);
            }
        }
        if !process.alive.load(Ordering::SeqCst) {
            break;
        }
    }
    release(&process, &registry, &opened, restart);
    writer.abort();
}

fn release(
    process: &Arc<ServerProcess>,
    registry: &Registry,
    opened: &HashSet<String>,
    restart: bool,
) {
    for uri in opened {
        let close = json!({"jsonrpc": "2.0", "method": "textDocument/didClose", "params": {"textDocument": {"uri": uri}}});
        let _sent = process.to_server.send(close.to_string().into_bytes());
    }
    *lock(&process.sink) = None;
    *lock(&process.idle_since) = Some(Instant::now());
    {
        let mut links = lock(&registry.links);
        *links = links.saturating_sub(1);
    }
    registry.touch();
    if restart || !process.alive.load(Ordering::SeqCst) {
        let _requested = process.kill.send(());
        lock(&registry.servers).retain(|server| !Arc::ptr_eq(server, process));
    }
}

fn reap(registry: &Registry, options: &DaemonOptions) -> bool {
    let now = Instant::now();
    lock(&registry.servers).retain(|server| {
        let expired = lock(&server.idle_since)
            .is_some_and(|since| now.duration_since(since) > options.server_idle);
        let dead = !server.alive.load(Ordering::SeqCst);
        if expired || dead {
            let _requested = server.kill.send(());
        }
        !(expired || dead)
    });
    let quiet = lock(&registry.servers).is_empty() && *lock(&registry.links) == 0;
    let last = lock(&registry.last_activity).unwrap_or(now);
    quiet && now.duration_since(last) > options.daemon_idle
}

/// Binds `socket` (replacing a stale socket file, refusing when a daemon already answers) and
/// serves links until idle. Calls `on_ready` once listening.
pub async fn serve(
    socket: &Path,
    options: DaemonOptions,
    on_ready: impl FnOnce(),
) -> std::io::Result<()> {
    if let Some(parent) = socket.parent() {
        std::fs::create_dir_all(parent)?;
        use std::os::unix::fs::PermissionsExt;
        let _restricted = std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700));
    }
    if socket.exists() {
        if ping(socket).await {
            return Err(std::io::Error::new(
                std::io::ErrorKind::AddrInUse,
                "an mhc-lsp daemon already serves this socket",
            ));
        }
        std::fs::remove_file(socket)?;
    }
    let listener = UnixListener::bind(socket)?;
    on_ready();
    let registry = Arc::new(Registry::default());
    registry.touch();
    let mut ticker = tokio::time::interval(options.reap_interval);
    ticker.tick().await;
    loop {
        tokio::select! {
            accepted = listener.accept() => {
                if let Ok((stream, _)) = accepted {
                    registry.touch();
                    tokio::spawn(handle_link(stream, Arc::clone(&registry)));
                }
            }
            _ = ticker.tick() => {
                if reap(&registry, &options) {
                    break;
                }
            }
        }
    }
    for server in lock(&registry.servers).drain(..) {
        let _requested = server.kill.send(());
    }
    let _removed = std::fs::remove_file(socket);
    Ok(())
}

/// Parses `serve [--socket PATH]` style arguments into the socket path.
pub fn socket_from_args(args: &[String]) -> PathBuf {
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        if arg == "--socket"
            && let Some(path) = iter.next()
        {
            return PathBuf::from(path);
        }
        if let Some(path) = arg.strip_prefix("--socket=") {
            return PathBuf::from(path);
        }
    }
    default_socket_path()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_ignores_object_key_order_and_separates_args() {
        let mut env = BTreeMap::new();
        env.insert("B".to_owned(), "2".to_owned());
        env.insert("A".to_owned(), "1".to_owned());
        let a = ConnectParams {
            command: "ls".into(),
            args: vec!["--log-level".into(), "4".into()],
            cwd: "/w".into(),
            env,
            initialization_options: Some(json!({"y": 1, "x": {"b": 2, "a": 1}})),
        };
        let mut b = a.clone();
        b.initialization_options =
            Some(serde_json::from_str(r#"{"x":{"a":1,"b":2},"y":1}"#).expect("json"));
        assert_eq!(a.key(), b.key());
        let mut c = a.clone();
        c.args = vec!["--log-level 4".into()];
        assert_ne!(a.key(), c.key());
        let mut d = a.clone();
        d.initialization_options = None;
        let mut e = a.clone();
        e.initialization_options = Some(json!({}));
        assert_eq!(d.key(), e.key());
        assert!(a.key().starts_with("sha256:"));
        assert_eq!(ConnectParams::from_json(&a.to_json()), Some(a));
    }

    #[test]
    fn socket_args() {
        assert_eq!(
            socket_from_args(&["serve".into(), "--socket".into(), "/x.sock".into()]),
            PathBuf::from("/x.sock")
        );
        assert_eq!(
            socket_from_args(&["--socket=/y.sock".into()]),
            PathBuf::from("/y.sock")
        );
    }
}
