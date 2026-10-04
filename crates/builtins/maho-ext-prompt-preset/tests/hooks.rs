use maho_ext_api::{BuildSystemPromptOptions, EventKind, Extension, ExtensionApi, ExtensionSessionProfile, LoadedExtension, SourceInfo, EventBus, ExtensionRuntime};
use maho_ext_prompt_preset::index::{has_user_system_prompt, with_user_appends};

#[test]
fn registers_prompt_and_model_hooks() {
    let mut api = ExtensionApi::new(LoadedExtension::new("preset", "/tmp".into(), SourceInfo::default()), ExtensionSessionProfile::default(), EventBus::default(), ExtensionRuntime::default());
    maho_ext_prompt_preset::PromptPreset.register(&mut api);
    assert_eq!(api.registered.handlers[&EventKind::BeforeAgentStart].len(), 1);
    assert_eq!(api.registered.handlers[&EventKind::ModelSelect].len(), 1);
}
#[test]
fn custom_prompt_truthiness_and_append_preservation() {
    let mut options = BuildSystemPromptOptions::default();
    assert!(!has_user_system_prompt(&options));
    options.custom_prompt = Some(String::new());
    assert!(!has_user_system_prompt(&options));
    options.custom_prompt = Some(" ".into());
    assert!(has_user_system_prompt(&options));
    options.append_system_prompt = Some("\nfixture ".into());
    assert_eq!(with_user_appends("base".into(), &options), "base\n\n\nfixture ");
    options.append_system_prompt = Some(String::new());
    assert_eq!(with_user_appends("base".into(), &options), "base");
}
