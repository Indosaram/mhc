struct Actions;
impl maho_ext_api::ExtensionActions for Actions {
    fn send_message(&self,_:maho_ext_api::CustomMessage,_:maho_ext_api::SendMessageOptions)->Result<(),maho_ext_api::ExtensionFailure>{panic!("not used")}
    fn send_user_message(&self,_:maho_ext_api::UserMessageContent,_:maho_ext_api::SendUserMessageOptions)->Result<(),maho_ext_api::ExtensionFailure>{panic!("not used")}
    fn append_entry(&self,_:&str,_:Option<serde_json::Value>)->Result<(),maho_ext_api::ExtensionFailure>{panic!("not used during registration")}
    fn get_all_tools(&self)->Result<Vec<maho_ext_api::ToolInfo>,maho_ext_api::ExtensionFailure>{panic!("not used during registration")}
}
#[test]
fn native_todo_factory_registers_state_command_renderers_and_cleanup() {
    use maho_ext_api::EventKind;
    let directory=tempfile::tempdir().unwrap();
    let loaded=maho_ext_host::loader::load_extensions(vec![maho_ext_host::loader::NativeExtensionFactory {
        path:"builtin:todotools".into(),source_info:Default::default(),
        extension:Box::new(maho_ext_todotools::index::NativeTodotoolsExtension {actions:std::sync::Arc::new(Actions),copy_markdown:std::sync::Arc::new(|_|Box::pin(async {Ok(())})),widget_sender:tokio::sync::mpsc::unbounded_channel().0}),
    }],directory.path(),Default::default());
    assert!(loaded.errors.is_empty(),"{:?}",loaded.errors);
    let extension=&loaded.extensions[0];
    assert!(extension.tools.iter().any(|tool|tool.definition.name=="todo"));
    assert!(extension.tool_renderers.contains_key("todo"));
    assert!(extension.commands.iter().any(|command|command.name=="todo"));
    assert_eq!(extension.handlers[&EventKind::SessionStart].len(),2);
    assert_eq!(extension.handlers[&EventKind::SessionTree].len(),2);
    for event in [EventKind::MessageEnd,EventKind::BeforeAgentStart,EventKind::SessionShutdown] {assert_eq!(extension.handlers[&event].len(),1);}
}
