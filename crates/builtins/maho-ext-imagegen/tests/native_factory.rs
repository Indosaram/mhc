use maho_ext_api::*;
use maho_ext_imagegen::ImageGen;

#[test]
fn client_factory_registers_real_context_tool_and_discovery_hooks() {
    let mut api=ExtensionApi::new(LoadedExtension::new("imagegen","/tmp".into(),SourceInfo::default()),ExtensionSessionProfile::default(),EventBus::default(),ExtensionRuntime::default());
    ImageGen::default().register(&mut api);
    let tool=&api.registered.tools.iter().find(|tool|tool.definition.name=="generate_image").expect("image tool").definition;
    assert_eq!(tool.exposure,Some(ToolExposure::Search));
    assert_eq!(tool.search_group.as_deref(),Some("imagegen"));
    assert!(tool.prompt_snippet.as_deref().is_some_and(|snippet|snippet.contains("png/jpeg/webp")),"deferred tool ships the pinned prompt snippet");
    assert_eq!(tool.parameters["required"],serde_json::json!(["prompt"]));
    assert_eq!(api.registered.handlers[&EventKind::ResourcesDiscover].len(),1);
    assert_eq!(api.registered.handlers[&EventKind::BeforeAgentStart].len(),1);
}
