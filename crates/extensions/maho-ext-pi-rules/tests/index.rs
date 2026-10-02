use maho_ext_api::*;
#[test]
fn registers_four_injection_hooks_and_presence_flags(){
    let mut api=ExtensionApi::new(LoadedExtension::new("pi-rules","/fixture".into(),SourceInfo::default()),ExtensionSessionProfile::default(),EventBus::default(),ExtensionRuntime::default());
    maho_ext_pi_rules::index::register_rule_injection_hooks(&mut api);
    for kind in [EventKind::SessionStart,EventKind::SessionCompact,EventKind::BeforeAgentStart,EventKind::ToolResult]{assert_eq!(api.registered.handlers[&kind].len(),1);}
    assert_eq!(api.registered.handlers.len(),4);assert_eq!(api.registered.flags.len(),2);
}
