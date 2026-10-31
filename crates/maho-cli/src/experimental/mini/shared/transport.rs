use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
pub struct JsonConnection<R, W> { input: BufReader<R>, output: W, closed: bool, buffered: Vec<u8> }
impl<R: AsyncRead + Unpin, W: AsyncWrite + Unpin> JsonConnection<R, W> {
    pub fn new(input: R, output: W) -> Self { Self { input: BufReader::new(input), output, closed: false, buffered: Vec::new() } }
    pub async fn send(&mut self, message: &Value) -> std::io::Result<()> {
        if self.closed { return Ok(()); }
        let mut line = serde_json::to_vec(message)?; line.push(b'\n');
        if let Err(error) = self.output.write_all(&line).await { self.closed = true; return Err(error); }
        Ok(())
    }
    pub async fn receive(&mut self) -> std::io::Result<Option<Value>> {
        self.receive_typed().await
    }
    pub async fn receive_typed<T: serde::de::DeserializeOwned>(&mut self) -> std::io::Result<Option<T>> {
        if self.closed { return Ok(None); }
        loop {
            match self.input.read_until(b'\n', &mut self.buffered).await {
                Ok(0) => { self.closed = true; return Ok(None); }
                Err(error) => { self.closed = true; return Err(error); }
                Ok(_) => {
                    let mut line = std::mem::take(&mut self.buffered);
                    if line.last() != Some(&b'\n') { self.closed = true; return Ok(None); }
                    line.pop();
                    if line.is_empty() { continue; }
                    return serde_json::from_slice(&line).map(Some).map_err(std::io::Error::other);
                }
            }
        }
    }
    pub async fn close(&mut self) -> std::io::Result<()> { self.closed = true; self.output.shutdown().await }
    pub fn closed(&self) -> bool { self.closed }
}
/// senpi `childConnection`: a spawned child is connected at birth; the parent reads its stdout and writes its stdin.
pub type ChildConnection = JsonConnection<tokio::process::ChildStdout, tokio::process::ChildStdin>;
/// senpi `childConnection` minus the close hook: takes the child's pipes, or fails when it was spawned without them.
pub fn child_connection(child: &mut tokio::process::Child) -> Result<ChildConnection, String> {
    match (child.stdout.take(), child.stdin.take()) {
        (Some(input), Some(output)) => Ok(JsonConnection::new(input, output)),
        _ => Err("Child process was spawned without pipes".to_owned()),
    }
}
/// senpi `childConnection`'s close: `signalSpawnedChild` (senpi#3706). A child without a pid (never started, or
/// already reaped) is never signalled; returns whether a signal was sent.
pub fn signal_spawned_child(child: &mut tokio::process::Child) -> bool {
    child.id().is_some() && child.start_kill().is_ok()
}
/// senpi `parentConnection`: the child's own view of the pipes its parent created.
pub fn parent_connection() -> JsonConnection<tokio::io::Stdin, tokio::io::Stdout> {
    JsonConnection::new(tokio::io::stdin(), tokio::io::stdout())
}
#[cfg(unix)]
pub type SocketConnection = JsonConnection<tokio::net::unix::OwnedReadHalf, tokio::net::unix::OwnedWriteHalf>;
#[cfg(unix)]
pub struct SocketTransport { pub path: std::path::PathBuf }
#[cfg(unix)]
impl SocketTransport {
    pub async fn listen(&self) -> std::io::Result<tokio::net::UnixListener> {
        if let Err(error) = tokio::fs::remove_file(&self.path).await && error.kind() != std::io::ErrorKind::NotFound { return Err(error); }
        tokio::net::UnixListener::bind(&self.path)
    }
    pub async fn connect(&self) -> std::io::Result<SocketConnection> {
        let socket = tokio::net::UnixStream::connect(&self.path).await?;
        let (input, output) = socket.into_split();
        Ok(JsonConnection::new(input, output))
    }
}
