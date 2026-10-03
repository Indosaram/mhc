use std::{collections::BTreeMap, path::{Path, PathBuf}, time::Duration};
use maho_ai::utils::abort::{AbortController, AbortSignal};
use serde_json::{Value, json};
use tokio::{io::{AsyncBufReadExt, AsyncWriteExt, BufReader}, net::{UnixListener, UnixStream}, sync::mpsc};
use super::{COORDINATOR_PROTOCOL_VERSION, routed_messages};
use crate::experimental::process::MAX_CONTROL_LINE_BYTES;
enum Event { Message(u64, Value), Disconnected(u64), PublicClosed(u64) }
enum Role { Server { id: String, endpoint: String }, Peer(String), Lease }
struct Connection { role: Role, output: mpsc::UnboundedSender<Value>, close: AbortController }
impl Drop for Connection { fn drop(&mut self) { self.close.abort(None); } }
struct SocketOwner(PathBuf);
impl Drop for SocketOwner { fn drop(&mut self) { let _ = std::fs::remove_file(&self.0); } }
async fn bind(path: &Path) -> Result<(UnixListener, SocketOwner), String> {
    use std::os::unix::{fs::FileTypeExt, fs::PermissionsExt};
    match tokio::fs::symlink_metadata(path).await {
        Ok(metadata) => {
            if !metadata.file_type().is_socket() { return Err(format!("Coordinator path is not a socket: {}", path.display())); }
            match UnixStream::connect(path).await {
                Ok(_) => return Err(format!("Coordinator socket is already active: {}", path.display())),
                Err(error) if matches!(error.kind(), std::io::ErrorKind::ConnectionRefused | std::io::ErrorKind::NotFound) => {},
                Err(error) => return Err(error.to_string()),
            }
            tokio::fs::remove_file(path).await.map_err(|error| error.to_string())?;
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {},
        Err(error) => return Err(error.to_string()),
    }
    let listener = UnixListener::bind(path).map_err(|error| error.to_string())?;
    let owner = SocketOwner(path.to_owned());
    tokio::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).await.map_err(|error| error.to_string())?;
    Ok((listener, owner))
}
fn open_control(socket: UnixStream, id: u64, events: mpsc::UnboundedSender<Event>, signal: AbortSignal) -> (mpsc::UnboundedSender<Value>, AbortController) {
    let (input, mut output) = socket.into_split();
    let (sender, mut messages) = mpsc::unbounded_channel::<Value>();
    let closed = AbortController::new(); let read_closed = closed.signal(); let write_closed = closed.signal();
    let reader_events = events.clone(); let reader_signal = signal.clone(); let reader_close = closed.clone();
    let writer_close = closed.clone();
    tokio::spawn(async move {
        let mut input = BufReader::new(input);
        let mut line = Vec::new();
        loop {
            let chunk = tokio::select! { biased; _ = reader_signal.cancelled() => break, _ = read_closed.cancelled() => break, chunk = input.fill_buf() => match chunk { Ok(chunk) if !chunk.is_empty() => chunk, _ => break } };
            let count = chunk.iter().position(|byte| *byte == b'\n').map_or(chunk.len(), |index| index + 1);
            if line.len() + count > MAX_CONTROL_LINE_BYTES { break; }
            line.extend_from_slice(&chunk[..count]); input.consume(count);
            if line.last() == Some(&b'\n') {
                let value = match serde_json::from_slice::<Value>(&line) { Ok(value) if value["type"].is_string() => value, _ => break };
                line.clear(); if reader_events.send(Event::Message(id, value)).is_err() { break; }
            }
        }
        reader_close.abort(None); let _ = reader_events.send(Event::Disconnected(id));
    });
    tokio::spawn(async move {
        loop {
            let value = tokio::select! { biased; _ = signal.cancelled() => break, _ = write_closed.cancelled() => break, value = messages.recv() => match value { Some(value) => value, None => break } };
            let line = match crate::experimental::process::encode_control_line(&value) { Ok(line) => line, Err(_) => break };
            let result = tokio::select! { biased; _ = signal.cancelled() => break, _ = write_closed.cancelled() => break, result = output.write_all(line.as_bytes()) => result };
            if result.is_err() { break; }
        }
        writer_close.abort(None); let _ = events.send(Event::Disconnected(id));
    });
    (sender, closed)
}
fn send(connections: &BTreeMap<u64, Connection>, id: u64, message: Value) { if let Some(connection) = connections.get(&id) { let _ = connection.output.send(message); } }
fn notify(connections: &BTreeMap<u64, Connection>, message: Value) { for connection in connections.values() { if matches!(connection.role, Role::Peer(_)) { let _ = connection.output.send(message.clone()); } } }
fn server_id(connections: &BTreeMap<u64, Connection>, current: Option<u64>) -> Option<String> { match &connections.get(&current?)?.role { Role::Server { id, .. } => Some(id.clone()), _ => None } }
pub async fn run_coordinator(public_path: &Path, control_path: &Path, signal: &AbortSignal) -> Result<(), String> {
    run_coordinator_ready(public_path, control_path, signal, None).await
}
pub async fn run_coordinator_ready(public_path: &Path, control_path: &Path, signal: &AbortSignal, ready: Option<tokio::sync::oneshot::Sender<()>>) -> Result<(), String> {
    let (control, _control_owner) = bind(control_path).await?;
    let (public, _public_owner) = bind(public_path).await?;
    if let Some(ready) = ready { let _ = ready.send(()); }
    let shutdown = AbortController::new();
    let (events, mut incoming) = mpsc::unbounded_channel();
    let mut connections = BTreeMap::<u64, Connection>::new(); let mut current = None;
    let mut proxies = BTreeMap::<u64, AbortController>::new(); let mut next_id = 1;
    let mut empty_deadline = Some(tokio::time::Instant::now() + Duration::from_secs(30));
    let result = loop {
        let idle = async { match empty_deadline { Some(deadline) => tokio::time::sleep_until(deadline).await, None => std::future::pending().await } };
        tokio::select! {
            biased;
            _ = signal.cancelled() => break Ok(()),
            _ = idle => break Ok(()),
            socket = control.accept() => {
                let (socket, _) = match socket { Ok(socket) => socket, Err(error) => break Err(error.to_string()) };
                let id = next_id; next_id += 1;
                let (output, close) = open_control(socket, id, events.clone(), shutdown.signal());
                connections.insert(id, Connection { role: Role::Lease, output, close }); empty_deadline = None;
            }
            socket = public.accept() => {
                let (mut socket, _) = match socket { Ok(socket) => socket, Err(error) => break Err(error.to_string()) };
                let endpoint = current.and_then(|id| connections.get(&id)).and_then(|connection| match &connection.role { Role::Server { endpoint, .. } => Some(endpoint.clone()), _ => None });
                if let Some(endpoint) = endpoint {
                    let id = next_id; next_id += 1;
                    let controller = AbortController::new(); let proxy_signal = controller.signal(); proxies.insert(id, controller);
                    let events = events.clone(); let shutdown = shutdown.signal();
                    tokio::spawn(async move {
                        tokio::select! { biased; _ = shutdown.cancelled() => {}, _ = proxy_signal.cancelled() => {}, _ = async { if let Ok(mut upstream) = UnixStream::connect(endpoint).await { let _ = tokio::io::copy_bidirectional(&mut socket, &mut upstream).await; } } => {} }
                        let _ = events.send(Event::PublicClosed(id));
                    }); empty_deadline = None;
                }
            }
            event = incoming.recv() => {
                match event {
                    Some(Event::PublicClosed(id)) => { proxies.remove(&id); }
                    Some(Event::Disconnected(id)) => {
                        if let Some(connection) = connections.remove(&id) {
                            match &connection.role {
                                Role::Server { id: server, .. } if current == Some(id) => { current = None; notify(&connections, json!({"type":"server_disconnected", "serverConnectionId":server})); }
                                Role::Peer(peer) => { if let Some(server) = current { send(&connections, server, json!({"type":"peer_disconnected", "peerId":peer})); } }
                                _ => {},
                            }
                        }
                    }
                    Some(Event::Message(id, message)) => {
                        let Some(connection) = connections.get(&id) else { continue; };
                        let outcome = match &connection.role {
                            Role::Lease => {
                                if message["protocol"].as_u64() != Some(COORDINATOR_PROTOCOL_VERSION as u64) { Err("Unsupported coordinator protocol".to_owned()) }
                                else if message["type"] == "register_server" {
                                    match (message["serverConnectionId"].as_str().filter(|value| !value.is_empty()), message["endpoint"].as_str().filter(|value| !value.is_empty())) {
                                        (Some(server), Some(endpoint)) => {
                                            let old_id = server_id(&connections, current); let previous = current.replace(id);
                                            let peers: Vec<_> = connections.values().filter_map(|connection| match &connection.role { Role::Peer(peer) => Some(peer.clone()), _ => None }).collect();
                                            connections.get_mut(&id).expect("accepted connection").role = Role::Server { id: server.to_owned(), endpoint: endpoint.to_owned() };
                                            send(&connections, id, json!({"type":"server_registered", "serverConnectionId":server, "peers":peers}));
                                            if let Some(previous) = previous { for (_, controller) in std::mem::take(&mut proxies) { controller.abort(None); } notify(&connections, json!({"type":"server_disconnected", "serverConnectionId":old_id})); send(&connections, previous, json!({"type":"server_replaced"})); }
                                            notify(&connections, json!({"type":"server_connected", "serverConnectionId":server})); Ok(())
                                        }
                                        _ => Err("Coordinator server registration requires nonempty strings".to_owned()),
                                    }
                                } else if message["type"] == "register_peer" {
                                    match message["peerId"].as_str().filter(|peer| !peer.is_empty() && *peer != "server") {
                                        Some(peer) if !connections.values().any(|connection| matches!(&connection.role, Role::Peer(existing) if existing == peer)) => {
                                            let peer = peer.to_owned(); connections.get_mut(&id).expect("accepted connection").role = Role::Peer(peer.clone());
                                            let mut registration = json!({"type":"peer_registered", "peerId":peer}); if let Some(server) = server_id(&connections, current) { registration["serverConnectionId"] = json!(server); }
                                            send(&connections, id, registration); if let Some(server) = current { send(&connections, server, json!({"type":"peer_connected", "peerId":peer})); } Ok(())
                                        }
                                        _ => Err("Coordinator peer is already connected or invalid".to_owned()),
                                    }
                                } else { Err("Coordinator connection did not register a role".to_owned()) }
                            }
                            Role::Server { .. } if current != Some(id) => Ok(()),
                            role => {
                                let from = match role { Role::Server { .. } => "server", Role::Peer(peer) => peer, Role::Lease => unreachable!() };
                                let peers: Vec<_> = connections.values().filter_map(|connection| match &connection.role { Role::Peer(peer) => Some(peer.clone()), _ => None }).collect();
                                routed_messages(from, &message, &peers, current.is_some()).map(|messages| { for (target, message) in messages { let target = if target == "server" { current } else { connections.iter().find_map(|(id, connection)| matches!(&connection.role, Role::Peer(peer) if *peer == target).then_some(*id)) }; if let Some(target) = target && let Ok(message) = serde_json::to_value(message) { send(&connections, target, message); } } })
                            }
                        };
                        if outcome.is_err() { connections.remove(&id); }
                    }
                    None => break Ok(()),
                }
                if connections.is_empty() && proxies.is_empty() { empty_deadline.get_or_insert(tokio::time::Instant::now() + Duration::from_millis(250)); } else { empty_deadline = None; }
            }
        }
    };
    shutdown.abort(None); drop(connections); for (_, controller) in proxies { controller.abort(None); }
    result
}
