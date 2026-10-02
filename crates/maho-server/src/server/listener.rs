use super::{errors::ServerError, types::{ByteConnection, ServerFuture}};
use std::sync::Arc;
use tokio::sync::mpsc;

pub struct AcceptedConnection {
    pub connection: Arc<dyn ByteConnection>,
    pub inbound: mpsc::Receiver<Result<Vec<u8>, ServerError>>,
}
pub type ByteConnectionAcceptor = Arc<dyn Fn(AcceptedConnection) -> ServerFuture<'static, ()> + Send + Sync>;
pub trait ServerListener: Send + Sync {
    fn start(&self, accept: ByteConnectionAcceptor) -> ServerFuture<'_, ()>;
    fn close(&self) -> ServerFuture<'_, ()>;
}
pub struct ServerListeners {
    listeners: Vec<Arc<dyn ServerListener>>,
}
impl ServerListeners {
    pub fn new(listeners: Vec<Arc<dyn ServerListener>>) -> Self { Self { listeners } }
    pub async fn start(&self, server: Arc<super::Server>, shutdown: tokio::sync::watch::Receiver<bool>) -> Result<(), ServerError> {
        let startup_server = server.clone();
        let accept = Arc::new(move |accepted: AcceptedConnection| {
            let server = server.clone(); let shutdown = shutdown.clone();
            Box::pin(async move { server.serve(accepted.connection, accepted.inbound, shutdown).await }) as ServerFuture<'static, ()>
        });
        for (index, listener) in self.listeners.iter().enumerate() {
            if let Err(error) = listener.start(accept.clone()).await {
                let mut errors = vec![error.message];
                for started in &self.listeners[..index] { if let Err(error) = started.close().await { errors.push(error.message); } }
                if let Err(error) = startup_server.close().await { errors.push(error.message); }
                return Err(ServerError::new("internal_error", &errors.join("; ")));
            }
        }
        Ok(())
    }
    pub async fn close(&self) -> Result<(), ServerError> {
        let mut errors = Vec::new();
        for listener in &self.listeners { if let Err(error) = listener.close().await { errors.push(error.message); } }
        if errors.is_empty() { Ok(()) } else { Err(ServerError::new("internal_error", &errors.join("; "))) }
    }
}
