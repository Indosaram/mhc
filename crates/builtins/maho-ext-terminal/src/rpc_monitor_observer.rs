//! One activity-neutral observer connection to a host or terminal endpoint (senpi
//! `rpc-monitor-observer.ts`): the socket handshake every RPC socket shares, one
//! `subscribe_sessions` frame, then every record the subscription delivers. It never attaches and
//! never sends input, so it counts as no attachment and refreshes no idle window. The classic RPC
//! client is session-scoped (it drops records tagged with another session), so the monitor reads
//! the stream itself.
use maho_rpc::host_observe_request::OBSERVE_REQUEST_FIELD;
use maho_rpc::jsonl::{JsonlLineReader, LineRecord, MAX_RPC_LINE_CHARACTERS};
use serde_json::Value;
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// One record the subscription delivered (upstream `RpcRecord`).
pub type RpcRecord = Value;

/// Upstream `RpcObserverHandlers`.
pub struct RpcObserverHandlers {
    /// Every record after the subscribe answer, in arrival order.
    pub record: Box<dyn Fn(RpcRecord) + Send + Sync>,
    /// The connection ended (host gone, refused, closed by `close()`); called once.
    pub closed: Box<dyn Fn(String) + Send + Sync>,
    /// One read's records were all delivered (upstream's microtask boundary after a `data` event).
    pub batch_end: Box<dyn Fn() + Send + Sync>,
}

/// Upstream `RpcObserver`.
pub struct RpcObserver {
    /// The subscribe answer (`snapshot`, `seq`, `hostGeneration`, `hostInstanceId`), or the
    /// rejection `rpc monitor: <socket>: <reason>`.
    pub subscribed: tokio::sync::oneshot::Receiver<Result<RpcRecord, String>>,
    close: Box<dyn Fn() + Send + Sync>,
}

impl RpcObserver {
    /// An observer from its answer and its close action (what `observe_stream` builds; a test
    /// double builds its own).
    pub fn new(
        subscribed: tokio::sync::oneshot::Receiver<Result<RpcRecord, String>>,
        close: impl Fn() + Send + Sync + 'static,
    ) -> Self {
        Self {
            subscribed,
            close: Box::new(close),
        }
    }

    /// Upstream `close()` (`socket.destroy()`): the reader ends and `closed("connection closed")`
    /// fires unless the connection had already ended.
    pub fn close(&self) {
        (self.close)();
    }

    /// The close action alone, for a caller that awaits `subscribed` separately.
    pub fn into_parts(
        self,
    ) -> (
        tokio::sync::oneshot::Receiver<Result<RpcRecord, String>>,
        Box<dyn Fn() + Send + Sync>,
    ) {
        (self.subscribed, self.close)
    }
}

pub const SUBSCRIBE_ID: &str = "monitor-subscribe";

/// The `subscribe_sessions` frame: the subscription marked observing (senpi#3956) - without it the
/// supervisor counts this watch as an attached client, so a watched host never idle-exits and a
/// `children`/`all` watch pins every task shard host on the machine.
pub fn subscribe_frame(subscription: &Value) -> Value {
    let mut frame = subscription.as_object().cloned().unwrap_or_default();
    frame.insert(OBSERVE_REQUEST_FIELD.to_owned(), Value::Bool(true));
    frame.insert("id".to_owned(), Value::from(SUBSCRIBE_ID));
    frame.insert("type".to_owned(), Value::from("subscribe_sessions"));
    Value::Object(frame)
}

