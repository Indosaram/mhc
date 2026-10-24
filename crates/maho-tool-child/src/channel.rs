//! senpi `tool-child/channel.ts`: the private channel between the server and one tool child generation (senpi#3566,
//! plan todo 4).
//!
//! The server listens on a per-generation socket inside a 0700 directory, guarded by a 0600 secret the child must send
//! first (`socket-transport.ts`), and accepts exactly one connection; the listener closes after it. The child learns the
//! address and the secret file from its environment, never from argv. Frames are `codec.ts` length-prefixed frames, never
//! stdout, so nothing a tool prints can corrupt the protocol. Any codec error is terminal for the channel.
use std::{
    collections::HashMap,
    io,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, MutexGuard, PoisonError,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    sync::{mpsc, oneshot, watch},
    task::JoinHandle,
};

use crate::{
    codec::{FrameDecoder, ToolChildCodecError, ToolChildCodecErrorCode, encode_frame},
    protocol::ToolChildMessage,
    rpc_socket::{
        authenticate_socket, read_socket_secret, resolve_socket_transport_address,
        send_socket_handshake,
    },
    socket_root::{claim_listener_files, remove_file_force, rm_socket},
};

pub const TOOL_CHILD_ROLE_ENV: &str = "SENPI_INTERNAL_ROLE";
pub const TOOL_CHILD_CHANNEL_ENV: &str = "SENPI_TOOL_CHILD_CHANNEL";
pub const TOOL_CHILD_SECRET_ENV: &str = "SENPI_TOOL_CHILD_CHANNEL_SECRET";

/// How long a closing channel keeps flushing frames it already accepted before the socket is destroyed.
const FLUSH_GRACE: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChannelCloseReason {
    Eof,
    Codec { code: ToolChildCodecErrorCode },
    Error { message: String },
}

fn codec_reason(error: &ToolChildCodecError) -> ChannelCloseReason {
    ChannelCloseReason::Codec { code: error.code }
}

type Listener = Arc<dyn Fn(ToolChildMessage) + Send + Sync>;

#[derive(Default)]
struct ChannelState {
    listeners: Vec<(u64, Listener)>,
    next_listener: u64,
    /// Frames that arrive before anyone listens (a child's `hello` races the server's subscription) wait here.
    early: Vec<ToolChildMessage>,
    reason: Option<ChannelCloseReason>,
    writer: Option<mpsc::UnboundedSender<Vec<u8>>>,
}

struct ChannelInner {
    state: Mutex<ChannelState>,
    closed: watch::Sender<Option<ChannelCloseReason>>,
}

impl ChannelInner {
    fn lock(&self) -> MutexGuard<'_, ChannelState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn finish(&self, next: ChannelCloseReason) {
        {
            let mut state = self.lock();
            if state.reason.is_some() {
                return;
            }
            state.reason = Some(next.clone());
            // Dropping the sender ends the writer task once it has flushed what was already queued.
            state.writer = None;
        }
        self.closed.send_replace(Some(next));
    }
}

/// One framed connection. Cloning shares the same connection.
#[derive(Clone)]
pub struct ToolChildChannel {
    inner: Arc<ChannelInner>,
}

/// What a channel user needs of the channel: send one frame (`Pick<ToolChildChannel, "send">`). Implemented by
/// [`ToolChildChannel`]; host calls, job updates and the launch gate take it as `Arc<dyn ChannelSend>`.
pub trait ChannelSend: Send + Sync {
    /// `false` once the channel is closed or the frame cannot be encoded.
    fn send(&self, message: ToolChildMessage) -> bool;
}

impl ChannelSend for ToolChildChannel {
    fn send(&self, message: ToolChildMessage) -> bool {
        ToolChildChannel::send(self, &message).unwrap_or(false)
    }
}

