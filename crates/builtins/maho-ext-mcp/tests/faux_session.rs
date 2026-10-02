use std::sync::{Arc, Mutex};
use maho_ext_api::{Extension, ExtensionApi, SourceInfo};
use maho_ext_mcp::{host_registry::HostMcpRegistry, service::McpService};
use maho_test_support::{faux::FauxScript, faux_session::FauxSession};

type ServiceCapture = Arc<Mutex<Option<Arc<tokio::sync::Mutex<McpService>>>>>;
struct McpExtension {registry: Arc<HostMcpRegistry>, capture: ServiceCapture}
impl Extension for McpExtension {
    fn register(&self, api: &mut ExtensionApi) {
        let service = maho_ext_mcp::index::register_mcp_lifecycle(api, self.registry.clone(), 1);
        *self.capture.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(service);
    }
}
#[tokio::test]
async fn faux_mcp_command_attaches_native_service_without_provider_turn() {
    let registry = Arc::new(HostMcpRegistry::default()); let capture = ServiceCapture::default();
    let session = FauxSession::new(FauxScript {name:"mcp-native-command".into(), prompt:"/mcp status".into(), responses:vec![]})
        .with_native_extension(maho_ext_host::loader::NativeExtensionFactory {path:"<builtin:mcp>".into(), source_info:SourceInfo::default(), extension:Box::new(McpExtension {registry:registry.clone(), capture:capture.clone()})});
    let result = tokio::time::timeout(std::time::Duration::from_secs(10), session.run_native()).await.unwrap().unwrap();
    assert_eq!(result["messages"], serde_json::json!([]));
    let service = capture.lock().unwrap().as_ref().unwrap().clone();
    assert!(service.lock().await.config.is_some());
    service.lock().await.dispose().await.unwrap(); registry.dispose().await.unwrap();
    assert_eq!(registry.size(), 0);
}
