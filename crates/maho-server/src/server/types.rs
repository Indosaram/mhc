use super::errors::ServerError;
pub use maho_chord::types::ContextRef as Context;
use maho_chord::types::JsonValue as Value;
use std::{future::Future, pin::Pin, sync::Arc};

pub type ServerFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, ServerError>> + Send + 'a>>;
pub type TerminationFuture = Pin<Box<dyn Future<Output = Option<ServerError>> + Send + 'static>>;
pub type Publisher = Arc<dyn Fn(maho_chord::types::JsString, Value, Context) -> ServerFuture<'static, ()> + Send + Sync>;

pub trait RoutedSessionAttachment: Send + Sync {
    fn invoke_service<'a>(
        &'a self,
        call: Value,
        publish: Publisher,
        context: Context,
    ) -> ServerFuture<'a, Option<Value>>;
    fn release(&self, context: Context) -> ServerFuture<'_, ()>;
}
pub trait RoutedSessionHandle: Send + Sync {
    fn attach_client(&self, context: Context)
    -> ServerFuture<'_, Arc<dyn RoutedSessionAttachment>>;
    /// Resolves with an error for unexpected termination, or `None` after an expected close.
    fn terminated(&self) -> Option<TerminationFuture> {
        None
    }
    fn close(&self, context: Context) -> ServerFuture<'_, ()>;
}
pub trait RoutedServerServiceAttachment: Send + Sync {
    fn invoke_service<'a>(
        &'a self,
        call: Value,
        publish: Publisher,
        context: Context,
    ) -> ServerFuture<'a, Option<Value>>;
    fn release(&self, context: Context) -> ServerFuture<'_, ()>;
}
pub trait RoutedServerServiceHost: Send + Sync {
    fn attach_client(
        &self,
        presentation: Arc<dyn RoutedServerPresentation>,
        context: Context,
    ) -> ServerFuture<'_, Arc<dyn RoutedServerServiceAttachment>>;
}
pub trait RoutedServerPresentation: Send + Sync {
    fn attach_session<'a>(&'a self, session_id: &'a str, context: Context) -> ServerFuture<'a, ()>;
    fn detach_session(&self, context: Context) -> ServerFuture<'_, ()>;
    fn prepare_session_removal<'a>(
        &'a self,
        session_id: &'a str,
        context: Context,
    ) -> ServerFuture<'a, ()>;
}
pub trait ServerHost: Send + Sync {
    fn server_services(&self) -> &dyn RoutedServerServiceHost;
    fn resolve_session<'a>(
        &'a self,
        session_id: &'a str,
        context: Context,
    ) -> ServerFuture<'a, Value>;
    fn open_session(
        &self,
        metadata: Value,
        context: Context,
    ) -> ServerFuture<'_, Arc<dyn RoutedSessionHandle>>;
}

pub trait ByteConnection: Send + Sync {
    fn closed(&self) -> bool;
    fn send<'a>(&'a self, chunk: &'a [u8]) -> ServerFuture<'a, ()>;
    fn close<'a>(&'a self, final_chunk: Option<&'a [u8]>) -> ServerFuture<'a, ()>;
}
