//! Async socket transport for the daemon endpoint (Node `net` equivalent).
//!
//! Unix domain sockets are implemented; Windows named pipes are a documented gap and fail
//! with `Unsupported` (see parity.md).

use std::io;
use std::path::Path;
use std::pin::Pin;

use tokio::io::{AsyncRead, AsyncWrite};

pub trait AsyncStream: AsyncRead + AsyncWrite + Send + Unpin {}

impl<T: AsyncRead + AsyncWrite + Send + Unpin> AsyncStream for T {}

pub type Stream = Pin<Box<dyn AsyncStream>>;

pub async fn connect(path: &Path) -> io::Result<Stream> {
    #[cfg(unix)]
    {
        Ok(Box::pin(tokio::net::UnixStream::connect(path).await?))
    }
    #[cfg(not(unix))]
    {
        let _unused = path;
        Err(unsupported())
    }
}

/// Bound endpoint accepting connections.
pub struct Listener {
    #[cfg(unix)]
    inner: tokio::net::UnixListener,
}

impl Listener {
    pub fn bind(path: &Path) -> io::Result<Self> {
        #[cfg(unix)]
        {
            Ok(Self {
                inner: tokio::net::UnixListener::bind(path)?,
            })
        }
        #[cfg(not(unix))]
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
        #[cfg(not(unix))]
        {
            Err(unsupported())
        }
    }
}

#[cfg(not(unix))]
fn unsupported() -> io::Error {
    io::Error::new(
        io::ErrorKind::Unsupported,
        crate::platform::UnsupportedPlatform("windows named pipes"),
    )
}
