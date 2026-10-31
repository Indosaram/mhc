//! Port of senpi packages/coding-agent/src/modes/interactive/session-control-server.ts (v2026.10.27-2).
//!
//! The terminal's control listener. Every connection must present the 32-byte secret first
//! (`authenticate_socket`, the handshake every RPC transport shares); a wrong secret is disconnected
//! before a single JSONL line is read. After that it is the RPC framing: one LF-terminated JSON
//! object per request, one response per request carrying its `id`, plus pushed feed events.
//!
//! The socket path is the endpoint's logical address on every platform (its secret lives beside it); on
//! win32 the listener binds the named pipe the host transport derives from that path and the secret
//! (senpi#4267), which is the address every client already connects to.
use futures::future::BoxFuture;
use maho_rpc::socket_event_fanout::SocketEventSinkActor;
use serde_json::{Map, Value, json};
use std::sync::{Arc, Mutex};

/// One authenticated connection. `transport` holds the connection's socket writer until the endpoint's
/// record writer takes it (`ControlAttachments::opened`), so every record leaves in one order.
#[derive(Clone)]
pub struct ControlConnection {
    pub id: u64,
    send: Arc<dyn Fn(Value) + Send + Sync>,
    pub transport: Arc<Mutex<Option<SocketEventSinkActor>>>,
    close: Arc<dyn Fn() + Send + Sync>,
}

impl ControlConnection {
    pub fn send(&self, record: Value) {
        (self.send)(record);
    }

    /// Ends the connection (senpi `transport.close`).
    pub fn close(&self) {
        (self.close)();
    }
}

/// A command frame: a JSON object with a string `type` (missing fields index as `null`).
pub type ControlCommand = Value;

pub trait ControlServerHandlers: Send + Sync + 'static {
    /// `None`: the command already answered on its own (through the endpoint's record writer).
    fn command(
        &self,
        connection: ControlConnection,
        command: ControlCommand,
    ) -> BoxFuture<'static, Option<Value>>;
    /// An authenticated connection is about to send its first line.
    fn opened(&self, _connection: &ControlConnection) {}
    /// Writes a command's answer; `false` sends it straight to the socket.
    fn respond(&self, _connection: &ControlConnection, _answer: &Value) -> bool {
        false
    }
    fn closed(&self, connection: &ControlConnection);
    fn listener_error(&self, error: String);
}

