use maho_ext_api::*;
use maho_ext_builtin_loose::redraws::Redraws;
use std::sync::Arc;

#[test]
fn tui_command_registers_with_the_pinned_description(){
    let mut api=ExtensionApi::new(LoadedExtension::new("redraws","/tmp".into(),SourceInfo::default()),ExtensionSessionProfile::default(),EventBus::default(),ExtensionRuntime::default());
    Redraws{full_redraws:Arc::new(|_|0)}.register(&mut api);
    let command=api.registered.commands.iter().find(|command|command.name=="tui").expect("tui command");
    assert_eq!(command.description.as_deref(),Some("Show TUI stats"));
    assert!(command.argument_hint.is_none());
    assert!(api.registered.handlers.is_empty(),"the redraws builtin registers no lifecycle handlers");
}