impl ToolChildChannel {
    /// `Ok(false)` once the channel is closed (or its writer is gone); an unencodable message is an `Err` and leaves
    /// the channel open, like `encodeFrame` throwing from `send`.
    pub fn send(&self, message: &ToolChildMessage) -> Result<bool, ToolChildCodecError> {
        let frame = encode_frame(message)?;
        let state = self.inner.lock();
        if state.reason.is_some() {
            return Ok(false);
        }
        Ok(state
            .writer
            .as_ref()
            .is_some_and(|writer| writer.send(frame).is_ok()))
    }

    /// Subscribes; the returned function unsubscribes.
    pub fn on_message(
        &self,
        listener: impl Fn(ToolChildMessage) + Send + Sync + 'static,
    ) -> Box<dyn FnOnce() + Send> {
        let listener: Listener = Arc::new(listener);
        let (id, early) = {
            let mut state = self.inner.lock();
            let id = state.next_listener;
            state.next_listener += 1;
            state.listeners.push((id, Arc::clone(&listener)));
            (id, std::mem::take(&mut state.early))
        };
        for message in early {
            listener(message);
        }
        let inner = Arc::clone(&self.inner);
        Box::new(move || {
            inner
                .lock()
                .listeners
                .retain(|(listener_id, _)| *listener_id != id)
        })
    }

    /// Resolves once the channel has closed, with the reason.
    pub async fn closed(&self) -> ChannelCloseReason {
        let mut receiver = self.inner.closed.subscribe();
        match receiver.wait_for(Option::is_some).await {
            Ok(reason) => reason.clone().unwrap_or(ChannelCloseReason::Eof),
            Err(_) => ChannelCloseReason::Eof,
        }
    }

    pub fn close(&self) {
        self.inner.finish(ChannelCloseReason::Eof);
    }
}

pub fn wrap_channel_socket<S>(socket: S) -> ToolChildChannel
where
    S: AsyncRead + AsyncWrite + Send + 'static,
{
    let (read_half, write_half) = tokio::io::split(socket);
    let (writer_tx, writer_rx) = mpsc::unbounded_channel();
    let (closed, closed_rx) = watch::channel(None);
    let inner = Arc::new(ChannelInner {
        state: Mutex::new(ChannelState {
            writer: Some(writer_tx),
            ..ChannelState::default()
        }),
        closed,
    });
    tokio::spawn(read_loop(Arc::clone(&inner), read_half, closed_rx.clone()));
    tokio::spawn(write_loop(
        Arc::clone(&inner),
        write_half,
        writer_rx,
        closed_rx,
    ));
    ToolChildChannel { inner }
}

async fn read_loop<R: AsyncRead>(
    inner: Arc<ChannelInner>,
    read_half: tokio::io::ReadHalf<R>,
    mut closed: watch::Receiver<Option<ChannelCloseReason>>,
) {
    let mut read_half = Box::pin(read_half);
    let mut decoder = FrameDecoder::new();
    let mut buffer = vec![0u8; 64 * 1024];
    loop {
        let read = tokio::select! {
            read = read_half.read(&mut buffer) => read,
            _ = closed.wait_for(Option::is_some) => return,
        };
        match read {
            Ok(0) => {
                // "end"
                match decoder.end() {
                    Ok(()) => inner.finish(ChannelCloseReason::Eof),
                    Err(error) => inner.finish(codec_reason(&error)),
                }
                return;
            }
            Ok(read) => {
                let messages = match decoder.push(&buffer[..read]) {
                    Ok(messages) => messages,
                    Err(error) => {
                        inner.finish(codec_reason(&error));
                        return;
                    }
                };
                for message in messages {
                    let listeners: Vec<Listener> = {
                        let mut state = inner.lock();
                        if state.reason.is_some() {
                            return;
                        }
                        if state.listeners.is_empty() {
                            state.early.push(message);
                            continue;
                        }
                        state
                            .listeners
                            .iter()
                            .map(|(_, listener)| Arc::clone(listener))
                            .collect()
                    };
                    for listener in listeners {
                        listener(message.clone());
                    }
                }
            }
            Err(error) => {
                inner.finish(ChannelCloseReason::Error {
                    message: error.to_string(),
                });
                return;
            }
        }
    }
}