/// Upstream `openRpcObserver`. A terminal control socket (`socketNeedsHandshake`) takes the secret
/// handshake; every other endpoint takes the authenticated public challenge first
/// (`connectAuthenticatedRpcSocket`, itself an observing read). `key_root` is `RpcKeyOptions.keyRoot`.
#[cfg(unix)]
pub async fn open_rpc_observer(
    socket_path: &str,
    subscription: &Value,
    handlers: RpcObserverHandlers,
    key_root: Option<&std::path::Path>,
) -> Result<RpcObserver, String> {
    use maho_rpc::socket_transport::{
        SOCKET_HANDSHAKE_TIMEOUT_MS, read_socket_secret, resolve_socket_transport_address,
        socket_secret_path,
    };
    let needs_handshake = maho_rpc::host_endpoints::is_tui_control_socket(socket_path);
    let (stream, secret) = if needs_handshake {
        let secret = read_socket_secret(std::path::Path::new(&socket_secret_path(socket_path)?))
            .map_err(|error| error.to_string())?;
        let address =
            resolve_socket_transport_address(socket_path, "linux", Some(secret.as_slice()))?;
        let stream = tokio::net::UnixStream::connect(address)
            .await
            .map_err(|error| error.to_string())?;
        (stream, Some(secret.to_vec()))
    } else {
        // Native divergence: upstream's first frame also carries `origin_host_instance`; the native
        // challenge (as maho-cli's host transport) sends the bare observing `get_protocol_info`.
        let (socket, _) = maho_rpc::host_probe::authenticated_host_socket(
            socket_path,
            &serde_json::json!({"type":"get_protocol_info","id":"rpc-auth-protocol","observe":true}),
            SOCKET_HANDSHAKE_TIMEOUT_MS,
            key_root,
        )
        .await
        .map_err(|error| error.to_string())?;
        (
            socket.into_stream().map_err(|error| error.to_string())?,
            None,
        )
    };
    Ok(observe_stream(
        stream,
        socket_path.to_owned(),
        subscription,
        secret,
        handlers,
    ))
}

