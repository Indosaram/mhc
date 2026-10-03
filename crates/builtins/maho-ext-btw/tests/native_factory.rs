use maho_ext_api::*;
use maho_ext_btw::Btw;
#[test]
fn side_query_factory_registers_command_and_owned_cleanup_handlers(){
    let mut api=ExtensionApi::new(LoadedExtension::new("btw","/tmp".into(),SourceInfo::default()),ExtensionSessionProfile::default(),EventBus::default(),ExtensionRuntime::default());
    Btw{thinking_level:std::sync::Arc::new(|_|panic!("registration must not read session thinking"))}.register(&mut api);
    assert!(api.registered.commands.iter().any(|command|command.name=="btw"));
    for kind in [EventKind::SessionBeforeSwitch,EventKind::SessionBeforeFork,EventKind::SessionShutdown,EventKind::Input]{assert_eq!(api.registered.handlers[&kind].len(),1);}
}