async fn write_loop<W: AsyncWrite>(
    inner: Arc<ChannelInner>,
    write_half: tokio::io::WriteHalf<W>,
    mut frames: mpsc::UnboundedReceiver<Vec<u8>>,
    mut closed: watch::Receiver<Option<ChannelCloseReason>>,
) {
    let mut write_half = Box::pin(write_half);
    let drain = async {
        while let Some(frame) = frames.recv().await {
            if let Err(error) = write_half.write_all(&frame).await {
                inner.finish(ChannelCloseReason::Error {
                    message: error.to_string(),
                });
                return;
            }
        }
    };
    // A closed channel still flushes what it already accepted, but never waits on a peer that stopped reading.
    let grace = async {
        let _ = closed.wait_for(Option::is_some).await;
        tokio::time::sleep(FLUSH_GRACE).await;
    };
    tokio::select! {
        () = drain => {}
        () = grace => {}
    }
    let _ = write_half.shutdown().await;
}

#[cfg(unix)]
type AcceptedStream = tokio::net::UnixStream;
#[cfg(windows)]
type AcceptedStream = tokio::net::windows::named_pipe::NamedPipeServer;

/// The platform listener: a unix socket at the logical path, or a secret-derived named pipe on Windows.
#[cfg(unix)]
struct Acceptor {
    listener: tokio::net::UnixListener,
}

#[cfg(unix)]
impl Acceptor {
    fn bind(address: &str) -> io::Result<Self> {
        Ok(Self {
            listener: tokio::net::UnixListener::bind(address)?,
        })
    }

    async fn accept(&mut self) -> io::Result<AcceptedStream> {
        self.listener.accept().await.map(|(stream, _)| stream)
    }
}

#[cfg(windows)]
struct Acceptor {
    address: String,
    server: tokio::net::windows::named_pipe::NamedPipeServer,
}

#[cfg(windows)]
impl Acceptor {
    fn bind(address: &str) -> io::Result<Self> {
        use tokio::net::windows::named_pipe::ServerOptions;
        let server = ServerOptions::new()
            .first_pipe_instance(true)
            .create(address)?;
        Ok(Self {
            address: address.to_owned(),
            server,
        })
    }

    async fn accept(&mut self) -> io::Result<AcceptedStream> {
        use tokio::net::windows::named_pipe::ServerOptions;
        self.server.connect().await?;
        let next = ServerOptions::new().create(&self.address)?;
        Ok(std::mem::replace(&mut self.server, next))
    }
}

pub struct ChannelListener {
    /// `TOOL_CHILD_CHANNEL_ENV` and `TOOL_CHILD_SECRET_ENV`.
    pub env: HashMap<String, String>,
    accepted: Mutex<Option<oneshot::Receiver<ToolChildChannel>>>,
    taken: Arc<AtomicBool>,
    accept_task: JoinHandle<()>,
    logical_path: PathBuf,
    secret_path: PathBuf,
}

impl ChannelListener {
    /// The first authenticated connection; the listener stops accepting once it arrives. Call once.
    pub async fn accepted(&self) -> io::Result<ToolChildChannel> {
        let receiver = self
            .accepted
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        let receiver =
            receiver.ok_or_else(|| io::Error::other("tool-child: channel already accepted"))?;
        receiver
            .await
            .map_err(|_| io::Error::other("tool-child: channel listener closed"))
    }

    pub async fn close(&self) -> io::Result<()> {
        self.taken.store(true, Ordering::SeqCst);
        // Stops accepting only: the accepted channel itself is not ours to end.
        self.accept_task.abort();
        // The socket goes before the secret: while our secret is in it the root is non-empty, so no sibling sweep can
        // remove it and no other user can put anything at its name that this removal would reach (senpi#3694).
        if cfg!(not(windows)) {
            rm_socket(&self.logical_path)?;
        }
        remove_file_force(&self.secret_path)?;
        // The socket root is shared by every generation of this session's tool-child dir (and, on the fallback,
        // by every process starting one), so rmdir removes it only once it is empty. A sibling that claimed the
        // root but has not written its secret yet can lose it here; `claim_listener_files` re-claims and retries.
        if let Some(root) = self.logical_path.parent() {
            let _ = std::fs::remove_dir(root);
        }
        Ok(())
    }
}

