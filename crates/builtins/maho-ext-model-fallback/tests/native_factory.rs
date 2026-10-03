use maho_ext_api::*;
use maho_ext_model_fallback::ModelFallback;

#[test]
fn fallback_factory_registers_mutating_command_and_disable_flag(){
    let mut api=ExtensionApi::new(LoadedExtension::new("fallback","/tmp".into(),SourceInfo::default()),ExtensionSessionProfile::default(),EventBus::default(),ExtensionRuntime::default());
    ModelFallback.register(&mut api);
    assert!(api.registered.commands.contains_key("fallback"));
    assert_eq!(api.get_flag("no-model-fallback"),Some(FlagValue::Boolean(false)));
}

