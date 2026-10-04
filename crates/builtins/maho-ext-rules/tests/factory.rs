use maho_ext_api::{Extension,ExtensionApi,LoadedExtension,EventKind};

#[test]
fn native_factory_registers_commands_flags_and_real_injection_hooks() {
    let mut api=ExtensionApi::new(LoadedExtension::new("rules","/tmp".into(),Default::default()),Default::default(),Default::default(),Default::default());
    maho_ext_rules::Rules.register(&mut api);
    for name in ["rules","reload-rules"] { assert!(api.registered.commands.iter().any(|command|command.name==name)); }
    for kind in [EventKind::SessionStart,EventKind::SessionCompact,EventKind::BeforeAgentStart,EventKind::ToolResult] { assert_eq!(api.registered.handlers[&kind].len(),1); }
    assert_eq!(api.registered.flags.len(),2);
    assert!(api.registered.entry_renderers.contains_key(maho_ext_rule_activation::types::RULE_ACTIVATION_ENTRY_TYPE));
    assert!(api.registered.tools.is_empty());
}