pub async fn open_channel_listener(dir: &Path, generation: u64) -> io::Result<ChannelListener> {
    let files = claim_listener_files(dir, generation).await?;
    let logical = files.logical_path.to_string_lossy().into_owned();
    let (accepted_tx, accepted_rx) = oneshot::channel();
    let taken = Arc::new(AtomicBool::new(false));
    let address = resolve_socket_transport_address(&logical, platform(), Some(&files.secret))
        .map_err(io::Error::other)?;
    let mut acceptor = Acceptor::bind(&address)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&files.logical_path, std::fs::Permissions::from_mode(0o600))?;
    }
    let accept_task = tokio::spawn({
        let taken = Arc::clone(&taken);
        let secret = files.secret;
        let logical_path = files.logical_path.clone();
        async move {
            let (authenticated_tx, mut authenticated_rx) = mpsc::unbounded_channel();
            let stream = loop {
                tokio::select! {
                    accepted = acceptor.accept() => {
                        let Ok(mut socket) = accepted else { continue };
                        if taken.load(Ordering::SeqCst) {
                            continue;
                        }
                        let authenticated_tx = authenticated_tx.clone();
                        tokio::spawn(async move {
                            if authenticate_socket(&mut socket, &secret).await.is_ok() {
                                let _ = authenticated_tx.send(socket);
                            }
                        });
                    }
                    Some(socket) = authenticated_rx.recv() => {
                        if taken.swap(true, Ordering::SeqCst) {
                            continue;
                        }
                        break socket;
                    }
                }
            };
            // `server.close()`: stop listening (and unlink the socket) before the channel is handed out.
            drop(acceptor);
            #[cfg(unix)]
            let _ = remove_file_force(&logical_path);
            #[cfg(not(unix))]
            let _ = logical_path;
            let _ = accepted_tx.send(wrap_channel_socket(stream));
        }
    });
    let env = HashMap::from([
        (TOOL_CHILD_CHANNEL_ENV.to_owned(), logical),
        (
            TOOL_CHILD_SECRET_ENV.to_owned(),
            files.secret_path.to_string_lossy().into_owned(),
        ),
    ]);
    Ok(ChannelListener {
        env,
        accepted: Mutex::new(Some(accepted_rx)),
        taken,
        accept_task,
        logical_path: files.logical_path,
        secret_path: files.secret_path,
    })
}

/// `process.platform`, as `resolve_socket_transport_address` spells it.
fn platform() -> &'static str {
    if cfg!(windows) { "win32" } else { "other" }
}

pub fn channel_env_present(env: &HashMap<String, String>) -> bool {
    let set = |key: &str| env.get(key).is_some_and(|value| !value.is_empty());
    set(TOOL_CHILD_CHANNEL_ENV) && set(TOOL_CHILD_SECRET_ENV)
}

