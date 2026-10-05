use maho_ext_api::*;
use maho_ext_cache_keepalive::CacheKeepalive;
use std::sync::Arc;
#[test]
fn keepalive_factory_registers_scheduler_lifecycle_and_renderer(){
    let mut api=ExtensionApi::new(LoadedExtension::new("keepalive","/tmp".into(),SourceInfo::default()),ExtensionSessionProfile::default(),EventBus::default(),ExtensionRuntime::default());
    CacheKeepalive{warm:Arc::new(|_,_,_,_|Box::pin(async{panic!("registration must not call provider")}))}.register(&mut api);
    for kind in [EventKind::SessionStart,EventKind::AgentEnd,EventKind::ModelSelect,EventKind::SessionParked,EventKind::SessionResumed,EventKind::AgentStart,EventKind::Input,EventKind::SessionShutdown]{assert_eq!(api.registered.handlers[&kind].len(),1);}
    assert!(api.registered.entry_renderers.contains_key("cache-keepalive"));
}
