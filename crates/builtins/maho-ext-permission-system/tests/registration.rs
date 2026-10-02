use maho_ext_api::{Extension,ExtensionApi,LoadedExtension,SourceInfo,ExtensionSessionProfile,EventBus,ExtensionRuntime,EventKind};
use maho_ext_permission_system::PermissionSystem;
#[test]
fn registers_policy_flags_and_lifecycle(){
    let mut api=ExtensionApi::new(LoadedExtension::new("permission","/tmp".into(),SourceInfo::default()),ExtensionSessionProfile::default(),EventBus::default(),ExtensionRuntime::default());
    PermissionSystem.register(&mut api);
    assert_eq!(api.registered.flags.iter().map(|flag|flag.name.as_str()).collect::<Vec<_>>(),vec!["permission","permission-preset"]);
    for kind in [EventKind::SessionStart,EventKind::ToolCall,EventKind::SessionShutdown]{assert_eq!(api.registered.handlers[&kind].len(),1);}
}