pub async fn connect_channel(env: &HashMap<String, String>) -> io::Result<ToolChildChannel> {
    let logical = env
        .get(TOOL_CHILD_CHANNEL_ENV)
        .filter(|value| !value.is_empty());
    let secret_path = env
        .get(TOOL_CHILD_SECRET_ENV)
        .filter(|value| !value.is_empty());
    let (Some(logical), Some(secret_path)) = (logical, secret_path) else {
        return Err(io::Error::other("tool-child: channel unavailable"));
    };
    let secret = read_socket_secret(Path::new(secret_path))?;
    let address = resolve_socket_transport_address(logical, platform(), Some(&secret))
        .map_err(io::Error::other)?;
    #[cfg(unix)]
    let mut socket = tokio::net::UnixStream::connect(&address).await?;
    #[cfg(windows)]
    let mut socket = {
        use tokio::net::windows::named_pipe::ClientOptions;
        // ERROR_PIPE_BUSY: every instance is serving a peer; the next one is created right after, so try again.
        const ERROR_PIPE_BUSY: i32 = 231;
        let mut attempts = 0;
        loop {
            match ClientOptions::new().open(&address) {
                Ok(client) => break client,
                Err(error) if error.raw_os_error() == Some(ERROR_PIPE_BUSY) && attempts < 100 => {
                    attempts += 1;
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
                Err(error) => return Err(error),
            }
        }
    };
    send_socket_handshake(&mut socket, &secret).await?;
    Ok(wrap_channel_socket(socket))
}

#[cfg(all(test, unix))]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use serde_json::json;
    use tokio::time::timeout;

    use super::*;

    const BOUND: Duration = Duration::from_secs(10);

    fn message(value: serde_json::Value) -> ToolChildMessage {
        serde_json::from_value(value).expect("valid tool-child message")
    }

    /// A fresh `<tempdir>/ch` channel directory; the tempdir removes everything under it when dropped.
    fn scratch() -> (tempfile::TempDir, PathBuf) {
        let root = tempfile::Builder::new()
            .prefix("tcc-")
            .tempdir()
            .expect("tempdir");
        let dir = root.path().join("ch");
        (root, dir)
    }

    fn next_message(channel: &ToolChildChannel) -> oneshot::Receiver<ToolChildMessage> {
        let (tx, rx) = oneshot::channel();
        let tx = Mutex::new(Some(tx));
        // The subscription lives as long as the channel does, like upstream's discarded unsubscribe.
        let _unsubscribe = channel.on_message(move |message| {
            if let Some(tx) = tx.lock().unwrap_or_else(PoisonError::into_inner).take() {
                let _ = tx.send(message);
            }
        });
        rx
    }

    #[tokio::test]
    async fn carries_frames_both_ways_between_one_authenticated_child_and_the_server() {
        let _serial = crate::socket_root::test_window::serial().await;
        let (_root, dir) = scratch();
        let listener = open_channel_listener(&dir, 3).await.expect("listener");
        let child = connect_channel(&listener.env).await.expect("child");
        let server = timeout(BOUND, listener.accepted())
            .await
            .expect("bounded")
            .expect("accepted");
        let at_server = next_message(&server);
        let hello = message(
            json!({ "type": "hello", "generation": 3, "pid": std::process::id(), "protocol": 2, "jobKinds": ["module"] }),
        );
        assert_eq!(child.send(&hello), Ok(true));
        assert_eq!(
            timeout(BOUND, at_server)
                .await
                .expect("bounded")
                .expect("message"),
            hello
        );
        let big = "x".repeat(3 * 1024 * 1024);
        let at_child = next_message(&child);
        let reply = message(json!({ "type": "host_reply", "callId": "c1", "value": big }));
        assert_eq!(server.send(&reply), Ok(true));
        assert_eq!(
            timeout(BOUND, at_child)
                .await
                .expect("bounded")
                .expect("message"),
            reply
        );
        child.close();
        assert_eq!(
            timeout(BOUND, server.closed()).await.expect("bounded"),
            ChannelCloseReason::Eof
        );
        listener.close().await.expect("close");
    }

    #[tokio::test]
    async fn keeps_the_channel_directory_private_and_removes_its_secret_on_close() {
        let _serial = crate::socket_root::test_window::serial().await;
        let (_root, dir) = scratch();
        let listener = open_channel_listener(&dir, 1).await.expect("listener");
        assert_eq!(
            std::fs::metadata(&dir).expect("stat").permissions().mode() & 0o777,
            0o700
        );
        let secret = listener.env[TOOL_CHILD_SECRET_ENV].clone();
        assert_eq!(
            std::fs::metadata(&secret)
                .expect("stat")
                .permissions()
                .mode()
                & 0o077,
            0
        );
        listener.close().await.expect("close");
        assert_eq!(
            std::fs::metadata(&secret).expect_err("removed").kind(),
            io::ErrorKind::NotFound
        );
    }

    #[tokio::test]
    async fn never_accepts_a_peer_without_the_secret_and_accepts_no_second_child() {
        let _serial = crate::socket_root::test_window::serial().await;
        let (_root, dir) = scratch();
        let listener = open_channel_listener(&dir, 2).await.expect("listener");
        let mut intruder = tokio::net::UnixStream::connect(&listener.env[TOOL_CHILD_CHANNEL_ENV])
            .await
            .expect("connect");
        intruder.write_all(&[7u8; 32]).await.expect("write");
        // The intruder is closed by the server: its read ends instead of waiting for a handshake it will never get.
        let mut sink = Vec::new();
        let _ = timeout(BOUND, intruder.read_to_end(&mut sink))
            .await
            .expect("intruder closed");
        let child = connect_channel(&listener.env).await.expect("child");
        let server = timeout(BOUND, listener.accepted())
            .await
            .expect("bounded")
            .expect("accepted");
        assert!(connect_channel(&listener.env).await.is_err());
        child.close();
        server.close();
        listener.close().await.expect("close");
    }

    #[tokio::test]
    async fn closes_on_a_malformed_frame_instead_of_resynchronizing() {
        let _serial = crate::socket_root::test_window::serial().await;
        let (_root, dir) = scratch();
        let listener = open_channel_listener(&dir, 4).await.expect("listener");
        let secret =
            read_socket_secret(Path::new(&listener.env[TOOL_CHILD_SECRET_ENV])).expect("secret");
        let mut raw = tokio::net::UnixStream::connect(&listener.env[TOOL_CHILD_CHANNEL_ENV])
            .await
            .expect("connect");
        send_socket_handshake(&mut raw, &secret)
            .await
            .expect("handshake");
        let server = timeout(BOUND, listener.accepted())
            .await
            .expect("bounded")
            .expect("accepted");
        let mut frame = 3u32.to_be_bytes().to_vec();
        frame.extend_from_slice(b"{x}");
        raw.write_all(&frame).await.expect("write");
        assert_eq!(
            timeout(BOUND, server.closed()).await.expect("bounded"),
            ChannelCloseReason::Codec {
                code: ToolChildCodecErrorCode::ToolChildMalformed
            }
        );
        drop(raw);
        listener.close().await.expect("close");
    }

    #[tokio::test]
    async fn delivers_frames_that_arrive_before_the_server_subscribes() {
        let _serial = crate::socket_root::test_window::serial().await;
        let (_root, dir) = scratch();
        let listener = open_channel_listener(&dir, 5).await.expect("listener");
        let child = connect_channel(&listener.env).await.expect("child");
        let hello = message(
            json!({ "type": "hello", "generation": 5, "pid": 1, "protocol": 2, "jobKinds": [] }),
        );
        child.send(&hello).expect("send hello");
        child
            .send(&message(json!({ "type": "pong", "seq": 9 })))
            .expect("send pong");
        let server = timeout(BOUND, listener.accepted())
            .await
            .expect("bounded")
            .expect("accepted");
        let (tx, mut rx) = mpsc::unbounded_channel();
        let _unsubscribe = server.on_message(move |message| {
            let _ = tx.send(message);
        });
        let mut seen = Vec::new();
        while seen.len() < 2 {
            seen.push(
                timeout(BOUND, rx.recv())
                    .await
                    .expect("bounded")
                    .expect("message"),
            );
        }
        let types: Vec<_> = seen.iter().map(ToolChildMessage::message_type).collect();
        assert_eq!(types, ["hello", "pong"]);
        child.close();
        listener.close().await.expect("close");
    }

    #[test]
    fn reports_whether_the_environment_names_a_channel() {
        let mut env = HashMap::new();
        assert!(!channel_env_present(&env));
        env.insert(TOOL_CHILD_CHANNEL_ENV.to_owned(), "/x".to_owned());
        assert!(!channel_env_present(&env));
        env.insert(TOOL_CHILD_SECRET_ENV.to_owned(), "/y".to_owned());
        assert!(channel_env_present(&env));
    }
}
