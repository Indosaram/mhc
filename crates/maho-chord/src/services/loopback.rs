use super::provider::RemoteServiceProvider;
use crate::types::*;
use std::sync::Arc;
pub struct LoopbackServiceTransport { provider: Arc<RemoteServiceProvider> }
pub fn create_loopback_service_transport(provider: Arc<RemoteServiceProvider>) -> Arc<LoopbackServiceTransport> { Arc::new(LoopbackServiceTransport { provider }) }
impl RemoteServiceTransport for LoopbackServiceTransport {
    fn invoke(&self, call: ServiceCall, context: ContextRef) -> BoxFuture<'_, Result<Option<JsonValue>, ServiceError>> { Box::pin(self.provider.invoke(call, context)) }
    fn subscribe(&self, service_id: JsString, mode: ServiceMode, listener: Arc<dyn Fn(ServiceProviderUpdate, ContextRef) + Send + Sync>, _: ContextRef) -> BoxFuture<'_, Result<Box<dyn ServiceSubscription>, ServiceError>> {
        Box::pin(async move { let subscription = self.provider.subscribe(&service_id, mode, Arc::new(move |u, c| { listener(u, c); Ok(()) }))?; Ok(Box::new(subscription) as Box<dyn ServiceSubscription>) })
    }
}
