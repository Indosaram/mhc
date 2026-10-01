use maho_server::server::{errors::ServerError, session_router::SessionRouter, types::*};
use serde_json::{Value, json};
use std::sync::{Arc, atomic::{AtomicUsize, Ordering}};

struct Host(Arc<AtomicUsize>);
impl ServerHost for Host {
    fn server_services(&self) -> &dyn RoutedServerServiceHost { self }
    fn resolve_session<'a>(&'a self, id: &'a str) -> ServerFuture<'a, Value> {
        Box::pin(async move { Ok(json!({"id":id})) })
    }
    fn open_session(&self, _metadata: Value) -> ServerFuture<'_, Arc<dyn RoutedSessionHandle>> {
        let releases = self.0.clone();
        Box::pin(async move { Ok(Arc::new(Handle(releases)) as Arc<dyn RoutedSessionHandle>) })
    }
}
impl RoutedServerServiceHost for Host {
    fn attach_client(&self, _presentation: Arc<dyn RoutedServerPresentation>) -> ServerFuture<'_, Arc<dyn RoutedServerServiceAttachment>> {
        Box::pin(async { Err(ServerError::new("internal_error", "Unused server services")) })
    }
}
struct Handle(Arc<AtomicUsize>);
impl RoutedSessionHandle for Handle {
    fn attach_client(&self) -> ServerFuture<'_, Arc<dyn RoutedSessionAttachment>> {
        let releases = self.0.clone();
        Box::pin(async move { Ok(Arc::new(Lease(releases)) as Arc<dyn RoutedSessionAttachment>) })
    }
    fn close(&self) -> ServerFuture<'_, ()> { Box::pin(async { Ok(()) }) }
}
struct Lease(Arc<AtomicUsize>);
impl RoutedSessionAttachment for Lease {
    fn invoke_service<'a>(&'a self, call: Value, _publish: Publisher, _context: Context) -> ServerFuture<'a, Option<Value>> {
        Box::pin(async move { Ok(Some(call)) })
    }
    fn release(&self) -> ServerFuture<'_, ()> {
        Box::pin(async move { self.0.fetch_add(1, Ordering::SeqCst); Ok(()) })
    }
}
#[tokio::test]
async fn released_attachment_rejects_calls_and_releases_lease_only_once() {
    let releases = Arc::new(AtomicUsize::new(0));
    let router = SessionRouter::new(Arc::new(Host(releases.clone())), "00000000-0000-4000-8000-000000000001".into());
    let attachment = router.attach("session").await.unwrap();
    attachment.release().await.unwrap();
    attachment.release().await.unwrap();
    let (_cancel, cancelled) = tokio::sync::watch::channel(false);
    let result = attachment.invoke(json!({}), Arc::new(|_, _| Box::pin(async { Ok(()) })), Context { cancelled }).await;
    assert_eq!(result.unwrap_err().code, "session_not_attached");
    assert_eq!(releases.load(Ordering::SeqCst), 1);
    router.close().await.unwrap();
}
