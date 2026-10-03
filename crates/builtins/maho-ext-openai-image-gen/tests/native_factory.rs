use maho_ext_api::*;
use maho_ext_openai_image_gen::OpenAiImageGen;

#[test]
fn native_factory_registers_effective_model_and_history_lifecycle() {
    let mut api=ExtensionApi::new(LoadedExtension::new("native-imagegen","/tmp".into(),SourceInfo::default()),ExtensionSessionProfile::default(),EventBus::default(),ExtensionRuntime::default());
    OpenAiImageGen.register(&mut api);
    for kind in [EventKind::SessionStart,EventKind::ModelSelect,EventKind::BeforeProviderRequest,EventKind::BeforeAgentStart,EventKind::MessageEnd,EventKind::SessionShutdown]{
        assert_eq!(api.registered.handlers[&kind].len(),1);
    }
}
