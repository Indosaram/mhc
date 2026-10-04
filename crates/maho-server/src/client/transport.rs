use super::errors::ClientError;
use std::{future::Future, pin::Pin, sync::Arc};

pub type TransportFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, ClientError>> + Send + 'a>>;

pub trait ByteTransport: Send + Sync {
    fn send<'a>(&'a self, chunk: &'a [u8]) -> TransportFuture<'a, ()>;
    fn close(&self);
}

pub type DataHandler = Arc<dyn Fn(&[u8]) + Send + Sync>;
pub struct ByteTransportHandlers {
    pub on_data: DataHandler,
    pub on_close: Arc<dyn Fn() + Send + Sync>,
    pub on_error: Arc<dyn Fn(ClientError) + Send + Sync>,
}

pub trait ByteTransportFactory: Send + Sync {
    fn connect(
        &self,
        handlers: ByteTransportHandlers,
    ) -> TransportFuture<'_, Arc<dyn ByteTransport>>;
}