/// The connected half of `openRpcObserver`: send the (optional) handshake secret and the subscribe
/// frame, then deliver every JSONL record until the stream ends or `close()` is called.
pub fn observe_stream<S>(
    stream: S,
    socket_path: String,
    subscription: &Value,
    handshake: Option<Vec<u8>>,
    handlers: RpcObserverHandlers,
) -> RpcObserver
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let (answer_tx, answer_rx) = tokio::sync::oneshot::channel();
    let (stop_tx, mut stop_rx) = tokio::sync::oneshot::channel::<()>();
    let frame = subscribe_frame(subscription);
    let answer = Arc::new(Mutex::new(Some(answer_tx)));
    tokio::spawn(async move {
        let mut stream = stream;
        let mut ended = false;
        let end = |ended: &mut bool, reason: String| {
            if std::mem::replace(ended, true) {
                return;
            }
            if let Some(answer) = answer
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take()
            {
                let _ = answer.send(Err(format!("rpc monitor: {socket_path}: {reason}")));
            }
            (handlers.closed)(reason);
        };
        let mut text = frame.to_string().into_bytes();
        text.push(b'\n');
        let sent = async {
            if let Some(secret) = &handshake {
                stream.write_all(secret).await?;
            }
            stream.write_all(&text).await
        }
        .await;
        if let Err(error) = sent {
            end(&mut ended, error.to_string());
            return;
        }
        let mut reader =
            JsonlLineReader::new(MAX_RPC_LINE_CHARACTERS).expect("nonzero RPC line limit");
        let mut chunk = vec![0u8; 8192];
        let deliver = |records: Vec<LineRecord>| -> bool {
            for line in records {
                let LineRecord::Line(line) = line else {
                    continue;
                };
                let Ok(record) = serde_json::from_str::<Value>(&line) else {
                    continue;
                };
                if record.get("type").and_then(Value::as_str) == Some("response")
                    && record.get("id").and_then(Value::as_str) == Some(SUBSCRIBE_ID)
                {
                    let sender = answer
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .take();
                    if record.get("success") == Some(&Value::Bool(true)) {
                        if let Some(sender) = sender {
                            let _ = sender.send(Ok(record));
                        }
                    } else {
                        if let Some(sender) = sender {
                            let error = crate::rpc_monitor_coalesce::js_string(record.get("error"));
                            let _ = sender.send(Err(format!(
                                "rpc monitor: {socket_path} refused the subscription: {error}"
                            )));
                        }
                        // `socket.destroy()`: the connection closes.
                        return false;
                    }
                    continue;
                }
                (handlers.record)(record);
            }
            true
        };
        loop {
            tokio::select! {
                biased;
                _ = &mut stop_rx => break,
                read = stream.read(&mut chunk) => match read {
                    Ok(0) => {
                        let records = reader.finish();
                        deliver(records);
                        break;
                    }
                    Ok(count) => {
                        let records = reader.push(&chunk[..count]);
                        let open = deliver(records);
                        (handlers.batch_end)();
                        if !open {
                            break;
                        }
                    }
                    Err(error) => {
                        end(&mut ended, error.to_string());
                        break;
                    }
                },
            }
        }
        end(&mut ended, "connection closed".to_owned());
    });
    let stop = Mutex::new(Some(stop_tx));
    RpcObserver::new(answer_rx, move || {
        if let Some(stop) = stop
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
        {
            let _ = stop.send(());
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tokio::io::AsyncBufReadExt;

    type Seen = Arc<Mutex<Vec<String>>>;
    fn handlers() -> (
        RpcObserverHandlers,
        Seen,
        Seen,
        tokio::sync::mpsc::UnboundedReceiver<()>,
    ) {
        let records: Seen = Arc::default();
        let closed: Seen = Arc::default();
        let (closed_tx, closed_rx) = tokio::sync::mpsc::unbounded_channel();
        let (r, c) = (records.clone(), closed.clone());
        (
            RpcObserverHandlers {
                record: Box::new(move |record| {
                    r.lock()
                        .unwrap()
                        .push(record["type"].as_str().unwrap_or("").to_owned())
                }),
                closed: Box::new(move |reason| {
                    c.lock().unwrap().push(reason);
                    let _ = closed_tx.send(());
                }),
                batch_end: Box::new(|| {}),
            },
            records,
            closed,
            closed_rx,
        )
    }

    #[tokio::test]
    async fn subscribes_once_marked_observing_then_delivers_records_in_order() {
        let (client, host) = tokio::io::duplex(4096);
        let (handlers, records, closed, mut closed_rx) = handlers();
        let observer = observe_stream(
            client,
            "/h.sock".into(),
            &json!({"target":{"kind":"all"}}),
            None,
            handlers,
        );
        let (read, mut write) = tokio::io::split(host);
        let mut lines = tokio::io::BufReader::new(read).lines();
        let frame: Value =
            serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
        assert_eq!(frame["type"], "subscribe_sessions");
        assert_eq!(frame["id"], SUBSCRIBE_ID);
        assert_eq!(frame["observe"], true);
        assert_eq!(frame["target"]["kind"], "all");
        write
            .write_all(
                b"not json\n{\"type\":\"response\",\"id\":\"monitor-subscribe\",\"success\":true,\"data\":{\"snapshot\":[]}}\n{\"type\":\"session_status\"}\n{\"type\":\"agent_idle\"}\n",
            )
            .await
            .unwrap();
        let answer = observer.subscribed.await.unwrap().unwrap();
        assert_eq!(answer["data"]["snapshot"], json!([]));
        drop(write);
        drop(lines);
        closed_rx.recv().await.unwrap();
        assert_eq!(
            *records.lock().unwrap(),
            vec!["session_status", "agent_idle"]
        );
        assert_eq!(*closed.lock().unwrap(), vec!["connection closed"]);
    }

    #[tokio::test]
    async fn a_refused_subscription_rejects_and_closes_the_connection() {
        let (client, mut host) = tokio::io::duplex(4096);
        let (handlers, records, closed, mut closed_rx) = handlers();
        let observer = observe_stream(client, "/h.sock".into(), &json!({}), None, handlers);
        host.write_all(b"{\"type\":\"response\",\"id\":\"monitor-subscribe\",\"success\":false,\"error\":\"nope\"}\n{\"type\":\"late\"}\n")
            .await
            .unwrap();
        assert_eq!(
            observer.subscribed.await.unwrap(),
            Err("rpc monitor: /h.sock refused the subscription: nope".to_owned())
        );
        closed_rx.recv().await.unwrap();
        assert!(records.lock().unwrap().is_empty());
        assert_eq!(*closed.lock().unwrap(), vec!["connection closed"]);
    }

    #[tokio::test]
    async fn close_before_the_answer_rejects_it_and_reports_closed_once() {
        let (client, host) = tokio::io::duplex(4096);
        let (handlers, _, closed, mut closed_rx) = handlers();
        let observer = observe_stream(
            client,
            "/h.sock".into(),
            &json!({}),
            Some(b"secret".to_vec()),
            handlers,
        );
        let mut first = vec![0u8; 6];
        let (mut read, _write) = tokio::io::split(host);
        read.read_exact(&mut first).await.unwrap();
        assert_eq!(first, b"secret", "the handshake secret precedes the frame");
        observer.close();
        observer.close();
        closed_rx.recv().await.unwrap();
        assert_eq!(
            observer.subscribed.await.unwrap(),
            Err("rpc monitor: /h.sock: connection closed".to_owned())
        );
        assert_eq!(*closed.lock().unwrap(), vec!["connection closed"]);
    }

    /// 3956-monitor-observer-occupancy.test.ts: every watch kind subscribes as an observer - the
    /// public challenge (`get_protocol_info`, observing) first, then `subscribe_sessions`; an
    /// endpoint answering without `data.auth` gets no `authenticate`.
    #[cfg(unix)]
    #[tokio::test]
    async fn every_target_subscribes_as_an_observer_never_as_an_attached_client() {
        for text in ["children", "all", "parent-3956"] {
            let dir = tempfile::tempdir().unwrap();
            let socket_path = dir.path().join("host.sock");
            let listener = tokio::net::UnixListener::bind(&socket_path).unwrap();
            let (lines_tx, mut lines_rx) = tokio::sync::mpsc::unbounded_channel::<Value>();
            let server = tokio::spawn(async move {
                let (client, _) = listener.accept().await.unwrap();
                let (read, mut write) = client.into_split();
                let mut lines = tokio::io::BufReader::new(read).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    let request: Value = serde_json::from_str(&line).unwrap();
                    let answer = json!({"type":"response","id":request["id"],"success":true,"data":{"snapshot":[]}});
                    let _ = lines_tx.send(request);
                    write
                        .write_all(format!("{answer}\n").as_bytes())
                        .await
                        .unwrap();
                }
            });
            let target =
                crate::rpc_monitor_targets::parse_rpc_target(text, Some("parent-3956")).unwrap();
            let subscription = crate::rpc_monitor_targets::subscription_for(
                &target,
                &crate::rpc_monitor_targets::RpcEvents::Kinds(vec!["session_status".into()]),
            );
            let (handlers, _, closed, _closed_rx) = handlers();
            let observer = open_rpc_observer(
                socket_path.to_str().unwrap(),
                &subscription,
                handlers,
                Some(dir.path()),
            )
            .await
            .unwrap();
            observer.subscribed.await.unwrap().unwrap();
            let first = lines_rx.recv().await.unwrap();
            let second = lines_rx.recv().await.unwrap();
            assert_eq!(
                (first["type"].as_str(), first["observe"].as_bool()),
                (Some("get_protocol_info"), Some(true))
            );
            assert_eq!(
                (second["type"].as_str(), second["observe"].as_bool()),
                (Some("subscribe_sessions"), Some(true))
            );
            assert!(lines_rx.try_recv().is_err(), "no authenticate, no attach");
            assert!(closed.lock().unwrap().is_empty());
            observer.close();
            server.abort();
        }
    }
}
