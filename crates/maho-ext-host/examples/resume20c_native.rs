use maho_ext_api::*;
use maho_ext_host::loader::NativeExtensionFactory;
use maho_test_support::{faux::FauxScript, faux_session::FauxSession};
use std::sync::{Arc, Mutex};

struct CatalogExtension(Arc<Mutex<Vec<String>>>);
impl Extension for CatalogExtension {
    fn register(&self, api: &mut ExtensionApi) {
        let retained = Arc::new(Mutex::new(ExtensionApi::new(api.registered.clone(), api.profile.clone(), api.events.clone(), api.runtime.clone())));
        let events = self.0.clone();
        api.on(EventKind::SessionStart, Arc::new(move |_, context| {
            let retained = retained.clone();
            let events = events.clone();
            Box::pin(async move {
                let mut api = retained.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                api.register_mcp_server("native", McpServerDeclaration { command: Some("catalog-v2".into()), ..Default::default() });
                api.register_command("catalog", None, None, Arc::new(move |args, context| {
                    let events = events.clone();
                    Box::pin(async move {
                        let catalog = context.get_registered_mcp_servers();
                        let command = catalog.first().and_then(|server| server.config.command.as_deref());
                        if command != Some("catalog-v2") || args != "native" {
                            return Err(ExtensionFailure::new("Native command saw a stale catalog"));
                        }
                        events.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push(format!("command:{args}:{}", command.unwrap_or_default()));
                        Ok(())
                    })
                }));
                if context.get_registered_mcp_servers().first().map(|server| server.name.as_str()) != Some("native") {
                    return Err(ExtensionFailure::new("Retained startup context did not observe late MCP registration"));
                }
                Ok(EventResult::None)
            })
        }));
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let events = Arc::new(Mutex::new(Vec::new()));
    let session = FauxSession::new(FauxScript { name: "resume20c-native".into(), prompt: "/catalog native".into(), responses: Vec::new() })
        .with_native_extension(NativeExtensionFactory { path: "<resume20c>".into(), source_info: SourceInfo { source: "inline".into(), ..Default::default() }, extension: Box::new(CatalogExtension(events.clone())) });
    let result = tokio::time::timeout(std::time::Duration::from_secs(10), session.run_native()).await??;
    let observed = events.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    if observed.as_slice() != ["command:native:catalog-v2"] || result["messages"].as_array().is_none_or(|messages| !messages.is_empty()) {
        return Err("Native session did not execute the late command without a provider turn".into());
    }
    println!("PASS native AgentSession startup -> late MCP/command registration -> /catalog native -> catalog-v2; provider turns=0");
    println!("cleanup: FauxSession disposed AgentSession and removed its temporary directory");
    Ok(())
}