pub struct ControlServer {
    stop: tokio::sync::watch::Sender<bool>,
    accept: Mutex<Option<tokio::task::JoinHandle<()>>>,
    clients: Arc<Mutex<Vec<tokio::task::JoinHandle<()>>>>,
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

impl ControlServer {
    pub async fn close(&self) {
        self.stop.send_replace(true);
        let accept = lock(&self.accept).take();
        if let Some(accept) = accept {
            accept.abort();
            drop(accept.await);
        }
        let clients = std::mem::take(&mut *lock(&self.clients));
        for client in clients {
            client.abort();
            drop(client.await);
        }
    }
}

#[cfg(unix)]
type ControlStream = tokio::net::UnixStream;
#[cfg(windows)]
type ControlStream = tokio::net::windows::named_pipe::NamedPipeServer;

#[cfg(unix)]
struct ControlListener(tokio::net::UnixListener);
#[cfg(unix)]
impl ControlListener {
    fn bind(socket_path: &str, secret: &[u8]) -> std::io::Result<Self> {
        use std::os::unix::fs::PermissionsExt;
        let address = maho_rpc::socket_transport::resolve_socket_transport_address(
            socket_path,
            "posix",
            Some(secret),
        )
        .map_err(std::io::Error::other)?;
        let listener = tokio::net::UnixListener::bind(&address)?;
        // A named pipe has no file to restrict: the secret, 0600 beside the logical path, is its gate.
        std::fs::set_permissions(socket_path, std::fs::Permissions::from_mode(0o600))?;
        Ok(Self(listener))
    }
    async fn accept(&mut self) -> std::io::Result<ControlStream> {
        Ok(self.0.accept().await?.0)
    }
}

#[cfg(windows)]
struct ControlListener {
    address: String,
    server: tokio::net::windows::named_pipe::NamedPipeServer,
}
#[cfg(windows)]
impl ControlListener {
    fn bind(socket_path: &str, secret: &[u8]) -> std::io::Result<Self> {
        let address = maho_rpc::socket_transport::resolve_socket_transport_address(
            socket_path,
            "win32",
            Some(secret),
        )
        .map_err(std::io::Error::other)?;
        let server = maho_rpc::host_lifecycle::win32_pipe::create_first_instance(&address)?;
        Ok(Self { address, server })
    }
    async fn accept(&mut self) -> std::io::Result<ControlStream> {
        self.server.connect().await?;
        let next = tokio::net::windows::named_pipe::ServerOptions::new()
            .reject_remote_clients(true)
            .create(&self.address)?;
        Ok(std::mem::replace(&mut self.server, next))
    }
}

pub async fn listen_control_socket(
    socket_path: &str,
    secret: [u8; maho_rpc::socket_transport::SOCKET_SECRET_BYTES],
    handlers: Arc<dyn ControlServerHandlers>,
) -> Result<ControlServer, String> {
    let mut listener =
        ControlListener::bind(socket_path, &secret).map_err(|error| error.to_string())?;
    let (stop, stopped) = tokio::sync::watch::channel(false);
    let clients = Arc::new(Mutex::new(Vec::new()));
    let tracked = clients.clone();
    let accept = tokio::spawn(async move {
        let mut next_connection = 0;
        loop {
            let mut stopped = stopped.clone();
            let stream = tokio::select! {
                stream = listener.accept() => stream,
                _ = stopped.wait_for(|stop| *stop) => return,
            };
            match stream {
                Ok(stream) => {
                    next_connection += 1;
                    let handlers = handlers.clone();
                    let id = next_connection;
                    let client = tokio::spawn(async move {
                        let mut stream = stream;
                        if maho_rpc::socket_transport::authenticate_socket(&mut stream, &secret)
                            .await
                            .is_err()
                        {
                            return;
                        }
                        accept_connection(stream, id, handlers).await;
                    });
                    let mut clients = lock(&tracked);
                    clients.retain(|client: &tokio::task::JoinHandle<()>| !client.is_finished());
                    clients.push(client);
                }
                Err(error) => handlers.listener_error(error.to_string()),
            }
        }
    });
    Ok(ControlServer {
        stop,
        accept: Mutex::new(Some(accept)),
        clients,
    })
}

async fn accept_connection(
    stream: ControlStream,
    id: u64,
    handlers: Arc<dyn ControlServerHandlers>,
) {
    #[cfg(unix)]
    let (mut read, write) = stream.into_split();
    #[cfg(windows)]
    let (mut read, write) = tokio::io::split(stream);
    let (closed_tx, mut closed_rx) = tokio::sync::watch::channel(false);
    let cut = closed_tx.clone();
    let actor = SocketEventSinkActor::new(
        write,
        maho_rpc::socket_event_fanout::DEFAULT_QUEUE_BYTES,
        maho_rpc::socket_event_fanout::DEFAULT_STALL_MS,
        Arc::default(),
        move |_| {
            cut.send_replace(true);
        },
    );
    let transport = Arc::new(Mutex::new(Some(actor)));
    let direct = transport.clone();
    let respond_handlers = handlers.clone();
    let connection_slot: Arc<Mutex<Option<ControlConnection>>> = Arc::default();
    let slot = connection_slot.clone();
    let close_signal = closed_tx.clone();
    let connection = ControlConnection {
        id,
        // Every record, answers and feed events alike, leaves through the endpoint's writer when it
        // has one, so this connection's output keeps one order.
        send: Arc::new(move |record: Value| {
            let connection = lock(&slot).clone();
            if let Some(connection) = connection
                && respond_handlers.respond(&connection, &record)
            {
                return;
            }
            if let Some(actor) = lock(&direct).as_ref()
                && let Ok(line) = maho_rpc::jsonl::serialize_json_line(&record)
            {
                drop(actor.enqueue(maho_rpc::socket_event_fanout::QueueEntry {
                    line,
                    key: None,
                    demoted_line: None,
                    on_written: None,
                }));
            }
        }),
        transport,
        close: Arc::new(move || {
            close_signal.send_replace(true);
        }),
    };
    *lock(&connection_slot) = Some(connection.clone());
    handlers.opened(&connection);
    let Ok(mut reader) =
        maho_rpc::jsonl::JsonlLineReader::new(maho_rpc::jsonl::MAX_RPC_LINE_CHARACTERS)
    else {
        return;
    };
    let mut chunk = vec![0u8; 8192];
    loop {
        use tokio::io::AsyncReadExt;
        let count = tokio::select! {
            count = read.read(&mut chunk) => count,
            _ = closed_rx.wait_for(|closed| *closed) => break,
        };
        let records = match count {
            Ok(0) | Err(_) => reader.finish(),
            Ok(count) => reader.push(&chunk[..count]),
        };
        for record in records {
            match record {
                maho_rpc::jsonl::LineRecord::Line(line) => {
                    let connection = connection.clone();
                    let handlers = handlers.clone();
                    tokio::spawn(async move { answer(connection, &line, handlers).await });
                }
                maho_rpc::jsonl::LineRecord::Oversized => {
                    connection.send(failure(None, "parse", "RPC line exceeds the size limit."))
                }
            }
        }
        if matches!(count, Ok(0) | Err(_)) {
            break;
        }
    }
    *lock(&connection_slot) = None;
    handlers.closed(&connection);
}

async fn answer(
    connection: ControlConnection,
    line: &str,
    handlers: Arc<dyn ControlServerHandlers>,
) {
    let Ok(parsed) = serde_json::from_str::<Value>(line) else {
        connection.send(failure(None, "parse", "Invalid JSON."));
        return;
    };
    let shape_error = maho_rpc::rpc_input_validation::rpc_command_shape_error(&parsed);
    let command = match parsed {
        command if shape_error.is_none() && command["type"].is_string() => command,
        _ => {
            connection.send(failure(
                None,
                "parse",
                shape_error.unwrap_or("RPC command needs a string `type`."),
            ));
            return;
        }
    };
    if let Some(answer) = handlers.command(connection.clone(), command).await {
        connection.send(answer);
    }
}

fn with_id(id: Option<&Value>, mut record: Map<String, Value>) -> Value {
    if let Some(id) = id {
        record.insert("id".into(), id.clone());
    }
    Value::Object(record)
}

pub fn failure(id: Option<&Value>, command: &str, error: &str) -> Value {
    let Value::Object(record) =
        json!({"type": "response", "command": command, "success": false, "error": error})
    else {
        return Value::Null;
    };
    with_id(id, record)
}

pub fn success(id: Option<&Value>, command: &str, data: Option<Value>) -> Value {
    let Value::Object(mut record) =
        json!({"type": "response", "command": command, "success": true})
    else {
        return Value::Null;
    };
    if let Some(data) = data {
        record.insert("data".into(), data);
    }
    with_id(id, record)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn answers_omit_an_absent_id_and_carry_data_only_when_given() {
        assert_eq!(
            failure(None, "parse", "Invalid JSON."),
            json!({"type":"response","command":"parse","success":false,"error":"Invalid JSON."})
        );
        assert_eq!(
            success(Some(&json!("7")), "wake", Some(json!({"admitted": []}))),
            json!({"id":"7","type":"response","command":"wake","success":true,"data":{"admitted":[]}})
        );
        assert_eq!(
            success(Some(&json!(3)), "set_session_name", None),
            json!({"id":3,"type":"response","command":"set_session_name","success":true})
        );
    }
}
