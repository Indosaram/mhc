use maho_ext_api::{ExtensionApi,LoadedExtension,SourceInfo,ExtensionSessionProfile,EventBus,ExtensionRuntime,EventKind};
#[tokio::test]
async fn native_entrypoint_registers_session_shutdown_and_instruction_handlers() {
    let root=tempfile::tempdir().unwrap();let registered=LoadedExtension::new("builtin:mcp",root.path().into(),SourceInfo::default());
    let mut api=ExtensionApi::new(registered,ExtensionSessionProfile::default(),EventBus::default(),ExtensionRuntime::default());
    let service=maho_ext_mcp::index::register_mcp_lifecycle(&mut api,std::sync::Arc::new(maho_ext_mcp::host_registry::HostMcpRegistry::default()),1);
    for event in [EventKind::SessionStart,EventKind::SessionShutdown,EventKind::BeforeAgentStart]{assert_eq!(api.registered.handlers[&event].len(),1);}
    assert!(service.lock().await.connections.is_empty());service.lock().await.dispose().await.unwrap();
}
