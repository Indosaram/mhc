use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
pub struct JsonConnection<R, W> { input: BufReader<R>, output: W, closed: bool }
impl<R: AsyncRead + Unpin, W: AsyncWrite + Unpin> JsonConnection<R, W> {
    pub fn new(input: R, output: W) -> Self { Self { input: BufReader::new(input), output, closed: false } }
    pub async fn send(&mut self, message: &Value) -> std::io::Result<()> {
        if self.closed { return Ok(()); }
        let mut line = serde_json::to_vec(message)?; line.push(b'\n');
        if let Err(error) = self.output.write_all(&line).await { self.closed = true; return Err(error); }
        Ok(())
    }
    pub async fn receive(&mut self) -> std::io::Result<Option<Value>> {
        if self.closed { return Ok(None); }
        loop {
            let mut line = Vec::new();
            match self.input.read_until(b'\n', &mut line).await {
                Ok(0) => { self.closed = true; return Ok(None); }
                Err(error) => { self.closed = true; return Err(error); }
                Ok(_) => {
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
