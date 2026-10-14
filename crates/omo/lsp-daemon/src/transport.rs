//! Async socket transport for the daemon endpoint (Node `net` equivalent).
//!
//! Unix domain sockets on unix; Windows named pipes (`\\.\pipe\omo-lsp-*`) on Windows, matching
//! Node's `net.connect`/`server.listen` on a pipe path (see parity.d/).

use std::io;
use std::path::Path;
use std::pin::Pin;

use tokio::io::{AsyncRead, AsyncWrite};

pub trait AsyncStream: AsyncRead + AsyncWrite + Send + Unpin {}

impl<T: AsyncRead + AsyncWrite + Send + Unpin> AsyncStream for T {}

pub type Stream = Pin<Box<dyn AsyncStream>>;

/// Delay between attempts while every pipe instance is busy (libuv waits on `ERROR_PIPE_BUSY`).
#[cfg(windows)]
const PIPE_BUSY_RETRY: std::time::Duration = std::time::Duration::from_millis(50);

pub async fn connect(path: &Path) -> io::Result<Stream> {
    #[cfg(unix)]
    {
        Ok(Box::pin(tokio::net::UnixStream::connect(path).await?))
    }
    #[cfg(windows)]
    {
        use tokio::net::windows::named_pipe::ClientOptions;
        use windows_sys::Win32::Foundation::ERROR_PIPE_BUSY;
        loop {
            match ClientOptions::new().open(path) {
                Ok(client) => return Ok(Box::pin(client)),
                Err(error) if error.raw_os_error() == i32::try_from(ERROR_PIPE_BUSY).ok() => {}
                Err(error) => return Err(error),
            }
            tokio::time::sleep(PIPE_BUSY_RETRY).await;
        }
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _unused = path;
        Err(unsupported())
    }
}

/// Bound endpoint accepting connections.
pub struct Listener {
    #[cfg(unix)]
    inner: tokio::net::UnixListener,
    #[cfg(windows)]
    path: std::ffi::OsString,
    #[cfg(windows)]
    pending: tokio::sync::Mutex<tokio::net::windows::named_pipe::NamedPipeServer>,
}

impl Listener {
    pub fn bind(path: &Path) -> io::Result<Self> {
        #[cfg(unix)]
        {
            Ok(Self {
                inner: tokio::net::UnixListener::bind(path)?,
            })
        }
        #[cfg(windows)]
        {
            use tokio::net::windows::named_pipe::ServerOptions;
            // First instance fails when another process already owns the pipe (EADDRINUSE).
            let pending = ServerOptions::new()
                .first_pipe_instance(true)
                .create(path)?;
            Ok(Self {
                path: path.as_os_str().to_os_string(),
                pending: tokio::sync::Mutex::new(pending),
            })
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _unused = path;
            Err(unsupported())
        }
    }

    pub async fn accept(&self) -> io::Result<Stream> {
        #[cfg(unix)]
        {
            let (stream, _address) = self.inner.accept().await?;
            Ok(Box::pin(stream))
        }
        #[cfg(windows)]
        {
            use tokio::net::windows::named_pipe::ServerOptions;
            let mut pending = self.pending.lock().await;
            pending.connect().await?;
            let next = ServerOptions::new().create(&self.path)?;
            Ok(Box::pin(std::mem::replace(&mut *pending, next)))
        }
        #[cfg(not(any(unix, windows)))]
        {
            Err(unsupported())
        }
    }
}

#[cfg(not(any(unix, windows)))]
fn unsupported() -> io::Error {
    io::Error::new(
        io::ErrorKind::Unsupported,
        crate::platform::UnsupportedPlatform("daemon socket transport"),
    )
}

#[cfg(test)]
#[path = "transport_tests.rs"]
mod tests;
