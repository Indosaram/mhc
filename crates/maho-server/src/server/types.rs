use super::errors::ServerError;
use serde_json::Value;
use std::{future::Future, pin::Pin, sync::Arc};
use tokio::sync::watch;

pub type ServerFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, ServerError>> + Send + 'a>>;
pub type Publisher = Arc<dyn Fn(String, Value) -> ServerFuture<'static, ()> + Send + Sync>;

#[derive(Clone)]
pub struct Context {
    pub cancelled: watch::Receiver<bool>,
}

pub trait RoutedSessionAttachment: Send + Sync {
    fn invoke_service<'a>(
        &'a self,
        call: Value,
        publish: Publisher,
        context: Context,
    ) -> ServerFuture<'a, Option<Value>>;
    fn release(&self) -> ServerFuture<'_, ()>;
}
pub trait RoutedSessionHandle: Send + Sync {
    fn attach_client(&self) -> ServerFuture<'_, Arc<dyn RoutedSessionAttachment>>;
    fn close(&self) -> ServerFuture<'_, ()>;
}
pub trait RoutedServerServiceAttachment: Send + Sync {
    fn invoke_service<'a>(
        &'a self,
        call: Value,
        publish: Publisher,
        context: Context,
    ) -> ServerFuture<'a, Option<Value>>;
    fn release(&self) -> ServerFuture<'_, ()>;
}
pub trait RoutedServerServiceHost: Send + Sync {
    fn attach_client(
        &self,
        presentation: Arc<dyn RoutedServerPresentation>,
    ) -> ServerFuture<'_, Arc<dyn RoutedServerServiceAttachment>>;
}
pub trait RoutedServerPresentation: Send + Sync {
    fn attach_session<'a>(&'a self, session_id: &'a str) -> ServerFuture<'a, ()>;
    fn detach_session(&self) -> ServerFuture<'_, ()>;
    fn prepare_session_removal<'a>(&'a self, session_id: &'a str) -> ServerFuture<'a, ()>;
}
pub trait ServerHost: Send + Sync {
    fn server_services(&self) -> &dyn RoutedServerServiceHost;
    fn resolve_session<'a>(&'a self, session_id: &'a str) -> ServerFuture<'a, Value>;
    fn open_session(&self, metadata: Value) -> ServerFuture<'_, Arc<dyn RoutedSessionHandle>>;
}

pub trait ByteConnection: Send + Sync {
    fn closed(&self) -> bool;
    fn send<'a>(&'a self, chunk: &'a [u8]) -> ServerFuture<'a, ()>;
    fn close<'a>(&'a self, final_chunk: Option<&'a [u8]>) -> ServerFuture<'a, ()>;
}
