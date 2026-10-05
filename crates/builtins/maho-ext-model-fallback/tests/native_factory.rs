use maho_ext_api::*;
use maho_ext_model_fallback::ModelFallback;

#[test]
fn fallback_factory_registers_mutating_command_and_disable_flag(){
    let mut api=ExtensionApi::new(LoadedExtension::new("fallback","/tmp".into(),SourceInfo::default()),ExtensionSessionProfile::default(),EventBus::default(),ExtensionRuntime::default());
    ModelFallback{is_using_oauth:std::sync::Arc::new(|_|panic!("registration must not query model auth"))}.register(&mut api);
    assert!(api.registered.commands.iter().any(|command|command.name=="fallback"));
    assert_eq!(api.get_flag("no-model-fallback"),Some(FlagValue::Boolean(false)));
}

